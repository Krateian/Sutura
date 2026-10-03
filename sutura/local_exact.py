#!/usr/bin/env python3
"""Localized exact self-union of residual self-intersection clusters.

After Stage 2 (manifold3d rebuild) and the reload-safe P-WELD pass a mesh can
still carry residual proper self-intersections: manifold3d enforces
combinatorial 2-manifold topology, so intra-shell topological folds and
overlapping caps are accepted as long as every edge has two incident faces.
The hypothesis behind this module is that those residuals are spatially
local, so a global exact arrangement (Method 6) or the global float
snap-rounding autorefine (Method 5) would be wasteful and does not finish on
dense scans.

MEASURED OUTCOME (2026-09, 40-mesh real-world corpus + a private heavy fold):
the locality hypothesis does NOT hold for densely folded scans.  Across the 12
corpus meshes with residual SI the tier accepts clusters on one mesh only
(``thingi10k_63785``, 81 -> 75 of 81) and on the private ``ornate-frame`` fold
it accepts 1 of 80 clusters because the whole shell is folded (97 %
``boundary_split``).  It is therefore OFF by default, opt-in with
``SUTURA_LOCAL_EXACT=1``; see ``docs/local-exact-notes.md``.  The code is kept
as a tested foundation for a future global snap-rounding pass.

This module resolves residuals patch-by-patch:

1.  Detect the self-intersecting faces (Rust exact classifier up to
    ``SI_MAX_FACES``, PyMeshLab's spatial-subdivision filter above it).
2.  Partition them into edge-connected clusters and dilate each cluster by
    ``LOCAL_EXACT_K_RINGS`` vertex rings, merging overlapping patches so the
    patches are strictly disjoint.
3.  Run the exact arrangement (``sutura_geom.arrangement_lite``) on the patch
    only, then keep the facets that lie on the outer hull of the WHOLE mesh:
    the generalized winding number is queried against a BVH built over every
    face of the full mesh, never the open patch alone.  A facet is kept when
    its outward-offset point is outside (``W < 0.5``) and its inward-offset
    point is inside (``W >= 0.5``); interior sheets and folded-back layers are
    discarded.
4.  Verify the retained patch: its directed boundary edges must equal the
    input patch's bit-identically and its own self-intersections must be zero.
    If an intersection touched or split a patch boundary edge the cluster is
    re-dilated (up to ``LOCAL_EXACT_MAX_K``) or skipped.
5.  Stitch the patch back with the original boundary vertices untouched, then
    accept the cluster only when the reload-honest (holes, non-manifold) count
    did not increase; otherwise roll the cluster back atomically.

Runs after Stage 2 + P-WELD, is gated by ``SI > 0`` and an intensity time
budget, and is byte-identical to the input when there is no self-intersection
(or when no cluster is accepted).  Pure numpy + stdlib; ``sutura_geom``,
pymeshlab and ``repair`` are imported lazily.
"""
import time

import numpy as np

# Per-intensity wall-clock budget for the tier (seconds); quick disables it.
LOCAL_EXACT_BUDGET_BY_INTENSITY = {
    'quick': 0.0,
    'balanced': 10.0,
    'thorough': 45.0,
    'extreme': 120.0,
}
LOCAL_EXACT_DEFAULT_BUDGET = 10.0

# Patch construction.
LOCAL_EXACT_K_RINGS = 2
LOCAL_EXACT_MAX_K = 4
LOCAL_EXACT_MAX_PATCH_FACES = 2000
LOCAL_EXACT_PER_CLUSTER_TIMEOUT = 3.0

# Winding outer-hull filter offsets: epsilon = min(diag*EPS_DIAG, min_edge*EPS_EDGE).
LOCAL_EXACT_EPS_DIAG = 1e-3
LOCAL_EXACT_EPS_EDGE = 0.1

# Hard cap on the number of clusters processed (defensive; the budget is the
# real brake).
LOCAL_EXACT_MAX_CLUSTERS = 5000

# Cap on the rejected-cluster entries kept in the report (stdout hygiene).
LOCAL_EXACT_MAX_REJECTED = 50


def _require_geom():
    """Return the ``sutura_geom`` module or raise ImportError."""
    import sutura_geom
    return sutura_geom


