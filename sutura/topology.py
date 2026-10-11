"""Integer/float topology kernel (Phase-2 profiling, 0.8.x).

One dispatch layer over the Rust ``sutura_geom`` extension for the three
topology primitives the repair pipeline performs most often on plain
``verts``/``tris`` arrays:

* :func:`edge_table` -- unique undirected edges + use counts + the half-edge
  -> edge-id inverse map.
* :func:`weld_vertices` -- ``np.unique(v, axis=0)`` (float32 position weld).
* :func:`weld_reload_equivalent` -- the whole P-HONEST STL save/reload weld.

The pure-numpy implementations in this module are the oracle: the Rust outputs
must be bit-for-bit identical (this is enforced by
``tests/test_topology_parity.py``).  When the extension is absent, or the input
is non-finite, the numpy path is used transparently.  ``SUTURA_TOPOLOGY=python``
(or :func:`set_engine`) forces the oracle.

stdlib + numpy only, by design: importable wherever ``defects.py`` is.
"""
import os
import time
import numpy as np

try:  # optional native acceleration
    import sutura_geom as _geom
except ImportError:  # pragma: no cover - exercised on installs without Rust
    _geom = None

_MODE = None  # None = auto, otherwise 'rust' or 'python'


def set_engine(mode):
    """Force the topology engine: ``'auto'``, ``'rust'`` or ``'python'``.

    Intended for tests and debugging; the default is ``'auto'`` (Rust when the
    extension is importable, numpy otherwise).  Raises ``ValueError`` on an
    unknown mode."""
    global _MODE
    if mode not in ('auto', 'rust', 'python'):
        raise ValueError("engine must be 'auto', 'rust' or 'python'")
    _MODE = None if mode == 'auto' else mode


def engine():
    """Return the active engine name (``'rust'`` or ``'python'``)."""
    if _MODE in ('rust', 'python'):
        return _MODE
    env = os.environ.get('SUTURA_TOPOLOGY', '').strip().lower()
    if env in ('rust', 'python'):
        return env
    return 'rust' if _rust_kernel_available() else 'python'


_RUST_FNS = (
    'edge_table',
    'weld_vertices',
    'weld_reload_equivalent',
    'remove_duplicate_faces',
    'remove_null_faces',
    'remove_duplicate_vertices',
    'remove_unreferenced_vertices',
    'remove_connected_components',
    'invert_face_orientation',
    'mesh_topology',
    'self_intersecting_faces',
)


def _rust_kernel_available():
    """True when the importable extension carries all topology kernel functions.

    An older/pre-kernel wheel imports fine but lacks the functions; the
    fallback must be transparent, so this is checked rather than assuming the
    module is enough."""
    return _geom is not None and all(hasattr(_geom, f) for f in _RUST_FNS)


def _use_rust():
    return _rust_kernel_available() and engine() == 'rust'


def _has_rust(name):
    return _geom is not None and hasattr(_geom, name) and engine() == 'rust'


# --- numpy oracle --------------------------------------------------------- #
def _edge_table_numpy(t):
    he = np.concatenate([t[:, [0, 1]], t[:, [1, 2]], t[:, [2, 0]]], axis=0)
    key = np.sort(he, axis=1)
    uniq, inv = np.unique(key, axis=0, return_inverse=True)
    inv = np.asarray(inv).reshape(-1).astype(np.int64)
    counts = np.bincount(inv, minlength=len(uniq)).astype(np.int64)
    return uniq, counts, inv


def _weld_vertices_numpy(v):
    uniq, inv = np.unique(v, axis=0, return_inverse=True)
    return uniq, np.asarray(inv).reshape(-1).astype(np.int64)


def _weld_reload_equivalent_numpy(verts, tris):
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int64)
    if len(v) == 0 or len(t) == 0:
        return v, t
    unique, inverse = np.unique(v, axis=0, return_inverse=True)
    inverse = np.asarray(inverse).reshape(-1)
    t = inverse[t.reshape(-1)].reshape(t.shape).astype(np.int64)
    nondeg = ((t[:, 0] != t[:, 1]) & (t[:, 1] != t[:, 2]) & (t[:, 0] != t[:, 2]))
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


def _remove_duplicate_faces_numpy(tris):
    t = np.asarray(tris, dtype=np.int64)
    if len(t) <= 1:
        return t
    keys = np.sort(t, axis=1)
    keys_rev = keys[::-1]
    _, first_rev = np.unique(keys_rev, axis=0, return_index=True)
    last_idx = len(keys) - 1 - first_rev
    last_idx.sort()
    return t[last_idx]


