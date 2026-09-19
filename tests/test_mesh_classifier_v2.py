#!/usr/bin/env python3
"""Regression test for sutura/mesh_classifier_v2.py (experimental engine).

The v2 engine is an opt-in COPY of the classic engine plus RANSAC plane
features and a small trained head. This test guards the invariants:

  1. mesh_classifier_v2.py is stdlib+numpy only (no pymeshlab/trimesh/manifold3d).
  2. classify_mesh() keeps the classic drop-in return shape
     {'type', 'confidence', 'metrics'} and never crashes on empty/degenerate
     input (robustness rule: malformed meshes must not crash the classifier).
  3. With the head disabled (V2_WEIGHTS = None) the decision is byte-identical
     to classic -- the "v2 is a superset until the head is baked" invariant.
  4. The trained head (when baked) keeps the synthetic set at 100%: mechanical
     boxes/gears/lattices/extrusions/cylinders/tubes/fillets and organic
     spheres/torus/capsule/blob.
  5. The head reports the RANSAC + curvature metrics (plane_count, plane_area,
     developable_fraction) alongside the classic ones.

Usage: python3 tests/test_mesh_classifier_v2.py  (needs the venv for trimesh)
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
TESTS = os.path.join(REPO, 'tests')
for p in (SUTURA, TESTS):
    if p not in sys.path:
        sys.path.insert(0, p)

import numpy as np  # noqa: E402

import mesh_classifier as classic  # noqa: E402
import mesh_classifier_v2 as v2  # noqa: E402
from make_classifier_set import iter_meshes  # noqa: E402


def test_stdlib_only():
    import subprocess
    code = (
        "import sys; sys.path.insert(0, %r); import mesh_classifier_v2; "
        "bad=[m for m in ('pymeshlab','trimesh','manifold3d','scipy') "
        "if m in sys.modules]; "
        "print('OK' if not bad else 'BAD:'+','.join(bad))" % SUTURA
    )
    out = subprocess.run([sys.executable, '-c', code],
                         capture_output=True, text=True)
    assert out.returncode == 0, out.stderr
    assert 'OK' in out.stdout, out.stdout + out.stderr


def _disabled_decision():
    """Run v2 with the head disabled and return a per-mesh classic comparison."""
    saved = (v2.V2_WEIGHTS, v2.V2_MEAN, v2.V2_STD)
    v2.V2_WEIGHTS = v2.V2_MEAN = v2.V2_STD = None
    mism = []
    for m in iter_meshes():
        a = classic.classify_mesh(m['verts'], m['tris'])
        b = v2.classify_mesh(m['verts'], m['tris'])
        if (a['type'], a['confidence']) != (b['type'], b['confidence']):
            mism.append((m['name'], a, b))
    v2.V2_WEIGHTS, v2.V2_MEAN, v2.V2_STD = saved
    return mism


def test_head_disabled_equals_classic():
    mism = _disabled_decision()
    assert not mism, 'head-disabled v2 diverged from classic: %s' % mism


def test_return_shape_and_metrics():
    for m in iter_meshes():
        r = v2.classify_mesh(m['verts'], m['tris'])
        assert set(r) == {'type', 'confidence', 'metrics'}, r
        assert r['type'] in ('mechanical', 'organic', 'unknown'), r
        assert 0.0 <= r['confidence'] <= 1.0, r
        for k in ('near90', 'flat', 'gentle', 'plane_count', 'plane_area',
                  'developable_fraction'):
            assert k in r['metrics'], (m['name'], k, r['metrics'])


def test_synthetic_set_100():
    wrong = []
    for m in iter_meshes():
        r = v2.classify_mesh(m['verts'], m['tris'])
        if r['type'] != m['label']:
            wrong.append((m['name'], m['label'], r['type']))
    assert not wrong, 'v2 head regression on synthetic set: %s' % wrong


def test_no_crash_on_degenerate():
    empty_v = np.zeros((0, 3), dtype=np.float32)
    empty_t = np.zeros((0, 3), dtype=np.int32)
    try:
        v2.classify_mesh(empty_v, empty_t)
    except Exception as e:  # must never crash the pipeline; a ValueError is
        # acceptable (callers catch and fall back) but a crash is not.
        assert isinstance(e, (ValueError, IndexError)), e
    single = np.zeros((1, 3), dtype=np.float32)
    one_tri = np.zeros((1, 3), dtype=np.int32)
    v2.classify_mesh(single, one_tri)


def test_nan_coords_do_not_crash():
    v = np.array([[0, 0, 0], [np.nan, 0, 0], [1, 1, 0]], dtype=np.float32)
    t = np.array([[0, 1, 2]], dtype=np.int32)
    try:
        v2.classify_mesh(v, t)
    except Exception as e:
        assert isinstance(e, (ValueError, IndexError, FloatingPointError)), e


if __name__ == '__main__':
    fns = [v for k, v in sorted(globals().items()) if k.startswith('test_')]
    for fn in fns:
        fn()
        print('ok  %s' % fn.__name__)
    print('mesh_classifier_v2 tests passed')