def _get_ml(ml):
    """Return a PyMeshLab module, importing it lazily when not supplied."""
    if ml is not None:
        return ml
    try:
        import pymeshlab as ml
        return ml
    except Exception:  # noqa: BLE001 - optional in this process
        return None


def detect_si_face_mask(ml, verts, tris):
    """Boolean ``(F,)`` mask of self-intersecting faces and the count.

    Uses the Rust exact classifier when the face count is within its cap; for
    larger meshes falls back to PyMeshLab's
    ``compute_selection_by_self_intersections_per_face`` (spatial subdivision).
    Returns ``(mask, count)``; on any failure returns an all-False mask with
    count ``-1`` (the caller treats that as "no SI", a safe no-op).
    """
    F = len(tris)
    if F == 0:
        return np.zeros(0, dtype=bool), 0
    ml = _get_ml(ml)
    geom = None
    try:
        geom = _require_geom()
    except Exception:  # noqa: BLE001 - extension optional
        geom = None
    if geom is not None:
        cap = int(getattr(geom, 'SI_MAX_FACES', 150000))
        if F <= cap:
            try:
                mask, count = geom.self_intersecting_faces(
                    np.ascontiguousarray(np.asarray(verts, dtype=np.float64)),
                    np.ascontiguousarray(np.asarray(tris, dtype=np.int32)))
                mask = np.asarray(mask, dtype=bool)
                if int(count) >= 0:
                    return mask, int(count)
            except Exception:  # noqa: BLE001 - fall through to PyMeshLab
                pass
    if ml is None:
        return np.zeros(F, dtype=bool), -1
    try:
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(
            vertex_matrix=np.asarray(verts, dtype=np.float64),
            face_matrix=np.asarray(tris, dtype=np.int32)))
        ms.apply_filter('compute_selection_by_self_intersections_per_face')
        sel = np.asarray(ms.current_mesh().face_selection_array(), dtype=bool)
        return sel, int(sel.sum())
    except Exception:  # noqa: BLE001 - detector failure must not break a repair
        return np.zeros(F, dtype=bool), -1


def _edge_keys_undirected(tris):
    """Canonical (min,max) integer key per half-edge, block-stacked.

    Returns ``keys`` of shape ``(3*F,)`` where entries ``[0:F]`` are the
    ``(0,1)`` edges, ``[F:2F]`` the ``(1,2)`` edges and ``[2F:3F]`` the
    ``(2,0)`` edges (the documented block-stacking)."""
    tris = np.asarray(tris, dtype=np.int64)
    F = len(tris)
    a = np.concatenate([tris[:, 0], tris[:, 1], tris[:, 2]])
    b = np.concatenate([tris[:, 1], tris[:, 2], tris[:, 0]])
    V = int(tris.max()) + 1 if F else 1
    return np.minimum(a, b) * V + np.maximum(a, b)


def extract_si_clusters(tris, si_mask):
    """Partition the self-intersecting faces into edge-connected components.

    Two SI faces belong to the same cluster when they share a topological
    edge. Returns a list of ``np.ndarray`` face-index arrays.
    """
    tris = np.asarray(tris, dtype=np.int64)
    F = len(tris)
    si_idx = np.flatnonzero(np.asarray(si_mask, dtype=bool))
    if len(si_idx) == 0:
        return []
    parent = np.arange(F, dtype=np.int64)

    def find(x):
        root = x
        while parent[root] != root:
            root = parent[root]
        while parent[x] != root:
            parent[x], x = root, parent[x]
        return root

    si_pos = np.zeros(F, dtype=bool)
    si_pos[si_idx] = True
    keys = _edge_keys_undirected(tris)
    edge_face = np.concatenate([np.arange(F), np.arange(F), np.arange(F)])
    # Only edges with both endpoints on SI faces can join two SI faces.
    keep = si_pos[edge_face]
    keys, edge_face = keys[keep], edge_face[keep]
    order = np.argsort(keys, kind='stable')
    keys, edge_face = keys[order], edge_face[order]
    # Group equal edge keys; union every pair in a group.
    starts = np.flatnonzero(np.concatenate([[True], keys[1:] != keys[:-1]]))
    ends = np.concatenate([starts[1:], [len(keys)]])
    for s, e in zip(starts.tolist(), ends.tolist()):
        if e - s < 2:
            continue
        f0 = int(edge_face[s])
        for k in range(s + 1, e):
            ra, rb = find(f0), find(int(edge_face[k]))
            if ra != rb:
                parent[rb] = ra
    groups = {}
    for f in si_idx.tolist():
        groups.setdefault(find(f), []).append(f)
    return [np.asarray(g, dtype=np.int64) for g in groups.values()]