def _remove_null_faces_numpy(verts, tris):
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int64)
    if len(t) == 0:
        return t
    nonrep = (t[:, 0] != t[:, 1]) & (t[:, 1] != t[:, 2]) & (t[:, 0] != t[:, 2])
    t_cand = t[nonrep]
    if len(t_cand) == 0:
        return t_cand
    p0 = v[t_cand[:, 0]]
    p1 = v[t_cand[:, 1]]
    p2 = v[t_cand[:, 2]]
    d1 = p1 - p0
    d2 = p2 - p0
    cross = np.cross(d1, d2)
    norm2 = np.sum(cross**2, axis=1)
    valid = (norm2 > 0.0) & np.isfinite(norm2)
    return t_cand[valid]


def _remove_duplicate_vertices_numpy(verts, tris):
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int64)
    if len(v) == 0:
        return v, t
    _, first_idx, inverse = np.unique(v, axis=0, return_index=True, return_inverse=True)
    order = np.argsort(first_idx)
    unique_verts = v[first_idx[order]]
    order_remap = np.empty_like(order)
    order_remap[order] = np.arange(len(order))
    new_t = order_remap[inverse[t]]
    valid = (new_t[:, 0] != new_t[:, 1]) & (new_t[:, 1] != new_t[:, 2]) & (new_t[:, 0] != new_t[:, 2])
    return unique_verts, new_t[valid]


def _remove_unreferenced_vertices_numpy(verts, tris):
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int64)
    if len(v) == 0:
        return v, t
    if len(t) == 0:
        return v[:0], t
    used = np.unique(t)
    if len(used) == len(v) and np.array_equal(used, np.arange(len(v))):
        return v, t
    remap = np.full(len(v), -1, dtype=np.int64)
    remap[used] = np.arange(len(used))
    return v[used], remap[t]


def _remove_connected_components_numpy(verts, tris, min_faces=25, remove_unref=True):
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int64)
    if len(t) == 0 or min_faces <= 1:
        return v, t
    F = len(t)
    _edges, _counts, inv = _edge_table_numpy(t)
    face_of_h = np.tile(np.arange(F, dtype=np.int64), 3)

    order = np.argsort(inv, kind='stable')
    sorted_inv = inv[order]
    sorted_face = face_of_h[order]

    parent = list(range(F))
    def find(x):
        root = x
        while parent[root] != root:
            root = parent[root]
        while parent[x] != root:
            nxt = parent[x]
            parent[x] = root
            x = nxt
        return root

    def union(x, y):
        rx, ry = find(x), find(y)
        if rx != ry:
            parent[rx] = ry

    starts = np.flatnonzero(np.r_[True, sorted_inv[1:] != sorted_inv[:-1]])
    ends = np.r_[starts[1:], len(sorted_inv)]
    for s, e in zip(starts, ends):
        if e - s > 1:
            f0 = int(sorted_face[s])
            for k in range(s + 1, e):
                union(f0, int(sorted_face[k]))

    roots = np.fromiter((find(i) for i in range(F)), dtype=np.int64, count=F)
    uniq_roots, root_inv, comp_sizes = np.unique(roots, return_inverse=True, return_counts=True)
    face_comp_sizes = comp_sizes[root_inv]

    keep_mask = (face_comp_sizes >= min_faces)
    if keep_mask.all():
        return v, t
    surv_t = t[keep_mask]
    if remove_unref:
        return _remove_unreferenced_vertices_numpy(v, surv_t)
    return v, surv_t


def _invert_face_orientation_numpy(tris):
    t = np.asarray(tris, dtype=np.int64)
    if len(t) == 0:
        return t
    return t[:, [1, 0, 2]]


def _re_orient_faces_coherently_numpy(tris):
    t = np.asarray(tris, dtype=np.int64)
    nf = len(t)
    if nf == 0:
        return t
    from collections import deque
    edge_to_faces = {}
    for fi, tri in enumerate(t):
        if tri[0] == tri[1] or tri[1] == tri[2] or tri[2] == tri[0]:
            raise ValueError("Mesh has some not 2-manifold faces, Orientability requires manifoldness")
        for i in range(3):
            e = (min(tri[i], tri[(i + 1) % 3]), max(tri[i], tri[(i + 1) % 3]))
            edge_to_faces.setdefault(e, []).append(fi)

    for flist in edge_to_faces.values():
        if len(flist) > 2:
            raise ValueError("Mesh has some not 2-manifold faces, Orientability requires manifoldness")

    out = t.copy()
    visited = np.zeros(nf, dtype=bool)
    for start_f in range(nf):
        if visited[start_f]:
            continue
        visited[start_f] = True
        queue = deque([start_f])
        while queue:
            curr = queue.popleft()
            curr_tri = out[curr]
            for k in range(3):
                a, b = curr_tri[k], curr_tri[(k + 1) % 3]
                e = (min(a, b), max(a, b))
                for nxt in edge_to_faces.get(e, []):
                    if visited[nxt]:
                        continue
                    nxt_tri = out[nxt]
                    pos_a = -1
                    pos_b = -1
                    for idx in range(3):
                        if nxt_tri[idx] == a:
                            pos_a = idx
                        if nxt_tri[idx] == b:
                            pos_b = idx
                    if pos_a != -1 and pos_b != -1 and (pos_a + 1) % 3 == pos_b:
                        nxt_tri[pos_a], nxt_tri[pos_b] = nxt_tri[pos_b], nxt_tri[pos_a]
                        out[nxt] = nxt_tri
                    visited[nxt] = True
                    queue.append(nxt)
    return out


