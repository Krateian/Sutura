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


def test_rust_backend_feature_detection():
    """When the installed sutura_geom exposes dressing_coat, it is used (with
    the c2f2116 signature); a positive drain on a build without the parameter
    falls back to the numpy path so the drain is still applied."""
    if sutura_geom is None or not hasattr(sutura_geom, 'dressing_coat'):
        print('    (sutura_geom.dressing_coat unavailable: Rust backend test skipped)')
        return
    takes_drain = dressing._rust_signature_has_drain(sutura_geom.dressing_coat)
    V = np.array(CUBE_V, float)
    T = np.array(CUBE_T[:-1], np.int64)
    # no drain -> Rust path
    _vo, _to, off = dressing.dressing_coat(V, T, intensity='quick', drain='none')
    assert off['engine'] == 'rust', off['engine']
    assert off['holes_after'] == 0 and off['nm_after'] == 0, off
    assert off['si_after'] in (0, None), off
    # positive drain -> numpy unless the build already takes a drain kwarg
    _vf, _tf, full = dressing.dressing_coat(V, T, intensity='quick', drain='full')
    assert full['drain']['drained'] is True, full['drain']
    if not takes_drain:
        assert full['engine'] == 'numpy', full['engine']
    assert full['holes_after'] == 0 and full['nm_after'] == 0, full


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


def test_drain_preset_defaults():
    """Quick drains off; Balanced/Thorough full; Extreme deep (offset report)."""
    assert dressing._resolve_drain(None, 'quick')[1] == 0.0
    assert dressing._resolve_drain(None, 'balanced')[1] == 1.0
    assert dressing._resolve_drain(None, 'thorough')[1] == 1.0
    assert dressing._resolve_drain(None, 'extreme')[1] == 1.25
    # explicit mode strings and numeric deltas
    assert dressing._resolve_drain('none', 'balanced')[1] == 0.0
    assert dressing._resolve_drain('half', 'balanced')[1] == 0.5
    assert dressing._resolve_drain('full', 'quick')[1] == 1.0
    assert dressing._resolve_drain('deep', 'quick')[1] == 1.25
    delta, factor, mode = dressing._resolve_drain(0.05, 'balanced')
    assert delta == 0.05 and factor is None and mode == 'custom'
    # unknown string falls back to off
    assert dressing._resolve_drain('bogus', 'balanced')[1] == 0.0


def test_drain_reduces_healthy_growth_field_level():
    """The field drain (F = s - r + delta_r) shrinks the coat toward the input
    surface on healthy regions and keeps the output watertight/SI-free.

    A closed cube with one face removed: 'full' drain must pull the coat toward
    the input surface on healthy regions -- its bounding-box diagonal must be
    smaller than no drain's -- and must stay 0 holes / 0 NM / 0 SI.  The face
    count is not a valid proxy: the Rust extractor yields the same face count
    for both, since the drain moves the isosurface without changing its grid
    topology, so the geometric extent is checked instead."""
    V = np.array(CUBE_V, float)
    T = np.array(CUBE_T[:-1], np.int64)
    _vo, _to, off = dressing.dressing_coat(V, T, intensity='quick', drain='none')
    _vf, _tf, full = dressing.dressing_coat(V, T, intensity='quick', drain='full')
    assert off['drain']['drained'] is False, off['drain']
    assert full['drain']['drained'] is True, full['drain']
    assert full['drain']['delta_r'] == off['r_base'], full['drain']

    def _span(v):
        v = np.asarray(v, float)
        return float(np.linalg.norm(v.max(0) - v.min(0)))

    assert _span(_vf) < _span(_vo), (_span(_vf), _span(_vo))
    for rep in (off, full):
        assert rep['holes_after'] == 0 and rep['nm_after'] == 0, rep
        assert rep['si_after'] in (0, None), rep


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


def test_resolve_dressing_drain_precedence():
    import repair
    assert repair.resolve_dressing_drain() is None
    assert repair.resolve_dressing_drain(cli_value='deep') == 'deep'
    assert repair.resolve_dressing_drain(cli_value=0.05) == 0.05
    # CLI wins over env
    assert repair.resolve_dressing_drain(cli_value='half',
                                         environ='full') == 'half'
    assert repair.resolve_dressing_drain(environ='deep') == 'deep'
    assert repair.resolve_dressing_drain(environ='0.1') == 0.1
    assert repair.resolve_dressing_drain(environ='bogus') is None


