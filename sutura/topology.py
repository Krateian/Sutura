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
try:
    import sutura_geom as _geom
except ImportError:
    _geom = None

_MODE = None  # None = auto, otherwise 'rust' or 'python'


def set_engine(mode):
    """Force the topology engine: ``'auto'``, ``'rust'`` or ``'python'``.

    Intended for tests and debugging; the default is ``'auto'`` (Rust when the
    extension is importable, raising if unavailable).  Raises ``ValueError`` on an
    unknown mode."""
    global _MODE
    if mode not in ('auto', 'rust', 'python'):
        raise ValueError("engine must be 'auto', 'rust' or 'python'")
    _MODE = None if mode == 'auto' else mode


def engine():
    """Return the active engine name (``'rust'`` or ``'python'``)."""
    if _MODE == 'python':
        return 'python'
    env = os.environ.get('SUTURA_TOPOLOGY', '').strip().lower()
    if env == 'python':
        return 'python'
    if not _rust_kernel_available():
        raise RuntimeError(
            "sutura_geom topology kernel required but unavailable in this environment"
        )
    return 'rust'


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
    're_orient_faces_coherently',
    'repair_non_manifold_edges',
    'repair_non_manifold_vertices',
    'remove_selected_faces',
    'compute_normal_per_face',
    'compute_normal_per_vertex',
    'mesh_topology',
    'mesh_geometric_measures',
    'self_intersecting_faces',
    'close_holes',
    'cluster_decimate',
    'decimate_qem',
    'resample_uniform',
)


def _rust_kernel_available():
    """True when the importable extension carries all topology kernel functions."""
    return _geom is not None and all(hasattr(_geom, f) for f in _RUST_FNS)


def _use_rust():
    return engine() == 'rust'


def _has_rust(name):
    return engine() == 'rust' and hasattr(_geom, name)


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

    # Calculate triangle double areas matching VCGLib DoubleArea
    p0 = v[t[:, 0]]
    p1 = v[t[:, 1]]
    p2 = v[t[:, 2]]
    cross = np.cross(p1 - p0, p2 - p0)
    double_areas = np.linalg.norm(cross, axis=1)

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

    to_del = []
    for fi in range(nf):
        edges = face_edges[fi]
        if any(len(edge_faces[e]) > 2 for e in edges):
            to_del.append(fi)

    if not to_del:
        return t.copy()

    to_del.sort(key=lambda fi: (double_areas[fi], fi))

    removed = np.zeros(nf, dtype=bool)
    for fi in to_del:
        if removed[fi]:
            continue
        edges = face_edges[fi]
        still_has_nm = False
        for e in edges:
            active_count = sum(1 for f in edge_faces[e] if not removed[f])
            if active_count > 2:
                still_has_nm = True
                break
        if still_has_nm:
            removed[fi] = True

    return t[~removed].copy()


