"""sutura/closing.py - Watertight closing for single-side 3D scans and reliefs.

Standalone module operating on numpy arrays (verts float64 Nx3, tris int Mx3).
Provides:
  - boundary_loops: ordered vertex-index loops from directed boundary edges
  - single_side_score: detector for single-sided open scans
  - relief_score: detector for roughly planar heightfield-like reliefs
  - poisson_close: screened Poisson surface reconstruction with adaptive depth
  - flat_back_close: extrusion to a flat offset back plane with planar cap
  - one_sided_hausdorff: fidelity metric measuring input-to-output distance
"""

from collections import defaultdict
import numpy as np

# Threshold constants
SINGLE_SIDE_DOMINANT_RATIO = 0.70  # Min fraction of boundary perimeter in dominant loop
SINGLE_SIDE_MIN_SPANNED_RATIO = 0.05  # Min ratio of loop vector area to mesh surface area

RELIEF_MAX_RMS_REL = 0.04  # Max plane-fit RMS relative to bbox diagonal
RELIEF_MIN_NORMAL_ALIGN = 0.70  # Min fraction of face normals pointing toward plane normal
RELIEF_MIN_ONE_SIDED = 0.90  # Min fraction of vertices on one side of plane

DEFAULT_FLAT_BACK_THICKNESS_REL = 0.005  # Default thickness = 0.005 * bbox_diag
POISSON_DEFAULT_SCALE = 2.0  # Poisson octree scale padding to ensure bubble closes watertight


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


def boundary_loops(verts, tris):
    """Return ordered boundary loops as lists of vertex indices.

    Follows directed half-edges induced by triangle winding so loop orientation
    is consistent with outward surface normals. Sorted descending by length.
    """
    tris = np.asarray(tris, dtype=np.int64)
    if len(tris) == 0:
        return []

    # Each triangle (v0, v1, v2) has directed edges: (v0->v1), (v1->v2), (v2->v0)
    edges = np.vstack([tris[:, [0, 1]], tris[:, [1, 2]], tris[:, [2, 0]]])
    max_v = int(tris.max()) + 1

    # Undirected keys to find boundary edges (count == 1)
    sorted_edges = np.sort(edges, axis=1)
    keys = sorted_edges[:, 0] * max_v + sorted_edges[:, 1]
    uniq, counts = np.unique(keys, return_counts=True)
    b_keys = set(uniq[counts == 1])
    if not b_keys:
        return []

    # Map directed boundary edges
    adj = defaultdict(list)
    for e in edges:
        k = min(e[0], e[1]) * max_v + max(e[0], e[1])
        if k in b_keys:
            adj[int(e[0])].append(int(e[1]))

    loops = []
    visited_edges = set()

    # Trace loops
    for u in list(adj.keys()):
        for v in adj[u]:
            if (u, v) in visited_edges:
                continue
            loop = [u]
            visited_edges.add((u, v))
            cur = v
            while cur != u:
                loop.append(cur)
                next_nodes = [nxt for nxt in adj[cur] if (cur, nxt) not in visited_edges]
                if not next_nodes:
                    break
                nxt = next_nodes[0]
                visited_edges.add((cur, nxt))
                cur = nxt
            if len(loop) >= 3:
                loops.append(loop)

    # Sort descending by vertex count
    loops.sort(key=len, reverse=True)
    return loops


def _loop_stats(verts, loop):
    """Compute perimeter, 3D vector spanned area, and centroid of a loop."""
    pts = verts[loop]
    diffs = pts[1:] - pts[:-1]
    last_diff = pts[0] - pts[-1]
    perim = float(np.sum(np.linalg.norm(diffs, axis=1)) + np.linalg.norm(last_diff))

    # 3D vector area via Stokes' theorem / polygon cross-product sum:
    # A = 0.5 * sum(p_i x p_{i+1})
    p_curr = pts
    p_next = np.roll(pts, -1, axis=0)
    vec_area = 0.5 * np.sum(np.cross(p_curr, p_next), axis=0)
    spanned_area = float(np.linalg.norm(vec_area))
    centroid = np.mean(pts, axis=0)
    return perim, spanned_area, vec_area, centroid