def _repair_non_manifold_edges_numpy(verts, tris):
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int64)
    nf = len(t)
    nv = len(v)
    if nf == 0:
        return t

    # Calculate triangle areas
    p0 = v[t[:, 0]]
    p1 = v[t[:, 1]]
    p2 = v[t[:, 2]]
    cross = np.cross(p1 - p0, p2 - p0)
    areas = 0.5 * np.linalg.norm(cross, axis=1)

    # Edge to incident faces mapping
    edge_faces = {}
    face_edges = []
    for fi in range(nf):
        tri = t[fi]
        e0 = (min(tri[0], tri[1]), max(tri[0], tri[1]))
        e1 = (min(tri[1], tri[2]), max(tri[1], tri[2]))
        e2 = (min(tri[2], tri[0]), max(tri[2], tri[0]))
        face_edges.append((e0, e1, e2))
        edge_faces.setdefault(e0, []).append(fi)
        edge_faces.setdefault(e1, []).append(fi)
        edge_faces.setdefault(e2, []).append(fi)

    sorted_edges = sorted(edge_faces.keys())
    active_nm = [e for e in sorted_edges if len(edge_faces[e]) > 2]
    if not active_nm:
        return t.copy()

    removed = np.zeros(nf, dtype=bool)

    while active_nm:
        edge = active_nm.pop()
        flist = edge_faces[edge]
        flist = [f for f in flist if not removed[f]]
        edge_faces[edge] = flist
        if len(flist) <= 2:
            continue

        best_idx = 0
        min_area = areas[flist[0]]
        min_f = flist[0]
        for idx in range(1, len(flist)):
            f = flist[idx]
            a = areas[f]
            if a < min_area or (a == min_area and f < min_f):
                min_area = a
                min_f = f
                best_idx = idx

        victim = flist[best_idx]
        flist[best_idx] = flist[-1]
        flist.pop()
        removed[victim] = True

        if len(flist) > 2:
            active_nm.append(edge)

        for other_e in face_edges[victim]:
            if other_e != edge:
                other_list = [f for f in edge_faces[other_e] if not removed[f]]
                edge_faces[other_e] = other_list
                if len(other_list) > 2:
                    active_nm.append(other_e)

    return t[~removed].copy()