def test_resolve_dressing_defects_and_scales():
    import repair
    # defect set: CLI > env > None; invalid values ignored
    assert repair.resolve_dressing_defects() is None
    assert repair.resolve_dressing_defects(cli_value='holes_nm') == 'holes_nm'
    assert repair.resolve_dressing_defects(cli_value='all') == 'all'
    assert repair.resolve_dressing_defects(cli_value='bogus') is None
    assert repair.resolve_dressing_defects(cli_value='all',
                                           environ='holes_nm') == 'all'
    assert repair.resolve_dressing_defects(environ='holes_nm') == 'holes_nm'
    assert repair.resolve_dressing_defects(environ='bogus') is None
    # scale factors: CLI > env > None; non-positive / unparseable ignored
    assert repair.resolve_dressing_rmax_scale() is None
    assert repair.resolve_dressing_rmax_scale(cli_value=0.25) == 0.25
    assert repair.resolve_dressing_rmax_scale(cli_value=0.25,
                                              environ='0.5') == 0.25
    assert repair.resolve_dressing_rmax_scale(environ='0.5') == 0.5
    assert repair.resolve_dressing_rmax_scale(cli_value=-1) is None
    assert repair.resolve_dressing_rmax_scale(environ='nope') is None
    assert repair.resolve_dressing_sigma_scale(cli_value=2.0) == 2.0
    assert repair.resolve_dressing_sigma_scale(environ='0.5') == 0.5
    assert repair.resolve_dressing_sigma_scale(cli_value=0) is None


def test_scale_factors_shrink_resolved_radii():
    """r_max_scale / sigma_scale multiply the resolved viscosity params while
    the defaults (None) are byte-identical to the unscaled call."""
    V = np.array(CUBE_V, float)
    T = np.array(CUBE_T[:-1], np.int64)
    _v0, _t0, base = dressing.dressing_coat(V, T, intensity='balanced',
                                            drain='none')
    _v1, _t1, shrunk = dressing.dressing_coat(
        V, T, intensity='balanced', drain='none',
        r_max_scale=0.25, sigma_scale=0.5)
    assert base['r_max_scale'] is None and base['sigma_scale'] is None
    assert shrunk['r_max_scale'] == 0.25
    assert shrunk['sigma_scale'] == 0.5
    assert shrunk['r_max'] < base['r_max']
    assert shrunk['sigma'] < base['sigma']
    # None keeps the unscaled values identical
    _v2, _t2, again = dressing.dressing_coat(V, T, intensity='balanced',
                                             drain='none', r_max_scale=None,
                                             sigma_scale=None)
    assert again['r_max'] == base['r_max'] and again['sigma'] == base['sigma']
    assert again['defects'] == base['defects'] == 'all'


def test_defect_set_reflected_in_report():
    V = np.array(CUBE_V, float)
    T = np.array(CUBE_T[:-1], np.int64)
    for ds in ('all', 'holes_nm'):
        _v, _t, rep = dressing.dressing_coat(V, T, intensity='quick',
                                             drain='none', defects=ds)
        assert rep['defects'] == ds, rep['defects']
    # an invalid value falls back to the module default
    _v, _t, rep = dressing.dressing_coat(V, T, intensity='quick',
                                         drain='none', defects='bogus')
    assert rep['defects'] == 'all', rep['defects']


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


def _ico(radius, invert=False, center=(0.0, 0.0, 0.0), sub=2):
    import trimesh
    m = trimesh.creation.icosphere(subdivisions=sub, radius=radius)
    v = np.asarray(m.vertices, float) + np.asarray(center, float)
    t = np.asarray(m.faces, np.int64)
    if invert:
        t = t[:, ::-1].copy()
    return v, t


def _merge(parts):
    vs, ts, off = [], [], 0
    for v, t in parts:
        vs.append(v)
        ts.append(t + off)
        off += len(v)
    return np.vstack(vs), np.vstack(ts)


def test_cleanup_drops_debris_keeps_material():
    """A tiny disconnected shell is dropped; the big material shell is kept."""
    inp = _ico(10.0)
    big = _ico(10.5)
    tiny = _ico(0.3, center=(0.0, 0.0, 0.0))
    cv, ct = _merge([big, tiny])
    v, t, info = dressing._clean_coat_components(inp[0], inp[1], cv, ct, 0.5)
    assert info['ran'] and info['components_before'] == 2, info
    assert info['components_after'] == 1, info
    assert info['removed_debris'] == 1 and info['removed_faces'] > 0, info
    assert len(t) < len(ct), (len(t), len(ct))
    assert manifold_ok(t)