def _repair_non_manifold_vertices_numpy(verts, tris):
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int64)
    nf = len(t)
    nv = len(v)
    if nf == 0 or nv == 0:
        return v.copy(), t.copy()

    v_out = [list(x) for x in v]
    t_out = [list(tri) for tri in t]

    edge_faces = {}
    vert_faces = {}
    for fi in range(nf):
        tri = t[fi]
        for k in range(3):
            e = (min(tri[k], tri[(k + 1) % 3]), max(tri[k], tri[(k + 1) % 3]))
            edge_faces.setdefault(e, []).append((fi, k))
            vert_faces.setdefault(tri[k], []).append(fi)

    nm_edges = {e for e, flist in edge_faces.items() if len(flist) > 2}
    nm_edge_verts = set()
    for e in nm_edges:
        nm_edge_verts.add(e[0])
        nm_edge_verts.add(e[1])

    ff_adj = {}
    for e, flist in edge_faces.items():
        if len(flist) == 2:
            (f0, k0), (f1, k1) = flist
            ff_adj[(f0, k0)] = (f1, k1)
            ff_adj[(f1, k1)] = (f0, k0)
        elif len(flist) == 1:
            (f0, k0) = flist[0]
            ff_adj[(f0, k0)] = (f0, k0)

    nm_verts = set()
    for vi, flist in vert_faces.items():
        if vi in nm_edge_verts:
            continue
        start_f = flist[0]
        visited_f = {start_f}
        q = [start_f]
        while q:
            curr_f = q.pop()
            c = t_out[curr_f].index(vi)
            for edge_k in [(c + 2) % 3, c]:
                if (curr_f, edge_k) in ff_adj:
                    adj_f, adj_k = ff_adj[(curr_f, edge_k)]
                    if adj_f not in visited_f and vi in t_out[adj_f]:
                        visited_f.add(adj_f)
                        q.append(adj_f)
        if len(visited_f) < len(flist):
            nm_verts.add(vi)

    if not nm_verts:
        return v.copy(), t.copy()

    visited_v = set()
    to_split = []
    for fi in range(nf):
        for corner in range(3):
            vi = t_out[fi][corner]
            if vi in nm_verts and vi not in visited_v:
                visited_v.add(vi)
                fan_faces = {fi}
                q = [fi]
                while q:
                    curr_f = q.pop()
                    c = t_out[curr_f].index(vi)
                    for edge_k in [(c + 2) % 3, c]:
                        if (curr_f, edge_k) in ff_adj:
                            adj_f, adj_k = ff_adj[(curr_f, edge_k)]
                            if adj_f not in fan_faces and vi in t_out[adj_f]:
                                fan_faces.add(adj_f)
                                q.append(adj_f)
                face_ints = []
                for f_idx in sorted(fan_faces):
                    face_ints.append((f_idx, t_out[f_idx].index(vi)))
                to_split.append((vi, face_ints))

    for orig_vi, face_ints in to_split:
        new_v_idx = len(v_out)
        v_out.append(v_out[orig_vi])
        for f_idx, corner in face_ints:
            t_out[f_idx][corner] = new_v_idx

    return np.array(v_out, dtype=np.float32), np.array(t_out, dtype=np.int64)


def _remove_selected_faces_numpy(tris, selected):
    t = np.asarray(tris, dtype=np.int64)
    sel = np.asarray(selected, dtype=bool)
    return t[~sel].copy()


def _compute_normal_per_face_numpy(verts, tris):
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    if len(t) == 0:
        return np.zeros((0, 3), dtype=np.float32)
    p0 = v[t[:, 0]]
    p1 = v[t[:, 1]]
    p2 = v[t[:, 2]]
    return np.cross(p1 - p0, p2 - p0).astype(np.float32)


def _compute_normal_per_vertex_numpy(verts, tris):
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    nv = len(v)
    if nv == 0:
        return np.zeros((0, 3), dtype=np.float32)
    if len(t) == 0:
        return np.zeros((nv, 3), dtype=np.float32)
    p0 = v[t[:, 0]]
    p1 = v[t[:, 1]]
    p2 = v[t[:, 2]]
    cross = np.cross(p1 - p0, p2 - p0)
    lens = np.linalg.norm(cross, axis=1, keepdims=True)
    lens[lens == 0] = 1.0
    norm_fn = cross / lens

    vn = np.zeros((nv, 3), dtype=np.float64)
    np.add.at(vn, t[:, 0], norm_fn)
    np.add.at(vn, t[:, 1], norm_fn)
    np.add.at(vn, t[:, 2], norm_fn)

    v_lens = np.linalg.norm(vn, axis=1, keepdims=True)
    v_lens[v_lens == 0] = 1.0
    return (vn / v_lens).astype(np.float32)


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
    nm_edge_verts = set(edges[counts > 2].ravel().tolist()) if nm_edges > 0 else set()

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
        if v_idx in nm_edge_verts:
            continue
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


def _mesh_geometric_measures_numpy(verts, tris):
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    nf = len(t)
    if nf == 0:
        return {'surface_area': 0.0}

    v0 = v[t[:, 0]]
    v1 = v[t[:, 1]]
    v2 = v[t[:, 2]]

    cross_e = np.cross(v1 - v0, v2 - v0)
    area = float(0.5 * np.sum(np.linalg.norm(cross_e, axis=1)))

    e0 = np.sort(t[:, [0, 1]], axis=1)
    e1 = np.sort(t[:, [1, 2]], axis=1)
    e2 = np.sort(t[:, [2, 0]], axis=1)
    all_e = np.vstack([e0, e1, e2])

    _, counts = np.unique(all_e, axis=0, return_counts=True)
    is_closed_manifold = bool(np.all(counts == 2))

    res = {'surface_area': area}
    if is_closed_manifold:
        vol = float(np.sum(v0 * np.cross(v1, v2)) / 6.0)
        res['mesh_volume'] = vol
    return res


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
    if engine() == 'python':
        return _edge_table_numpy(t)
    return _geom.edge_table(t)


