# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""sutura/repeat_repair.py - Repeated-element repair for meshes with congruent features.

Repairs meshes containing repeated geometric features (knurls, gear teeth, tiles,
regular arrayed bumps) where one or a few copies are broken or missing.
Features:
  1. segment_elements: Feature-edge dihedral segmentation + PCA descriptor extraction.
  2. align: Robust rigid alignment via PCA orientation frame + point-to-point ICP.
  3. repair_repeat_manual (Method #12): User-specified source and target point transplant.
  4. detect_repetition: Fast regularity analysis for rotational or translational patterns.
  5. repair_repeat_auto (Method #11): Consensus-based predict-and-verify automated repair.
  6. Robust volumetric transplant using the Rust Manifold CSG kernel (no fragile seam stitching).
"""

import numpy as np

import sutura_geom

import topology

from sutura_engine.wall import sample_surface as _surface_sample

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


def _rotation_matrix(angle, direction, point=None):
    """4x4 rotation about ``direction`` through ``point`` (Rodrigues, numpy).

    Drop-in for the former ``trimesh.transformations.rotation_matrix``: the
    rotation part follows Rodrigues' formula and the translation is
    ``point - R @ point`` so the axis passes through ``point``.
    """
    direction = np.asarray(direction, dtype=np.float64)
    n = float(np.linalg.norm(direction))
    if n < 1e-12:
        return np.eye(4)
    x, y, z = direction / n
    c = float(np.cos(angle))
    s = float(np.sin(angle))
    C = 1.0 - c
    R = np.array([
        [c + x * x * C, x * y * C - z * s, x * z * C + y * s],
        [y * x * C + z * s, c + y * y * C, y * z * C - x * s],
        [z * x * C - y * s, z * y * C + x * s, c + z * z * C],
    ])
    M = np.eye(4)
    M[:3, :3] = R
    if point is not None:
        p = np.asarray(point, dtype=np.float64)
        M[:3, 3] = p - R @ p
    return M


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


def _decimate_faces(verts, tris, target):
    """Quadric-decimate to ``target`` faces via PyMeshLab (lazy import).

    Replaces the former ``trimesh`` simplifier; PyMeshLab is already a runtime
    dependency, so the engine needs no ``trimesh`` for this.  Returns
    ``(verts float64, tris int32)``.
    """
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float64),
                        face_matrix=np.asarray(tris, np.int32)))
    ms.apply_filter('meshing_decimation_quadric_edge_collapse',
                    targetfacenum=int(target), preservetopology=True)
    m = ms.current_mesh()
    return (np.asarray(m.vertex_matrix(), dtype=np.float64),
            np.asarray(m.face_matrix(), dtype=np.int32))


def check_mesh_validity(verts, tris):
    """Check mesh validity (watertight 2-manifold) without closing or Manifold.

    Pure NumPy/topology check that does not require the Rust Manifold kernel.
    Returns (is_valid, err_msg).
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    if len(verts) < 4 or len(tris) < 4:
        return False, "Mesh has too few vertices or triangles"

    # Degenerate face check
    if np.any(tris[:, 0] == tris[:, 1]) or np.any(tris[:, 1] == tris[:, 2]) or np.any(tris[:, 2] == tris[:, 0]):
        return False, "Mesh contains degenerate triangles with duplicate vertices"

    # Edge topology check: each undirected edge must appear exactly twice
    edges = np.vstack([
        tris[:, [0, 1]],
        tris[:, [1, 2]],
        tris[:, [2, 0]]
    ])
    sorted_edges = np.ascontiguousarray(np.sort(edges, axis=1))
    edge_dtype = [('v0', sorted_edges.dtype), ('v1', sorted_edges.dtype)]
    struct_edges = sorted_edges.view(edge_dtype).reshape(-1)
    _, counts = np.unique(struct_edges, return_counts=True)
    if np.any(counts != 2):
        n_open = int(np.sum(counts == 1))
        n_nm = int(np.sum(counts > 2))
        return False, f"Non-watertight mesh: {n_open} boundary edges, {n_nm} non-manifold edges"

    return True, ""


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

    # If unmerged STL (where len(verts) ~ 3 * n_faces), weld coincident vertices
    # so dihedral angles across shared edges can be determined.
    if len(verts) >= 2 * n_faces:
        _, u_idx, inv_idx = np.unique(np.round(verts, 4), axis=0, return_index=True, return_inverse=True)
        if len(u_idx) < len(verts):
            verts = verts[u_idx]
            tris = inv_idx[tris]

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

    edges = np.column_stack((fi_list, fj_list)) if fi_list else np.zeros((0, 2), dtype=np.int64)
    n_comp, labels = topology.connected_components((n_faces, edges))
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

    edges_nb = np.column_stack((nb_fi, nb_fj)) if nb_fi else np.zeros((0, 2), dtype=np.int64)
    n_elements, elem_labels = topology.connected_components((n_faces, edges_nb))
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


def align(src_pts, dst_pts, allow_reflection=False, allow_scale=False, max_iters=20):
    """Rigid or similarity alignment: PCA initial frame search + point-to-point ICP refinement.

    Parameters:
        src_pts: (N, 3) float64 array of source points.
        dst_pts: (M, 3) float64 array of destination points.
        allow_reflection: bool, whether improper rotations (reflections) are allowed.
        allow_scale: bool, whether uniform scale is estimated and applied.
        max_iters: int, maximum ICP iterations.

    Returns:
        (T, rel_rms) where T is a 4x4 matrix and rel_rms is normalized RMS residual.
    """
    src_pts = np.asarray(src_pts, dtype=np.float64)
    dst_pts = np.asarray(dst_pts, dtype=np.float64)

    if len(src_pts) == 0 or len(dst_pts) == 0:
        return np.eye(4), float('inf')

    c_src = np.mean(src_pts, axis=0)
    c_dst = np.mean(dst_pts, axis=0)

    # Estimate uniform scale if requested
    scale_factor = 1.0
    if allow_scale:
        r_src = np.sqrt(np.mean(np.sum((src_pts - c_src) ** 2, axis=1)))
        r_dst = np.sqrt(np.mean(np.sum((dst_pts - c_dst) ** 2, axis=1)))
        s_cand = float(r_dst / (r_src + 1e-12))
        if 0.5 <= s_cand <= 2.0:
            scale_factor = s_cand

    src_scaled = (src_pts - c_src) * scale_factor + c_src

    # PCA frames
    cov_src = (src_scaled - c_src).T @ (src_scaled - c_src) / max(len(src_scaled), 1)
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

    tree = sutura_geom.KdTree(dst_pts)
    best_msd = float('inf')
    best_R = np.eye(3)
    best_t = c_dst - c_src

    for S in signs:
        R_cand = r_dst @ S @ r_src.T
        t_cand = c_dst - R_cand @ c_src
        transformed = (src_scaled @ R_cand.T) + t_cand
        dists, _ = tree.query(transformed, k=1)
        msd = float(np.mean(dists ** 2))
        if msd < best_msd:
            best_msd = msd
            best_R = R_cand
            best_t = t_cand

    # ICP refinement
    R_total = best_R.copy()
    t_total = best_t.copy()
    current_pts = (src_scaled @ R_total.T) + t_total

    for _ in range(max_iters):
        dists, idxs = tree.query(current_pts, k=1)
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

        R_total = R_step @ R_total
        t_total = R_step @ t_total + t_step

        current_pts = (current_pts @ R_step.T) + t_step

        if np.linalg.norm(t_step) < 1e-5 and np.linalg.norm(R_step - np.eye(3)) < 1e-5:
            break

    # Build final 4x4 matrix T that maps original src_pts -> dst_pts:
    T = np.eye(4)
    T[:3, :3] = scale_factor * R_total
    T[:3, 3] = t_total + (1.0 - scale_factor) * (R_total @ c_src)

    dists, _ = tree.query(current_pts, k=1)
    rms = float(np.sqrt(np.mean(dists ** 2)))
    orig_scale = float(np.linalg.norm(dst_pts.max(axis=0) - dst_pts.min(axis=0)))
    rel_rms = rms / (orig_scale + 1e-12)

    return T, rel_rms


def _execute_boolean_transplant(verts, tris, template_pts, transforms,
                                margin=OBB_EXPANSION_MARGIN, force_bridge=None):
    """Execute the volumetric boolean transplant in the Rust core.

    The OBB is computed here (numpy eig), then ``sutura_geom`` runs the same
    Manifold CSG sequence that the retired Python bridge used to run: intersect the
    template box, cut the destination cavity, move the element and union it.
    ``force_bridge`` is accepted for backwards compatibility and ignored.
    """
    del force_bridge
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int32)
    c_src, ext_src, frame_src = _compute_obb(template_pts, margin=margin)
    matrices = np.asarray(transforms, dtype=np.float64)
    if matrices.ndim != 3 or matrices.shape[1:] != (4, 4):
        raise ValueError("transforms must be a list of 4x4 matrices")
    rep_v, rep_t, vol_change, dst_boxes, is_watertight = \
        sutura_geom.boolean_transplant(
            verts, tris, c_src, ext_src, frame_src, matrices)
    return (np.asarray(rep_v, dtype=np.float64),
            np.asarray(rep_t, dtype=np.int64),
            float(vol_change), list(dst_boxes), bool(is_watertight))


def _compute_hausdorff_outside(orig_v, orig_t, result_v, result_t, dst_boxes, samples=5000):
    """One-sided Hausdorff distance from the original mesh to the result mesh
    outside ``dst_boxes`` (operates on plain vertex/triangle arrays)."""
    sample_pts, _ = _surface_sample(orig_v, orig_t, samples, seed=0)

    # Filter out points inside any destination box
    outside = np.ones(len(sample_pts), dtype=bool)
    for c_box, ext_box, f_box in dst_boxes:
        in_box = _is_inside_obb(sample_pts, c_box, ext_box, f_box, margin_factor=1.0)
        outside &= ~in_box

    outside_pts = sample_pts[outside]
    if len(outside_pts) == 0:
        return 0.0

    diag = float(np.linalg.norm(np.ptp(np.asarray(orig_v), axis=0)))
    if diag <= 1e-12:
        return 0.0

    _, dists, _ = sutura_geom.closest_points(result_v, result_t, outside_pts)
    return float(np.max(dists)) / diag


from sutura_engine.hull import extract_outer_shell


def detect_rotational_symmetry_free(verts, tris, n_samples=3000, max_order=64):
    """Segmentation-free detection of rotational symmetry ($C_n$) across candidate orders.

    Estimates candidate axes from PCA, samples the mesh surface, and uses the nearest-neighbour tree
    overlap testing across candidate rotation orders with multi-step verification.

    Returns:
        (score, info_dict)
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    if len(verts) < 4 or len(tris) < 4:
        return 0.0, {'pattern_type': 'none', 'reason': 'Too few vertices or triangles'}

    n_pts = n_samples
    dense_pts, _ = _surface_sample(verts, tris, n_pts * 2, seed=42)
    query_pts, _ = _surface_sample(verts, tris, n_pts, seed=123)

    c = np.mean(dense_pts, axis=0)
    span = dense_pts.max(axis=0) - dense_pts.min(axis=0)
    diag = float(np.linalg.norm(span))
    if diag <= 1e-12:
        return 0.0, {'pattern_type': 'none', 'reason': 'Zero bounding box diagonal'}

    tol = 0.015 * diag
    cov = (dense_pts - c).T @ (dense_pts - c) / max(len(dense_pts), 1)
    _, eigvecs = np.linalg.eigh(cov)
    tree = sutura_geom.KdTree(dense_pts)

    candidate_orders = list(range(2, max_order + 1)) + [72, 80, 90, 100, 120]
    best_match = None
    best_score = 0.0

    candidate_axes = [
        np.array([1.0, 0.0, 0.0]),
        np.array([0.0, 1.0, 0.0]),
        np.array([0.0, 0.0, 1.0]),
    ] + [eigvecs[:, i] for i in range(3)]

    # The per-candidate surface-distance lookups dominate the runtime (a few
    # hundred exact nearest-neighbour queries of the sampled query points). Build every
    # candidate rotation one at a time with the original scalar expression (so
    # the resulting coordinates and distances are bit-identical to the scalar
    # loop) but defer the lookups into TWO batched, threaded KD-tree queries:
    # one for the first-order check of every candidate, one for the multi-step
    # verification of the candidates that passed the first gate. The scalar
    # reductions and the best-match selection then run unchanged in the
    # original axis-major / order-minor iteration order.
    q0 = query_pts - c
    candidates = []
    for ax in candidate_axes:
        norm_ax = np.linalg.norm(ax)
        if norm_ax < 1e-6:
            continue
        ax = ax / norm_ax

        if abs(ax[0]) < 0.9:
            u = np.cross(ax, [1.0, 0.0, 0.0])
        else:
            u = np.cross(ax, [0.0, 1.0, 0.0])
        u = u / (np.linalg.norm(u) + 1e-12)
        v_vec = np.cross(ax, u)
        v_vec = v_vec / (np.linalg.norm(v_vec) + 1e-12)

        for n in candidate_orders:
            th = 2.0 * np.pi / n
            R = _rotation_matrix(th, ax, point=c)[:3, :3]
            rot = q0 @ R.T + c
            move_d = float(np.mean(np.linalg.norm(rot - query_pts, axis=1)))
            candidates.append((ax, u, v_vec, n, th, rot, move_d))

    if candidates:
        rot_stack = np.stack([cand[5] for cand in candidates])
        all_dists = tree.query(rot_stack.reshape(-1, 3), k=1)[0].reshape(len(candidates), -1)
    else:
        all_dists = np.zeros((0, len(query_pts)))

    first = []
    step_jobs = []
    for i, (ax, u, v_vec, n, th, rot, move_d) in enumerate(candidates):
        dists = all_dists[i]
        frac = float((dists < tol).mean())
        mean_d = float(dists.mean())
        rel_dev = mean_d / (move_d + 1e-6)
        first.append((frac, mean_d, rel_dev))

        if frac >= 0.70 and (rel_dev < 0.30 or (n >= 40 and rel_dev < 0.65)):
            steps_to_check = [2]
            if n >= 4:
                steps_to_check.append(n // 2)
            step_rots = []
            for s_k in steps_to_check:
                R_k = _rotation_matrix(s_k * th, ax, point=c)[:3, :3]
                step_rots.append(q0 @ R_k.T + c)
            step_jobs.append((i, step_rots))

    step_frac_by_i = {}
    if step_jobs:
        flat_steps = []
        for _i, step_rots in step_jobs:
            flat_steps.extend(step_rots)
        step_dists = tree.query(np.stack(flat_steps).reshape(-1, 3), k=1)[0].reshape(len(flat_steps), -1)
        k = 0
        for _i, step_rots in step_jobs:
            step_frac_by_i[_i] = [float((step_dists[k + j] < tol).mean()) for j in range(len(step_rots))]
            k += len(step_rots)

    for i, (ax, u, v_vec, n, th, rot, move_d) in enumerate(candidates):
        step_fracs = step_frac_by_i.get(i)
        if step_fracs is None:
            continue
        frac, mean_d, _rel_dev = first[i]

        if all(sf >= 0.55 for sf in step_fracs):
            avg_frac = (frac + sum(step_fracs)) / (1.0 + len(step_fracs))
            score = float(np.clip(avg_frac * (1.0 - min(mean_d / tol, 1.0) * 0.4), 0.0, 1.0))
            is_better = score > best_score
            if best_match is not None and not is_better:
                prev_n = best_match['order']
                if n > prev_n and n % prev_n == 0 and (n // prev_n) <= 4 and score >= best_score * 0.90:
                    is_better = True
            if is_better:
                best_score = score
                best_match = {
                    'pattern_type': 'rotational',
                    'order': n,
                    'center': c,
                    'axis': ax,
                    'u_axis': u,
                    'v_axis': v_vec,
                    'step_angle': th,
                    'score': score,
                    'match_fraction': frac,
                    'mean_distance': mean_d,
                    'method': 'segmentation_free',
                }

    if best_match is not None and best_score >= 0.60:
        return best_score, best_match
    return 0.0, {'pattern_type': 'none', 'reason': 'No rotational symmetry detected'}


def _detect_repetition_patches(verts, tris):
    """Analyze mesh using dihedral feature-edge patch segmentation."""
    patches = segment_elements(verts, tris)
    if len(patches) < REPETITION_MIN_MEMBERS:
        return 0.0, {'reason': f'Too few segmented patches ({len(patches)})'}

    # 1. Cluster candidate patches by invariant descriptors
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

    edges = np.asarray(sim_pairs, dtype=np.int64)
    n_clusters, cluster_labels = topology.connected_components((n_p, edges))
    cluster_counts = np.bincount(cluster_labels, minlength=n_clusters)
    best_c = int(np.argmax(cluster_counts))

    if cluster_counts[best_c] < REPETITION_MIN_MEMBERS:
        return 0.0, {'reason': f'Largest cluster has only {cluster_counts[best_c]} members'}

    member_indices = np.where(cluster_labels == best_c)[0]
    members = [patches[idx] for idx in member_indices]
    centroids = np.array([m['centroid'] for m in members])

    # 2. Check for Rotational and Helical Patterns on Cylinder
    c_rough = np.mean(centroids, axis=0)
    cov_p = (centroids - c_rough).T @ (centroids - c_rough) / len(centroids)
    w_p, V_p = np.linalg.eigh(cov_p)

    patch_normals = []
    for m in members:
        f_in_e = m['faces']
        v0 = verts[tris[f_in_e, 0]]
        v1 = verts[tris[f_in_e, 1]]
        v2 = verts[tris[f_in_e, 2]]
        cross = np.cross(v1 - v0, v2 - v0)
        norm = np.sum(cross, axis=0)
        patch_normals.append(norm / (np.linalg.norm(norm) + 1e-12))
    cov_n = np.array(patch_normals).T @ np.array(patch_normals)
    w_n, Vn = np.linalg.eigh(cov_n)

    candidates = [Vn[:, 0], V_p[:, 0], V_p[:, 1], V_p[:, 2]]

    best_axis = None
    best_rel_std = 1.0
    best_basis = None
    best_circle = None

    for ax in candidates:
        norm_ax = np.linalg.norm(ax)
        if norm_ax < 1e-6:
            continue
        ax = ax / norm_ax
        if abs(ax[0]) < 0.9:
            u = np.cross(ax, [1, 0, 0])
        else:
            u = np.cross(ax, [0, 1, 0])
        u /= np.linalg.norm(u)
        v_vec = np.cross(ax, u)

        x_2d = (centroids - c_rough) @ u
        y_2d = (centroids - c_rough) @ v_vec
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
                if rel_std < best_rel_std:
                    best_rel_std = rel_std
                    best_axis = ax
                    best_basis = (u, v_vec)
                    best_circle = (cx, cy, fit_r)
        except Exception:
            pass

    centroids_span = float(np.linalg.norm(np.ptp(centroids, axis=0)))
    if best_axis is not None and best_circle[2] > 1e-3 and best_rel_std < 0.08 and best_circle[2] <= 2.5 * (centroids_span if centroids_span > 1e-4 else 1.0):
        u, v_vec = best_basis
        cx, cy, fit_r = best_circle
        c_circle_3d = c_rough + cx * u + cy * v_vec
        z_coords = (centroids - c_circle_3d) @ best_axis
        x_rel = (centroids - c_circle_3d) @ u
        y_rel = (centroids - c_circle_3d) @ v_vec
        theta_vals = np.arctan2(y_rel, x_rel) % (2.0 * np.pi)

        z_span = float(np.max(z_coords) - np.min(z_coords))
        z_std = float(np.std(z_coords))

        # 2a. Pure Rotational Pattern (planar circular array)
        if z_span < 0.05 * fit_r or z_std < 0.02 * fit_r:
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
                    'axis': best_axis,
                    'u_axis': u,
                    'v_axis': v_vec,
                    'radius': fit_r,
                    'step_angle': 2.0 * np.pi / best_n,
                    'members': members,
                    'group_size': len(members),
                }

        # 2b. Helical (Screw) Pattern
        if z_span >= 0.05 * fit_r and z_std >= 0.02 * fit_r:
            s = np.argsort(z_coords)
            z_sorted = z_coords[s]
            th_sorted = theta_vals[s]
            dz = np.diff(z_sorted)
            nz_dz = dz[dz > 1e-3 * fit_r]
            if len(nz_dz) > 0:
                h_min = np.min(nz_dz)
                ratios = dz / h_min
                nz_r = np.round(ratios) > 0
                if np.all(np.abs(ratios - np.round(ratios)) < 0.20) and np.any(nz_r):
                    h_step = float(np.median(dz[nz_r] / np.round(ratios)[nz_r]))
                    if h_step > 1e-4:
                        k_fit = np.round((z_sorted - z_sorted[0]) / h_step).astype(int)
                        err_z = np.abs((z_sorted - z_sorted[0]) - k_fit * h_step) / h_step
                    else:
                        k_fit = None
                else:
                    k_fit = None

                if k_fit is not None:

                    dth = np.diff(th_sorted)
                    dth_wrap = (dth + np.pi) % (2.0 * np.pi) - np.pi
                    dk = np.diff(k_fit)
                    valid = dk >= 1
                    if np.any(valid):
                        th_step = float(np.median(dth_wrap[valid] / dk[valid]))
                        th_pred = th_sorted[0] + k_fit * th_step
                        err_th = np.abs((th_sorted - th_pred + np.pi) % (2.0 * np.pi) - np.pi)
                        if np.max(err_th) < 0.15 and np.max(err_z) < 0.15:
                            score = float(np.clip(1.0 - (np.mean(err_th) / 0.15 + np.mean(err_z) / 0.15) / 2.0, 0.0, 1.0))
                            k_members = np.zeros(len(members), dtype=int)
                            k_members[s] = k_fit
                            return score, {
                                'pattern_type': 'helical',
                                'center': c_circle_3d,
                                'axis': best_axis,
                                'u_axis': u,
                                'v_axis': v_vec,
                                'radius': fit_r,
                                'step_angle': th_step,
                                'step_h': h_step,
                                'k_indices': k_members,
                                'k_min': int(np.min(k_fit)),
                                'k_max': int(np.max(k_fit)),
                                'members': members,
                                'group_size': len(members),
                            }

    # 3. Check for Translational / Grid Pattern (2D planar or 3D lattice)
    ax1, ax2, ax3 = V_p[:, 2], V_p[:, 1], V_p[:, 0]
    u_coords = (centroids - c_rough) @ ax1
    v_coords = (centroids - c_rough) @ ax2
    w_coords = (centroids - c_rough) @ ax3

    def _find_step(coords):
        span = float(coords.max() - coords.min())
        if span <= 1e-4:
            return 0.0
        sorted_c = np.sort(coords)
        diffs = np.diff(sorted_c)
        gaps = diffs[diffs > 0.08 * span]
        if len(gaps) > 0:
            return float(np.median(gaps))
        nz = diffs[diffs > 1e-2]
        return float(np.min(nz)) if len(nz) > 0 else 0.0

    step_u = _find_step(u_coords)
    step_v = _find_step(v_coords)
    step_w = _find_step(w_coords)

    if step_u > 1e-2:
        p_idx = np.round((u_coords - np.min(u_coords)) / step_u).astype(int)
        q_idx = np.round((v_coords - np.min(v_coords)) / step_v).astype(int) if step_v > 1e-2 else np.zeros_like(p_idx)
        p_max = int(np.max(p_idx))
        q_max = int(np.max(q_idx))

        # Check for 3D lattice grid (e.g. rubik 3x3x3)
        min_planar_step = min(step_u, step_v) if step_v > 1e-2 else step_u
        if step_w >= 0.2 * min_planar_step and len(np.unique(np.round((w_coords - np.min(w_coords)) / step_w))) > 1:
            r_idx = np.round((w_coords - np.min(w_coords)) / step_w).astype(int)
            r_max = int(np.max(r_idx))
            if (p_max + 1) * (q_max + 1) * (r_max + 1) <= 128:
                score = 0.85
                return score, {
                    'pattern_type': 'translational',
                    'origin': c_rough,
                    'axis_u': ax1,
                    'axis_v': ax2,
                    'axis_w': ax3,
                    'step_u': step_u,
                    'step_v': step_v,
                    'step_w': step_w,
                    'p_indices': p_idx,
                    'q_indices': q_idx,
                    'r_indices': r_idx,
                    'members': members,
                    'group_size': len(members),
                }

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


def detect_repetition(verts, tris):
    """Analyze mesh for repeated congruent elements and identify pattern parameters.

    Uses a dual-route architecture:
      1. Segmentation-free surface sampling + KD-tree rotational symmetry detection ($C_n$).
      2. Feature-edge dihedral patch segmentation + descriptor clustering for discrete
         patches, rotational arrays, helical screw threads, and 2D/3D translational grids.

    Returns:
        (score, info_dict) where score is 0.0..1.0 and info_dict provides details.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)

    if len(verts) < 4 or len(tris) < 4:
        return 0.0, {'pattern_type': 'none', 'reason': 'Too few vertices or triangles'}

    # 1. Fast segmentation-free rotational symmetry check
    free_score, free_info = detect_rotational_symmetry_free(verts, tris)

    # 2. Patch-based segmentation route
    if len(tris) <= 125000:
        patch_score, patch_info = _detect_repetition_patches(verts, tris)
    elif len(tris) <= 500000:
        try:
            dv, dt = _decimate_faces(verts, tris, 35000)
            patch_score, patch_info = _detect_repetition_patches(dv, dt)
        except Exception:
            patch_score, patch_info = 0.0, {'reason': f'Decimation failed on mesh with {len(tris)} faces'}
    else:
        patch_score, patch_info = 0.0, {'reason': f'Mesh face count ({len(tris)}) exceeds patch segmentation cap'}

    # Prefer patch route if confident, otherwise segmentation-free route
    if patch_score >= 0.50 and patch_score >= free_score:
        return patch_score, patch_info
    if free_score >= 0.60:
        return free_score, free_info
    if patch_score >= 0.50:
        return patch_score, patch_info
    if free_score >= 0.50:
        return free_score, free_info

    if free_score > patch_score and free_score > 0.0:
        return free_score, free_info
    return patch_score, patch_info