def test_cleanup_keeps_true_cavity_drops_stray_inverted_shell():
    """A nested inverted shell is kept only when the input is genuinely empty
    there (a hollow-shell input); against a solid input it is dropped."""
    cov_pos = _ico(10.5)
    cov_cav = _ico(5.0, invert=True)
    cv, ct = _merge([cov_pos, cov_cav])

    solid = _ico(10.0)
    _v, _t, info = dressing._clean_coat_components(
        solid[0], solid[1], cv, ct, 0.5)
    assert info['input_winding'] is True, info
    assert info['removed_negative'] == 1, info
    assert info['components_after'] == 1, info

    shell = _merge([_ico(10.0), _ico(5.0, invert=True)])
    v, t, info2 = dressing._clean_coat_components(shell[0], shell[1], cv, ct, 0.5)
    assert info2['kept_cavities'] == 1, info2
    assert info2['components_after'] == 2, info2
    assert manifold_ok(t)


def test_cleanup_noop_and_all_debris_fallback():
    """A single-component coat passes through byte-for-byte; a coat made only
    of debris falls back to the original rather than returning nothing."""
    inp = _ico(10.0)
    single = _ico(10.5)
    v, t, info = dressing._clean_coat_components(
        inp[0], inp[1], single[0], single[1], 0.5)
    assert info['ran'] and info['components_after'] == 1, info
    assert info['removed_faces'] == 0 and np.array_equal(t, single[1])

    cv, ct = _merge([_ico(0.2), _ico(0.25, center=(3.0, 0.0, 0.0))])
    _v, _t, info2 = dressing._clean_coat_components(inp[0], inp[1], cv, ct, 0.5)
    assert info2['ran'] is False, info2
    assert 'remove all' in (info2['reason'] or ''), info2
    assert np.array_equal(_t, ct)


def test_resolve_dressing_default_switch():
    """The single default switch turns Dressing into the Auto fallback; the
    disable/force flags still win, and the OFF default is unchanged."""
    import os
    import repair
    saved = os.environ.pop('SUTURA_DRESSING_DEFAULT', None)
    try:
        assert repair.resolve_dressing() is False
        assert repair.resolve_dressing(default_enabled=True) == 'auto'
        assert repair.resolve_dressing(default_enabled=False) is False
        os.environ['SUTURA_DRESSING_DEFAULT'] = '1'
        assert repair.resolve_dressing() == 'auto'
        os.environ['SUTURA_DRESSING_DEFAULT'] = '0'
        assert repair.resolve_dressing() is False
        os.environ['SUTURA_DRESSING_DEFAULT'] = '1'
        assert repair.resolve_dressing(no_dressing=True) is False
        assert repair.resolve_dressing(force=True) is True
    finally:
        os.environ.pop('SUTURA_DRESSING_DEFAULT', None)
        if saved is not None:
            os.environ['SUTURA_DRESSING_DEFAULT'] = saved


def test_dressing_trigger_si_only():
    """The 'auto' fallback fires on an SI-only residual only under
    --si-mode repair (the default 'report' measures but does not escalate) and
    never on a clean result; an unmeasurable SI never triggers."""
    import repair
    assert repair._dressing_wanted('auto', 0, 0, 0) is False
    assert repair._dressing_wanted('auto', 0, 0, None) is False
    # default 'report': SI alone does not escalate
    assert repair._dressing_wanted('auto', 0, 0, 5) is False
    assert repair._dressing_wanted('auto', 0, 0, 5, 'off') is False
    # 'repair': SI alone escalates (historical behaviour)
    assert repair._dressing_wanted('auto', 0, 0, 5, 'repair') is True
    assert repair._dressing_wanted('auto', 1, 0, 0) is True
    assert repair._dressing_wanted('auto', 0, 2, 0) is True
    # forced Dressing overrides si-mode
    assert repair._dressing_wanted(True, 0, 0, 0) is True
    assert repair._dressing_wanted(True, 0, 0, 0, 'off') is True
    assert repair._dressing_wanted(False, 1, 1, 9) is False


