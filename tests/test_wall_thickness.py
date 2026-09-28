#!/usr/bin/env python3
"""Unit tests for thin-wall analysis and thicken-to-min (registry method 15).

Synthetic boxes only: a 2x2x1 slab has a 1.0 wall, so the SDF estimate must
find it and a thicken to 2.0 must grow the slab; a solid 4x4x4 cube is already
above any small target and is returned unchanged. No corpus runs.

Usage: <venv>/bin/python tests/test_wall_thickness.py
(any interpreter with numpy + scipy + trimesh).
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402


def _box(extents):
    import trimesh
    m = trimesh.creation.box(extents=extents)
    return (np.asarray(m.vertices, dtype=np.float64),
            np.asarray(m.faces, dtype=np.int64))


def test_estimate_slab_thickness():
    import wall_thickness as W
    v, t = _box((2, 2, 1))
    rep = W.estimate_wall_thickness(v, t, resolution=32, samples=8000,
                                    min_thickness=2.0)
    assert 0.85 <= rep['min'] <= 1.15, rep['min']
    assert 0.85 <= rep['median'] <= 1.15, rep['median']
    assert len(rep['per_vertex']) == len(v), rep
    assert rep['method'].startswith('sdf-ray'), rep['method']
    assert rep['thin_count'] > 0, rep


def test_thicken_slab():
    import wall_thickness as W
    v, t = _box((2, 2, 1))
    ov, ot, rep = W.thicken_to_min(v, t, min_thickness=2.0,
                                   resolution=32, samples=8000)
    assert rep.get('error') is None, rep
    assert rep['delta'] > 0.3, rep
    assert len(ot) > 0, rep
    extents = np.asarray(ov).max(axis=0) - np.asarray(ov).min(axis=0)
    assert extents[2] > 1.6, extents           # thinner axis grew toward 2.0
    assert extents[0] > 2.0 and extents[1] > 2.0, extents


def test_thicken_solid_cube_unchanged():
    import wall_thickness as W
    v, t = _box((4, 4, 4))
    ov, ot, rep = W.thicken_to_min(v, t, min_thickness=1.0,
                                   resolution=24, samples=6000)
    assert rep['delta'] == 0.0, rep
    assert np.array_equal(ov, v) and np.array_equal(ot, t), rep


def test_marching_tetrahedra_closed():
    import wall_thickness as W
    field = np.zeros((8, 8, 8))
    field[2:6, 2:6, 2:6] = 1.0
    v, t = W._marching_tetrahedra(field, np.zeros(3), 1.0)
    assert len(v) > 0 and len(t) > 0, (len(v), len(t))


def test_registry_entry_wired():
    import methods
    m = methods.get_method(15)
    assert m is not None and m.id == 'wall_thicken', m
    assert m.family == 'envelope', m.family
    assert m.invents_geometry is True
    assert m.needs_user_input is True          # never auto-ranked
    assert m.kwargs.get('wall_thicken') is True, m.kwargs
    assert m.available()[0] is True, m.available()
    # excluded from the ranking because it needs an explicit user choice
    from object_analysis import ObjectAnalysis as A
    nums = [r.num for r in methods.rank_methods(A())]
    assert 15 not in nums, nums
    # aliases resolve
    assert methods.get_method('wall').num == 15
    assert methods.get_method('m15').num == 15


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('wall thickness tests passed')


if __name__ == '__main__':
    main()
