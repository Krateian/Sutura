"""sutura/proxy_repair.py - Proxy-template repair for heavily broken meshes.

Standalone module for reconstructing clean, watertight models from meshes with
severe defects (open holes, self-intersections, non-manifold edges, debris).
Pipeline:
  1. Build a coarse watertight proxy (uniform resampling / marching cubes / level-set).
  2. Detect healthy original surface regions (filtering defects and dilating by 1-2 rings).
  3. Refine proxy toward healthy mesh resolution.
  4. Project proxy vertices onto healthy original surface with distance and normal gates.
  5. Check for self-intersections and rollback intersecting vertices to proxy positions.
  6. Honest manifold3d validity check.
"""

import numpy as np

# Threshold constants
DEFAULT_VOXEL_DIVISOR = 128
MIN_GRID_CELLS = 32
MAX_GRID_CELLS = 256

DEFAULT_DILATION_RINGS = 2
MIN_DEBRIS_COMPONENT_FACES = 20

PROJECTION_DIST_FACTOR = 2.0  # max projection distance = k * voxel
PROJECTION_MIN_NORMAL_DOT = 0.5  # proxy normal vs healthy normal alignment

SI_FACE_CAP = 200_000  # cap self-intersection checks above 200k faces
MAX_OUTPUT_FACE_CAP = 1_500_000  # cap output face count to prevent runaway refinement


def _referenced_only(verts, tris):
    """Drop unreferenced vertices and remap triangle indices."""
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    if len(tris) == 0:
        return np.zeros((0, 3), dtype=np.float64), np.zeros((0, 3), dtype=np.int64)
    used = np.unique(tris)
    remap = np.full(len(verts), -1, dtype=np.int64)
    remap[used] = np.arange(len(used))
    return verts[used], remap[tris]


def _check_validity(verts, tris):
    """Strict validity check: 2-manifold, 0 boundary edges, and manifold3d construction."""
    if len(verts) < 4 or len(tris) < 4:
        return False, "Too few vertices or faces"

    # Edge counts check
    edges = np.sort(np.vstack([tris[:, [0, 1]], tris[:, [1, 2]], tris[:, [2, 0]]]), axis=1)
    n = int(edges.max()) + 1
    keys = edges[:, 0].astype(np.int64) * n + edges[:, 1]
    uniq, counts = np.unique(keys, return_counts=True)
    if not np.all(counts == 2):
        n_open = int(np.sum(counts == 1))
        n_nm = int(np.sum(counts > 2))
        return False, f"Not 2-manifold: {n_open} boundary edges, {n_nm} non-manifold edges"

    try:
        import manifold3d
        mf = manifold3d.Manifold(mesh=manifold3d.Mesh(
            vert_properties=np.asarray(verts, np.float64),
            tri_verts=np.asarray(tris, np.int32),
        ))
        if mf.status() != manifold3d.Error.NoError:
            return False, f"manifold3d status: {mf.status()}"
        if mf.is_empty():
            return False, "manifold3d is empty"
    except Exception as e:
        return False, f"manifold3d check failed: {e}"

    return True, ""


def _keep_largest_component(verts, tris):
    """Keep only the largest connected component of faces."""
    if len(tris) == 0:
        return verts, tris
    import scipy.sparse as sp
    from scipy.sparse.csgraph import connected_components

    n_v = len(verts)
    i = np.concatenate([tris[:, 0], tris[:, 1], tris[:, 2]])
    j = np.concatenate([tris[:, 1], tris[:, 2], tris[:, 0]])
    data = np.ones(len(i), dtype=bool)
    adj = sp.coo_matrix((data, (i, j)), shape=(n_v, n_v))
    n_components, labels = connected_components(adj, directed=False)
    if n_components <= 1:
        return verts, tris

    face_labels = labels[tris[:, 0]]
    uniq, counts = np.unique(face_labels, return_counts=True)
    largest_label = uniq[np.argmax(counts)]
    keep_faces = tris[face_labels == largest_label]
    used_verts = np.unique(keep_faces)
    remap = np.full(n_v, -1, dtype=np.int32)
    remap[used_verts] = np.arange(len(used_verts))
    return verts[used_verts], remap[keep_faces]