def _vertex_face_csr(tris):
    """CSR vertex -> incident face indices (``face_of_vert`` + ``vert_start``)."""
    tris = np.asarray(tris, dtype=np.int64)
    F = len(tris)
    V = int(tris.max()) + 1 if F else 1
    verts = tris.ravel()
    faces = np.repeat(np.arange(F, dtype=np.int64), 3)
    order = np.argsort(verts, kind='stable')
    verts, faces = verts[order], faces[order]
    counts = np.bincount(verts, minlength=V)
    start = np.zeros(V + 1, dtype=np.int64)
    np.cumsum(counts, out=start[1:])
    return faces, start


def _faces_vertices(faces, tris):
    """Unique vertex indices referenced by ``faces`` (pass tris directly)."""
    return np.unique(np.asarray(tris, dtype=np.int64)[np.asarray(faces, dtype=np.int64)].ravel())


def dilate_and_merge_patches(tris, clusters, k_rings=LOCAL_EXACT_K_RINGS,
                             max_patch_faces=LOCAL_EXACT_MAX_PATCH_FACES):
    """Dilate clusters by ``k_rings`` vertex rings and merge overlaps.

    Returns ``(patches, skipped)``: ``patches`` is a list of dicts
    ``{'faces': sorted int64 array, 'si_faces': int64 array, 'k': int}`` that
    are pairwise disjoint and within ``max_patch_faces``; ``skipped`` counts
    raw patches dropped by the size brake.
    """
    tris = np.asarray(tris, dtype=np.int64)
    if not clusters:
        return [], 0
    face_of_vert, vert_start = _vertex_face_csr(tris)

    def grow(seed, k):
        cur = np.asarray(seed, dtype=np.int64)
        for _ in range(int(k)):
            vs = _faces_vertices(cur, tris)
            cnt = vert_start[vs + 1] - vert_start[vs]
            total = int(cnt.sum())
            if total == 0:
                break
            idx = np.repeat(vert_start[vs], cnt)
            offs = np.arange(total) - np.repeat(np.cumsum(cnt) - cnt, cnt)
            cur = np.unique(face_of_vert[idx + offs])
        return cur

    raw = [grow(c, k_rings) for c in clusters]
    # Union-find over raw patches; merge any two sharing a face.
    n = len(raw)
    parent = list(range(n))

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    face_to_patch = {}
    for pi, faces in enumerate(raw):
        for f in faces.tolist():
            prev = face_to_patch.get(f)
            if prev is None:
                face_to_patch[f] = pi
            else:
                ra, rb = find(prev), find(pi)
                if ra != rb:
                    parent[rb] = ra
    merged = {}
    for pi in range(n):
        merged.setdefault(find(pi), []).append(pi)

    patches, skipped = [], 0
    for roots in merged.values():
        root = roots[0]
        faces = raw[root]
        si = np.asarray(clusters[root], dtype=np.int64)
        for other in roots[1:]:
            faces = np.union1d(faces, raw[other])
            si = np.concatenate([si, np.asarray(clusters[other], dtype=np.int64)])
        if len(faces) > max_patch_faces:
            skipped += 1
            continue
        patches.append({'faces': np.sort(np.unique(faces)),
                        'si_faces': np.unique(si), 'k': int(k_rings)})
    return patches, skipped


def _directed_boundary_edges(t_local):
    """Set of directed boundary edges ``(u, v)`` of a local triangle soup.

    An undirected edge is a boundary edge when it is used exactly once; its
    direction is taken from the face that uses it.
    """
    t = np.asarray(t_local, dtype=np.int64)
    F = len(t)
    if F == 0:
        return set()
    keys = _edge_keys_undirected(t)
    uniq, counts = np.unique(keys, return_counts=True)
    single = set(int(k) for k in uniq[counts == 1].tolist())
    if not single:
        return set()
    a = np.concatenate([t[:, 0], t[:, 1], t[:, 2]])
    b = np.concatenate([t[:, 1], t[:, 2], t[:, 0]])
    V = int(t.max()) + 1
    out = set()
    for i in range(len(a)):
        k = int(min(a[i], b[i])) * V + int(max(a[i], b[i]))
        if k in single:
            out.add((int(a[i]), int(b[i])))
    return out