def _surface_area(verts, tris):
    """Total surface area of mesh triangles."""
    if len(tris) == 0:
        return 0.0
    v0 = verts[tris[:, 0]]
    v1 = verts[tris[:, 1]]
    v2 = verts[tris[:, 2]]
    cross = np.cross(v1 - v0, v2 - v0)
    return float(np.sum(0.5 * np.linalg.norm(cross, axis=1)))


def single_side_score(verts, tris):
    """Score in [0..1] and info dict. High when there is one dominant open boundary
    loop whose spanned area is a significant fraction of the mesh surface area.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)

    loops = boundary_loops(verts, tris)
    if not loops:
        return 0.0, {
            'n_loops': 0,
            'dominant_loop_len': 0,
            'dominant_ratio': 0.0,
            'spanned_area_ratio': 0.0,
            'reason': 'closed',
        }

    total_s = _surface_area(verts, tris)
    if total_s <= 1e-12:
        return 0.0, {'n_loops': len(loops), 'reason': 'zero_area'}

    perims = []
    spanned_areas = []
    for l in loops:
        p, a, _, _ = _loop_stats(verts, l)
        perims.append(p)
        spanned_areas.append(a)

    total_perim = sum(perims)
    if total_perim <= 1e-12:
        return 0.0, {'n_loops': len(loops), 'reason': 'zero_perimeter'}

    dominant_idx = int(np.argmax(perims))
    dom_perim = perims[dominant_idx]
    dom_spanned = spanned_areas[dominant_idx]
    dom_loop = loops[dominant_idx]

    dom_ratio = dom_perim / total_perim
    spanned_ratio = dom_spanned / total_s

    # Dominance factor: ramps from 0 at 0.40 to 1 at 0.85
    s_dom = float(np.clip((dom_ratio - 0.40) / 0.45, 0.0, 1.0))
    # Spanned area factor: ramps from 0 at 0.02 to 1 at 0.25
    s_span = float(np.clip((spanned_ratio - 0.02) / 0.23, 0.0, 1.0))

    score = round(s_dom * s_span, 4)
    info = {
        'n_loops': len(loops),
        'dominant_loop_len': len(dom_loop),
        'dominant_ratio': round(dom_ratio, 4),
        'spanned_area_ratio': round(spanned_ratio, 4),
        'dominant_loop': dom_loop,
    }
    return score, info


def relief_score(verts, tris):
    """Score in [0..1] and info dict. High when dominant loop is roughly planar
    (plane-fit RMS small relative to bbox diag) and surface is mostly on one side
    of that plane (heightfield-like).
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)

    loops = boundary_loops(verts, tris)
    if not loops:
        return 0.0, {'reason': 'closed'}

    # Find dominant loop
    perims = [_loop_stats(verts, l)[0] for l in loops]
    dom_loop = loops[int(np.argmax(perims))]
    pts = verts[dom_loop]

    diag = float(np.linalg.norm(verts.max(axis=0) - verts.min(axis=0)))
    if diag <= 1e-12:
        return 0.0, {'reason': 'degenerate_bbox'}

    # Plane fit to dominant loop via SVD
    c = np.mean(pts, axis=0)
    centered = pts - c
    _, _, vh = np.linalg.svd(centered)
    n = vh[2]

    # Plane-fit RMS
    residuals = np.dot(centered, n)
    rms = float(np.sqrt(np.mean(residuals ** 2)))
    rms_rel = rms / diag

    # Orient normal toward surface
    v0 = verts[tris[:, 0]]
    v1 = verts[tris[:, 1]]
    v2 = verts[tris[:, 2]]
    face_normals = np.cross(v1 - v0, v2 - v0)
    fn_sum = np.sum(face_normals, axis=0)
    if np.dot(fn_sum, n) < 0:
        n = -n

    # Check heightfield properties
    all_proj = np.dot(verts - c, n)
    one_sided_ratio = float(np.mean(all_proj >= -0.02 * diag))

    fn_norm = np.linalg.norm(face_normals, axis=1, keepdims=True)
    valid_fn = fn_norm[:, 0] > 1e-12
    if np.any(valid_fn):
        unit_fn = face_normals[valid_fn] / fn_norm[valid_fn]
        normal_align_ratio = float(np.mean(np.dot(unit_fn, n) > 0.0))
    else:
        normal_align_ratio = 0.0

    # Planarity factor: 1 when rms_rel <= 0.01, 0 when rms_rel >= 0.06
    s_planar = float(np.clip((0.06 - rms_rel) / 0.05, 0.0, 1.0))
    # One-sidedness factor: 1 when one_sided_ratio >= 0.95
    s_onesided = float(np.clip((one_sided_ratio - 0.75) / 0.20, 0.0, 1.0))
    # Normal alignment factor: 1 when normal_align_ratio >= 0.85
    s_normal = float(np.clip((normal_align_ratio - 0.60) / 0.25, 0.0, 1.0))

    score = round(s_planar * s_onesided * s_normal, 4)
    info = {
        'plane_normal': n.tolist(),
        'plane_center': c.tolist(),
        'rms_rel': round(rms_rel, 5),
        'one_sided_ratio': round(one_sided_ratio, 4),
        'normal_align_ratio': round(normal_align_ratio, 4),
        'dominant_loop': dom_loop,
    }
    return score, info