def _mesh_topology_numpy(verts, tris):
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int64)
    F = len(t)
    V = len(v)
    if F == 0:
        return {
            'boundary_edges': 0,
            'connected_components_number': 0,
            'edges_number': 0,
            'faces_number': 0,
            'genus': 0,
            'incident_faces_on_non_two_manifold_edges': 0,
            'incident_faces_on_non_two_manifold_vertices': 0,
            'is_mesh_two_manifold': True,
            'non_two_manifold_edges': 0,
            'non_two_manifold_vertices': 0,
            'number_holes': 0,
            'unreferenced_vertices': V,
            'vertices_number': V,
        }
    edges, counts, inv = _edge_table_numpy(t)
    E = len(edges)
    boundary_edges = int(np.sum(counts == 1))
    nm_edges = int(np.sum(counts > 2))

    face_of_h = np.tile(np.arange(F, dtype=np.int64), 3)
    nm_edge_faces = int(len(np.unique(face_of_h[counts[inv] > 2]))) if nm_edges > 0 else 0

    order = np.argsort(inv, kind='stable')
    sorted_inv = inv[order]
    sorted_face = face_of_h[order]
    parent = list(range(F))
    def find(x):
        root = x
        while parent[root] != root:
            root = parent[root]
        while parent[x] != root:
            nxt = parent[x]
            parent[x] = root
            x = nxt
        return root
    def union(x, y):
        rx, ry = find(x), find(y)
        if rx != ry:
            parent[rx] = ry
    starts = np.flatnonzero(np.r_[True, sorted_inv[1:] != sorted_inv[:-1]])
    ends = np.r_[starts[1:], len(sorted_inv)]
    boundary_directed = {}
    for s, e in zip(starts, ends):
        deg = e - s
        if deg > 1:
            f0 = int(sorted_face[s])
            for k in range(s + 1, e):
                union(f0, int(sorted_face[k]))
        elif deg == 1:
            orig_h = order[s]
            block = orig_h // F
            f_id = orig_h % F
            tri = t[f_id]
            u, w = tri[block], tri[(block + 1) % 3]
            boundary_directed[int(u)] = int(w)

    n_components = len(set(find(i) for i in range(F)))

    from collections import defaultdict
    v_faces = defaultdict(list)
    for f_idx, tri in enumerate(t):
        for vi in set(tri):
            if 0 <= vi < V:
                v_faces[vi].append(f_idx)

    nmv_count = 0
    nmv_incident_faces = set()
    for v_idx, inc_faces in v_faces.items():
        if len(inc_faces) <= 1:
            continue
        f_pos = {f: i for i, f in enumerate(inc_faces)}
        p_star = list(range(len(inc_faces)))
        def find_star(x):
            while p_star[x] != x:
                p_star[x] = p_star[p_star[x]]
                x = p_star[x]
            return x
        def union_star(x, y):
            rx, ry = find_star(x), find_star(y)
            if rx != ry:
                p_star[rx] = ry
        edge_map = defaultdict(list)
        for f in inc_faces:
            tri = t[f]
            for w in set(tri):
                if w != v_idx and 0 <= w < V:
                    edge_map[w].append(f_pos[f])
        for w, flist in edge_map.items():
            if len(flist) > 1:
                f0 = flist[0]
                for other in flist[1:]:
                    union_star(f0, other)
        if len(set(find_star(i) for i in range(len(inc_faces)))) > 1:
            nmv_count += 1
            nmv_incident_faces.update(inc_faces)

    is_two_manifold = (nm_edges == 0 and nmv_count == 0)
    unref_verts = V - len(v_faces)

    if is_two_manifold:
        visited = set()
        holes = 0
        for start in list(boundary_directed.keys()):
            if start in visited:
                continue
            curr = start
            while curr in boundary_directed and curr not in visited:
                visited.add(curr)
                curr = boundary_directed[curr]
                if curr == start:
                    holes += 1
                    break
        v_ref = V - unref_verts
        chi = v_ref - E + F
        genus = int(n_components - (holes + chi) / 2)
    else:
        genus = -1
        holes = -1

    return {
        'boundary_edges': boundary_edges,
        'connected_components_number': n_components,
        'edges_number': E,
        'faces_number': F,
        'genus': genus,
        'incident_faces_on_non_two_manifold_edges': nm_edge_faces,
        'incident_faces_on_non_two_manifold_vertices': len(nmv_incident_faces),
        'is_mesh_two_manifold': is_two_manifold,
        'non_two_manifold_edges': nm_edges,
        'non_two_manifold_vertices': nmv_count,
        'number_holes': holes,
        'unreferenced_vertices': unref_verts,
        'vertices_number': V,
    }


def _tri_tri_intersect_clean(v1, v2):
    """Möller clean-room triangle-triangle proper transversal intersection test."""
    e1 = v1[1] - v1[0]
    e2 = v1[2] - v1[0]
    n1 = np.cross(e1, e2)
    d1 = -np.dot(n1, v1[0])

    dv2 = [np.dot(n1, v2[k]) + d1 for k in range(3)]
    if (dv2[0] > 1e-12 and dv2[1] > 1e-12 and dv2[2] > 1e-12) or (dv2[0] < -1e-12 and dv2[1] < -1e-12 and dv2[2] < -1e-12):
        return False
    if any(abs(x) <= 1e-12 for x in dv2):
        return False

    f1 = v2[1] - v2[0]
    f2 = v2[2] - v2[0]
    n2 = np.cross(f1, f2)
    d2 = -np.dot(n2, v2[0])

    dv1 = [np.dot(n2, v1[k]) + d2 for k in range(3)]
    if (dv1[0] > 1e-12 and dv1[1] > 1e-12 and dv1[2] > 1e-12) or (dv1[0] < -1e-12 and dv1[1] < -1e-12 and dv1[2] < -1e-12):
        return False
    if any(abs(x) <= 1e-12 for x in dv1):
        return False

    D = np.cross(n1, n2)
    len_D2 = np.dot(D, D)
    if len_D2 < 1e-24:
        return False

    def get_interval(v, dv):
        pts = []
        for i in range(3):
            j = (i + 1) % 3
            if dv[i] * dv[j] < 0:
                t = dv[i] / (dv[i] - dv[j])
                pt = v[i] + t * (v[j] - v[i])
                pts.append(np.dot(pt, D))
        if len(pts) != 2:
            return None
        return min(pts), max(pts)

    i1 = get_interval(v1, dv1)
    i2 = get_interval(v2, dv2)
    if i1 is None or i2 is None:
        return False
    t0 = max(i1[0], i2[0])
    t1 = min(i1[1], i2[1])
    return (t1 - t0) > 1e-9