def _match_patch_vertices(v_patch, arr_v):
    """Map each arrangement vertex to a patch-local index, or -1 if new.

    Matching is bit-exact on float64 coordinates: the arrangement round-trips
    input f64 through exact rationals and back, so an original vertex is
    recovered exactly."""
    npat = len(v_patch)
    if npat == 0 or len(arr_v) == 0:
        return np.full(len(arr_v), -1, dtype=np.int64)
    vp = np.ascontiguousarray(np.asarray(v_patch, dtype=np.float64))
    av = np.ascontiguousarray(np.asarray(arr_v, dtype=np.float64))
    both = np.concatenate([vp, av])
    uniq, inv = np.unique(both, axis=0, return_inverse=True)
    inv = np.asarray(inv).reshape(-1)
    pat_uid = inv[:npat]
    arr_uid = inv[npat:]
    uid_to_pat = np.full(len(uniq), -1, dtype=np.int64)
    # first patch vertex wins; duplicate coordinates are ambiguous but the
    # caller rejects a patch whose boundary cannot be recovered unambiguously.
    for local, uid in enumerate(pat_uid.tolist()):
        if uid_to_pat[uid] < 0:
            uid_to_pat[uid] = local
    return uid_to_pat[arr_uid]


def _facet_winding_keep(bvh, arr_v, arr_t, diag):
    """Boolean mask of facets on the outer hull of the whole mesh.

    For each facet the centroid ``c`` and unit normal ``n`` are computed, and
    the generalized winding number is queried at ``c +/- eps*n`` against the
    whole-mesh BVH.  Kept when outward is outside (``W < 0.5``) and inward is
    inside (``W >= 0.5``)."""
    t = np.asarray(arr_t, dtype=np.int64)
    v = np.asarray(arr_v, dtype=np.float64)
    F = len(t)
    if F == 0:
        return np.zeros(0, dtype=bool)
    a, b, c = v[t[:, 0]], v[t[:, 1]], v[t[:, 2]]
    n = np.cross(b - a, c - a)
    nn = np.linalg.norm(n, axis=1)
    good = nn > 0.0
    n = n / np.where(nn > 0.0, nn, 1.0)[:, None]
    cent = (a + b + c) / 3.0
    # min edge length per facet
    e = np.stack([np.linalg.norm(b - a, axis=1),
                  np.linalg.norm(c - b, axis=1),
                  np.linalg.norm(a - c, axis=1)], axis=1)
    hmin = e.min(axis=1)
    eps = np.minimum(diag * LOCAL_EXACT_EPS_DIAG, hmin * LOCAL_EXACT_EPS_EDGE)
    eps = np.where((eps > 0.0) & good, eps, 0.0)
    good &= eps > 0.0
    if not good.any():
        return np.zeros(F, dtype=bool)
    p_plus = cent + eps[:, None] * n
    p_minus = cent - eps[:, None] * n
    pts = np.concatenate([p_plus, p_minus]).astype(np.float64)
    try:
        w = np.asarray(bvh.winding_points(np.ascontiguousarray(pts)),
                       dtype=np.float64)
    except Exception:  # noqa: BLE001 - an extension failure is a no-op
        return np.zeros(F, dtype=bool)
    w_plus, w_minus = w[:F], w[F:]
    return good & (w_plus < 0.5) & (w_minus >= 0.5)


