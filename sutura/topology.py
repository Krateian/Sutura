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


_RUST_FNS = ('edge_table', 'weld_vertices', 'weld_reload_equivalent')


def _rust_kernel_available():
    """True when the importable extension actually carries all three kernels.

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
    if len(v) == 0 or len(t) == 0:
        return v, t
    used = np.unique(t)
    if len(used) == len(v) and np.array_equal(used, np.arange(len(v))):
        return v, t
    remap = np.full(len(v), -1, dtype=np.int64)
    remap[used] = np.arange(len(used))
    return v[used], remap[t]


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
    if len(v) == 0 or len(t) == 0:
        return v, t
    if _has_rust('remove_unreferenced_vertices'):
        try:
            return _geom.remove_unreferenced_vertices(v, t)
        except Exception:
            pass
    return _remove_unreferenced_vertices_numpy(v, t)


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
    'connected_components',
    'engine',
    'set_engine',
]
