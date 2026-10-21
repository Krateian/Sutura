#!/usr/bin/env python3
"""Regression tests for the native cluster-decimation pass (Faz 4 W3a).

Covers ``topology.cluster_decimate`` (the Rust vertex-clustering pass that
replaces PyMeshLab's ``meshing_decimation_clustering``) plus the two render
call sites that now route through it (``viewer_data_render._decimate_lod`` and
``repeat_picker_render.decimate``):

  1. a fine threshold preserves every vertex, a coarse one collapses the mesh;
  2. face count is monotonically non-increasing in the threshold and never
     exceeds the input;
  3. the output is deterministic (identical across repeated calls);
  4. the wrapper is native-only -- forcing the Python engine raises
     ``RuntimeError`` instead of silently degrading;
  5. an invalid threshold raises ``ValueError``;
  6. both render helpers reduce a dense mesh to at or below their target.

Needs the ``sutura_geom`` extension with ``cluster_decimate``; skips otherwise.

Usage: /tmp/pml/venv/bin/python tests/test_cluster_decimate.py
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


def _have_native():
    return topology._rust_kernel_available()


def test_fine_threshold_preserves_every_vertex():
    v, t = _grid(10, 1.0)
    ov, ot, rep = topology.cluster_decimate(v, t, 0.25)
    assert rep['faces_in'] == len(t)
    assert rep['faces_out'] == len(t)
    assert len(ot) == len(t)
    assert len(ov) == len(v)


def test_coarse_threshold_collapses_everything():
    v, t = _grid(10, 1.0)
    ov, ot, rep = topology.cluster_decimate(v, t, 100.0)
    assert rep['faces_out'] == 0
    assert len(ot) == 0
    assert len(ov) == 0


def test_threshold_is_monotonic_and_bounded():
    v, t = _grid(12, 1.0)
    counts = [topology.cluster_decimate(v, t, thr)[2]['faces_out']
              for thr in (0.25, 1.0, 2.0, 4.0, 8.0)]
    assert counts[0] == len(t)
    assert all(counts[i] >= counts[i + 1] for i in range(len(counts) - 1))
    assert counts[-1] < counts[0]
    assert all(c <= len(t) for c in counts)


def test_deterministic_output():
    v, t = _grid(12, 1.0)
    first = topology.cluster_decimate(v, t, 1.5)
    for _ in range(5):
        again = topology.cluster_decimate(v, t, 1.5)
        assert np.array_equal(first[0], again[0])
        assert np.array_equal(first[1], again[1])
        assert first[2] == again[2]


def test_native_only_raises_without_extension():
    v, t = _grid(6, 1.0)
    topology.set_engine('python')
    try:
        try:
            topology.cluster_decimate(v, t, 1.0)
        except RuntimeError:
            pass
        else:
            raise AssertionError('expected RuntimeError under the python engine')
    finally:
        topology.set_engine('auto')


def test_invalid_threshold_raises():
    v, t = _grid(4, 1.0)
    for bad in (0.0, -1.0, float('nan'), float('inf')):
        try:
            topology.cluster_decimate(v, t, bad)
        except ValueError:
            pass
        else:
            raise AssertionError('expected ValueError for threshold=%r' % bad)


def test_empty_mesh_passthrough():
    ov, ot, rep = topology.cluster_decimate(np.zeros((0, 3)), np.zeros((0, 3), np.int64), 1.0)
    assert len(ov) == 0 and len(ot) == 0
    assert rep['faces_in'] == 0 and rep['faces_out'] == 0


def test_decimate_lod_reaches_target():
    from viewer_data_render import _decimate_lod
    v, t = _grid(60, 1.0)
    assert len(t) > 200
    lv, lt = _decimate_lod(v, t, target=200)
    assert 0 < len(lt) <= 200
    assert lv.shape[1] == 3 and lt.shape[1] == 3


def test_repeat_picker_decimate_reaches_target():
    from repeat_picker_render import decimate
    v, t = _grid(60, 1.0)
    assert len(t) > 200
    cv, ct = decimate(v, t, target=200)
    assert 0 < len(ct) <= 200
    assert cv.shape[1] == 3 and ct.shape[1] == 3


def main():
    if not _have_native():
        print('SKIP: sutura_geom.cluster_decimate not available')
        return 0
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('cluster decimate tests passed')
    return 0


if __name__ == '__main__':
    sys.exit(main())