def weld_vertices(verts):
    """``np.unique(verts.astype(float32), axis=0, return_inverse=True)``.

    Returns ``(unique, inverse)`` with ``inverse`` int64 and length ``len(verts)``.
    """
    v = np.ascontiguousarray(verts, dtype=np.float32)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if engine() == 'python' or not (v.size == 0 or bool(np.isfinite(v).all())):
        return _weld_vertices_numpy(v)
    return _geom.weld_vertices(v)


def weld_reload_equivalent(verts, tris):
    """STL save/reload-equivalent weld (P-HONEST).  Bit-for-bit parity with the
    numpy oracle.  See ``sutura_engine.pipeline.p_weld.weld_reload_equivalent``."""
    v = np.ascontiguousarray(verts, dtype=np.float32)
    t = np.ascontiguousarray(tris, dtype=np.int64)
    finite = v.size == 0 or bool(np.isfinite(v).all())
    if engine() == 'python' or not finite:
        return _weld_reload_equivalent_numpy(v, t)
    return _geom.weld_reload_equivalent(v, t)


def remove_duplicate_faces(tris):
    """Remove duplicate faces matching VCGLib meshing_remove_duplicate_faces.

    Preserves the last occurrence of duplicate triplets in original order.
    """
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if len(t) <= 1:
        return t
    if engine() == 'python':
        return _remove_duplicate_faces_numpy(t)
    return _geom.remove_duplicate_faces(t)


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
    if engine() == 'python' or not (v.size == 0 or bool(np.isfinite(v).all())):
        return _remove_null_faces_numpy(v, t)
    return _geom.remove_null_faces(v, t)


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
    if engine() == 'python' or not bool(np.isfinite(v).all()):
        return _remove_duplicate_vertices_numpy(v, t)
    return _geom.remove_duplicate_vertices(v, t)


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
    if engine() == 'python':
        return _remove_unreferenced_vertices_numpy(v, t)
    return _geom.remove_unreferenced_vertices(v, t)


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
    if engine() == 'python':
        return _remove_connected_components_numpy(v, t, min_faces=min_faces, remove_unref=remove_unref)
    return _geom.remove_connected_components(v, t, int(min_faces), bool(remove_unref))


def invert_face_orientation(tris):
    """Invert face winding (matching VCGLib meshing_invert_face_orientation).

    Swaps vertices 0 and 1 of each triangle.
    """
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if len(t) == 0:
        return t
    if engine() == 'python':
        return _invert_face_orientation_numpy(t)
    return _geom.invert_face_orientation(t)


def re_orient_faces_coherently(tris):
    """Re-orient faces coherently across manifold edges.

    Matching VCGLib meshing_re_orient_faces_coherently.
    """
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if len(t) <= 1:
        return t
    if engine() == 'python':
        return _re_orient_faces_coherently_numpy(t)
    return _geom.re_orient_faces_coherently(t)


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
    if engine() == 'python':
        return _repair_non_manifold_edges_numpy(v, t)
    return _geom.repair_non_manifold_edges(v, t)


def repair_non_manifold_vertices(verts, tris=None):
    """Repair non-manifold pinch vertices by splitting them (matching VCGLib meshing_repair_non_manifold_vertices).

    Accepts either (verts, tris) as arrays or a PyMeshLab MeshSet / Mesh object.
    Returns (verts, tris) as arrays, or modifies MeshSet in-place if MeshSet passed.
    """
    is_ms = False
    if tris is None:
        if hasattr(verts, 'current_mesh'):
            ms = verts
            m = ms.current_mesh()
            v = m.vertex_matrix()
            t = m.face_matrix()
            is_ms = True
        elif hasattr(verts, 'vertex_matrix'):
            v = verts.vertex_matrix()
            t = verts.face_matrix()
        else:
            raise ValueError("Expected (verts, tris) or a mesh/MeshSet object")
    else:
        v = verts
        t = tris
    v = np.ascontiguousarray(v, dtype=np.float32)
    t = np.ascontiguousarray(t, dtype=np.int64)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if len(v) == 0 or len(t) == 0:
        out_v, out_t = v, t
    elif engine() == 'python':
        out_v, out_t = _repair_non_manifold_vertices_numpy(v, t)
    else:
        out_v, out_t = _geom.repair_non_manifold_vertices(v, t)

    if is_ms and len(out_v) > len(v):
        import pymeshlab as ml
        ms.clear()
        ms.add_mesh(ml.Mesh(vertex_matrix=out_v, face_matrix=out_t.astype(np.int32)))
        return ms
    return out_v, out_t