def exact_self_union_patch(bvh, verts, patch_idx, v_patch, t_local, si_faces_local,
                           diag):
    """Run the exact arrangement on one patch and keep its outer-hull facets.

    Returns ``(result, reason)`` where ``result`` is a dict with
    ``ret_local_t`` (indices into ``arr_v``), ``arr_v``, ``local_to_patch`` or
    None when the patch is rejected with ``reason``."""
    geom = _require_geom()
    try:
        arr_v, arr_t, _rep = geom.arrangement_lite(
            np.ascontiguousarray(v_patch, dtype=np.float64),
            np.ascontiguousarray(t_local, dtype=np.int32))
    except BaseException as e:  # noqa: BLE001 - a Rust panic must not crash
        if isinstance(e, (KeyboardInterrupt, SystemExit)):
            raise
        return None, 'arrangement_failed: %s' % type(e).__name__
    arr_v = np.asarray(arr_v, dtype=np.float64)
    arr_t = np.asarray(arr_t, dtype=np.int64)
    if len(arr_t) == 0:
        return None, 'empty_arrangement'

    keep = _facet_winding_keep(bvh, arr_v, arr_t, diag)
    ret = arr_t[keep]
    # Drop degenerate / repeated faces produced by the CDT and drop faces that
    # reference the same vertex twice.
    if len(ret):
        ok = ((ret[:, 0] != ret[:, 1]) & (ret[:, 1] != ret[:, 2])
              & (ret[:, 0] != ret[:, 2]))
        ret = ret[ok]
    if len(ret) == 0:
        return None, 'no_outer_hull'

    local_to_patch = _match_patch_vertices(v_patch, arr_v)  # -1 = new vertex

    # Boundary conformance: every derived boundary directed edge must be
    # between original patch vertices and must equal the input patch boundary.
    input_bnd = _directed_boundary_edges(t_local)
    # Recompute the arrangement's boundary; any endpoint that is new means an
    # intersection touched/split a patch boundary edge.
    ret_vids = np.unique(ret.ravel())
    arr_bnd = _directed_boundary_edges(ret)
    ret_bnd = set()
    for (u, v) in arr_bnd:
        pu, pv = int(local_to_patch[u]), int(local_to_patch[v])
        if pu < 0 or pv < 0:
            return None, 'boundary_split'
        ret_bnd.add((pu, pv))
    if ret_bnd != input_bnd:
        return None, 'boundary_mismatch'

    # Absolute self-intersections among the retained patch facets.
    try:
        _m, cnt = geom.self_intersecting_faces(
            np.ascontiguousarray(arr_v, dtype=np.float64),
            np.ascontiguousarray(ret, dtype=np.int32))
    except Exception:  # noqa: BLE001 - treat a failure as unverified
        return None, 'si_check_failed'
    if int(cnt) != 0:
        return None, 'residual_si:%d' % int(cnt)

    return ({'arr_v': arr_v, 'ret': ret, 'local_to_patch': local_to_patch,
             'patch_idx': patch_idx}, None)