def _ear_clip_2d(pts):
    """2D ear-clipping triangulation for simple polygons (convex or non-convex).

    Returns triangle index array (K, 3).
    """
    n = len(pts)
    if n < 3:
        return np.zeros((0, 3), dtype=np.int32)
    indices = list(range(n))

    # Determine winding from signed 2D area
    area = 0.5 * np.sum(pts[:, 0] * np.roll(pts[:, 1], -1) - pts[:, 1] * np.roll(pts[:, 0], -1))
    if area < 0:
        indices.reverse()

    def is_convex(p0, p1, p2):
        return (p1[0] - p0[0]) * (p2[1] - p0[1]) - (p1[1] - p0[1]) * (p2[0] - p0[0]) > 1e-12

    def pt_in_tri(p, a, b, c):
        def sign(p1, p2, p3):
            return (p1[0] - p3[0]) * (p2[1] - p3[1]) - (p2[0] - p3[0]) * (p1[1] - p3[1])
        d1 = sign(p, a, b)
        d2 = sign(p, b, c)
        d3 = sign(p, c, a)
        has_neg = (d1 < -1e-12) or (d2 < -1e-12) or (d3 < -1e-12)
        has_pos = (d1 > 1e-12) or (d2 > 1e-12) or (d3 > 1e-12)
        return not (has_neg and has_pos)

    triangles = []
    max_iter = n * n * 2
    count = 0
    while len(indices) > 3 and count < max_iter:
        count += 1
        ear_found = False
        L = len(indices)
        for i in range(L):
            prev_idx = indices[(i - 1) % L]
            curr_idx = indices[i]
            next_idx = indices[(i + 1) % L]

            p0, p1, p2 = pts[prev_idx], pts[curr_idx], pts[next_idx]
            if not is_convex(p0, p1, p2):
                continue

            # Ear test: no other vertex inside triangle
            is_ear = True
            for j in range(L):
                if j in ((i - 1) % L, i, (i + 1) % L):
                    continue
                if pt_in_tri(pts[indices[j]], p0, p1, p2):
                    is_ear = False
                    break

            if is_ear:
                triangles.append([prev_idx, curr_idx, next_idx])
                indices.pop(i)
                ear_found = True
                break

        if not ear_found:
            # Fallback for degenerate loops: clip first triangle
            triangles.append([indices[0], indices[1], indices[2]])
            indices.pop(1)

    if len(indices) == 3:
        triangles.append([indices[0], indices[1], indices[2]])

    return np.array(triangles, dtype=np.int32)


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