def remove_selected_faces(tris, selected):
    """Remove selected faces matching VCGLib meshing_remove_selected_faces."""
    t = np.ascontiguousarray(tris, dtype=np.int64)
    sel = np.ascontiguousarray(selected, dtype=bool)
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if len(t) == 0:
        return t
    if engine() == 'python':
        return _remove_selected_faces_numpy(t, sel)
    return _geom.remove_selected_faces(t, sel)


def close_holes(verts, tris, maxholesize=1000, self_intersection=True,
                newfaceselected=False):
    """Close boundary loops matching VCGLib ``meshing_close_holes``.

    Native only (Faz 4): the Rust ``sutura_geom.close_holes`` ear-clipping
    closer is required -- there is deliberately NO numpy oracle, so a missing
    extension or symbol raises ``RuntimeError`` rather than silently degrading.

    ``maxholesize`` is the largest boundary loop (in edges) to close;
    ``self_intersection`` rejects ears that would cross the surface and
    ``newfaceselected`` is accepted for PyMeshLab signature parity (it has no
    effect on the returned arrays).  Original vertices are preserved
    bit-for-bit and the new triangles are appended.

    Returns ``(verts, tris, closed_holes, new_faces)``.
    """
    v = np.ascontiguousarray(verts, dtype=np.float64)
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if len(t) == 0:
        return v, t, 0, 0
    if not _has_rust('close_holes'):
        raise RuntimeError(
            "sutura_geom.close_holes required but unavailable in this "
            "environment (no numpy oracle for the hole closer)"
        )
    out_v, out_t, closed, new_faces = _geom.close_holes(
        v, t, int(maxholesize), bool(self_intersection), bool(newfaceselected))
    return (np.asarray(out_v), np.asarray(out_t).astype(np.int64),
            int(closed), int(new_faces))


def cluster_decimate(verts, tris, threshold):
    """Cluster-decimate ``verts``/``tris`` at the absolute ``threshold``.

    Native only (Faz 4 W3a): the Rust ``sutura_geom.cluster_decimate``
    vertex-clustering pass is required -- there is deliberately NO numpy
    oracle, so a missing extension or symbol raises ``RuntimeError`` rather
    than silently degrading.

    ``threshold`` is an absolute cell size in model units (the call site
    converts VCG's percentage threshold via ``pct / 100 * bbox_diagonal``).
    Each occupied cell contributes one representative at the mean of its
    member positions; faces whose three vertices share a cell are dropped;
    unreferenced representatives are removed.

    Returns ``(verts, tris, report)`` with ``report`` carrying
    ``threshold``/``faces_in``/``faces_out``.
    """
    v = np.ascontiguousarray(verts, dtype=np.float64)
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    thr = float(threshold)
    if not np.isfinite(thr) or thr <= 0.0:
        raise ValueError("threshold must be a positive finite number")
    if len(t) == 0:
        return v, t, {'threshold': thr, 'faces_in': 0, 'faces_out': 0}
    if not _has_rust('cluster_decimate'):
        raise RuntimeError(
            "sutura_geom.cluster_decimate required but unavailable in this "
            "environment (no numpy oracle for cluster decimation)"
        )
    out_v, out_t, faces_in, faces_out = _geom.cluster_decimate(v, t, thr)
    return (np.asarray(out_v), np.asarray(out_t).astype(np.int64),
            {'threshold': thr, 'faces_in': int(faces_in),
             'faces_out': int(faces_out)})