def _self_intersecting_faces_numpy(verts, tris, max_faces=5000, timeout_s=60.0):
    """Pure-NumPy fallback oracle for self-intersecting face detection.

    Bounded to max_faces (default 5,000) and timeout_s (default 60s) to guard
    against O(N^2) hangs. For larger or timed-out meshes, returns (all-false mask, -1).
    """
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    n_faces = len(t)
    if n_faces < 2:
        return np.zeros(n_faces, dtype=bool), 0
    if n_faces > max_faces:
        return np.zeros(n_faces, dtype=bool), -1
    t_start = time.perf_counter()
    mask = np.zeros(n_faces, dtype=bool)
    mins = np.min(v[t], axis=1)
    maxs = np.max(v[t], axis=1)
    for i in range(n_faces):
        if i % 100 == 0 and (time.perf_counter() - t_start) > timeout_s:
            return np.zeros(n_faces, dtype=bool), -1
        ti = t[i]
        bmin_i = mins[i]
        bmax_i = maxs[i]
        overlap = (bmax_i[0] >= mins[i+1:, 0]) & (bmin_i[0] <= maxs[i+1:, 0]) & \
                  (bmax_i[1] >= mins[i+1:, 1]) & (bmin_i[1] <= maxs[i+1:, 1]) & \
                  (bmax_i[2] >= mins[i+1:, 2]) & (bmin_i[2] <= maxs[i+1:, 2])
        cand_indices = np.where(overlap)[0] + (i + 1)
        for j in cand_indices:
            tj = t[j]
            if ti[0] in tj or ti[1] in tj or ti[2] in tj:
                continue
            if _tri_tri_intersect_clean(v[ti], v[tj]):
                mask[i] = True
                mask[j] = True
    return mask, int(mask.sum())


# --- public API ----------------------------------------------------------- #
def edge_table(tris):
    """Unique undirected edges, per-edge use counts and half-edge inverse.

    Returns ``(edges, counts, inverse)`` (int64): ``edges`` is ``(N, 2)``
    sorted lexicographically; ``counts[n]`` is how many half-edges use
    ``edges[n]``; ``inverse[h]`` is the edge id of the ``h``-th half-edge in the
    concatenated order ``[(v0,v1)..., (v1,v2)..., (v2,v0)...]``.
    """
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if _use_rust():
        try:
            return _geom.edge_table(t)
        except Exception:
            pass
    return _edge_table_numpy(t)


def weld_vertices(verts):
    """``np.unique(verts.astype(float32), axis=0, return_inverse=True)``.

    Returns ``(unique, inverse)`` with ``inverse`` int64 and length ``len(verts)``.
    """
    v = np.ascontiguousarray(verts, dtype=np.float32)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if _use_rust() and (v.size == 0 or bool(np.isfinite(v).all())):
        try:
            return _geom.weld_vertices(v)
        except Exception:
            pass
    return _weld_vertices_numpy(v)


def weld_reload_equivalent(verts, tris):
    """STL save/reload-equivalent weld (P-HONEST).  Bit-for-bit parity with the
    numpy oracle.  See ``sutura_engine.xray.weld_reload_equivalent``."""
    v = np.ascontiguousarray(verts, dtype=np.float32)
    t = np.ascontiguousarray(tris, dtype=np.int64)
    finite = v.size == 0 or bool(np.isfinite(v).all())
    if _use_rust() and finite:
        try:
            return _geom.weld_reload_equivalent(v, t)
        except Exception:
            pass
    return _weld_reload_equivalent_numpy(v, t)


def remove_duplicate_faces(tris):
    """Remove duplicate faces matching VCGLib meshing_remove_duplicate_faces.

    Preserves the last occurrence of duplicate triplets in original order.
    """
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if len(t) <= 1:
        return t
    if _has_rust('remove_duplicate_faces'):
        try:
            return _geom.remove_duplicate_faces(t)
        except Exception:
            pass
    return _remove_duplicate_faces_numpy(t)


def remove_null_faces(verts, tris):
    """Remove null / degenerate faces matching VCGLib meshing_remove_null_faces.

    Removes repeated vertices (a==b, b==c, c==a) and zero-area / collinear faces.
    """
    v = np.ascontiguousarray(verts, dtype=np.float32)
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if len(t) == 0:
        return t
    if _has_rust('remove_null_faces') and (v.size == 0 or bool(np.isfinite(v).all())):
        try:
            return _geom.remove_null_faces(v, t)
        except Exception:
            pass
    return _remove_null_faces_numpy(v, t)