def repair_repeat_manual(verts, tris, source_point, target_point, margin=OBB_EXPANSION_MARGIN, force_bridge=None):
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

    orig_v, orig_t = verts, tris
    diag = float(np.linalg.norm(np.ptp(np.asarray(verts), axis=0)))

    # Precondition: must be solid manifold
    is_valid, err_msg = check_mesh_validity(verts, tris)
    if not is_valid:
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
        rep_v, rep_t, vol_change, dst_boxes, is_watertight = _execute_boolean_transplant(
            verts, tris, src_pts, [T], margin=margin, force_bridge=force_bridge
        )
        h_outside = _compute_hausdorff_outside(orig_v, orig_t, rep_v, rep_t, dst_boxes)

        return rep_v, rep_t, {
            'watertight': bool(is_watertight),
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


def repair_repeat_auto(verts, tris, margin=OBB_EXPANSION_MARGIN, force_bridge=None):
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

    orig_v, orig_t = verts, tris

    is_valid, err_msg = check_mesh_validity(verts, tris)
    if not is_valid:
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

        if score < 0.5 or pattern_type not in ('rotational', 'translational', 'helical'):
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

        if 'members' in info:
            members = info['members']
            # Choose healthy template closest to median area and scale
            areas = [m['area'] for m in members]
            med_area = float(np.median(areas))
            best_template_idx = int(np.argmin([abs(m['area'] - med_area) for m in members]))
            template = members[best_template_idx]
            template_pts = verts[template['verts_idx']]
            elem_diag = template['diag']
            c_templ = np.mean(template_pts, axis=0)
            def _calc_deviation(pred_pts):
                # Forward distance: clean template to target mesh (detects dents / missing features)
                _, dists_fwd, _ = sutura_geom.closest_points(orig_v, orig_t, pred_pts)
                dev_dent = float(np.sqrt(np.mean(dists_fwd ** 2))) / (elem_diag + 1e-12)

                # Reverse distance: target member patch to template (detects bumps / extra features)
                c_p = np.mean(pred_pts, axis=0)
                d_m = [np.linalg.norm(m['centroid'] - c_p) for m in members]
                if min(d_m) <= 0.5 * elem_diag:
                    best_m = members[int(np.argmin(d_m))]
                    m_pts = verts[best_m['verts_idx']]
                    tree_p = sutura_geom.KdTree(pred_pts)
                    d_rev, _ = tree_p.query(m_pts, k=1)
                    dev_bump = float(np.sqrt(np.mean(d_rev ** 2))) / (elem_diag + 1e-12)
                else:
                    dev_bump = 0.0

                return max(dev_dent, dev_bump)

            positions_checked = 0
            per_pos_devs = []
            transforms_to_repair = []

            if pattern_type == 'rotational':
                order = info['order']
                c_rot = info['center']
                axis = info['axis']
                step_angle = info['step_angle']
                u_ax = info['u_axis']
                v_ax = info['v_axis']

                th_template = np.arctan2(
                    (template['centroid'] - c_rot) @ v_ax,
                    (template['centroid'] - c_rot) @ u_ax,
                ) % (2.0 * np.pi)
                th_base = np.arctan2(
                    (members[0]['centroid'] - c_rot) @ v_ax,
                    (members[0]['centroid'] - c_rot) @ u_ax,
                ) % (2.0 * np.pi)
                k_template = int(np.round(((th_template - th_base) % (2.0 * np.pi)) / step_angle))

                for k in range(order):
                    positions_checked += 1
                    rot_angle = (k - k_template) * step_angle
                    R_k = _rotation_matrix(rot_angle, axis, point=c_rot)
                    pred_pts = (template_pts @ R_k[:3, :3].T) + R_k[:3, 3]

                    dev = _calc_deviation(pred_pts)
                    per_pos_devs.append({'index': k, 'deviation': round(dev, 4)})

                    if DAMAGE_DEVIATION_MIN <= dev <= DAMAGE_DEVIATION_MAX:
                        transforms_to_repair.append(R_k)

            elif pattern_type == 'translational':
                origin = info['origin']
                ax_u = info['axis_u']
                ax_v = info['axis_v']
                step_u = info['step_u']
                step_v = info['step_v']
                p_indices = info['p_indices']
                q_indices = info['q_indices']
                has_w = 'axis_w' in info and 'step_w' in info and 'r_indices' in info
                if has_w:
                    ax_w = info['axis_w']
                    step_w = info['step_w']
                    r_indices = info['r_indices']
                    r_max = int(np.max(r_indices))
                    template_r = r_indices[best_template_idx]
                else:
                    ax_w = np.zeros(3)
                    step_w = 0.0
                    r_max = 0
                    template_r = 0

                p_max = int(np.max(p_indices))
                q_max = int(np.max(q_indices))

                template_p = p_indices[best_template_idx]
                template_q = q_indices[best_template_idx]

                for p_val in range(p_max + 1):
                    for q_val in range(q_max + 1):
                        for r_val in range(r_max + 1):
                            positions_checked += 1
                            shift = ((p_val - template_p) * step_u * ax_u +
                                     (q_val - template_q) * step_v * ax_v +
                                     (r_val - template_r) * step_w * ax_w)
                            T_shift = np.eye(4)
                            T_shift[:3, 3] = shift

                            pred_pts = template_pts + shift

                            dev = _calc_deviation(pred_pts)
                            idx_key = (p_val, q_val, r_val) if has_w else (p_val, q_val)
                            per_pos_devs.append({'index': idx_key, 'deviation': round(dev, 4)})

                            if DAMAGE_DEVIATION_MIN <= dev <= DAMAGE_DEVIATION_MAX:
                                transforms_to_repair.append(T_shift)

            elif pattern_type == 'helical':
                c_circle = info['center']
                axis = info['axis']
                th_step = info['step_angle']
                h_step = info['step_h']
                k_min = info['k_min']
                k_max = info['k_max']
                k_indices = info['k_indices']

                template_k = k_indices[best_template_idx]

                for k_val in range(k_min, k_max + 1):
                    positions_checked += 1
                    delta_k = k_val - template_k
                    rot_angle = delta_k * th_step
                    R_k = _rotation_matrix(rot_angle, axis, point=c_circle)
                    T_k = np.eye(4)
                    T_k[:3, :3] = R_k[:3, :3]
                    T_k[:3, 3] = R_k[:3, 3] + delta_k * h_step * axis

                    pred_pts = (template_pts @ T_k[:3, :3].T) + T_k[:3, 3]

                    dev = _calc_deviation(pred_pts)
                    per_pos_devs.append({'index': k_val, 'deviation': round(dev, 4)})

                    if DAMAGE_DEVIATION_MIN <= dev <= DAMAGE_DEVIATION_MAX:
                        transforms_to_repair.append(T_k)

        else:
            # Segmentation-free rotational pattern: sector-based predict-and-verify
            order = info['order']
            c_rot = info['center']
            axis = info['axis']
            step_angle = info['step_angle']
            u_ax = info['u_axis']
            v_ax = info['v_axis']

            v_proj = verts - c_rot
            v_u = np.dot(v_proj, u_ax)
            v_v = np.dot(v_proj, v_ax)
            v_theta = np.arctan2(v_v, v_u) % (2.0 * np.pi)

            diag_total = float(np.linalg.norm(np.ptp(np.asarray(verts), axis=0))) or 1.0
            sector_devs = []
            sector_pts_list = []
            positions_checked = 0
            per_pos_devs = []
            transforms_to_repair = []

            # 1. Identify healthiest sector with minimal variation
            for k in range(order):
                th_k = k * step_angle
                mask_k = np.abs((v_theta - th_k + np.pi) % (2.0 * np.pi) - np.pi) <= (step_angle * 0.55)
                pts_k = verts[mask_k]
                sector_pts_list.append(pts_k)
                if len(pts_k) < 4:
                    sector_devs.append(1.0)
                    continue

                R_step = _rotation_matrix(step_angle, axis, point=c_rot)[:3, :3]
                pred_pts = (pts_k - c_rot) @ R_step.T + c_rot
                _, dists, _ = sutura_geom.closest_points(orig_v, orig_t, pred_pts)
                dev = float(np.mean(dists))
                sector_devs.append(dev)

            best_k = int(np.argmin(sector_devs))
            template_pts = sector_pts_list[best_k]
            elem_diag = float(np.linalg.norm(template_pts.max(0) - template_pts.min(0))) or 1.0

            # 2. Compare each sector directly to the template sector
            for k in range(order):
                positions_checked += 1
                rot_angle = (k - best_k) * step_angle
                R_k = _rotation_matrix(rot_angle, axis, point=c_rot)
                pts_k = sector_pts_list[k]
                if len(pts_k) >= 4:
                    T_k, _ = align(template_pts, pts_k, max_iters=5)
                else:
                    T_k = R_k
                pred_pts = (template_pts @ T_k[:3, :3].T) + T_k[:3, 3]
                _, dists, _ = sutura_geom.closest_points(orig_v, orig_t, pred_pts)
                dev = float(np.sqrt(np.mean(dists ** 2))) / elem_diag
                max_dev = float(np.max(dists)) / elem_diag
                per_pos_devs.append({'index': k, 'deviation': round(dev, 4)})

                if max_dev >= 0.04 or dev >= 0.015:
                    transforms_to_repair.append(T_k)
                    dam_pts_mask = dists >= max(0.4 * np.max(dists), 0.005 * elem_diag)
                    if np.sum(dam_pts_mask) >= 3:
                        dam_template_pts = template_pts[dam_pts_mask]
                        dam_span = dam_template_pts.max(0) - dam_template_pts.min(0)
                        if np.max(dam_span) < 0.8 * elem_diag:
                            c_dam_loc = np.mean(dam_template_pts, axis=0)
                            local_mask = np.linalg.norm(template_pts - c_dam_loc, axis=1) <= (np.linalg.norm(dam_span) * 0.75 + 1.5)
                            if np.sum(local_mask) >= 8:
                                template_pts = template_pts[local_mask]

        if transforms_to_repair:
            rep_v, rep_t, vol_change, dst_boxes, is_watertight = _execute_boolean_transplant(
                verts, tris, template_pts, transforms_to_repair, margin=margin, force_bridge=force_bridge
            )
            positions_repaired = len(transforms_to_repair)
            h_outside = (_compute_hausdorff_outside(orig_v, orig_t, rep_v, rep_t, dst_boxes)
                         if dst_boxes else 0.0)
            notes = [f"{positions_repaired} repeated elements were replaced by healthy copies"]
        else:
            rep_v, rep_t = verts, tris
            vol_change = 0.0
            h_outside = 0.0
            is_watertight = True
            positions_repaired = 0
            notes = ["All detected pattern positions are healthy"]

        return rep_v, rep_t, {
            'watertight': bool(is_watertight),
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