def decimate_qem(verts, tris, target_faces, target_error=None,
                 lock_border=False, preserve_normal=False):
    """Quadric edge-collapse decimation to ``target_faces``.

    Native only (Faz 4 W3b): the Rust ``sutura_geom.decimate_qem`` (a wrapper
    over the MIT ``meshopt`` / meshoptimizer C++ core) is required -- there is
    deliberately NO numpy oracle, so a missing extension or symbol raises
    ``RuntimeError`` rather than silently degrading.

    The input is welded on the f32 bit pattern of the position first (meshopt
    needs a shared index buffer; split STL-style corners otherwise read as
    topological borders), degenerate faces created by the weld are dropped,
    and surviving vertices keep their exact f64 input positions.  ``target_faces``
    is approached as closely as ``target_error`` (default ``1.0`` = the whole
    mesh extent) allows, so the result may carry more faces than requested when
    the error bound is tight.

    ``lock_border`` maps to meshopt's ``LockBorder`` (the PyMeshLab
    ``preserveboundary`` flag); ``preserve_normal`` passes per-vertex normals
    as collapse attributes.  Normal/topology preservation has no meshopt
    equivalent, so callers keep their own validity guards.

    Returns ``(verts, tris, report)`` with ``report`` carrying ``faces_in``/
    ``faces_out``/``weld_merged``/``welded_vertices``/``degenerate_removed``/
    ``target_error_used``/``result_error``/``lock_border``/``attributes_used``/
    ``no_op``/``engine``.
    """
    v = np.ascontiguousarray(verts, dtype=np.float64)
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    target = int(target_faces)
    if target < 0:
        raise ValueError("target_faces must be >= 0")
    if not _has_rust('decimate_qem'):
        raise RuntimeError(
            "sutura_geom.decimate_qem required but unavailable in this "
            "environment (no numpy oracle for quadric decimation)"
        )
    attributes = None
    if preserve_normal and len(v) > 0:
        attributes = compute_normal_per_vertex(v, t)
    err = None if target_error is None else float(target_error)
    out_v, out_t, report = _geom.decimate_qem(
        v, t, target, err, bool(lock_border), attributes)
    return (np.asarray(out_v), np.asarray(out_t).astype(np.int64),
            dict(report))


def resample_uniform(verts, tris, voxel, offset=0.0, absdist=False,
                     raystab_full=False, surface=None, vertex_mode=None):
    """Resample ``verts``/``tris`` on a uniform voxel grid.

    Native only (Faz 4 W3c): the Rust ``sutura_geom.resample_uniform`` (a
    generalized-winding distance field on a uniform grid, extracted with dual
    contouring) is required -- there is deliberately NO numpy oracle, so a
    missing extension or symbol raises ``RuntimeError`` rather than silently
    degrading.  This replaces the two ``generate_resampled_uniform_mesh`` call
    sites in the proxy tier.

    ``voxel`` is an absolute cell size in model units; ``offset`` shifts the
    extracted iso-surface and ``absdist=True`` resamples the unsigned
    nearest-surface distance instead of the signed one.  The output is a
    closed, two-manifold regular surface; the caller still validates it.

    ``raystab_full=True`` (signed field only, ignored when ``absdist``) lets
    the ray-stabbing vote decide the sign in every grid cell rather than only
    the generalized-winding-ambiguous band.  A folded/partial surface can
    leave the winding below 0.5 over a region the solid encloses (an open
    flap); the winding-only field then reads it as outside and dual contouring
    truncates the surface.  The default ``False`` is byte-identical to the
    winding-only field.

    Returns ``(verts, tris, report)`` with ``report`` carrying ``voxel``/
    ``offset``/``absdist``/``mode``/``dims``/``faces``/``coarsened``/
    ``manifold``/``raystab_full``.
    """
    v = np.ascontiguousarray(verts, dtype=np.float64)
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    cell = float(voxel)
    if not np.isfinite(cell) or cell <= 0.0:
        raise ValueError("voxel must be a positive finite number")
    off = float(offset)
    if not np.isfinite(off):
        raise ValueError("offset must be finite")
    use_raystab = bool(raystab_full) and not absdist
    if len(v) < 3 or len(t) == 0:
        return v, t, {'voxel': cell, 'offset': off, 'absdist': bool(absdist),
                      'mode': 'unsigned' if absdist else 'signed',
                      'dims': (0, 0, 0), 'faces': 0, 'coarsened': False,
                      'manifold': True, 'raystab_full': use_raystab,
                      'surface': surface or 'dual',
                      'vertex_mode': vertex_mode or 'qef'}
    if not _has_rust('resample_uniform'):
        raise RuntimeError(
            "sutura_geom.resample_uniform required but unavailable in this "
            "environment (no numpy oracle for voxel resampling)"
        )
    out_v, out_t, info = _geom.resample_uniform(
        v, t, cell, off, bool(absdist), use_raystab, surface, vertex_mode)
    report = dict(info)
    report['dims'] = tuple(int(d) for d in report.get('dims', (0, 0, 0)))
    return (np.asarray(out_v), np.asarray(out_t).astype(np.int64), report)


