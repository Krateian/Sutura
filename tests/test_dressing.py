#!/usr/bin/env python3
"""Regression tests for Dressing (#16, variable-viscosity volumetric skinning).

Runs under the venv with the maturin-built sutura_geom extension.  The field
build / extraction path needs numpy + scipy + sutura_geom; the tier/adoption
checks additionally use pymeshlab and skip when it is unavailable.
pytest-compatible; runnable directly.

    <venv>/bin/python tests/test_dressing.py
"""
import os
import sys
from collections import Counter

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)
if REPO not in sys.path:
    sys.path.insert(0, REPO)

from sutura_engine import dressing  # noqa: E402
from sutura_engine.methods import all_methods, get_method, rank_methods  # noqa: E402

try:
    import sutura_geom  # noqa: E402
except Exception:  # pragma: no cover
    sutura_geom = None

try:
    import pymeshlab as ml  # noqa: E402
except Exception:  # pragma: no cover
    ml = None


# closed tetrahedron (no defects)
TET_V = [(0, 0, 0), (5, 0, 0), (0, 5, 0), (0, 0, 5)]
TET_T = [(0, 2, 1), (0, 1, 3), (0, 3, 2), (1, 2, 3)]

# open cube (one missing face)
CUBE_V = [(x, y, z) for x in (0, 1) for y in (0, 1) for z in (0, 1)]
CUBE_T = [(0, 1, 3), (0, 3, 2), (4, 6, 7), (4, 7, 5), (0, 4, 5), (0, 5, 1),
          (2, 3, 7), (2, 7, 6), (0, 2, 6), (0, 6, 4), (1, 5, 7), (1, 7, 3)]


def _grid(n=9):
    """A flat triangulated patch: (n-1)^2 * 2 triangles of unit edge."""
    xs, ys = np.meshgrid(np.arange(n), np.arange(n))
    v = np.column_stack([xs.ravel(), ys.ravel(), np.zeros(n * n)]).astype(float)
    tris = []
    for i in range(n - 1):
        for j in range(n - 1):
            a = i * n + j
            b, c, d = a + 1, a + n, a + n + 1
            tris.append((a, b, d))
            tris.append((a, d, c))
    return v, np.array(tris)


def manifold_ok(tris):
    d = Counter()
    for a, b, c in tris:
        for u, v in ((a, b), (b, c), (c, a)):
            d[(int(u), int(v))] += 1
    for (u, v), cnt in d.items():
        if cnt != 1 or d.get((v, u), 0) != 1:
            return False
    return True


def test_voxel_follows_feature_size_not_diagonal():
    # A coarse fragment must NOT be oversampled at diag/200 (the thingi10k_100827
    # 7.3 % failure): the feature anchor (median edge / 1.5) wins.
    v, t = _grid(9)
    diag = dressing._diag(v)
    assert len(t) < 500
    vox = dressing.resolve_dressing_voxel(v, t, diag, 'balanced', None)
    med = dressing._median_edge_length(v, t)
    assert abs(vox - med / 1.5) < 1e-9, (vox, med)
    assert vox > diag / 200.0


def test_voxel_clipped_to_preset_band_for_dense_meshes():
    import trimesh
    m = trimesh.creation.icosphere(subdivisions=4)  # ~5120 faces
    v = np.asarray(m.vertices, float)
    t = np.asarray(m.faces, np.int64)
    diag = dressing._diag(v)
    vox = dressing.resolve_dressing_voxel(v, t, diag, 'balanced', None)
    lo, hi = diag / 350.0, diag / 200.0
    assert lo - 1e-12 <= vox <= hi + 1e-12, (vox, lo, hi)


def test_coat_is_two_manifold_and_si_free():
    for V, T, name in ((TET_V, TET_T, 'tet'), (CUBE_V, CUBE_T[:-1], 'open_cube')):
        v, t, rep = dressing.dressing_coat(np.array(V, float),
                                           np.array(T, np.int64),
                                           intensity='quick')
        assert rep['ran'], (name, rep.get('error'))
        assert rep['holes_after'] == 0, (name, rep)
        assert rep['nm_after'] == 0, (name, rep)
        assert rep['si_after'] in (0, None), (name, rep)
        assert manifold_ok(t), name
        if sutura_geom is not None and len(t) <= dressing.SI_MAX_FACES:
            _mask, count = sutura_geom.self_intersecting_faces(
                np.asarray(v, np.float64), np.asarray(t, np.int64))
            assert count == 0, (name, count)


def test_decimation_rollback_on_topology_violation():
    v, t = _grid(11)  # 200 triangles
    orig = dressing._qem_decimate
    topo = {
        'v': np.asarray(v, float),
        't': np.asarray(t[:-1], np.int64),  # remove a face -> a hole
    }

    def broken(_ml, _v, _t, _target):
        return topo['v'], topo['t']

    dressing._qem_decimate = broken
    try:
        dv, dt, rec = dressing._decimate_coat(object(), v, t, 10)
    finally:
        dressing._qem_decimate = orig
    assert rec['decimated'] is False, rec
    assert rec['reason'] == 'topology_violation', rec
    assert len(dt) == len(t)  # the un-decimated coat is restored
    assert np.array_equal(dt, t)


