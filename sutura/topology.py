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


__all__ = [
    'edge_table',
    'weld_vertices',
    'weld_reload_equivalent',
    'engine',
    'set_engine',
]