def close_holes_ms(ms, ml=None, maxholesize=1000, self_intersection=True,
                   newfaceselected=False, refinehole=False,
                   refineholeedgelen=None):
    """Apply ``meshing_close_holes`` to a MeshSet in place.

    Uses the native :func:`close_holes`; the Liepa-style ``refinehole=True``
    refinement is not ported yet (Faz 4D), so such calls are routed to
    PyMeshLab's filter unchanged.  Returns ``(closed_holes, new_faces)`` for
    the native path and ``None`` when the PyMeshLab fallback is used.
    """
    if refinehole:
        if ml is None:
            import pymeshlab as ml  # noqa: F401
        kw = {'maxholesize': int(maxholesize),
              'selfintersection': bool(self_intersection),
              'newfaceselected': bool(newfaceselected),
              'refinehole': True}
        if refineholeedgelen is not None:
            kw['refineholeedgelen'] = refineholeedgelen
        ms.apply_filter('meshing_close_holes', **kw)
        return None
    m = ms.current_mesh()
    v = m.vertex_matrix()
    t = m.face_matrix()
    v_new, t_new, closed, new_faces = close_holes(
        v, t, maxholesize=maxholesize, self_intersection=self_intersection,
        newfaceselected=newfaceselected)
    if len(t_new) != len(t):
        if ml is None:
            import pymeshlab as ml
        ms.clear()
        ms.add_mesh(ml.Mesh(vertex_matrix=v_new,
                            face_matrix=t_new.astype(np.int32)))
    return closed, new_faces


def compute_normal_per_face(verts, tris=None):
    """Compute unnormalized per-face normals matching VCGLib compute_normal_per_face ((p1-p0) x (p2-p0))."""
    if tris is None:
        if hasattr(verts, 'current_mesh'):
            m = verts.current_mesh()
            v = m.vertex_matrix()
            t = m.face_matrix()
        elif hasattr(verts, 'vertex_matrix'):
            v = verts.vertex_matrix()
            t = verts.face_matrix()
        else:
            raise ValueError("Expected (verts, tris) or a mesh/MeshSet object")
    else:
        v = verts
        t = tris
    v = np.ascontiguousarray(v, dtype=np.float64)
    t = np.ascontiguousarray(t, dtype=np.int64)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if len(t) == 0:
        return np.zeros((0, 3), dtype=np.float32)
    if engine() == 'python':
        return _compute_normal_per_face_numpy(v, t)
    return _geom.compute_normal_per_face(v, t)


def compute_normal_per_vertex(verts, tris=None):
    """Compute unit per-vertex normals matching VCGLib compute_normal_per_vertex (Simple Average)."""
    if tris is None:
        if hasattr(verts, 'current_mesh'):
            m = verts.current_mesh()
            v = m.vertex_matrix()
            t = m.face_matrix()
        elif hasattr(verts, 'vertex_matrix'):
            v = verts.vertex_matrix()
            t = verts.face_matrix()
        else:
            raise ValueError("Expected (verts, tris) or a mesh/MeshSet object")
    else:
        v = verts
        t = tris
    v = np.ascontiguousarray(v, dtype=np.float64)
    t = np.ascontiguousarray(t, dtype=np.int64)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if len(v) == 0:
        return np.zeros((0, 3), dtype=np.float32)
    if engine() == 'python':
        return _compute_normal_per_vertex_numpy(v, t)
    return _geom.compute_normal_per_vertex(v, t)


def mesh_topology(verts, tris=None):
    """Complete topological measures of a triangle mesh (matching VCGLib get_topological_measures).

    Accepts either (verts, tris) as arrays or a PyMeshLab MeshSet / Mesh object.
    """
    if tris is None:
        if hasattr(verts, 'mesh_number') and verts.mesh_number() == 0:
            v = np.zeros((0, 3), dtype=np.float32)
            t = np.zeros((0, 3), dtype=np.int64)
        elif hasattr(verts, 'current_mesh'):
            try:
                m = verts.current_mesh()
                v = m.vertex_matrix()
                t = m.face_matrix()
            except Exception:
                v = np.zeros((0, 3), dtype=np.float32)
                t = np.zeros((0, 3), dtype=np.int64)
        elif hasattr(verts, 'vertex_matrix'):
            v = verts.vertex_matrix()
            t = verts.face_matrix()
        else:
            raise ValueError("Expected (verts, tris) or a mesh/MeshSet object")
    else:
        v = verts
        t = tris
    v = np.ascontiguousarray(v, dtype=np.float32)
    t = np.ascontiguousarray(t, dtype=np.int64)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if engine() == 'python':
        return _mesh_topology_numpy(v, t)
    return _geom.mesh_topology(v, t)