def test_decimation_noop_when_within_budget():
    v, t = _grid(9)
    dv, dt, rec = dressing._decimate_coat(None, v, t, len(t))
    assert rec['decimated'] is False
    assert rec['reason'] == 'already_within_budget'


def test_qem_decimation_preserves_topology():
    """Real PyMeshLab QEM (optimalplacement=False) keeps the mesh 0 holes /
    0 NM and does not introduce exact self-intersections."""
    if ml is None or sutura_geom is None:
        print('    (pymeshlab/sutura_geom unavailable: QEM test skipped)')
        return
    import trimesh
    m = trimesh.creation.icosphere(subdivisions=4)  # ~5120 faces
    v = np.asarray(m.vertices, np.float64)
    t = np.asarray(m.faces, np.int64)
    dv, dt, rec = dressing._decimate_coat(ml, v, t, len(t) // 2)
    assert rec['decimated'] is True, rec
    assert len(dt) < len(t)
    h, nm = __import__('repair').reload_strict_holes_nm(dv, dt)
    assert h == 0 and nm == 0, (h, nm)
    _mask, si = sutura_geom.self_intersecting_faces(dv, dt)
    assert si == 0, si


def test_drain_hook_is_a_reported_noop():
    """The drain step (inward offset compensating the coat growth) is a stable
    hook: a positive amount is accepted and reported as not yet drained."""
    v, t, rep = dressing.dressing_coat(np.array(TET_V, float),
                                       np.array(TET_T, np.int64),
                                       intensity='quick', drain=0.3)
    assert rep['drain'] is not None, rep
    assert rep['drain']['drained'] is False, rep['drain']
    assert rep['drain']['amount'] == 0.3, rep['drain']
    # zero drain leaves the key unset (no-op)
    _v2, _t2, rep2 = dressing.dressing_coat(np.array(TET_V, float),
                                            np.array(TET_T, np.int64),
                                            intensity='quick', drain=0.0)
    assert rep2['drain'] is None, rep2


def test_registry_method_16_and_opt_in():
    m = get_method('16')
    assert m is not None and m.id == 'dressing'
    assert m.num == 16 and get_method('dressing').num == 16
    assert get_method('coat').num == 16
    assert m.needs_user_input is True
    assert m.invents_geometry is True
    # opt-in: excluded from the auto ranking
    nums = [r.num for r in rank_methods(type('A', (), {})())]
    assert 16 not in nums
    assert any(x.num == 16 for x in all_methods())


def test_resolve_dressing_defaults_off():
    import repair
    assert repair.resolve_dressing() is False
    assert repair.resolve_dressing(force=True) is True
    assert repair.resolve_dressing(no_dressing=True, force=True) is False
    assert repair.resolve_dressing(environ='1') is True
    assert repair.resolve_dressing(environ='0') is False
    assert repair.resolve_dressing(environ='off') is False


def test_dressing_tier_adoption_gate():
    """The tier adopts only a watertight, SI-free, fidelity-clean candidate."""
    if ml is None:
        print('    (pymeshlab unavailable: dressing tier adoption test skipped)')
        return
    import repair

    v = np.asarray(TET_V, float)
    t = np.asarray(TET_T, np.int64)
    base = {'v': v, 't': t}
    clean = np.asarray(v, float)

    def fake_ok(_v, _t, **kw):
        rec = {'engine': 'numpy', 'fidelity_ok': True, 'si_exact_unknown': False,
               'si_after': 0, 'si_before': 0, 'voxel': 0.5, 'faces_coat': len(t),
               'decimated': False, 'holes_after': 0, 'nm_after': 0,
               'warnings': [], 'seconds': 0.1, 'ran': True}
        return clean, t, rec

    def fake_low_fid(_v, _t, **kw):
        _cv, _ct, rec = fake_ok(_v, _t, **kw)
        rec = dict(rec)
        rec['fidelity_ok'] = False
        rec['hausdorff_rel_max'] = 0.5
        return _cv, _ct, rec

    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=base['v'], face_matrix=base['t'].astype(np.int32)))
    after = ms.apply_filter('get_topological_measures')

    orig = dressing.dressing_coat
    try:
        dressing.dressing_coat = fake_ok
        stats = {}
        _ms, _after = repair.dressing_tier(ml, ml.MeshSet(), after, stats,
                                           base['v'], base['t'], '/tmp',
                                           dressing=True)
        assert stats['dressing']['adopted'] is True, stats['dressing']

        dressing.dressing_coat = fake_low_fid
        stats2 = {}
        repair.dressing_tier(ml, ml.MeshSet(), after, stats2,
                             base['v'], base['t'], '/tmp', dressing=True)
        assert stats2['dressing']['adopted'] is False, stats2['dressing']
        assert 'fidelity' in (stats2['dressing']['reason'] or '')
    finally:
        dressing.dressing_coat = orig


if __name__ == '__main__':
    fns = [v for k, v in sorted(globals().items())
           if k.startswith('test_') and callable(v)]
    for fn in fns:
        fn()
        print(f'ok  {fn.__name__}')
    print('dressing tests passed')
