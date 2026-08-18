#!/usr/bin/env python3
"""Regression test for sutura/defects.py (hole / non-manifold detection).

Checks that:
  1. defects.py is stdlib+numpy only (importing it must NOT pull in
     pymeshlab/trimesh/manifold3d), so it stays importable anywhere.
  2. detect() finds holes and non-manifold regions on a broken mesh, and
     returns nothing for a clean mesh.
Usage: python3 tests/test_defects.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402


def _clean_cube():
    # 8 vertices of a unit cube, 12 triangles
    v = np.array([
        (0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0),
        (0, 0, 1), (1, 0, 1), (1, 1, 1), (0, 1, 1)], dtype=np.float32)
    t = np.array([
        (0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7),
        (0, 1, 5), (0, 5, 4), (1, 2, 6), (1, 6, 5),
        (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7)], dtype=np.int32)
    return v, t


def test_stdlib_only():
    import defects  # noqa: F401
    for heavy in ('pymeshlab', 'trimesh', 'manifold3d'):
        assert heavy not in sys.modules, (
            'defects import pulled in %s; it must stay stdlib+numpy only' % heavy)


def test_clean_cube_no_defects():
    import defects
    v, t = _clean_cube()
    d = defects.detect(v, t)
    assert d['holes'] == [], d['holes']
    assert d['non_manifold'] == [], d['non_manifold']


def test_removed_face_is_a_hole():
    import defects
    v, t = _clean_cube()
    # drop the bottom face (triangles 0 and 1) -> one square hole
    t = t[2:]
    d = defects.detect(v, t)
    assert len(d['holes']) == 1, d['holes']
    h = d['holes'][0]
    assert h['vertices'] == 4, h
    assert 1.0 < h['diameter'] < 1.5, h  # square hole diagonal ~1.41


def test_non_manifold_detected():
    import defects
    v, t = _clean_cube()
    # add a second copy of a face over an existing one -> non-manifold edge
    t = np.vstack([t, t[0]])
    d = defects.detect(v, t)
    assert len(d['non_manifold']) >= 1, d['non_manifold']


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('defects tests passed')


if __name__ == '__main__':
    main()