def local_exact_self_union(ml, verts, tris, time_budget=LOCAL_EXACT_DEFAULT_BUDGET,
                           k_rings=LOCAL_EXACT_K_RINGS,
                           max_patch_faces=LOCAL_EXACT_MAX_PATCH_FACES,
                           per_cluster_timeout=LOCAL_EXACT_PER_CLUSTER_TIMEOUT):
    """Resolve residual self-intersection clusters locally and exactly.

    Runs after Stage 2 + P-WELD.  Returns ``(verts, tris, report)``; when
    ``time_budget <= 0``, there is no self-intersection, or no cluster is
    accepted, the input arrays are returned unchanged (byte-identical no-op)
    and ``report['applied']`` is False.
    """
    report = {'ran': False, 'applied': False, 'reason': None,
              'si_before': 0, 'si_after': None, 'clusters': 0,
              'patches': 0, 'accepted': 0, 'skipped': 0,
              'rejected': [], 'budget_s': (None if time_budget is None
                                           else float(time_budget)),
              'seconds': 0.0}
    v0 = np.asarray(verts)
    t0 = np.asarray(tris)
    t_in = t0
    if time_budget is not None and time_budget <= 0.0:
        report['reason'] = 'budget'
        return v0, t_in, report
    if len(t0) == 0:
        report['reason'] = 'empty'
        return v0, t_in, report
    t0 = np.asarray(t0, dtype=np.int64)
    try:
        geom = _require_geom()
    except Exception as e:  # noqa: BLE001 - extension absent -> no-op
        report['reason'] = 'extension_unavailable: %s' % e
        return v0, t_in, report

    t_start = time.perf_counter()
    ml = _get_ml(ml)
    si_mask, si_count = detect_si_face_mask(ml, v0, t0)
    report['si_before'] = int(si_count)
    report['ran'] = True
    if si_count <= 0 or not si_mask.any():
        report['reason'] = 'no_si'
        report['si_after'] = int(max(si_count, 0))
        report['seconds'] = round(time.perf_counter() - t_start, 3)
        return v0, t_in, report

    clusters = extract_si_clusters(t0, si_mask)[:LOCAL_EXACT_MAX_CLUSTERS]
    report['clusters'] = len(clusters)
    if not clusters:
        report['reason'] = 'no_clusters'
        report['seconds'] = round(time.perf_counter() - t_start, 3)
        return v0, t_in, report

    patches, skipped_large = dilate_and_merge_patches(
        t0, clusters, k_rings=k_rings, max_patch_faces=max_patch_faces)
    report['patches'] = len(patches)
    report['skipped'] += int(skipped_large)
    if not patches:
        report['reason'] = 'all_patches_too_large'
        report['seconds'] = round(time.perf_counter() - t_start, 3)
        return v0, t_in, report
    # Largest SI first (the plan's priority order).
    patches.sort(key=lambda p: len(p['si_faces']), reverse=True)

    diag = float(np.linalg.norm(
        np.asarray(v0, dtype=np.float64).max(0)
        - np.asarray(v0, dtype=np.float64).min(0)))
    if not np.isfinite(diag) or diag <= 0.0:
        report['reason'] = 'degenerate_bbox'
        report['seconds'] = round(time.perf_counter() - t_start, 3)
        return v0, t_in, report

    # ONE BVH over the WHOLE mesh (non-negotiable correction #1).
    try:
        bvh = geom.PyMeshBvh(
            np.ascontiguousarray(np.asarray(v0, dtype=np.float64)),
            np.ascontiguousarray(t0.astype(np.int32)))
    except Exception as e:  # noqa: BLE001
        report['reason'] = 'bvh_failed: %s' % e
        report['seconds'] = round(time.perf_counter() - t_start, 3)
        return v0, t_in, report

    face_of_vert, vert_start = _vertex_face_csr(t0)

    def grow(seed, k):
        cur = np.unique(np.asarray(seed, dtype=np.int64))
        for _ in range(int(k)):
            vs = _faces_vertices(cur, t0)
            cnt = vert_start[vs + 1] - vert_start[vs]
            total = int(cnt.sum())
            if total == 0:
                break
            idx = np.repeat(vert_start[vs], cnt)
            offs = np.arange(total) - np.repeat(np.cumsum(cnt) - cnt, cnt)
            cur = np.unique(face_of_vert[idx + offs])
        return cur

    # Baseline reload-honest topology.
    try:
        from repair import reload_strict_holes_nm as _reload
    except Exception:  # noqa: BLE001
        def _reload(vv, tt):
            import defects
            wv, wt = _weld_reload(vv, tt)
            d = defects.detect(wv, wt)
            return len(d['holes']), len(d['non_manifold'])
    base_h, base_nm = _reload(v0, t0)

    cur_v = np.asarray(v0, dtype=np.float32)
    cur_t = t0.astype(np.int32).copy()
    cur_h, cur_nm = base_h, base_nm
    claimed = np.zeros(len(t0), dtype=bool)
    patch_faces_list = [p['faces'] for p in patches]

    for pi, patch in enumerate(patches):
        if time.perf_counter() - t_start > time_budget:
            report['reason'] = 'budget'
            break
        faces = patch['faces']
        if claimed[faces].any():
            report['skipped'] += 1
            continue
        vp_idx = np.unique(t0[faces].ravel())
        remap = np.full(int(t0.max()) + 1, -1, dtype=np.int64)
        remap[vp_idx] = np.arange(len(vp_idx))
        t_local = remap[t0[faces]]

        # Try the built patch, then grow the ring on boundary failures.
        result, reason = None, None
        try_faces = [faces]
        for kk in range(patch['k'] + 1, LOCAL_EXACT_MAX_K + 1):
            if kk > LOCAL_EXACT_MAX_K:
                break
            grown = grow(patch['si_faces'], kk)
            if len(grown) > max_patch_faces:
                break
            # keep only if disjoint from every other patch
            if _overlaps_any(grown, patch_faces_list, pi):
                break
            try_faces.append(grown)

        for cand_faces in try_faces:
            vp_idx = np.unique(t0[cand_faces].ravel())
            remap = np.full(int(t0.max()) + 1, -1, dtype=np.int64)
            remap[vp_idx] = np.arange(len(vp_idx))
            t_local = remap[t0[cand_faces]]
            v_patch = np.asarray(v0, dtype=np.float64)[vp_idx]
            result, reason = exact_self_union_patch(
                bvh, v0, cand_faces, v_patch, t_local, None, diag)
            if result is not None:
                break
        if result is None:
            report['skipped'] += 1
            report['rejected'].append(
                {'cluster': pi, 'faces': int(len(faces)), 'reason': reason})
            continue

        # Build the stitched candidate.  Original vertices keep their indices;
        # new CDT vertices are appended (float32).
        local_to_patch = result['local_to_patch']
        new_mask = local_to_patch < 0
        n_new = int(new_mask.sum())
        n_old = len(cur_v)
        appended = result['arr_v'][new_mask].astype(np.float32)
        cand_arr_v = np.vstack([cur_v, appended]) if n_new else cur_v
        remap_g = np.full(len(result['arr_v']), -1, dtype=np.int64)
        old_local = ~new_mask
        remap_g[old_local] = vp_idx[local_to_patch[old_local]]
        extra = np.flatnonzero(new_mask)
        remap_g[extra] = n_old + np.arange(n_new)
        ret_global = remap_g[result['ret']]
        keep_mask = np.ones(len(cur_t), dtype=bool)
        keep_mask[cand_faces] = False
        cand_t = np.vstack([cur_t[keep_mask], ret_global.astype(np.int32)])

        try:
            cand_h, cand_nm = _reload(cand_arr_v, cand_t)
        except Exception as e:  # noqa: BLE001
            report['skipped'] += 1
            report['rejected'].append(
                {'cluster': pi, 'faces': int(len(faces)),
                 'reason': 'reload_check_failed: %s' % e})
            continue
        if cand_h > cur_h or cand_nm > cur_nm:
            report['skipped'] += 1
            report['rejected'].append(
                {'cluster': pi, 'faces': int(len(faces)),
                 'reason': 'new_holes_nm:%d/%d' % (cand_h, cand_nm)})
            continue

        # Commit.
        cur_v, cur_t = cand_arr_v, cand_t
        cur_h, cur_nm = cand_h, cand_nm
        claimed[cand_faces] = True
        report['accepted'] += 1

    applied = report['accepted'] > 0
    report['applied'] = bool(applied)
    report['rejected'] = report['rejected'][:LOCAL_EXACT_MAX_REJECTED]
    if not applied:
        report['reason'] = report['reason'] or 'no_cluster_accepted'
        report['si_after'] = report['si_before']
        report['seconds'] = round(time.perf_counter() - t_start, 3)
        return v0, t_in, report

    report['si_after'] = _si_after_count(ml, cur_v, cur_t)
    report['holes_before'] = int(base_h)
    report['non_manifold_before'] = int(base_nm)
    report['holes_after'] = int(cur_h)
    report['non_manifold_after'] = int(cur_nm)
    report['seconds'] = round(time.perf_counter() - t_start, 3)
    if report['reason'] == 'budget':
        report['reason'] = None
    return np.asarray(cur_v, dtype=np.float32), np.asarray(cur_t, dtype=np.int32), report