def remove_duplicate_vertices(verts, tris):
    """Remove duplicate vertices matching VCGLib meshing_remove_duplicate_vertices.

    Welds coincident vertices keeping first-occurrence order, drops degenerate faces,
    and remaps surviving faces.
    """
    v = np.ascontiguousarray(verts, dtype=np.float32)
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if len(v) == 0:
        return v, t
    if _has_rust('remove_duplicate_vertices') and bool(np.isfinite(v).all()):
        try:
            return _geom.remove_duplicate_vertices(v, t)
        except Exception:
            pass
    return _remove_duplicate_vertices_numpy(v, t)


def remove_unreferenced_vertices(verts, tris):
    """Remove unreferenced vertices matching VCGLib meshing_remove_unreferenced_vertices.

    Compacts vertex array preserving original relative order of referenced vertices.
    """
    v = np.ascontiguousarray(verts, dtype=np.float32)
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if len(v) == 0:
        return v, t
    if len(t) == 0:
        return v[:0], t
    if _has_rust('remove_unreferenced_vertices'):
        try:
            return _geom.remove_unreferenced_vertices(v, t)
        except Exception:
            pass
    return _remove_unreferenced_vertices_numpy(v, t)


def remove_connected_components(verts, tris, min_faces=25, remove_unref=True):
    """Remove connected components with fewer than min_faces triangles.

    Matching VCGLib meshing_remove_connected_component_by_face_number.
    """
    v = np.ascontiguousarray(verts, dtype=np.float32)
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if len(t) == 0 or min_faces <= 1:
        return v, t
    if _has_rust('remove_connected_components'):
        try:
            return _geom.remove_connected_components(v, t, int(min_faces), bool(remove_unref))
        except Exception:
            pass
    return _remove_connected_components_numpy(v, t, min_faces=min_faces, remove_unref=remove_unref)


def invert_face_orientation(tris):
    """Invert face winding (matching VCGLib meshing_invert_face_orientation).

    Swaps vertices 0 and 1 of each triangle.
    """
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if len(t) == 0:
        return t
    if _has_rust('invert_face_orientation'):
        try:
            return _geom.invert_face_orientation(t)
        except Exception:
            pass
    return _invert_face_orientation_numpy(t)


def re_orient_faces_coherently(tris):
    """Re-orient faces coherently across manifold edges.

    Matching VCGLib meshing_re_orient_faces_coherently.
    """
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if len(t) <= 1:
        return t
    if _has_rust('re_orient_faces_coherently'):
        try:
            return _geom.re_orient_faces_coherently(t)
        except Exception:
            raise
    return _re_orient_faces_coherently_numpy(t)


def repair_non_manifold_edges(verts, tris):
    """Repair non-manifold edges by pruning incident faces (matching VCGLib meshing_repair_non_manifold_edges)."""
    v = np.ascontiguousarray(verts, dtype=np.float32)
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if len(t) == 0:
        return t
    if _has_rust('repair_non_manifold_edges'):
        try:
            return _geom.repair_non_manifold_edges(v, t)
        except Exception:
            pass
    return _repair_non_manifold_edges_numpy(v, t)


def mesh_topology(verts, tris):
    """Complete topological measures of a triangle mesh (matching VCGLib get_topological_measures)."""
    v = np.ascontiguousarray(verts, dtype=np.float32)
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if _has_rust('mesh_topology'):
        try:
            return _geom.mesh_topology(v, t)
        except Exception:
            pass
    return _mesh_topology_numpy(v, t)


def native_si_enabled():
    """Return True if native self-intersection detection is enabled.

    Controlled by environment variable ``SUTURA_NATIVE_SI`` (default OFF: 0/false).
    When enabled, self-intersection queries use exact geometric predicates via
    sutura_geom rather than PyMeshLab's VCGLib filter.
    """
    env = os.environ.get('SUTURA_NATIVE_SI', '').strip().lower()
    return env in ('1', 'true', 'yes', 'on')


def native_orient_enabled():
    """Return True if native coherent face reorientation is enabled.

    Controlled by environment variable ``SUTURA_NATIVE_ORIENT`` (default OFF: 0/false).
    When enabled, coherent face winding propagation uses native dual-graph BFS via
    sutura_geom rather than PyMeshLab's VCGLib filter.
    """
    env = os.environ.get('SUTURA_NATIVE_ORIENT', '').strip().lower()
    return env in ('1', 'true', 'yes', 'on')


def native_nme_enabled():
    """Return True if native non-manifold edge repair is enabled.

    Controlled by environment variable ``SUTURA_NATIVE_NME`` (default OFF: 0/false).
    When enabled, non-manifold edge repair prunes incident faces via sutura_geom
    rather than PyMeshLab's VCGLib filter.
    """
    env = os.environ.get('SUTURA_NATIVE_NME', '').strip().lower()
    return env in ('1', 'true', 'yes', 'on')