def mesh_geometric_measures(verts, tris=None):
    """Complete geometric measures of a triangle mesh (matching VCGLib get_geometric_measures).

    Accepts either (verts, tris) as arrays or a PyMeshLab MeshSet / Mesh object.
    """
    if tris is None:
        if hasattr(verts, 'mesh_number') and verts.mesh_number() == 0:
            v = np.zeros((0, 3), dtype=np.float32)
            t = np.zeros((0, 3), dtype=np.int64)
        elif hasattr(verts, 'current_mesh'):
            try:
                m = verts.current_mesh()
                v = m.vertex_matrix()
                t = m.face_matrix()
            except Exception:
                v = np.zeros((0, 3), dtype=np.float32)
                t = np.zeros((0, 3), dtype=np.int64)
        elif hasattr(verts, 'vertex_matrix'):
            v = verts.vertex_matrix()
            t = verts.face_matrix()
        else:
            raise ValueError("Expected (verts, tris) or a mesh/MeshSet object")
    else:
        v = verts
        t = tris
    v = np.ascontiguousarray(v, dtype=np.float32)
    t = np.ascontiguousarray(t, dtype=np.int64)
    if v.ndim != 2 or v.shape[1] != 3:
        raise ValueError("verts must be Nx3")
    if t.ndim != 2 or t.shape[1] != 3:
        raise ValueError("tris must be Mx3")
    if engine() == 'python':
        return _mesh_geometric_measures_numpy(v, t)
    return _geom.mesh_geometric_measures(v, t)


get_topological_measures = mesh_topology
get_geometric_measures = mesh_geometric_measures
meshing_repair_non_manifold_vertices = repair_non_manifold_vertices
meshing_repair_non_manifold_edges = repair_non_manifold_edges
meshing_remove_unreferenced_vertices = remove_unreferenced_vertices
meshing_remove_selected_faces = remove_selected_faces
meshing_re_orient_faces_coherently = re_orient_faces_coherently
meshing_invert_face_orientation = invert_face_orientation
meshing_remove_connected_component_by_face_number = remove_connected_components
meshing_compute_normal_per_face = compute_normal_per_face
meshing_compute_normal_per_vertex = compute_normal_per_vertex


def native_si_enabled():
    """Return True if native self-intersection detection is enabled.

    Default ON: self-intersection queries use the exact geometric predicates
    in sutura_geom rather than PyMeshLab's VCGLib filter. Opt out with
    ``SUTURA_SI_EXACT=0``; the legacy ``SUTURA_NATIVE_SI=0`` is also accepted.
    Precedence: ``SUTURA_SI_EXACT`` > ``SUTURA_NATIVE_SI`` > default True.
    """
    env_exact = os.environ.get('SUTURA_SI_EXACT', '').strip().lower()
    if env_exact in ('1', 'true', 'yes', 'on'):
        return True
    if env_exact in ('0', 'false', 'no', 'off'):
        return False
    env = os.environ.get('SUTURA_NATIVE_SI', '').strip().lower()
    if env in ('0', 'false', 'no', 'off'):
        return False
    return True


def native_orient_enabled():
    """Return True if native coherent face reorientation is enabled.

    Default True (active). Opt out with ``SUTURA_NATIVE_ORIENT=0``.
    """
    env = os.environ.get('SUTURA_NATIVE_ORIENT', '').strip().lower()
    return env not in ('0', 'false', 'no', 'off')


def native_nme_enabled():
    """Return True if native non-manifold edge repair is enabled.

    Default True (active). Opt out with ``SUTURA_NATIVE_NME=0``.
    """
    env = os.environ.get('SUTURA_NATIVE_NME', '').strip().lower()
    return env not in ('0', 'false', 'no', 'off')