def poisson_close(verts, tris, depth=None, tmpdir=None):
    """Close single-sided mesh watertight using PyMeshLab Screened Poisson reconstruction.

    Ensures outward consistent normals, adapts depth (8..11), uses scale padding
    so reconstruction closes inside bounding volume, and keeps largest component.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)

    if len(verts) < 3 or len(tris) == 0:
        return verts, tris, {
            'watertight': False,
            'faces': len(tris),
            'error': 'Empty or degenerate input mesh',
            'notes': [],
        }

    try:
        import pymeshlab as ml

        # Adaptive depth from vertex count if None
        if depth is None:
            n_v = len(verts)
            if n_v < 5_000:
                depth = 8
            elif n_v < 25_000:
                depth = 9
            elif n_v < 100_000:
                depth = 10
            else:
                depth = 11
        depth = int(np.clip(depth, 8, 11))

        in_v, in_t = _referenced_only(verts, tris)

        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=in_v, face_matrix=np.asarray(in_t, np.int32)))

        # 1. Coherently orient faces
        ms.apply_filter('meshing_re_orient_faces_coherently')
        ms.apply_filter('compute_normal_per_face')

        # 2. Check outward orientation vs centroid and invert if inward
        cur = ms.current_mesh()
        cur_v = cur.vertex_matrix()
        cur_f = cur.face_matrix()
        cur_fn = cur.face_normal_matrix()
        face_centers = cur_v[cur_f].mean(axis=1)
        centroid = cur_v.mean(axis=0)
        if np.sum((face_centers - centroid) * cur_fn) < 0:
            ms.apply_filter('meshing_invert_face_orientation')
            ms.apply_filter('compute_normal_per_face')

        # 3. Compute per-vertex normals
        ms.apply_filter('compute_normal_per_vertex')

        # 4. Screened Poisson reconstruction with scale=POISSON_DEFAULT_SCALE
        ms.apply_filter(
            'generate_surface_reconstruction_screened_poisson',
            depth=depth,
            scale=POISSON_DEFAULT_SCALE,
        )
        # Ensure any boundary opening from octree domain clipping is closed
        ms.apply_filter('meshing_close_holes', maxholesize=10000)

        out_mesh = ms.current_mesh()
        out_v = np.asarray(out_mesh.vertex_matrix(), dtype=np.float64)
        out_t = np.asarray(out_mesh.face_matrix(), dtype=np.int32)

        # 5. Keep largest connected component
        out_v, out_t = _keep_largest_component(out_v, out_t)

        # 6. Validity check
        is_wt, err_msg = _check_validity(out_v, out_t)

        report = {
            'faces': len(out_t),
            'watertight': is_wt,
            'depth': depth,
            'notes': ['back surface was estimated (Poisson)'],
        }
        if not is_wt:
            report['error'] = err_msg

        return out_v, out_t, report

    except Exception as e:
        return verts, tris, {
            'watertight': False,
            'faces': len(tris),
            'error': str(e),
            'notes': [],
        }


def flat_back_close(verts, tris, thickness=None):
    """Close relief-like mesh watertight with a flat back plane and side walls.

    - Dominant loop defines the outline.
    - Minor secondary holes are closed via pymeshlab.
    - Back plane is set through loop centroid shifted along -n to
      (min over ALL vertices of n·(x-c)) minus thickness.
    - Default thickness = 0.005 * bbox_diag.
    - Side walls bridge the dominant loop to its back-plane projection.
    - Back cap triangulated via 2D ear clipping.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)

    if len(verts) < 3 or len(tris) == 0:
        return verts, tris, {
            'watertight': False,
            'faces': len(tris),
            'error': 'Empty or degenerate input mesh',
            'notes': [],
        }

    try:
        import pymeshlab as ml

        in_v, in_t = _referenced_only(verts, tris)
        loops = boundary_loops(in_v, in_t)
        if not loops:
            is_wt, _ = _check_validity(in_v, in_t)
            return in_v, in_t, {
                'watertight': is_wt,
                'faces': len(in_t),
                'thickness': 0.0,
                'notes': ['mesh is already closed'],
            }

        # Find dominant loop
        perims = [_loop_stats(in_v, l)[0] for l in loops]
        dom_idx = int(np.argmax(perims))
        dom_loop = loops[dom_idx]
        dom_len = len(dom_loop)

        # Close any other smaller holes using pymeshlab
        other_loops = [loops[i] for i in range(len(loops)) if i != dom_idx]
        if other_loops:
            max_small_hole_len = max(len(l) for l in other_loops)
            # Only run if small holes are strictly smaller than dominant loop
            if max_small_hole_len < dom_len:
                ms = ml.MeshSet()
                ms.add_mesh(ml.Mesh(vertex_matrix=in_v, face_matrix=np.asarray(in_t, np.int32)))
                ms.apply_filter('meshing_close_holes', maxholesize=max_small_hole_len + 1)
                cur = ms.current_mesh()
                in_v = np.asarray(cur.vertex_matrix(), dtype=np.float64)
                in_t = np.asarray(cur.face_matrix(), dtype=np.int32)
                # Re-extract dominant loop on hole-closed mesh
                loops = boundary_loops(in_v, in_t)
                if not loops:
                    is_wt, _ = _check_validity(in_v, in_t)
                    return in_v, in_t, {
                        'watertight': is_wt,
                        'faces': len(in_t),
                        'thickness': 0.0,
                        'notes': ['flat back added'],
                    }
                perims = [_loop_stats(in_v, l)[0] for l in loops]
                dom_loop = loops[int(np.argmax(perims))]

        pts = in_v[dom_loop]
        c = np.mean(pts, axis=0)
        diag = float(np.linalg.norm(in_v.max(axis=0) - in_v.min(axis=0)))
        if diag <= 1e-12:
            raise ValueError('Degenerate bounding box')

        # Fit plane to dominant loop via SVD
        centered = pts - c
        _, _, vh = np.linalg.svd(centered)
        n = vh[2]

        # Orient n towards surface front
        v0 = in_v[in_t[:, 0]]
        v1 = in_v[in_t[:, 1]]
        v2 = in_v[in_t[:, 2]]
        face_normals = np.cross(v1 - v0, v2 - v0)
        if np.dot(np.sum(face_normals, axis=0), n) < 0:
            n = -n

        # Back plane offset:
        # min over ALL vertices of n·(x-c) minus thickness
        if thickness is None:
            thickness_val = DEFAULT_FLAT_BACK_THICKNESS_REL * diag
        else:
            thickness_val = float(thickness)

        all_proj = np.dot(in_v - c, n)
        min_proj = float(np.min(all_proj))
        h_back = min_proj - thickness_val

        # Project dominant loop vertices to back plane
        loop_pts = in_v[dom_loop]
        loop_proj = loop_pts - (np.dot(loop_pts - c, n)[:, None] - h_back) * n

        n_orig_v = len(in_v)
        n_loop = len(dom_loop)
        proj_indices = np.arange(n_orig_v, n_orig_v + n_loop)
        all_verts = np.vstack([in_v, loop_proj])

        # Side walls: connect original boundary edges to projected edges
        # In dom_loop, edge is (dom_loop[i] -> dom_loop[i+1]).
        # Side wall triangles with outward normals:
        side_tris = []
        for i in range(n_loop):
            i_next = (i + 1) % n_loop
            v_curr = dom_loop[i]
            v_next = dom_loop[i_next]
            p_curr = proj_indices[i]
            p_next = proj_indices[i_next]

            side_tris.append([v_curr, p_curr, v_next])
            side_tris.append([v_next, p_curr, p_next])

        # Planar back cap via 2D ear clipping
        # Build 2D orthonormal basis (u_axis, v_axis) on plane
        u_axis = vh[0]
        u_axis = u_axis - np.dot(u_axis, n) * n
        u_axis = u_axis / np.linalg.norm(u_axis)
        v_axis = np.cross(n, u_axis)

        pts_2d = np.column_stack([np.dot(loop_proj - c, u_axis), np.dot(loop_proj - c, v_axis)])
        cap_local = _ear_clip_2d(pts_2d)

        # Check cap winding: outward normal must point in -n
        v0_cap = loop_proj[cap_local[0, 0]]
        v1_cap = loop_proj[cap_local[0, 1]]
        v2_cap = loop_proj[cap_local[0, 2]]
        cap_normal = np.cross(v1_cap - v0_cap, v2_cap - v0_cap)
        if np.dot(cap_normal, -n) > 0:
            cap_tris = proj_indices[cap_local]
        else:
            cap_tris = proj_indices[cap_local[:, [0, 2, 1]]]

        all_tris = np.vstack([in_t, side_tris, cap_tris])

        is_wt, err_msg = _check_validity(all_verts, all_tris)

        report = {
            'faces': len(all_tris),
            'watertight': is_wt,
            'thickness': thickness_val,
            'notes': ['flat back added'],
        }
        if not is_wt:
            report['error'] = err_msg

        return all_verts, all_tris, report

    except Exception as e:
        return verts, tris, {
            'watertight': False,
            'faces': len(tris),
            'error': str(e),
            'notes': [],
        }


def one_sided_hausdorff(in_v, in_t, out_v, out_t, samples=100000):
    """Distance from input surface samples to output, relative to input bbox diagonal: (max, mean).

    Measures fidelity of input scan preservation on the closed output mesh.
    """
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
        hd.add_mesh(ml.Mesh(vertex_matrix=in_v, face_matrix=in_t))  # mesh 0: input
        hd.add_mesh(ml.Mesh(vertex_matrix=out_v, face_matrix=out_t))  # mesh 1: output

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
