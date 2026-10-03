#!/usr/bin/env python3
"""Regression test for sutura/defects.py (hole / non-manifold detection).

Checks that:
  1. defects.py is stdlib+numpy only (importing it must NOT pull in
     pymeshlab/trimesh/manifold3d), so it stays importable anywhere.
  2. detect() finds holes and non-manifold regions on a broken mesh, and
     returns nothing for a clean mesh.
  3. detect_non_manifold() pairs each half-edge with the correct face
     (block-stacked layout: half-edge h belongs to face h % F).
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


def test_non_manifold_face_pairing_block_stacked():
    import defects
    # edge (1,3) is used by faces 1, 2 and 3; face 0 does not touch it.  The
    # old `np.repeat(arange(F), 3)` pairing returned [0, 1] (face 0 is not
    # even incident to the non-manifold edge).
    v = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1], [1, 1, 1]],
                 dtype=np.float64)
    t = np.array([[0, 1, 2], [1, 3, 4], [1, 3, 0], [1, 3, 2]], dtype=np.int64)
    d = defects.detect_non_manifold(v, t, with_indices=True)
    assert len(d) == 1, d
    assert d[0]['faces'] == 3, d
    assert d[0]['faces_idx'] == [1, 2, 3], d
    # a case where the region partition itself differed under the buggy pairing
    # (one merged region (0,1,3,4,5,6,7) vs the correct (0,1,3,5,7) + (2,))
    v8 = np.zeros((6, 3))
    t8 = np.array([[2, 3, 3], [3, 3, 5], [4, 4, 4], [3, 2, 5],
                   [2, 1, 5], [0, 5, 3], [0, 0, 2], [0, 0, 3]], dtype=np.int64)
    regions = sorted(r['faces_idx']
                     for r in defects.detect_non_manifold(v8, t8, with_indices=True))
    assert regions == [[0, 1, 3, 5, 7], [2]], regions


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('defects tests passed')


if __name__ == '__main__':
    main()