# Session BVH and SI cache to avoid redundant builds when geometry is unchanged
_BVH_CACHE = {
    'verts_ref': None,
    'tris_ref': None,
    'verts_ptr': None,
    'tris_ptr': None,
    'verts_shape': None,
    'tris_shape': None,
    'v_sample': None,
    't_sample': None,
    'bvh': None,
    'mask': None,
    'count': None,
}


def clear_bvh_cache():
    """Clear cached BVH and SI results."""
    _BVH_CACHE['verts_ref'] = None
    _BVH_CACHE['tris_ref'] = None
    _BVH_CACHE['verts_ptr'] = None
    _BVH_CACHE['tris_ptr'] = None
    _BVH_CACHE['verts_shape'] = None
    _BVH_CACHE['tris_shape'] = None
    _BVH_CACHE['v_sample'] = None
    _BVH_CACHE['t_sample'] = None
    _BVH_CACHE['bvh'] = None
    _BVH_CACHE['mask'] = None
    _BVH_CACHE['count'] = None


def self_intersecting_faces(verts, tris, bvh=None):
    """Detect self-intersecting faces using exact geometric predicates.

    Returns (mask, count) where mask is a boolean numpy array of length len(tris)
    and count is the number of flagged faces (or -1 if skipped due to size limits).
    """
    v = np.ascontiguousarray(verts, dtype=np.float64)
    t = np.ascontiguousarray(tris, dtype=np.int32)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if len(t) < 2:
        return np.zeros(len(t), dtype=bool), 0

    if bvh is None:
        v_ptr = v.__array_interface__['data'][0]
        t_ptr = t.__array_interface__['data'][0]
        is_hit = False
        if verts is _BVH_CACHE['verts_ref'] and tris is _BVH_CACHE['tris_ref']:
            is_hit = True
        elif (
            v_ptr == _BVH_CACHE['verts_ptr']
            and t_ptr == _BVH_CACHE['tris_ptr']
            and v.shape == _BVH_CACHE['verts_shape']
            and t.shape == _BVH_CACHE['tris_shape']
            and _BVH_CACHE['mask'] is not None
        ):
            if (
                _BVH_CACHE['v_sample'] is not None
                and _BVH_CACHE['t_sample'] is not None
                and np.array_equal(v[:min(4, len(v))], _BVH_CACHE['v_sample'])
                and np.array_equal(t[:min(4, len(t))], _BVH_CACHE['t_sample'])
            ):
                is_hit = True

        if is_hit and _BVH_CACHE['mask'] is not None and len(_BVH_CACHE['mask']) == len(t):
            return _BVH_CACHE['mask'].copy(), _BVH_CACHE['count']

    if _has_rust('self_intersecting_faces'):
        try:
            if bvh is not None:
                if hasattr(bvh, 'self_intersecting_faces'):
                    return bvh.self_intersecting_faces()
                return _geom.self_intersecting_faces(v, t, bvh=bvh)

            new_bvh = None
            if hasattr(_geom, 'PyMeshBvh'):
                try:
                    new_bvh = _geom.PyMeshBvh(v, t)
                except Exception:
                    new_bvh = None

            if new_bvh is not None and hasattr(new_bvh, 'self_intersecting_faces'):
                mask, count = new_bvh.self_intersecting_faces()
            else:
                mask, count = _geom.self_intersecting_faces(v, t)

            _BVH_CACHE['verts_ref'] = verts
            _BVH_CACHE['tris_ref'] = tris
            _BVH_CACHE['verts_ptr'] = v.__array_interface__['data'][0]
            _BVH_CACHE['tris_ptr'] = t.__array_interface__['data'][0]
            _BVH_CACHE['verts_shape'] = v.shape
            _BVH_CACHE['tris_shape'] = t.shape
            _BVH_CACHE['v_sample'] = v[:min(4, len(v))].copy()
            _BVH_CACHE['t_sample'] = t[:min(4, len(t))].copy()
            _BVH_CACHE['bvh'] = new_bvh
            _BVH_CACHE['mask'] = mask
            _BVH_CACHE['count'] = count
            return mask, count
        except Exception:
            pass
    return _self_intersecting_faces_numpy(v, t)


