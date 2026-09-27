"""sutura/repeat_repair.py - Repeated-element repair for meshes with congruent features.

Repairs meshes containing repeated geometric features (knurls, gear teeth, tiles,
regular arrayed bumps) where one or a few copies are broken or missing.
Features:
  1. segment_elements: Feature-edge dihedral segmentation + PCA descriptor extraction.
  2. align: Robust rigid alignment via PCA orientation frame + point-to-point ICP.
  3. repair_repeat_manual (Method #12): User-specified source and target point transplant.
  4. detect_repetition: Fast regularity analysis for rotational or translational patterns.
  5. repair_repeat_auto (Method #11): Consensus-based predict-and-verify automated repair.
  6. Robust volumetric transplant using manifold3d CSG booleans (no fragile seam stitching).
"""

import numpy as np
import scipy.sparse as sp
from scipy.sparse.csgraph import connected_components
from scipy.spatial import cKDTree
import trimesh
import manifold3d

# Threshold constants
DEFAULT_FEATURE_ANGLE_DEG = 30.0
MIN_ELEMENT_FACES = 4
BASE_AREA_THRESHOLD_FRACTION = 0.05
OBB_EXPANSION_MARGIN = 0.10
OBB_MIN_EPSILON = 1e-3

DAMAGE_DEVIATION_MIN = 0.06
DAMAGE_DEVIATION_MAX = 0.85
REPETITION_MIN_MEMBERS = 3
REPETITION_SIMILARITY_TOL = 0.30


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


def _build_manifold(verts, tris):
    """Build a manifold3d Manifold from verts and tris, verifying validity."""
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int32)
    if len(verts) < 4 or len(tris) < 4:
        return None, "Mesh has too few vertices or triangles"

    try:
        mf = manifold3d.Manifold(mesh=manifold3d.Mesh(
            vert_properties=verts,
            tri_verts=tris,
        ))
        if mf.status() != manifold3d.Error.NoError:
            return None, f"manifold3d error status: {mf.status()}"
        if mf.is_empty():
            return None, "manifold3d solid is empty"
        return mf, ""
    except Exception as e:
        return None, f"Failed to build manifold3d solid: {e}"


def _compute_obb(pts, margin=OBB_EXPANSION_MARGIN, epsilon=OBB_MIN_EPSILON):
    """Compute oriented bounding box (OBB) parameters (center, extents, rotation frame)."""
    pts = np.asarray(pts, dtype=np.float64)
    c = np.mean(pts, axis=0)
    cov = (pts - c).T @ (pts - c) / max(len(pts), 1)
    w, v = np.linalg.eigh(cov)
    frame = v[:, [2, 1, 0]]
    if np.linalg.det(frame) < 0:
        frame[:, 2] = -frame[:, 2]

    # Project into PCA frame
    proj = (pts - c) @ frame
    p_min = np.min(proj, axis=0)
    p_max = np.max(proj, axis=0)
    span = p_max - p_min
    p_mid = 0.5 * (p_min + p_max)

    center = c + frame @ p_mid
    extents = span * (1.0 + 2.0 * margin) + 2.0 * epsilon
    extents = np.maximum(extents, epsilon * 2.0)

    return center, extents, frame


def _make_obb_manifold(center, extents, frame):
    """Create a manifold3d solid for an oriented bounding box."""
    cube = manifold3d.Manifold.cube(extents, center=True)
    T_box = np.eye(4)
    T_box[:3, :3] = frame
    T_box[:3, 3] = center
    return cube.transform(T_box[:3, :])


def _is_inside_obb(pts, center, extents, frame, margin_factor=1.0):
    """Boolean mask of whether points lie inside an oriented bounding box."""
    pts = np.asarray(pts, dtype=np.float64)
    local = (pts - center) @ frame
    half = (extents * margin_factor) / 2.0
    return np.all(np.abs(local) <= half, axis=1)


