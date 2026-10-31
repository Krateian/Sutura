#!/usr/bin/env python3
"""Tamponade shape-suitability gate tests (no Rust extension required).

Fixtures are generated in memory: an open plane with a central hole, an open
cylinder shell with a side hole, a wavy cloth-like sheet with holes, a
multi-petal flower made of flat petals and a slender open tube. The gate must
leave solid interiors alone and skip the open shells. The slender tube is a
documented limitation: it is not skipped yet, which is harmless because the plug
is rejected later.
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, 'sutura'))

import numpy as np  # noqa: E402

import repair  # noqa: E402
import methods as method_registry  # noqa: E402
import sutura_engine.pipeline.tiers.tamponade as tamp_mod  # noqa: E402
from sutura_engine import tamponade as tamp  # noqa: E402


_CUBE_V = np.array([
    [0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
    [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1],
], dtype=np.float64)
_CUBE_F = np.array([
    [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
    [0, 1, 5], [0, 5, 4], [1, 2, 6], [1, 6, 5],
    [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7],
], dtype=np.int64)
_OPEN1_F = np.array([f for f in _CUBE_F
                     if tuple(f) not in {(4, 5, 6), (4, 6, 7)}],
                    dtype=np.int64)


def _grid_sheet(n=12, hole=5, wave=0.0):
    xs = np.linspace(-10.0, 10.0, n + 1)
    verts = []
    for j in range(n + 1):
        for i in range(n + 1):
            x, y = xs[i], xs[j]
            z = wave * np.sin(x * 0.7) * np.cos(y * 0.5)
            verts.append([x, y, z])
    tris = []
    start = (n - hole) // 2
    stop = start + hole
    for j in range(n):
        for i in range(n):
            if start <= i < stop and start <= j < stop:
                continue
            a = j * (n + 1) + i
            b, c, d = a + 1, a + n + 2, a + n + 1
            tris.append([a, b, c])
            tris.append([a, c, d])
    return np.asarray(verts, dtype=np.float64), np.asarray(tris, dtype=np.int64)


def _cylinder_shell(nc=24, nz=8, radius=5.0, height=10.0):
    verts = []
    for k in range(nz + 1):
        z = height * k / nz
        for i in range(nc):
            a = 2.0 * np.pi * i / nc
            verts.append([radius * np.cos(a), radius * np.sin(a), z])
    tris = []
    for k in range(nz):
        for i in range(nc):
            j = (i + 1) % nc
            a = k * nc + i
            b = k * nc + j
            c = (k + 1) * nc + j
            d = (k + 1) * nc + i
            tris.append([a, b, c])
            tris.append([a, c, d])
    # Cut one side quad out so the tier has a hole to consider.
    tris = tris[2:]
    return np.asarray(verts, dtype=np.float64), np.asarray(tris, dtype=np.int64)


def _slender_tube(radius=1.0, height=80.0, nc=24, nz=40):
    verts = []
    for k in range(nz + 1):
        z = height * k / nz
        for i in range(nc):
            a = 2.0 * np.pi * i / nc
            verts.append([radius * np.cos(a), radius * np.sin(a), z])
    tris = []
    for k in range(nz):
        for i in range(nc):
            j = (i + 1) % nc
            a = k * nc + i
            b = k * nc + j
            c = (k + 1) * nc + j
            d = (k + 1) * nc + i
            tris.append([a, b, c])
            tris.append([a, c, d])
    del tris[(nz // 2) * nc * 2:(nz // 2) * nc * 2 + 2]
    return np.asarray(verts, dtype=np.float64), np.asarray(tris, dtype=np.int64)


def _flower(petals=6):
    verts = []
    tris = []
    for p in range(petals):
        a0 = 2.0 * np.pi * p / petals
        a1 = a0 + 0.46
        base = len(verts)
        verts.extend([
            [2.0 * np.cos(a0), 2.0 * np.sin(a0), 0.0],
            [6.0 * np.cos(a0), 6.0 * np.sin(a0), 0.0],
            [2.0 * np.cos(a1), 2.0 * np.sin(a1), 0.0],
            [4.0 * np.cos(a1), 4.0 * np.sin(a1), 0.0],
        ])
        tris.append([base, base + 1, base + 2])
        tris.append([base + 1, base + 3, base + 2])
    return np.asarray(verts, dtype=np.float64), np.asarray(tris, dtype=np.int64)


def _holes(v, t):
    import defects
    return defects.detect_holes(v, t, with_indices=True)


def test_open_plane_is_not_suitable():
    v, t = _grid_sheet()
    shape = tamp_mod._tamponade_shape(v, t, _holes(v, t))
    assert shape['skip'] is True, shape
    assert shape['reason'] == 'no_volume', shape
    assert shape['effective_thickness_norm'] <= tamp_mod.TAMP_SHAPE_THICKNESS, shape


def test_cylinder_shell_is_not_suitable():
    v, t = _cylinder_shell()
    try:
        import sutura_geom
        has_winding = hasattr(sutura_geom, 'PyMeshBvh')
    except ImportError:
        has_winding = False
    old = tamp_mod._solid_area_fraction
    if not has_winding:
        tamp_mod._solid_area_fraction = lambda _v, _t: 0.0
    try:
        shape = tamp_mod._tamponade_shape(v, t, _holes(v, t))
    finally:
        tamp_mod._solid_area_fraction = old
    assert shape['skip'] is True, shape
    assert shape['reason'] == 'open_shell', shape
    assert shape['perimeter_ratio'] > tamp_mod.TAMP_SHAPE_PERIMETER_RATIO, shape


def test_solid_area_fraction_separates_shell_from_solid():
    try:
        import sutura_geom
        if not hasattr(sutura_geom, 'PyMeshBvh'):
            return
    except ImportError:
        return
    sheet = tamp_mod._solid_area_fraction(*_grid_sheet())
    tube = tamp_mod._solid_area_fraction(*_cylinder_shell())
    cube = tamp_mod._solid_area_fraction(_CUBE_V, _CUBE_F)
    assert sheet is not None and sheet <= 0.10, sheet
    assert tube is not None and tube <= 0.10, tube
    assert cube is not None and cube >= 0.90, cube


def test_slender_open_tube_is_not_skipped_yet():
    # Known limitation: a long thin tube looks solid to generalized winding
    # because its missing end caps are a small part of the surface. A missed
    # skip is harmless; the plug is rejected later.
    v, t = _slender_tube()
    shape = tamp_mod._tamponade_shape(v, t, _holes(v, t))
    assert shape['skip'] is False, shape


def test_wavy_sheet_with_holes_is_not_suitable():
    v, t = _grid_sheet(wave=1.5)
    shape = tamp_mod._tamponade_shape(v, t, _holes(v, t))
    assert shape['skip'] is True, shape


def test_multi_petal_flower_is_not_suitable():
    v, t = _flower()
    shape = tamp_mod._tamponade_shape(v, t, _holes(v, t))
    assert shape['skip'] is True, shape


def test_solid_open_box_remains_suitable():
    v, t = _CUBE_V, _OPEN1_F
    shape = tamp_mod._tamponade_shape(v, t, _holes(v, t))
    assert shape['skip'] is False, shape
    assert shape['genus'] is None or shape['genus'] >= -1, shape


def test_thin_shell_rule_uses_corroborating_fields():
    v, t = _grid_sheet(wave=0.5)
    holes = _holes(v, t)
    old = tamp_mod._thin_sample_fraction
    tamp_mod._thin_sample_fraction = lambda _v, _t: 0.50
    try:
        shape = tamp_mod._tamponade_shape(v, t, holes)
    finally:
        tamp_mod._thin_sample_fraction = old
    assert shape['skip'] is True, shape
    assert shape['reason'] == 'thin_shell', shape


def _tier(v, t, force=False, intensity='balanced'):
    stats = {}
    calls = {'n': 0}
    real = tamp.run_tamponade

    def fake(_v, _t, **_kw):
        calls['n'] += 1
        return _v, _t, {'ran': True, 'healthy_faces_kept': len(_t)}
    tamp.run_tamponade = fake
    try:
        repair.tamponade_tier(v, t, v, t, {'genus': -1}, stats, '/tmp',
                              tamponade=True, intensity=intensity,
                              prepass=True, tamponade_force=force)
    finally:
        tamp.run_tamponade = real
    return stats['tamponade'], calls['n']


def test_tier_skips_sheet_without_calling_the_plugin():
    v, t = _grid_sheet()
    rec, n = _tier(v, t)
    assert n == 0, n
    assert rec['ran'] is False and rec['adopted'] is False, rec
    assert rec['gate'] == 'shape_card', rec
    assert rec['reason_key'] == 'tamponade_skip_no_volume', rec
    assert rec['shape']['skip'] is True, rec


def test_tier_skips_open_shell_without_calling_the_plugin():
    v, t = _cylinder_shell()
    old = tamp_mod._solid_area_fraction
    tamp_mod._solid_area_fraction = lambda _v, _t: 0.0
    try:
        rec, n = _tier(v, t)
    finally:
        tamp_mod._solid_area_fraction = old
    assert n == 0, n
    assert rec['ran'] is False and rec['adopted'] is False, rec
    assert rec['gate'] == 'shape_card', rec
    assert rec['reason_key'] == 'tamponade_skip_open_shell', rec
    assert rec['shape']['reason'] == 'open_shell', rec


def test_forced_method_bypasses_the_gate():
    v, t = _grid_sheet()
    rec, n = _tier(v, t, force=True)
    assert n == 1, n
    assert rec['ran'] is True, rec
    assert rec['gate'] == 'user_forced', rec


def test_tier_runs_on_solid_open_box():
    rec, n = _tier(_CUBE_V, _OPEN1_F)
    assert n == 1, n
    assert rec['ran'] is True, rec
    assert rec.get('gate') != 'shape_card', rec


def test_registry_method_17_forces_the_tier():
    m = method_registry.get_method(17)
    assert m is not None, 'method 17 must be registered'
    assert m.kwargs.get('tamponade') is True, m.kwargs
    assert m.kwargs.get('tamponade_force') is True, m.kwargs


def main():
    tests = [v for k, v in sorted(globals().items())
             if k.startswith('test_') and callable(v)]
    for fn in tests:
        fn()
    print('tamponade shape-gate tests passed (%d)' % len(tests))


if __name__ == '__main__':
    main()