def build_proxy(verts, tris, voxel=None):
    """Construct a watertight, coarse proxy surface for the input mesh.

    Attempts in order:
      (a) PyMeshLab generate_resampled_uniform_mesh (absdist=False, offset=0.0)
      (a2) PyMeshLab generate_resampled_uniform_mesh (absdist=True, offset=voxel)
      (b) PyMeshLab generate_alpha_wrap fallback

    Voxel size is clamped so grid dimensions stay within [32, 256].
    Returns (pv, pt, info_dict).
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)

    if len(verts) < 3 or len(tris) == 0:
        raise ValueError("Cannot build proxy from empty or degenerate mesh")

    bbox_span = verts.max(axis=0) - verts.min(axis=0)
    max_dim = float(np.max(bbox_span))
    diag = float(np.linalg.norm(bbox_span))
    if max_dim <= 1e-12:
        raise ValueError("Degenerate bounding box")

    if voxel is None:
        voxel = diag / DEFAULT_VOXEL_DIVISOR

    # Clamp voxel grid to [MIN_GRID_CELLS, MAX_GRID_CELLS]
    if max_dim / voxel > MAX_GRID_CELLS:
        voxel = max_dim / MAX_GRID_CELLS
    if max_dim / voxel < MIN_GRID_CELLS:
        voxel = max_dim / MIN_GRID_CELLS

    import pymeshlab as ml

    in_v, in_t = _referenced_only(verts, tris)

    # Strategy 1: Signed resampling with uniform voxel grid (absdist=False, offset=0)
    try:
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=in_v, face_matrix=np.asarray(in_t, np.int32)))
        ms.apply_filter(
            'generate_resampled_uniform_mesh',
            cellsize=ml.PureValue(voxel),
            offset=ml.PureValue(0.0),
            absdist=False,
        )
        out = ms.current_mesh()
        pv = np.asarray(out.vertex_matrix(), dtype=np.float64)
        pt = np.asarray(out.face_matrix(), dtype=np.int32)
        pv, pt = _keep_largest_component(pv, pt)
        is_wt, _ = _check_validity(pv, pt)
        if is_wt and len(pt) >= 4:
            return pv, pt, {
                'method': 'uniform_resampled_signed',
                'voxel': voxel,
                'faces': len(pt),
            }
    except Exception:
        pass

    # Strategy 2: Unsigned distance offset resampling (absdist=True, offset=voxel)
    try:
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=in_v, face_matrix=np.asarray(in_t, np.int32)))
        ms.apply_filter(
            'generate_resampled_uniform_mesh',
            cellsize=ml.PureValue(voxel),
            offset=ml.PureValue(voxel),
            absdist=True,
        )
        out = ms.current_mesh()
        pv = np.asarray(out.vertex_matrix(), dtype=np.float64)
        pt = np.asarray(out.face_matrix(), dtype=np.int32)
        pv, pt = _keep_largest_component(pv, pt)
        is_wt, _ = _check_validity(pv, pt)
        if is_wt and len(pt) >= 4:
            return pv, pt, {
                'method': 'uniform_resampled_unsigned',
                'voxel': voxel,
                'faces': len(pt),
            }
    except Exception:
        pass

    # Strategy 3: Alpha wrap fallback (guaranteed watertight 2-manifold)
    try:
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=in_v, face_matrix=np.asarray(in_t, np.int32)))
        alpha = voxel * 2.0
        offset = voxel / 4.0
        ms.apply_filter(
            'generate_alpha_wrap',
            alpha=ml.PureValue(alpha),
            offset=ml.PureValue(offset),
        )
        out = ms.current_mesh()
        pv = np.asarray(out.vertex_matrix(), dtype=np.float64)
        pt = np.asarray(out.face_matrix(), dtype=np.int32)
        pv, pt = _keep_largest_component(pv, pt)
        is_wt, _ = _check_validity(pv, pt)
        if is_wt and len(pt) >= 4:
            return pv, pt, {
                'method': 'alpha_wrap',
                'voxel': voxel,
                'faces': len(pt),
            }
    except Exception as e:
        raise RuntimeError(f"Failed to build watertight proxy: {e}")

    raise RuntimeError("All proxy generation strategies failed to produce a valid 2-manifold")


def health_mask(verts, tris, dilation_rings=DEFAULT_DILATION_RINGS, si_face_cap=SI_FACE_CAP):
    """Compute per-face health boolean mask.

    False for:
      - Faces touching boundary edges or non-manifold edges.
      - Small disconnected debris components (< 20 faces).
      - Self-intersecting faces (capped at 200k faces).
      - Dilated by 1-2 rings of neighboring faces.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    n_faces = len(tris)
    if n_faces == 0:
        return np.zeros(0, dtype=bool)

    healthy = np.ones(n_faces, dtype=bool)
    max_v = int(tris.max()) + 1

    # 1. Boundary & Non-manifold edges
    edges = np.vstack([tris[:, [0, 1]], tris[:, [1, 2]], tris[:, [2, 0]]])
    sorted_edges = np.sort(edges, axis=1)
    keys = sorted_edges[:, 0] * max_v + sorted_edges[:, 1]
    uniq, counts = np.unique(keys, return_counts=True)
    bad_keys = set(uniq[counts != 2])

    bad_face_indices = set()
    for f_idx, tri in enumerate(tris):
        e0 = min(tri[0], tri[1]) * max_v + max(tri[0], tri[1])
        e1 = min(tri[1], tri[2]) * max_v + max(tri[1], tri[2])
        e2 = min(tri[2], tri[0]) * max_v + max(tri[2], tri[0])
        if e0 in bad_keys or e1 in bad_keys or e2 in bad_keys:
            bad_face_indices.add(f_idx)

    # 2. Debris components
    import scipy.sparse as sp
    from scipy.sparse.csgraph import connected_components

    edge_to_faces = {}
    for f_idx, tri in enumerate(tris):
        for e in [
            (min(tri[0], tri[1]), max(tri[0], tri[1])),
            (min(tri[1], tri[2]), max(tri[1], tri[2])),
            (min(tri[2], tri[0]), max(tri[2], tri[0])),
        ]:
            edge_to_faces.setdefault(e, []).append(f_idx)

    fi, fj = [], []
    for f_list in edge_to_faces.values():
        if len(f_list) == 2:
            fi.extend([f_list[0], f_list[1]])
            fj.extend([f_list[1], f_list[0]])

    if fi:
        adj = sp.coo_matrix((np.ones(len(fi), dtype=bool), (fi, fj)), shape=(n_faces, n_faces))
        _, labels = connected_components(adj, directed=False)
        uniq_l, counts_l = np.unique(labels, return_counts=True)
        small_labels = set(uniq_l[counts_l < MIN_DEBRIS_COMPONENT_FACES])
        for f_idx, l in enumerate(labels):
            if l in small_labels:
                bad_face_indices.add(f_idx)

    # 3. Self-intersections via PyMeshLab
    if n_faces <= si_face_cap:
        try:
            import pymeshlab as ml
            ms = ml.MeshSet()
            ms.add_mesh(ml.Mesh(vertex_matrix=verts, face_matrix=np.asarray(tris, np.int32)))
            ms.apply_filter('compute_selection_by_self_intersections_per_face')
            si_sel = ms.current_mesh().face_selection_array()
            for f_idx in np.where(si_sel)[0]:
                bad_face_indices.add(int(f_idx))
        except Exception:
            pass

    for idx in bad_face_indices:
        healthy[idx] = False

    # 4. Dilate unhealthy set by rings
    v_to_faces = [[] for _ in range(max_v)]
    for f_idx, tri in enumerate(tris):
        v_to_faces[tri[0]].append(f_idx)
        v_to_faces[tri[1]].append(f_idx)
        v_to_faces[tri[2]].append(f_idx)

    for _ in range(dilation_rings):
        unhealthy_v = set()
        for f_idx in np.where(~healthy)[0]:
            tri = tris[f_idx]
            unhealthy_v.add(tri[0])
            unhealthy_v.add(tri[1])
            unhealthy_v.add(tri[2])
        for v in unhealthy_v:
            for f_idx in v_to_faces[v]:
                healthy[f_idx] = False

    return healthy