def native_nmv_enabled():
    """Return True if native non-manifold vertex repair is enabled.

    Default True (active). Opt out with ``SUTURA_NATIVE_NMV=0``.
    """
    env = os.environ.get('SUTURA_NATIVE_NMV', '').strip().lower()
    return env not in ('0', 'false', 'no', 'off')


def native_measures_enabled():
    """Return True if native geometric measures calculation is enabled.

    Controlled by environment variable ``SUTURA_NATIVE_MEASURES`` (default OFF: 0/false).
    When enabled, geometric measures (mesh volume and surface area) use native
    geometry via sutura_geom rather than PyMeshLab's VCGLib filter.
    """
    env = os.environ.get('SUTURA_NATIVE_MEASURES', '').strip().lower()
    return env in ('1', 'true', 'yes', 'on')


# Exact self-intersection face-count policy cap.  The Rust extension's own
# ``SI_MAX_FACES`` (2,000,000 as of sutura_geom 0.1.0) overrides it; this is
# only the fallback for an older wheel that does not export the attribute.
SI_MAX_FACES = 2_000_000


def _policy_cap():
    """Face cap above which the exact SI classifier reports 'unmeasurable'."""
    if _geom is None:
        return SI_MAX_FACES
    return int(getattr(_geom, 'SI_MAX_FACES', SI_MAX_FACES))


def si_face_count(verts, tris, limit=None):
    """Exact proper-crossing self-intersection face count, or ``None``.

    ``None`` means *unmeasurable*: no Rust kernel, more than ``limit`` faces
    (default :func:`_policy_cap`), or the classifier's own skip sentinel.
    ``0`` means *measured, no crossings* -- never the old silent ``-1 -> 0``.

    This is the count-only entry point used by the exact-count callers
    (``pipeline.utils._exact_si_face_count``, Graft, Dressing); the
    mask-based dispatcher :func:`self_intersecting_faces` keeps the same
    sentinel but also returns the (possibly numpy) mask.
    """
    t = np.asarray(tris, dtype=np.int32)
    if len(t) == 0:
        return 0
    if limit is None:
        limit = _policy_cap()
    if len(t) > limit:
        return None
    if _geom is None or not hasattr(_geom, 'self_intersecting_faces'):
        return None
    try:
        v = np.asarray(verts, dtype=np.float64)
        _mask, count = _geom.self_intersecting_faces(v, t)
    except Exception:  # noqa: BLE001 - a classifier failure is not fatal
        return None
    return None if int(count) < 0 else int(count)


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

    if engine() == 'python':
        return _self_intersecting_faces_numpy(v, t)

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
        return new_bvh.self_intersecting_faces()
    return _geom.self_intersecting_faces(v, t)


def dilate_face_selection(tris, selected, rings=1):
    """Expand a face selection by vertex one-rings (PyMeshLab FaceDilate)."""
    t = np.ascontiguousarray(tris, dtype=np.int64)
    sel = np.asarray(selected, dtype=bool).copy()
    if len(t) == 0 or not sel.any() or int(rings) <= 0:
        return sel
    nv = int(t.max()) + 1
    v_to_faces = [[] for _ in range(nv)]
    for fi, tri in enumerate(t):
        for v in tri:
            v_to_faces[int(v)].append(fi)
    for _ in range(int(rings)):
        out = sel.copy()
        for fi in np.where(sel)[0]:
            for v in t[fi]:
                out[v_to_faces[int(v)]] = True
        sel = out
    return sel


def select_self_intersections_ms(ms, ml=None, mask=None, count=None):
    """Return the self-intersecting face mask for a MeshSet.

    With SUTURA_NATIVE_SI the exact classifier is used and the mask is
    returned array-only (no MeshSet selection is written; callers must use
    the returned mask). The PyMeshLab fallback keeps the legacy behaviour and
    writes the selection so the old opt-out path still works.

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
    'repair_non_manifold_vertices',
    'remove_selected_faces',
    'close_holes',
    'close_holes_ms',
    'compute_normal_per_face',
    'compute_normal_per_vertex',
    'mesh_topology',
    'mesh_geometric_measures',
    'connected_components',
    'native_si_enabled',
    'native_orient_enabled',
    'native_nme_enabled',
    'native_nmv_enabled',
    'native_measures_enabled',
    'self_intersecting_faces',
    'select_self_intersections_ms',
    'si_face_count',
    'SI_MAX_FACES',
    'engine',
    'set_engine',
]
