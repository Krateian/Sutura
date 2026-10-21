#!/usr/bin/env python3
"""Regression tests for the native quadric decimation pass (Faz 4 W3b).

Covers ``topology.decimate_qem`` (the Rust ``meshopt`` wrapper that replaces
PyMeshLab's ``meshing_decimation_quadric_edge_collapse``) plus the five call
sites that now route through it:

  * ``dressing._qem_decimate`` / ``_decimate_coat`` (coat decimation + rollback),
  * ``ftetwild._decimate_boundary`` (border-locked boundary LOD),
  * ``repeat._decimate_faces`` (picker LOD),
  * ``proxy._decimate_to_budget`` (proxy face-budget cap, both call sites).

Checks:
  1. a dense mesh is reduced towards the target and never above it;
  2. surviving vertex positions are copied bit-identically from the input;
  3. f32-bit-pattern split corners weld, welded degenerate faces are dropped;
  4. ``target_faces`` at or above the input is a byte-identical no-op;
  5. ``lock_border=True`` keeps every border vertex;
  6. ``preserve_normal=True`` runs the attribute path;
  7. output is deterministic across repeated calls;
  8. the wrapper is native-only -- forcing the Python engine raises
     ``RuntimeError`` instead of silently degrading;
  9. invalid arguments raise ``ValueError``;
 10. the five call sites return sane geometry without PyMeshLab.

Needs the ``sutura_geom`` extension with ``decimate_qem``; skips otherwise.

Usage: /tmp/pml/venv/bin/python tests/test_decimate_qem.py
"""
import os
import sys

os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402

import topology  # noqa: E402


def _grid(n, step=1.0):
    """Flat ``n x n`` grid of unit-spaced quads (two triangles each)."""
    xs = np.arange(n, dtype=np.float64) * step
    gx, gy = np.meshgrid(xs, xs, indexing='ij')
    verts = np.column_stack([gx.ravel(), gy.ravel(), np.zeros(n * n)])
    idx = np.arange(n * n).reshape(n, n)
    faces = []
    for j in range(n - 1):
        for i in range(n - 1):
            faces.append([idx[i, j], idx[i + 1, j], idx[i + 1, j + 1]])
            faces.append([idx[i, j], idx[i + 1, j + 1], idx[i, j + 1]])
    return verts, np.asarray(faces, dtype=np.int64)


def _icosphere(subdiv=3):
    """Closed, smooth, welded sphere (20 * 4**subdiv faces)."""
    t = (1.0 + 5.0 ** 0.5) / 2.0
    raw = [(-1, t, 0), (1, t, 0), (-1, -t, 0), (1, -t, 0),
           (0, -1, t), (0, 1, t), (0, -1, -t), (0, 1, -t),
           (t, 0, -1), (t, 0, 1), (-t, 0, -1), (-t, 0, 1)]
    verts = []
    for a, b, c in raw:
        n = (a * a + b * b + c * c) ** 0.5
        verts.append([a / n, b / n, c / n])
    verts = np.asarray(verts, dtype=np.float64)
    faces = np.asarray([
        [0, 11, 5], [0, 5, 1], [0, 1, 7], [0, 7, 10], [0, 10, 11],
        [1, 5, 9], [5, 11, 4], [11, 10, 2], [10, 7, 6], [7, 1, 8],
        [3, 9, 4], [3, 4, 2], [3, 2, 6], [3, 6, 8], [3, 8, 9],
        [4, 9, 5], [2, 4, 11], [6, 2, 10], [8, 6, 7], [9, 8, 1],
    ], dtype=np.int64)
    for _ in range(subdiv):
        cache = {}

        def mid(a, b):
            key = (a, b) if a < b else (b, a)
            if key not in cache:
                p = verts[a] + verts[b]
                idx = len(verts_list)
                verts_list.append(p / np.linalg.norm(p))
                cache[key] = idx
            return cache[key]

        verts_list = [v for v in verts]
        out = []
        for a, b, c in faces:
            ab, bc, ca = mid(a, b), mid(b, c), mid(c, a)
            out += [[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]]
        verts = np.asarray(verts_list, dtype=np.float64)
        faces = np.asarray(out, dtype=np.int64)
    return verts, faces


