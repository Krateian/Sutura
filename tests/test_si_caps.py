#!/usr/bin/env python3
"""Regression: exact self-intersection count contract + no session BVH cache.

Covers the two P1 defects:

* Bug A -- the exact classifier's ``-1`` "unmeasurable" (mesh above the face
  cap) sentinel must never be read as ``0`` ("measured, no crossings").  The
  fix introduces ``topology.si_face_count`` returning ``None`` for the
  sentinel and routes every call site through it.
* Bug B -- the deleted session BVH/SI cache must not return a stale count
  after an in-place input mutation.

Usage: python3 tests/test_si_caps.py
       (sutura_geom is used when importable; the empty/None and sentinel
        cases run without it.)
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, REPO)
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402

import topology  # noqa: E402
from sutura_engine.pipeline import stage1, utils  # noqa: E402


def _crossing_pairs(k):
    """``k`` disjoint, properly-crossing triangle pairs -> 2k SI faces."""
    V = []
    T = []
    for i in range(k):
        x = (i % 100) * 4.0
        y = (i // 100) * 4.0
        a = len(V)
        V += [[x, y, 0.0], [x + 1, y, 0.0], [x, y + 1, 0.0]]
        b = len(V)
        V += [[x + 0.25, y + 0.25, -1.0], [x + 0.25, y + 0.25, 1.0],
              [x + 0.75, y + 0.25, -0.2]]
        T += [[a, a + 1, a + 2], [b, b + 1, b + 2]]
    return np.ascontiguousarray(V, np.float64), np.ascontiguousarray(T, np.int64)


def _clean_cube():
    v = np.array([
        (0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0),
        (0, 0, 1), (1, 0, 1), (1, 1, 1), (0, 1, 1)], dtype=np.float32)
    t = np.array([
        (0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7),
        (0, 1, 5), (0, 5, 4), (1, 2, 6), (1, 6, 5),
        (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7)], dtype=np.int32)
    return v, t


class _FakeMesh:
    def __init__(self, sel):
        self._sel = np.asarray(sel, dtype=float)

    def face_selection_array(self):
        return self._sel

    def vertex_matrix(self):
        return np.zeros((3, 3), dtype=np.float32)

    def face_matrix(self):
        return np.zeros((1, 3), dtype=np.int32)


class _FakeMeshSet:
    """Minimal MeshSet stub: only the pieces _count_si / _si_count_and_mask use."""

    def __init__(self, sel=(0.0,)):
        self._m = _FakeMesh(sel)
        self.filters = []

    def apply_filter(self, name, **kwargs):
        self.filters.append(name)

    def current_mesh(self):
        return self._m


def _have_rust():
    return topology._geom is not None and hasattr(
        topology._geom, 'self_intersecting_faces')


def test_contract():
    v, t = _clean_cube()
    # Empty mesh: measured, no triangles -> 0 (not None).
    assert topology.si_face_count(v[:0], t[:0]) == 0
    assert topology.si_face_count(v, t[:0]) == 0
    # Clean closed cube: measured, no crossings -> 0.
    assert topology.si_face_count(v, t) == 0

    if _have_rust():
        cv, ct = _crossing_pairs(3000)  # 6000 faces
        assert topology.si_face_count(cv, ct) == 6000
        # Above the caller's limit -> None, never 0.
        assert topology.si_face_count(cv, ct, limit=5000) is None
        assert topology.si_face_count(cv, ct, limit=10 ** 9) == 6000

    # No Rust kernel -> None for a non-empty mesh (not a silent 0).
    saved = topology._geom
    try:
        topology._geom = None
        assert topology.si_face_count(v, t) is None
        assert topology.si_face_count(v[:0], t[:0]) == 0
    finally:
        topology._geom = saved

    # The session cache is gone for good.
    assert not hasattr(topology, 'clear_bvh_cache')
    assert '_BVH_CACHE' not in vars(topology)
    assert 'si_face_count' in topology.__all__
    assert 'SI_MAX_FACES' in topology.__all__
    assert 'clear_bvh_cache' not in topology.__all__
    print('contract OK (empty/clean -> 0, over-limit/no-kernel -> None)')


def test_sites_agree():
    from sutura_engine import dressing, graft
    if not _have_rust():
        print('sites: skip (no Rust kernel)')
        return
    cv6, ct6 = _crossing_pairs(3000)   # 6000 faces, under Graft's 20k brake
    c6 = topology.si_face_count(cv6, ct6)
    assert c6 == 6000
    assert utils._exact_si_face_count(cv6, ct6) == c6
    assert dressing.si_face_count(cv6, ct6) == c6
    assert graft._si_count(cv6, ct6) == c6

    cv30, ct30 = _crossing_pairs(15000)  # 30000 faces, above Graft's 20k brake
    c30 = topology.si_face_count(cv30, ct30)
    assert c30 == 30000
    assert utils._exact_si_face_count(cv30, ct30) == c30
    assert dressing.si_face_count(cv30, ct30) == c30
    assert graft._si_count(cv30, ct30) is None
    print('sites OK (topology/utils/dressing/graft agree; Graft capped -> None)')


def test_sentinel_falls_back():
    """A -1 sentinel at the operational sites falls back to PyMeshLab, not 0."""
    saved_native = topology.native_si_enabled
    saved_sif = topology.self_intersecting_faces
    try:
        topology.native_si_enabled = lambda: True
        topology.self_intersecting_faces = lambda *a, **k: (
            np.zeros(3, dtype=bool), -1)

        ms = _FakeMeshSet(sel=(1.0, 0.0, 1.0))
        assert utils._count_si(None, ms) == 2, utils._count_si(None, ms)

        ms2 = _FakeMeshSet(sel=(1.0, 0.0, 1.0))
        n, mask = stage1._si_count_and_mask(ms2)
        assert n == 2, n
        assert mask.dtype == bool and int(mask.sum()) == 2
    finally:
        topology.native_si_enabled = saved_native
        topology.self_intersecting_faces = saved_sif
    print('sentinel OK (-1 -> PyMeshLab fallback, never 0)')


def test_no_stale_cache():
    v = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0],
                  [0.25, 0.25, -1], [0.25, 0.25, 1], [0.75, 0.25, -0.2]],
                 dtype=np.float64)
    t = np.array([[0, 1, 2], [3, 4, 5]], dtype=np.int64)
    _, c1 = topology.self_intersecting_faces(v, t)
    assert c1 == 2, c1
    # Mutate the SAME array in place: the pairs no longer cross.
    v[3:6] += np.array([0.0, 0.0, 50.0])
    _, c2 = topology.self_intersecting_faces(v, t)
    assert c2 == 0, 'stale cached count after in-place mutation: %r' % c2
    print('no-cache OK (in-place mutation is re-measured)')


def test_report_site_reports_none():
    """validate's native SI count is None (not 0) when the classifier skips."""
    try:
        import pymeshlab  # noqa: F401
    except Exception:
        print('report site: skip (no pymeshlab)')
        return
    from sutura_engine.pipeline import validate

    saved_native = topology.native_si_enabled
    saved_sif = topology.self_intersecting_faces
    try:
        topology.native_si_enabled = lambda: True
        topology.self_intersecting_faces = lambda *a, **k: (
            np.zeros(3, dtype=bool), -1)
        v, t = _clean_cube()
        rep = validate.validate_mesh_from_arrays(v, t, si_mode='report')
        assert rep['validation']['self_intersections'] is None, rep['validation']
    finally:
        topology.native_si_enabled = saved_native
        topology.self_intersecting_faces = saved_sif
    print('report site OK (skipped classifier -> None, not 0)')


def main():
    test_contract()
    test_sites_agree()
    test_sentinel_falls_back()
    test_no_stale_cache()
    test_report_site_reports_none()
    print('test_si_caps: all passed')


if __name__ == '__main__':
    main()