def _overlaps_any(faces, patch_faces_list, skip):
    for j, other in enumerate(patch_faces_list):
        if j == skip:
            continue
        if np.isin(faces, other, assume_unique=False).any():
            return True
    return False


def _si_after_count(ml, verts, tris):
    _mask, count = detect_si_face_mask(ml, verts, tris)
    return int(max(count, 0))


def _weld_reload(verts, tris):
    """Minimal local copy of ``repair.weld_reload_equivalent`` (fallback when
    ``repair`` cannot be imported)."""
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int64)
    if len(v) == 0 or len(t) == 0:
        return v, t
    unique, inverse = np.unique(v, axis=0, return_inverse=True)
    inverse = np.asarray(inverse).reshape(-1)
    t = inverse[t.reshape(-1)].reshape(t.shape).astype(np.int64)
    nondeg = ((t[:, 0] != t[:, 1]) & (t[:, 1] != t[:, 2])
              & (t[:, 0] != t[:, 2]))
    t = t[nondeg]
    if len(t) == 0:
        return np.zeros((0, 3), dtype=np.float32), t
    keys = np.sort(t, axis=1)
    _uniq, first = np.unique(keys, axis=0, return_index=True)
    t = t[np.sort(first)]
    used = np.unique(t)
    remap = np.full(len(unique), -1, dtype=np.int64)
    remap[used] = np.arange(len(used))
    return unique[used], remap[t]