def _split_corner_cube():
    """Unit cube with every corner duplicated per face (24 verts, 12 tris)."""
    v8 = np.asarray([[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
                     [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]], np.float64)
    f = np.asarray([[0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
                    [0, 1, 5], [0, 5, 4], [1, 2, 6], [1, 6, 5],
                    [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7]], dtype=np.int64)
    verts = []
    tris = []
    for tri in f:
        base = len(verts)
        for i in tri:
            verts.append(v8[i])
        tris.append([base, base + 1, base + 2])
    return np.asarray(verts, np.float64), np.asarray(tris, dtype=np.int64)


def _have_native():
    return topology._rust_kernel_available()


def test_sphere_reaches_target():
    v, t = _icosphere(3)
    assert len(t) > 300
    ov, ot, rep = topology.decimate_qem(v, t, 100)
    assert rep['engine'] == 'meshopt'
    assert rep['no_op'] is False
    assert 0 < len(ot) <= 100
    assert rep['faces_in'] == len(t) and rep['faces_out'] == len(ot)
    assert ov.shape[1] == 3 and ot.shape[1] == 3
    assert np.all(np.isfinite(ov))


def test_positions_are_from_the_input():
    v, t = _icosphere(3)
    ov, _ot, _rep = topology.decimate_qem(v, t, 100)
    for p in ov:
        assert np.any(np.all(np.isclose(v, p, rtol=0.0, atol=0.0), axis=1)), p


def test_split_corners_weld_and_degenerates_dropped():
    v, t = _split_corner_cube()
    assert len(v) == 36  # 12 triangles x 3 split corners
    t = np.vstack([t, np.array([[0, 0, 1]], dtype=np.int64)])  # degenerate once welded
    ov, ot, rep = topology.decimate_qem(v, t, 6)
    assert rep['welded_vertices'] == 8
    assert rep['weld_merged'] == 28
    assert rep['degenerate_removed'] >= 1
    assert 0 < len(ot) <= 6


def test_target_at_or_above_input_is_byte_identical():
    v, t = _icosphere(2)
    ov, ot, rep = topology.decimate_qem(v, t, len(t))
    assert rep['no_op'] is True
    assert np.array_equal(ov, v)
    assert np.array_equal(ot, t)


def test_lock_border_keeps_border_vertices():
    v, t = _grid(3)  # 9 verts, 8 tris, all 8 outer verts on the border
    border = [p for p in v if p[0] in (0.0, 2.0) or p[1] in (0.0, 2.0)]
    ov, _ot, rep = topology.decimate_qem(v, t, 3, lock_border=True)
    assert rep['lock_border'] is True
    for p in border:
        assert np.any(np.all(ov == p, axis=1)), p


def test_preserve_normal_runs():
    v, t = _icosphere(2)
    ov, ot, rep = topology.decimate_qem(v, t, 50, preserve_normal=True)
    assert rep['attributes_used'] == 3
    assert 0 < len(ot) <= 50


def test_deterministic_output():
    v, t = _icosphere(3)
    first = topology.decimate_qem(v, t, 150)
    for _ in range(4):
        again = topology.decimate_qem(v, t, 150)
        assert np.array_equal(first[0], again[0])
        assert np.array_equal(first[1], again[1])
        assert first[2] == again[2]


def test_native_only_raises_without_extension():
    v, t = _grid(6)
    topology.set_engine('python')
    try:
        try:
            topology.decimate_qem(v, t, 10)
        except RuntimeError:
            pass
        else:
            raise AssertionError('expected RuntimeError under the python engine')
    finally:
        topology.set_engine('auto')


def test_invalid_arguments_raise():
    v, t = _grid(4)
    for bad in (-1, -100):
        try:
            topology.decimate_qem(v, t, bad)
        except ValueError:
            pass
        else:
            raise AssertionError('expected ValueError for target_faces=%r' % bad)
    bad_v = np.zeros((3, 2))
    try:
        topology.decimate_qem(bad_v, t, 2)
    except ValueError:
        pass
    else:
        raise AssertionError('expected ValueError for a non-Nx3 verts array')


def test_empty_mesh_is_a_noop():
    ov, ot, rep = topology.decimate_qem(
        np.zeros((0, 3)), np.zeros((0, 3), np.int64), 10)
    assert len(ov) == 0 and len(ot) == 0
    assert rep['no_op'] is True and rep['faces_out'] == 0


def test_dressing_coat_decimation_rollback():
    from sutura_engine import dressing
    v, t = _icosphere(4)  # ~5120 faces
    dv, dt, rec = dressing._decimate_coat(v, t, len(t) // 2)
    assert rec['decimated'] is True, rec
    assert len(dt) < len(t)
    assert dv.dtype == np.float64 and dt.dtype == np.int64


def test_ftetwild_boundary_decimation_is_border_locked():
    from sutura_engine.pipeline.tiers import ftetwild
    v, t = _icosphere(4)
    # Half the faces carry an extra unreferenced vertex, as the real bridge
    # output does.
    vv = np.vstack([v, np.array([[0.0, 0.0, 5.0]])])
    out = ftetwild._decimate_boundary(vv, t, len(t) // 2)
    assert out is not None
    dv, dt = out
    assert len(dt) < len(t)
    assert dv.dtype == np.float64 and dt.dtype == np.int64


def test_repeat_decimate_faces_is_native():
    from sutura_engine.methods import repeat
    v, t = _icosphere(4)
    dv, dt = repeat._decimate_faces(v, t, 500)
    assert dt.dtype == np.int32
    assert 0 < len(dt) <= 500


def test_proxy_decimate_to_budget_guards():
    from sutura_engine.methods import proxy
    v, t = _icosphere(4)
    out = proxy._decimate_to_budget(v, t, len(t) // 2)
    if out is not None:
        dv, dt = out
        assert len(dt) <= len(t)
        assert dt.dtype == np.int32


def main():
    if not _have_native():
        print('SKIP: sutura_geom.decimate_qem not available')
        return 0
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('decimate_qem tests passed')
    return 0


if __name__ == '__main__':
    sys.exit(main())