def one_sided_hausdorff(in_v, in_t, out_v, out_t, samples=100000):
    """Distance from healthy input surface samples to output surface relative to bbox diag: (max, mean)."""
    in_v = np.asarray(in_v, dtype=np.float64)
    in_t = np.asarray(in_t, dtype=np.int32)
    out_v = np.asarray(out_v, dtype=np.float64)
    out_t = np.asarray(out_t, dtype=np.int32)

    if len(in_t) == 0 or len(out_t) == 0:
        return None, None

    in_v, in_t = _referenced_only(in_v, in_t)
    out_v, out_t = _referenced_only(out_v, out_t)

    diag = float(np.linalg.norm(in_v.max(axis=0) - in_v.min(axis=0)))
    if diag <= 1e-12:
        return None, None

    try:
        import pymeshlab as ml

        hd = ml.MeshSet()
        hd.add_mesh(ml.Mesh(vertex_matrix=in_v, face_matrix=in_t))
        hd.add_mesh(ml.Mesh(vertex_matrix=out_v, face_matrix=out_t))

        r = hd.apply_filter(
            'get_hausdorff_distance',
            sampledmesh=0,
            targetmesh=1,
            samplevert=True,
            sampleface=True,
            samplenum=samples,
            maxdist=ml.PercentageValue(100),
        )
        max_d = float(r.get('max') or 0.0) / diag
        mean_d = float(r.get('mean') or 0.0) / diag
        return round(max_d, 6), round(mean_d, 6)

    except Exception:
        return None, None