def select_self_intersections_ms(ms, ml=None, mask=None, count=None):
    """Select self-intersecting faces in MeshSet in-place.

    When SUTURA_NATIVE_SI is enabled, computes the mask via native exact
    predicates and applies it to the MeshSet. Otherwise delegates to
    PyMeshLab's 'compute_selection_by_self_intersections_per_face'.

    Returns (mask, count).
    """
    if not native_si_enabled():
        ms.apply_filter('compute_selection_by_self_intersections_per_face')
        res_mask = np.asarray(ms.current_mesh().face_selection_array(), dtype=bool)
        return res_mask, int(res_mask.sum())

    m = ms.current_mesh()
    if mask is None or count is None:
        v = m.vertex_matrix()
        t = m.face_matrix()
        mask, count = self_intersecting_faces(v, t)

    try:
        ms.apply_filter('set_selection_none')
        if count > 0:
            m.add_face_custom_scalar_attribute(mask.astype(np.float64), '_native_si')
            ms.apply_filter('compute_selection_by_condition_per_face', condselect='_native_si > 0.5')
    except Exception:
        pass
    return mask, int(count)


def connected_components(graph=None, *, directed=False, connection='weak',
                         return_labels=True, n_nodes=None, edges=None):
    """Connected components of an undirected graph.

    Drop-in replacement for ``scipy.sparse.csgraph.connected_components`` (with
    ``directed=False``). Supports:
      * A sparse matrix (e.g. ``scipy.sparse.coo_matrix`` or ``csr_matrix``)
      * A tuple ``(n_nodes, edges)`` where ``edges`` is an ``(E, 2)`` array
        or a tuple of 1D coordinate arrays ``(row, col)``
      * Explicit ``n_nodes`` and ``edges`` keyword arguments

    Uses disjoint-set union-find with path compression and canonical min-root
    ordering, producing components numbered ``0 .. n_components-1`` matching
    SciPy's convention (component 0 contains node 0, component 1 contains the
    lowest-indexed node not in component 0, etc.).

    Returns ``(n_components, labels)`` if ``return_labels`` is True, else
    ``n_components``.
    """
    if graph is not None:
        if isinstance(graph, tuple) and len(graph) == 2:
            n_nodes = int(graph[0])
            e = graph[1]
            if isinstance(e, tuple) and len(e) == 2:
                edges = np.column_stack((e[0], e[1]))
            else:
                edges = e
        elif hasattr(graph, 'tocoo'):
            coo = graph.tocoo()
            n_nodes = int(coo.shape[0])
            edges = np.column_stack((coo.row, coo.col))
        elif hasattr(graph, 'row') and hasattr(graph, 'col'):
            n_nodes = int(graph.shape[0])
            edges = np.column_stack((graph.row, graph.col))
        elif isinstance(graph, np.ndarray) and graph.ndim == 2:
            if graph.shape[1] == 2 and n_nodes is not None:
                edges = graph
            else:
                r, c = np.nonzero(graph)
                n_nodes = int(graph.shape[0])
                edges = np.column_stack((r, c))

    if n_nodes is None:
        raise ValueError("Could not determine n_nodes for connected_components")
    if edges is None:
        edges = np.zeros((0, 2), dtype=np.int64)

    if n_nodes == 0:
        return (0, np.zeros(0, dtype=np.int64)) if return_labels else 0

    edges = np.asarray(edges, dtype=np.int64)
    if len(edges) == 0:
        return (n_nodes, np.arange(n_nodes, dtype=np.int64)) if return_labels else n_nodes

    u_col = edges[:, 0]
    v_col = edges[:, 1]
    u_ord = np.minimum(u_col, v_col)
    v_ord = np.maximum(u_col, v_col)
    m = (u_ord != v_ord) & (u_ord >= 0) & (v_ord < n_nodes)
    us = u_ord[m].tolist()
    vs = v_ord[m].tolist()

    parent = list(range(n_nodes))

    def find(x):
        root = x
        while parent[root] != root:
            root = parent[root]
        while parent[x] != root:
            nxt = parent[x]
            parent[x] = root
            x = nxt
        return root

    for iu, iv in zip(us, vs):
        ru, rv = find(iu), find(iv)
        if ru < rv:
            parent[rv] = ru
        elif rv < ru:
            parent[ru] = rv

    roots = np.fromiter((find(i) for i in range(n_nodes)), dtype=np.int64, count=n_nodes)
    uniq_roots, labels = np.unique(roots, return_inverse=True)
    n_comp = int(len(uniq_roots))

    return (n_comp, labels) if return_labels else n_comp


__all__ = [
    'edge_table',
    'weld_vertices',
    'weld_reload_equivalent',
    'remove_duplicate_faces',
    'remove_null_faces',
    'remove_duplicate_vertices',
    'remove_unreferenced_vertices',
    'remove_connected_components',
    'invert_face_orientation',
    're_orient_faces_coherently',
    'repair_non_manifold_edges',
    'mesh_topology',
    'connected_components',
    'native_si_enabled',
    'native_orient_enabled',
    'native_nme_enabled',
    'self_intersecting_faces',
    'select_self_intersections_ms',
    'clear_bvh_cache',
    'engine',
    'set_engine',
]