def test_resolve_si_mode():
    """SI-mode precedence: CLI > env > default 'report'; invalid env falls
    back to the default."""
    import repair
    assert repair.resolve_si_mode() == 'report'
    assert repair.resolve_si_mode('repair') == 'repair'
    assert repair.resolve_si_mode('off') == 'off'
    assert repair.resolve_si_mode(None, environ={'SUTURA_SI_MODE': 'off'}) == 'off'
    assert repair.resolve_si_mode(None, environ={'SUTURA_SI_MODE': 'bogus'}) == 'report'
    assert repair.resolve_si_mode('repair',
                                  environ={'SUTURA_SI_MODE': 'off'}) == 'repair'


def test_normal_angle_and_cad_helpers():
    """normal-angle is ~0 for an identical mesh, grows on a voxel staircase;
    cad_likeness separates a box (CAD) from a sphere (organic)."""
    import trimesh
    import repair
    box = trimesh.creation.box(extents=[2.0, 2.0, 2.0])
    bv = np.asarray(box.vertices, float)
    bt = np.asarray(box.faces, np.int64)
    iso = trimesh.creation.icosphere(subdivisions=3)
    iv = np.asarray(iso.vertices, float)
    it = np.asarray(iso.faces, np.int64)

    na = dressing.normal_angle_deviation(bv, bt, bv, bt)
    assert na is not None and na['p95'] < 1e-6, na
    snapped = np.round(iv / 0.05) * 0.05
    stair = dressing.normal_angle_deviation(snapped, it, iv, it)
    assert stair is not None and stair['p95'] > 1.0, stair

    assert abs(abs(dressing.signed_volume(bv, bt)) - 8.0) < 1e-6
    assert dressing.count_components(bt) == 1
    assert repair.cad_likeness(bv, bt)['cad_like'] is True
    assert repair.cad_likeness(iv, it)['cad_like'] is False


def test_dressing_gate_rejects_volume_parts_normal():
    """A watertight, fidelity-clean candidate is still rejected on a volume
    blow-up, a component explosion or a staircased normal profile."""
    if ml is None:
        print('    (pymeshlab unavailable: dressing gate test skipped)')
        return
    import repair

    v = np.asarray(TET_V, float)
    t = np.asarray(TET_T, np.int64)

    def fake_bad(_v, _t, **kw):
        rec = {'engine': 'numpy', 'fidelity_ok': True, 'si_exact_unknown': False,
               'si_after': 0, 'si_before': 0, 'voxel': 0.5, 'faces_coat': len(t),
               'decimated': False, 'holes_after': 0, 'nm_after': 0, 'ran': True,
               'warnings': [], 'seconds': 0.1, 'volume_after': 1e9,
               'components_after': 99, 'normal_angle_p95': 80.0}
        return np.asarray(v, float), t, rec

    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t.astype(np.int32)))
    after = ms.apply_filter('get_topological_measures')

    orig = dressing.dressing_coat
    try:
        dressing.dressing_coat = fake_bad
        stats = {}
        repair.dressing_tier(ml, ms, after, stats, v, t, '/tmp',
                             dressing=True)
        rec = stats['dressing']
        assert rec['adopted'] is False, rec
        assert (rec['volume_ok'] is False and rec['parts_ok'] is False
                and rec['normal_ok'] is False), rec
        assert ('volume delta' in rec['reason'] and 'components' in rec['reason']
                and 'normal p95' in rec['reason']), rec
    finally:
        dressing.dressing_coat = orig