def segment_elements(verts, tris, angle_deg=DEFAULT_FEATURE_ANGLE_DEG):
    """Segment mesh into candidate element patches by feature-edge dihedral angle.

    Drops the largest 'base' substrate regions and returns candidate patches with
    geometric descriptors (area, centroid, PCA eigenvalues, principal frame, diag).
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    n_faces = len(tris)
    if n_faces == 0:
        return []

    # 1. Compute face normals and areas
    v0 = verts[tris[:, 0]]
    v1 = verts[tris[:, 1]]
    v2 = verts[tris[:, 2]]
    cross = np.cross(v1 - v0, v2 - v0)
    areas = 0.5 * np.linalg.norm(cross, axis=1)
    normals = cross / (2.0 * areas[:, None] + 1e-12)

    # 2. Build edge-to-face adjacency
    edge_to_f = {}
    for fi, tri in enumerate(tris):
        for u, w in [
            (min(tri[0], tri[1]), max(tri[0], tri[1])),
            (min(tri[1], tri[2]), max(tri[1], tri[2])),
            (min(tri[2], tri[0]), max(tri[2], tri[0])),
        ]:
            edge_to_f.setdefault((u, w), []).append(fi)

    # 3. Connect faces across non-feature edges (dihedral <= angle_deg)
    fi_list, fj_list = [], []
    cos_thresh = np.cos(np.radians(angle_deg))
    for (u, w), fl in edge_to_f.items():
        if len(fl) == 2:
            f1, f2 = fl[0], fl[1]
            dot = np.clip(np.dot(normals[f1], normals[f2]), -1.0, 1.0)
            if dot >= cos_thresh:
                fi_list.extend([f1, f2])
                fj_list.extend([f2, f1])

    adj = sp.coo_matrix(
        (np.ones(len(fi_list), dtype=bool), (fi_list, fj_list)),
        shape=(n_faces, n_faces),
    )
    n_comp, labels = connected_components(adj, directed=False)
    comp_areas = np.array([np.sum(areas[labels == i]) for i in range(n_comp)])
    total_area = float(np.sum(areas))

    # 4. Drop large base components (> BASE_AREA_THRESHOLD_FRACTION of total area)
    base_labels = set(np.where(comp_areas > total_area * BASE_AREA_THRESHOLD_FRACTION)[0])
    non_base_faces = np.where(~np.isin(labels, list(base_labels)))[0]

    if len(non_base_faces) == 0:
        return []

    # 5. Connect adjacent non-base faces into distinct element patches
    nb_set = set(non_base_faces)
    nb_fi, nb_fj = [], []
    for (u, w), fl in edge_to_f.items():
        if len(fl) == 2 and fl[0] in nb_set and fl[1] in nb_set:
            nb_fi.extend([fl[0], fl[1]])
            nb_fj.extend([fl[1], fl[0]])

    adj_nb = sp.coo_matrix(
        (np.ones(len(nb_fi), dtype=bool), (nb_fi, nb_fj)),
        shape=(n_faces, n_faces),
    )
    n_elements, elem_labels = connected_components(adj_nb, directed=False)
    unique_elems = np.unique(elem_labels[non_base_faces])

    patches = []
    for e_id in unique_elems:
        f_in_e = non_base_faces[elem_labels[non_base_faces] == e_id]
        if len(f_in_e) < MIN_ELEMENT_FACES:
            continue

        patch_vidx = np.unique(tris[f_in_e])
        pts = verts[patch_vidx]
        patch_area = float(np.sum(areas[f_in_e]))
        if patch_area <= 1e-12:
            continue

        # Area-weighted centroid
        face_centroids = (v0[f_in_e] + v1[f_in_e] + v2[f_in_e]) / 3.0
        centroid = np.sum(face_centroids * areas[f_in_e, None], axis=0) / patch_area

        # PCA frame and eigenvalues
        cov = (pts - centroid).T @ (pts - centroid) / max(len(pts), 1)
        eigvals, eigvecs = np.linalg.eigh(cov)
        frame = eigvecs[:, [2, 1, 0]]
        if np.linalg.det(frame) < 0:
            frame[:, 2] = -frame[:, 2]

        span = pts.max(axis=0) - pts.min(axis=0)
        diag = float(np.linalg.norm(span))

        patches.append({
            'faces': f_in_e,
            'verts_idx': patch_vidx,
            'centroid': centroid,
            'area': patch_area,
            'eigvals': eigvals[[2, 1, 0]],
            'frame': frame,
            'diag': diag,
        })

    return patches


def align(src_pts, dst_pts, allow_reflection=False, max_iters=20):
    """Rigid alignment: PCA initial frame search + point-to-point ICP refinement.

    Returns (T, rel_rms) where T is a 4x4 matrix and rel_rms is normalized RMS residual.
    """
    src_pts = np.asarray(src_pts, dtype=np.float64)
    dst_pts = np.asarray(dst_pts, dtype=np.float64)

    if len(src_pts) == 0 or len(dst_pts) == 0:
        return np.eye(4), float('inf')

    c_src = np.mean(src_pts, axis=0)
    c_dst = np.mean(dst_pts, axis=0)

    # PCA frames
    cov_src = (src_pts - c_src).T @ (src_pts - c_src) / max(len(src_pts), 1)
    cov_dst = (dst_pts - c_dst).T @ (dst_pts - c_dst) / max(len(dst_pts), 1)

    _, v_src = np.linalg.eigh(cov_src)
    _, v_dst = np.linalg.eigh(cov_dst)

    r_src = v_src[:, [2, 1, 0]]
    r_dst = v_dst[:, [2, 1, 0]]

    if np.linalg.det(r_src) < 0:
        r_src[:, 2] = -r_src[:, 2]
    if np.linalg.det(r_dst) < 0:
        r_dst[:, 2] = -r_dst[:, 2]

    # Orthogonal sign combinations
    proper_signs = [
        np.diag([1.0, 1.0, 1.0]),
        np.diag([1.0, -1.0, -1.0]),
        np.diag([-1.0, 1.0, -1.0]),
        np.diag([-1.0, -1.0, 1.0]),
    ]
    improper_signs = [
        np.diag([-1.0, 1.0, 1.0]),
        np.diag([1.0, -1.0, 1.0]),
        np.diag([1.0, 1.0, -1.0]),
        np.diag([-1.0, -1.0, -1.0]),
    ]
    signs = proper_signs + (improper_signs if allow_reflection else [])

    tree = cKDTree(dst_pts)
    best_msd = float('inf')
    best_T = np.eye(4)

    for S in signs:
        R_cand = r_dst @ S @ r_src.T
        t_cand = c_dst - R_cand @ c_src
        transformed = (src_pts @ R_cand.T) + t_cand
        dists, _ = tree.query(transformed)
        msd = float(np.mean(dists ** 2))
        if msd < best_msd:
            best_msd = msd
            best_T[:3, :3] = R_cand
            best_T[:3, 3] = t_cand

    # ICP refinement
    T = best_T.copy()
    current_pts = (src_pts @ T[:3, :3].T) + T[:3, 3]

    for _ in range(max_iters):
        dists, idxs = tree.query(current_pts)
        target_pts = dst_pts[idxs]

        p_bar = np.mean(current_pts, axis=0)
        q_bar = np.mean(target_pts, axis=0)

        H = (current_pts - p_bar).T @ (target_pts - q_bar)
        U, _, Vt = np.linalg.svd(H)
        R_step = Vt.T @ U.T

        if not allow_reflection and np.linalg.det(R_step) < 0:
            Vt[-1, :] = -Vt[-1, :]
            R_step = Vt.T @ U.T

        t_step = q_bar - R_step @ p_bar

        T_step = np.eye(4)
        T_step[:3, :3] = R_step
        T_step[:3, 3] = t_step
        T = T_step @ T

        current_pts = (current_pts @ R_step.T) + t_step

        if np.linalg.norm(t_step) < 1e-5 and np.linalg.norm(R_step - np.eye(3)) < 1e-5:
            break

    dists, _ = tree.query(current_pts)
    rms = float(np.sqrt(np.mean(dists ** 2)))
    scale = float(np.linalg.norm(src_pts.max(axis=0) - src_pts.min(axis=0)))
    rel_rms = rms / (scale + 1e-12)

    return T, rel_rms


def _volumetric_transplant(manifold_m, src_pts, T, margin=OBB_EXPANSION_MARGIN):
    """Execute robust volumetric transplantation: (M - Box_dst) | T(M & Box_src)."""
    c_src, ext_src, frame_src = _compute_obb(src_pts, margin=margin)
    box_src_m = _make_obb_manifold(c_src, ext_src, frame_src)

    # Destination OBB
    box_dst_m = box_src_m.transform(T[:3, :])
    c_dst = T[:3, :3] @ c_src + T[:3, 3]
    ext_dst = ext_src
    frame_dst = T[:3, :3] @ frame_src

    # Boolean operations
    element_solid = manifold_m ^ box_src_m
    element_prime = element_solid.transform(T[:3, :])

    cavity_m = manifold_m - box_dst_m
    result_m = cavity_m + element_prime

    return result_m, (c_dst, ext_dst, frame_dst)


def _compute_hausdorff_outside(orig_tm, result_tm, dst_boxes, samples=5000):
    """Compute one-sided Hausdorff distance from orig mesh to result mesh outside dst_boxes."""
    sample_pts, _ = trimesh.sample.sample_surface(orig_tm, samples)

    # Filter out points inside any destination box
    outside = np.ones(len(sample_pts), dtype=bool)
    for c_box, ext_box, f_box in dst_boxes:
        in_box = _is_inside_obb(sample_pts, c_box, ext_box, f_box, margin_factor=1.0)
        outside &= ~in_box

    outside_pts = sample_pts[outside]
    if len(outside_pts) == 0:
        return 0.0

    diag = float(np.linalg.norm(orig_tm.extents))
    if diag <= 1e-12:
        return 0.0

    _, dists, _ = trimesh.proximity.closest_point(result_tm, outside_pts)
    return float(np.max(dists)) / diag


def detect_repetition(verts, tris):
    """Analyze mesh for repeated congruent elements and identify pattern parameters.

    Returns (score, info_dict) where score is 0.0..1.0 and info_dict provides details.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)

    patches = segment_elements(verts, tris)
    if len(patches) < REPETITION_MIN_MEMBERS:
        return 0.0, {'reason': f'Too few segmented patches ({len(patches)})'}

    # 1. Cluster candidate patches by invariant descriptors (area, scale, eigenvalue ratios)
    n_p = len(patches)
    sim_pairs = []
    for i in range(n_p):
        for j in range(i + 1, n_p):
            a1, a2 = patches[i]['area'], patches[j]['area']
            area_diff = abs(a1 - a2) / max(a1, a2, 1e-12)
            d1, d2 = patches[i]['diag'], patches[j]['diag']
            diag_diff = abs(d1 - d2) / max(d1, d2, 1e-12)
            if area_diff < REPETITION_SIMILARITY_TOL and diag_diff < REPETITION_SIMILARITY_TOL:
                sim_pairs.append((i, j))

    if not sim_pairs:
        return 0.0, {'reason': 'No similar patch pairs found'}

    fi, fj = zip(*sim_pairs)
    adj = sp.coo_matrix(
        (np.ones(len(fi) * 2, dtype=bool), (fi + fj, fj + fi)),
        shape=(n_p, n_p),
    )
    n_clusters, cluster_labels = connected_components(adj, directed=False)
    cluster_counts = np.bincount(cluster_labels, minlength=n_clusters)
    best_c = int(np.argmax(cluster_counts))

    if cluster_counts[best_c] < REPETITION_MIN_MEMBERS:
        return 0.0, {'reason': f'Largest cluster has only {cluster_counts[best_c]} members'}

    member_indices = np.where(cluster_labels == best_c)[0]
    members = [patches[idx] for idx in member_indices]
    centroids = np.array([m['centroid'] for m in members])

    # 2. Check for Rotational Pattern
    c_rough = np.mean(centroids, axis=0)
    cov = (centroids - c_rough).T @ (centroids - c_rough) / len(centroids)
    w, v_eig = np.linalg.eigh(cov)
    axis = v_eig[:, 0]
    u_ax = v_eig[:, 2]
    v_ax = v_eig[:, 1]

    # Project centroids onto 2D plane
    x_2d = centroids @ u_ax
    y_2d = centroids @ v_ax

    # Algebraic circle fit: A [2cx, 2cy, k] = x^2 + y^2
    try:
        A = np.column_stack([2.0 * x_2d, 2.0 * y_2d, np.ones(len(x_2d))])
        b = x_2d ** 2 + y_2d ** 2
        sol, _, _, _ = np.linalg.lstsq(A, b, rcond=None)
        cx, cy = float(sol[0]), float(sol[1])
        r_sq = sol[2] + cx ** 2 + cy ** 2
        if r_sq > 0:
            fit_r = float(np.sqrt(r_sq))
            radial_dists = np.sqrt((x_2d - cx) ** 2 + (y_2d - cy) ** 2)
            rel_std = float(np.std(radial_dists) / fit_r)

            if fit_r > 1e-3 and rel_std < 0.08:
                plane_z = float(np.mean(centroids @ axis))
                c_circle_3d = cx * u_ax + cy * v_ax + plane_z * axis
                theta_vals = np.arctan2(y_2d - cy, x_2d - cx) % (2.0 * np.pi)

                best_n = None
                for n_cand in range(3, 33):
                    step = 2.0 * np.pi / n_cand
                    th0 = theta_vals[0]
                    diffs = (theta_vals - th0) % step
                    diffs = np.minimum(diffs, step - diffs)
                    if (np.max(diffs) / step) < 0.05:
                        best_n = n_cand
                        break

                if best_n is not None:
                    step = 2.0 * np.pi / best_n
                    th0 = theta_vals[0]
                    diffs = (theta_vals - th0) % step
                    diffs = np.minimum(diffs, step - diffs)
                    score = float(np.clip(1.0 - (np.mean(diffs) / step), 0.0, 1.0))
                    return score, {
                        'pattern_type': 'rotational',
                        'order': best_n,
                        'center': c_circle_3d,
                        'axis': axis,
                        'u_axis': u_ax,
                        'v_axis': v_ax,
                        'radius': fit_r,
                        'step_angle': 2.0 * np.pi / best_n,
                        'members': members,
                        'group_size': len(members),
                    }
    except Exception:
        pass

    # 3. Check for Translational / Grid Pattern
    ax1, ax2 = v_eig[:, 2], v_eig[:, 1]
    u_coords = (centroids - c_rough) @ ax1
    v_coords = (centroids - c_rough) @ ax2

    u_diffs = np.abs(u_coords[:, None] - u_coords[None, :])
    nz_u = u_diffs[u_diffs > 1e-2]
    step_u = float(np.min(nz_u)) if len(nz_u) > 0 else 0.0

    v_diffs = np.abs(v_coords[:, None] - v_coords[None, :])
    nz_v = v_diffs[v_diffs > 1e-2]
    step_v = float(np.min(nz_v)) if len(nz_v) > 0 else 0.0

    if step_u > 1e-2:
        p_idx = np.round((u_coords - np.min(u_coords)) / step_u).astype(int)
        q_idx = np.round((v_coords - np.min(v_coords)) / step_v).astype(int) if step_v > 1e-2 else np.zeros_like(p_idx)
        p_max = int(np.max(p_idx))
        q_max = int(np.max(q_idx))
        if (p_max + 1) * (q_max + 1) <= 64:
            score = 0.85
            return score, {
                'pattern_type': 'translational',
                'origin': c_rough,
                'axis_u': ax1,
                'axis_v': ax2,
                'step_u': step_u,
                'step_v': step_v,
                'p_indices': p_idx,
                'q_indices': q_idx,
                'members': members,
                'group_size': len(members),
            }

    return 0.3, {
        'pattern_type': 'irregular',
        'members': members,
        'group_size': len(members),
    }