def proxy_template_repair(verts, tris, voxel=None, max_iters=3):
    """Repair broken mesh by proxy-template projection and collision rollback.

    Returns (repaired_v, repaired_t, report_dict).
    Never raises on bad input — returns (verts, tris, report with 'error').
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)

    if len(verts) < 3 or len(tris) == 0:
        return verts, tris, {
            'watertight': False,
            'output_faces': len(tris),
            'proxy_faces': 0,
            'projected_fraction': 0.0,
            'reverted_count': 0,
            'error': 'Empty or degenerate input mesh',
            'notes': [],
        }

    try:
        import trimesh
        import pymeshlab as ml

        # 1. Build watertight proxy
        pv, pt, proxy_info = build_proxy(verts, tris, voxel=voxel)
        proxy_faces_count = len(pt)
        used_voxel = proxy_info['voxel']

        # 2. Extract healthy original surface
        h_mask = health_mask(verts, tris, dilation_rings=DEFAULT_DILATION_RINGS)
        has_healthy = bool(np.any(h_mask))

        if not has_healthy:
            # Entire input was defective; return coarse proxy directly
            is_wt, _ = _check_validity(pv, pt)
            return pv, pt, {
                'watertight': is_wt,
                'proxy_faces': proxy_faces_count,
                'output_faces': len(pt),
                'projected_fraction': 0.0,
                'reverted_count': 0,
                'notes': ['regions without healthy source were reconstructed from a coarse proxy'],
            }

        healthy_v, healthy_t = _referenced_only(verts, tris[h_mask])

        # 3. Refine proxy toward healthy mesh resolution
        # Compute median edge length of healthy faces
        e0 = healthy_v[healthy_t[:, 1]] - healthy_v[healthy_t[:, 0]]
        e1 = healthy_v[healthy_t[:, 2]] - healthy_v[healthy_t[:, 1]]
        e2 = healthy_v[healthy_t[:, 0]] - healthy_v[healthy_t[:, 2]]
        lengths = np.concatenate([
            np.linalg.norm(e0, axis=1),
            np.linalg.norm(e1, axis=1),
            np.linalg.norm(e2, axis=1),
        ])
        target_edge_len = float(np.median(lengths))

        # Remesh proxy if target edge length is significantly finer than proxy
        if target_edge_len > 1e-6 and len(pt) < min(len(tris) * 2, MAX_OUTPUT_FACE_CAP // 2):
            try:
                ms_remesh = ml.MeshSet()
                ms_remesh.add_mesh(ml.Mesh(vertex_matrix=pv, face_matrix=np.asarray(pt, np.int32)))
                ms_remesh.apply_filter(
                    'meshing_isotropic_explicit_remeshing',
                    targetlen=ml.PureValue(target_edge_len),
                    iterations=3,
                )
                out_remesh = ms_remesh.current_mesh()
                candidate_pv = np.asarray(out_remesh.vertex_matrix(), dtype=np.float64)
                candidate_pt = np.asarray(out_remesh.face_matrix(), dtype=np.int32)
                candidate_pv, candidate_pt = _keep_largest_component(candidate_pv, candidate_pt)
                is_wt, _ = _check_validity(candidate_pv, candidate_pt)
                if is_wt and len(candidate_pt) <= MAX_OUTPUT_FACE_CAP:
                    pv, pt = candidate_pv, candidate_pt
            except Exception:
                pass  # Keep coarse proxy if remeshing fails

        # 4. Proximity & projection onto healthy original surface
        healthy_mesh = trimesh.Trimesh(vertices=healthy_v, faces=healthy_t, process=False)
        closest_pts, distances, triangle_indices = trimesh.proximity.closest_point(healthy_mesh, pv)

        proxy_mesh = trimesh.Trimesh(vertices=pv, faces=pt, process=False)
        proxy_normals = proxy_mesh.vertex_normals
        healthy_normals = healthy_mesh.face_normals[triangle_indices]

        normal_dots = np.sum(proxy_normals * healthy_normals, axis=1)
        max_dist = PROJECTION_DIST_FACTOR * used_voxel

        proj_mask = (distances < max_dist) & (normal_dots > PROJECTION_MIN_NORMAL_DOT)
        projected_pv = pv.copy()
        projected_pv[proj_mask] = closest_pts[proj_mask]

        projected_fraction = float(np.mean(proj_mask))

        # 5. Collision rollback loop (revert vertices in self-intersecting faces)
        total_reverted = 0
        if len(pt) <= SI_FACE_CAP:
            for _ in range(max_iters):
                ms_si = ml.MeshSet()
                ms_si.add_mesh(ml.Mesh(
                    vertex_matrix=projected_pv,
                    face_matrix=np.asarray(pt, np.int32),
                ))
                ms_si.apply_filter('compute_selection_by_self_intersections_per_face')
                si_faces = ms_si.current_mesh().face_selection_array()
                if not np.any(si_faces):
                    break
                bad_vert_indices = np.unique(pt[si_faces])
                reverted_now = np.sum(proj_mask[bad_vert_indices])
                if reverted_now == 0:
                    break
                # Revert to unprojected proxy position
                projected_pv[bad_vert_indices] = pv[bad_vert_indices]
                proj_mask[bad_vert_indices] = False
                total_reverted += int(reverted_now)

        # 6. Final validity check
        is_wt, err_msg = _check_validity(projected_pv, pt)
        if not is_wt:
            # If projection damaged manifoldness, safely fall back to unprojected proxy
            projected_pv = pv
            is_wt, err_msg = _check_validity(projected_pv, pt)

        # 7. One-sided Hausdorff (distance from healthy original surface to output)
        h_max, h_mean = one_sided_hausdorff(healthy_v, healthy_t, projected_pv, pt)

        report = {
            'watertight': is_wt,
            'proxy_faces': proxy_faces_count,
            'output_faces': len(pt),
            'projected_fraction': round(projected_fraction, 4),
            'reverted_count': total_reverted,
            'hausdorff_max': h_max,
            'hausdorff_mean': h_mean,
            'notes': ['regions without healthy source were reconstructed from a coarse proxy'],
        }
        if not is_wt:
            report['error'] = err_msg

        return projected_pv, pt, report

    except Exception as e:
        return verts, tris, {
            'watertight': False,
            'output_faces': len(tris),
            'proxy_faces': 0,
            'projected_fraction': 0.0,
            'reverted_count': 0,
            'error': str(e),
            'notes': [],
        }