def test_dressing_force_adopt_bypasses_shape_gates():
    """force_adopt adopts a shape-gate-rejected coat, but still refuses a
    candidate with holes / non-manifold edges."""
    if ml is None:
        print('    (pymeshlab unavailable: force-adopt test skipped)')
        return
    import repair

    v = np.asarray(TET_V, float)
    t = np.asarray(TET_T, np.int64)

    def fake_bad(_v, _t, **kw):
        rec = {'engine': 'numpy', 'fidelity_ok': True, 'si_exact_unknown': False,
               'si_after': 0, 'si_before': 0, 'voxel': 0.5, 'faces_coat': len(t),
               'decimated': False, 'holes_after': 0, 'nm_after': 0, 'ran': True,
               'warnings': [], 'seconds': 0.1, 'volume_after': 1e9,
               'components_after': 99, 'normal_angle_p95': 80.0}
        return np.asarray(v, float), t, rec

    def fake_holes(_v, _t, **kw):
        rec = {'engine': 'numpy', 'fidelity_ok': True, 'si_exact_unknown': False,
               'si_after': 0, 'si_before': 0, 'voxel': 0.5, 'faces_coat': 12,
               'decimated': False, 'holes_after': 0, 'nm_after': 0, 'ran': True,
               'warnings': [], 'seconds': 0.1}
        return (np.asarray(CUBE_V, float), np.asarray(CUBE_T[:-1], np.int64),
                rec)

    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t.astype(np.int32)))
    after = ms.apply_filter('get_topological_measures')

    orig = dressing.dressing_coat
    try:
        dressing.dressing_coat = fake_bad
        # Without force the shape gates reject it.
        stats = {}
        repair.dressing_tier(ml, ms, after, stats, v, t, '/tmp',
                             dressing=True, force_adopt=False)
        rec = stats['dressing']
        assert rec['adopted'] is False and rec['shape_ok'] is False, rec
        assert rec.get('forced') is False, rec
        # With force the same candidate is adopted and marked forced.
        stats = {}
        repair.dressing_tier(ml, ml.MeshSet(), after, stats, v, t, '/tmp',
                             dressing=True, force_adopt=True)
        rec = stats['dressing']
        assert rec['adopted'] is True, rec
        assert rec['forced'] is True and rec['shape_ok'] is False, rec
        assert 'forced' in (rec['reason'] or ''), rec
        # force never bypasses the strict watertight requirement.
        dressing.dressing_coat = fake_holes
        stats = {}
        repair.dressing_tier(ml, ml.MeshSet(), after, stats, v, t, '/tmp',
                             dressing=True, force_adopt=True)
        rec = stats['dressing']
        assert rec['adopted'] is False, rec
    finally:
        dressing.dressing_coat = orig


def test_dressing_suggestions_payload():
    """The CLI suggestion payload points at the opt-in or the force flag."""
    import repair

    # Not watertight, Dressing never ran -> plain opt-in suggestion.
    sug = repair._dressing_suggestions({'stage1': {'two_manifold': False}})
    assert len(sug) == 1 and sug[0]['method'] == 'dressing', sug
    assert sug[0]['force'] is False, sug
    assert sug[0]['flag'] == repair.DRESSING_SUGGEST_FLAG, sug
    assert sug[0]['warning'] == 'may_deform', sug
    txt = repair._dressing_suggestion_text(sug[0])
    assert '--experimental-dressing' in txt and 'deform' in txt, txt

    # Watertight -> no suggestion.
    wt = {'stage1': {'two_manifold': True, 'holes_remaining': 0,
                     'non_manifold_edges_remaining': 0}}
    assert repair._dressing_suggestions(wt) == []

    # Hard error / budget decline -> no suggestion.
    assert repair._dressing_suggestions({'error': 'malformed'}) == []
    assert repair._dressing_suggestions({'status': 'budget_declined'}) == []

    # Dressing ran but the gate rejected it -> force suggestion.
    gate = {'stage1': {'two_manifold': False},
            'dressing': {'ran': True, 'adopted': False, 'reason': 'gate',
                         'volume_delta_rel': 0.2, 'normal_angle_p95': 42.0,
                         'fidelity_ok': False}}
    sug = repair._dressing_suggestions(gate)
    assert len(sug) == 1 and sug[0]['force'] is True, sug
    assert sug[0]['flag'] == repair.DRESSING_FORCE_ADOPT_FLAG, sug
    txt = repair._dressing_suggestion_text(sug[0])
    assert repair.DRESSING_FORCE_ADOPT_FLAG in txt, txt
    assert 'volume 20.00%' in txt and '42.0' in txt, txt

    # Dressing already adopted -> no suggestion.
    adopted = {'stage1': {'two_manifold': False},
               'dressing': {'ran': True, 'adopted': True}}
    assert repair._dressing_suggestions(adopted) == []

    # Multi-object: one open object drives the suggestion.
    multi = {'object_reports': [
        {'stage1': {'two_manifold': True, 'holes_remaining': 0}},
        {'stage1': {'two_manifold': False}}]}
    assert len(repair._dressing_suggestions(multi)) == 1


if __name__ == '__main__':
    fns = [v for k, v in sorted(globals().items())
           if k.startswith('test_') and callable(v)]
    for fn in fns:
        fn()
        print(f'ok  {fn.__name__}')
    print('dressing tests passed')
