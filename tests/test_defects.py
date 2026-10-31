#!/usr/bin/env python3
"""Regression test for sutura/defects.py (hole / non-manifold detection).

Checks that:
  1. defects.py is stdlib+numpy only (importing it must NOT pull in
     pymeshlab/trimesh/Manifold), so it stays importable anywhere.
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
    for heavy in ('pymeshlab', 'trimesh'):
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


def _nm_vertex_pair():
    # two tetrahedra sharing ONLY vertex 0: no edge is shared by > 2 faces, so
    # the legacy edge detector sees nothing while diagnosis_v2 reports a
    # non-manifold vertex.
    v = np.array([[0, 0, 0],
                  [1, 0, 0], [0, 1, 0], [0, 0, 1],
                  [-1, 0, 0], [0, -1, 0], [0, 0, -1]], dtype=np.float64)
    t = np.array([[0, 1, 2], [0, 2, 3], [0, 3, 1], [1, 2, 3],
                  [0, 4, 5], [0, 5, 6], [0, 6, 4], [4, 5, 6]], dtype=np.int64)
    return v, t


def test_decode_rle_roundtrip():
    import defects
    rle = {'encoding': 'rle', 'length': 6, 'omitted': False,
           'runs': [(0, 2, 1), (3, 3, 4)]}
    assert defects._decode_rle(rle, 6).tolist() == [1, 1, 0, 4, 4, 4]
    # omitted / unknown encoding / missing mask -> zeros, never a crash
    for bad in (None, {},
                {'encoding': 'rle', 'length': 4, 'omitted': True,
                 'runs': [(0, 4, 7)]},
                {'encoding': 'raw', 'length': 4, 'omitted': False,
                 'runs': [(0, 4, 7)]}):
        assert defects._decode_rle(bad, 4).tolist() == [0, 0, 0, 0], bad


def test_colour_palette_is_shared():
    import defects
    assert defects._COLOR_DEGENERATE == defects.DEFECT_COLORS['degenerate']
    assert defects._COLOR_NON_MANIFOLD == defects.DEFECT_COLORS['non_manifold']
    assert defects._COLOR_FLIPPED == defects.DEFECT_COLORS['flipped']
    # the v2-only kinds must be distinct from the shared three and each other
    shared = {defects.DEFECT_COLORS[k]
              for k in ('degenerate', 'non_manifold', 'flipped')}
    new = {defects.DEFECT_COLORS[k] for k in
           ('self_intersection', 'duplicate', 'noise', 'interior', 'thin_wall')}
    assert not (shared & new), shared & new
    assert len(new) == 5, new


def test_defect_map_v2_falls_back_without_sutura_geom():
    import defects
    import sys
    import builtins
    v, t = _clean_cube()
    saved = sys.modules.pop('sutura_geom', None)
    real_import = builtins.__import__

    def _blocked(name, *a, **k):
        if name == 'sutura_geom':
            raise ImportError('blocked for fallback test')
        return real_import(name, *a, **k)

    builtins.__import__ = _blocked
    try:
        got = defects.defect_map_v2(v, t, with_indices=True)
        cols = defects.defect_type_colors_v2(v, t)
        assert defects.defect_masks_v2(v, t) is None
    finally:
        builtins.__import__ = real_import
        if saved is not None:
            sys.modules['sutura_geom'] = saved
    assert got == defects.detect(v, t, with_indices=True)
    assert cols.tolist() == defects.defect_type_colors(v, t).tolist()


def test_defect_map_v2_adds_non_manifold_vertices():
    import defects
    v, t = _nm_vertex_pair()
    if defects.defect_masks_v2(v, t) is None:
        return  # sutura_geom unavailable: fallback path, nothing v2 to add
    legacy = defects.detect(v, t, with_indices=True)
    merged = defects.defect_map_v2(v, t, with_indices=True)
    assert legacy['non_manifold'] == [], legacy['non_manifold']
    assert len(merged['non_manifold']) >= 1, merged['non_manifold']
    dv = set()
    for r in merged['non_manifold']:
        dv.update(r['verts_idx'])
    assert 0 in dv, dv  # the shared vertex must be reported
    # the legacy hole list is preserved verbatim
    assert merged['holes'] == legacy['holes']


def test_v2_colours_never_lose_legacy_kinds():
    import defects
    # a classic non-manifold EDGE: v2 cannot see it, the legacy map can, so the
    # merged colour map must still paint those faces red.
    v = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1],
                  [0, -1, 0]], dtype=np.float64)
    t = np.array([[0, 1, 2], [0, 1, 3], [0, 1, 4]], dtype=np.int64)
    red = defects.DEFECT_COLORS['non_manifold']
    legacy = defects.defect_type_colors(v, t)
    merged = defects.defect_type_colors_v2(v, t)
    for f in range(len(t)):
        if tuple(legacy[f]) != (0, 0, 0):
            assert tuple(merged[f]) == tuple(legacy[f]), (f, legacy[f], merged[f])
    assert int((merged == red).all(axis=1).sum()) == int((legacy == red).all(axis=1).sum())


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('defects tests passed')


if __name__ == '__main__':
    main()