def repair_repeat_manual(verts, tris, source_point, target_point, margin=OBB_EXPANSION_MARGIN):
    """Manual repeated-element repair (Method #12).

    Caller provides 3D source_point and target_point coordinates. Transplants
    the element nearest source_point onto the element nearest target_point.
    Returns (repaired_v, repaired_t, report).
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)

    if len(verts) < 4 or len(tris) < 4:
        return verts, tris, {
            'watertight': False,
            'repaired': False,
            'volume_change': 0.0,
            'hausdorff_outside': 0.0,
            'error': 'Mesh has too few vertices or triangles',
            'notes': [],
        }

    orig_mesh = trimesh.Trimesh(vertices=verts, faces=tris, process=False)
    diag = float(np.linalg.norm(orig_mesh.extents))

    # Precondition: must be solid manifold
    mf, err_msg = _build_manifold(verts, tris)
    if mf is None:
        return verts, tris, {
            'watertight': False,
            'repaired': False,
            'volume_change': 0.0,
            'hausdorff_outside': 0.0,
            'error': err_msg,
            'notes': [],
        }

    try:
        patches = segment_elements(verts, tris)
        if len(patches) < 2:
            return verts, tris, {
                'watertight': True,
                'repaired': False,
                'volume_change': 0.0,
                'hausdorff_outside': 0.0,
                'error': 'Fewer than 2 candidate element patches found',
                'notes': [],
            }

        # Find patch nearest source_point
        p_src = np.asarray(source_point, dtype=np.float64)
        p_dst = np.asarray(target_point, dtype=np.float64)

        dists_src = [np.linalg.norm(p['centroid'] - p_src) for p in patches]
        src_idx = int(np.argmin(dists_src))
        src_patch = patches[src_idx]

        dists_dst = [np.linalg.norm(p['centroid'] - p_dst) for p in patches]
        dst_idx = int(np.argmin(dists_dst))
        dst_patch = patches[dst_idx]

        src_pts = verts[src_patch['verts_idx']]
        dst_pts = verts[dst_patch['verts_idx']]

        # Align source to destination
        T, rel_rms = align(src_pts, dst_pts)

        # Volumetric transplant
        result_mf, dst_box = _volumetric_transplant(mf, src_pts, T, margin=margin)

        out_mesh = result_mf.to_mesh()
        rep_v = np.asarray(out_mesh.vert_properties, dtype=np.float64)
        rep_t = np.asarray(out_mesh.tri_verts, dtype=np.int64)
        rep_tm = trimesh.Trimesh(vertices=rep_v, faces=rep_t, process=False)

        vol_change = float(abs(result_mf.volume() - mf.volume()))
        h_outside = _compute_hausdorff_outside(orig_mesh, rep_tm, [dst_box])

        return rep_v, rep_t, {
            'watertight': bool(result_mf.status() == manifold3d.Error.NoError),
            'repaired': True,
            'volume_change': round(vol_change, 6),
            'hausdorff_outside': round(h_outside, 6),
            'notes': ['1 repeated element manually replaced by healthy copy'],
        }

    except Exception as e:
        return verts, tris, {
            'watertight': False,
            'repaired': False,
            'volume_change': 0.0,
            'hausdorff_outside': 0.0,
            'error': str(e),
            'notes': [],
        }


def repair_repeat_auto(verts, tris, margin=OBB_EXPANSION_MARGIN):
    """Automated repeated-element repair (Method #11).

    Detects regular repeating patterns (rotational or translational), identifies
    damaged or missing copies by consensus verification, and transplants healthy copies.
    Returns (repaired_v, repaired_t, report).
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)

    if len(verts) < 4 or len(tris) < 4:
        return verts, tris, {
            'watertight': False,
            'repaired': False,
            'pattern_type': 'none',
            'positions_checked': 0,
            'positions_repaired': 0,
            'volume_change': 0.0,
            'hausdorff_outside': 0.0,
            'error': 'Mesh has too few vertices or triangles',
            'notes': [],
        }

    orig_mesh = trimesh.Trimesh(vertices=verts, faces=tris, process=False)

    mf, err_msg = _build_manifold(verts, tris)
    if mf is None:
        return verts, tris, {
            'watertight': False,
            'repaired': False,
            'pattern_type': 'none',
            'positions_checked': 0,
            'positions_repaired': 0,
            'volume_change': 0.0,
            'hausdorff_outside': 0.0,
            'error': err_msg,
            'notes': [],
        }

    try:
        score, info = detect_repetition(verts, tris)
        pattern_type = info.get('pattern_type', 'none')

        if score < 0.5 or pattern_type not in ('rotational', 'translational'):
            return verts, tris, {
                'watertight': True,
                'repaired': False,
                'pattern_type': pattern_type,
                'positions_checked': 0,
                'positions_repaired': 0,
                'volume_change': 0.0,
                'hausdorff_outside': 0.0,
                'notes': ['No regular repeating pattern detected with sufficient confidence'],
            }

        members = info['members']
        # Choose healthy template closest to median area and scale
        areas = [m['area'] for m in members]
        med_area = float(np.median(areas))
        best_template_idx = int(np.argmin([abs(m['area'] - med_area) for m in members]))
        template = members[best_template_idx]
        template_pts = verts[template['verts_idx']]
        elem_diag = template['diag']

        positions_checked = 0
        positions_repaired = 0
        per_pos_devs = []
        dst_boxes = []

        current_mf = mf

        if pattern_type == 'rotational':
            order = info['order']
            c_rot = info['center']
            axis = info['axis']
            step_angle = info['step_angle']

            # Find base angle of template
            u_ax = info['u_axis']
            v_ax = info['v_axis']
            th_base = np.arctan2(
                (template['centroid'] - c_rot) @ v_ax,
                (template['centroid'] - c_rot) @ u_ax,
            )

            for k in range(order):
                positions_checked += 1
                rot_angle = k * step_angle
                # Rotation matrix around axis through c_rot
                R_k = trimesh.transformations.rotation_matrix(rot_angle, axis, point=c_rot)

                # Predict template vertex locations
                pred_pts = (template_pts @ R_k[:3, :3].T) + R_k[:3, 3]

                # Verify against actual surface
                _, dists, _ = trimesh.proximity.closest_point(orig_mesh, pred_pts)
                dev = float(np.sqrt(np.mean(dists ** 2))) / (elem_diag + 1e-12)
                per_pos_devs.append({'index': k, 'deviation': round(dev, 4)})

                if DAMAGE_DEVIATION_MIN <= dev <= DAMAGE_DEVIATION_MAX:
                    # Broken position: transplant
                    current_mf, dst_box = _volumetric_transplant(
                        current_mf, template_pts, R_k, margin=margin
                    )
                    dst_boxes.append(dst_box)
                    positions_repaired += 1

        elif pattern_type == 'translational':
            origin = info['origin']
            ax_u = info['axis_u']
            ax_v = info['axis_v']
            step_u = info['step_u']
            step_v = info['step_v']
            p_indices = info['p_indices']
            q_indices = info['q_indices']

            p_max = int(np.max(p_indices))
            q_max = int(np.max(q_indices))

            template_p = p_indices[best_template_idx]
            template_q = q_indices[best_template_idx]

            for p_val in range(p_max + 1):
                for q_val in range(q_max + 1):
                    positions_checked += 1
                    shift = (p_val - template_p) * step_u * ax_u + (q_val - template_q) * step_v * ax_v
                    T_shift = np.eye(4)
                    T_shift[:3, 3] = shift

                    pred_pts = template_pts + shift

                    _, dists, _ = trimesh.proximity.closest_point(orig_mesh, pred_pts)
                    dev = float(np.sqrt(np.mean(dists ** 2))) / (elem_diag + 1e-12)
                    per_pos_devs.append({'index': (p_val, q_val), 'deviation': round(dev, 4)})

                    if DAMAGE_DEVIATION_MIN <= dev <= DAMAGE_DEVIATION_MAX:
                        current_mf, dst_box = _volumetric_transplant(
                            current_mf, template_pts, T_shift, margin=margin
                        )
                        dst_boxes.append(dst_box)
                        positions_repaired += 1

        out_mesh = current_mf.to_mesh()
        rep_v = np.asarray(out_mesh.vert_properties, dtype=np.float64)
        rep_t = np.asarray(out_mesh.tri_verts, dtype=np.int64)
        rep_tm = trimesh.Trimesh(vertices=rep_v, faces=rep_t, process=False)

        vol_change = float(abs(current_mf.volume() - mf.volume()))
        h_outside = _compute_hausdorff_outside(orig_mesh, rep_tm, dst_boxes) if dst_boxes else 0.0

        notes = []
        if positions_repaired > 0:
            notes.append(f"{positions_repaired} repeated elements were replaced by healthy copies")
        else:
            notes.append("All detected pattern positions are healthy")

        return rep_v, rep_t, {
            'watertight': bool(current_mf.status() == manifold3d.Error.NoError),
            'repaired': bool(positions_repaired > 0),
            'pattern_type': pattern_type,
            'group_size': info.get('group_size', 0),
            'positions_checked': positions_checked,
            'positions_repaired': positions_repaired,
            'per_position_deviation': per_pos_devs,
            'volume_change': round(vol_change, 6),
            'hausdorff_outside': round(h_outside, 6),
            'notes': notes,
        }

    except Exception as e:
        return verts, tris, {
            'watertight': False,
            'repaired': False,
            'pattern_type': 'none',
            'positions_checked': 0,
            'positions_repaired': 0,
            'volume_change': 0.0,
            'hausdorff_outside': 0.0,
            'error': str(e),
            'notes': [],
        }
