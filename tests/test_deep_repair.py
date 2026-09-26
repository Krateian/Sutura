#!/usr/bin/env python3
"""Regression tests for the deep-repair ladder (`repair.deep_repair_ladder`).

- 'full' produces exactly the pre-ladder output (the fTetWild block moved
  into the ladder unchanged), with and without a (fake) fTetWild tier.
- 'off' runs no tier and reports `deep_repair.available` (remaining holes /
  non-manifold edges, tiers, estimate) when something is left.
- 'local' never changes a face outside the deleted region, and its guard
  rejects a result that does.
- Mode resolution: CLI flag > env > config > default, and the pre-ladder
  fTetWild flags keep their meaning.

No fTetWild needed (the tier is faked where it is exercised). Needs the venv
(pymeshlab). Usage:
    ~/.local/share/sutura/venv/bin/python tests/test_deep_repair.py
"""
import json
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402
import pymeshlab as ml  # noqa: E402

import repair  # noqa: E402

OPEN_SAMPLE = os.path.join(REPO, 'tests', 'real-world-samples', 'thingi10k_100827.stl')
CUBE_V = [(x, y, z) for x in (0, 1) for y in (0, 1) for z in (0, 1)]
CUBE_T = [(0, 1, 3), (0, 3, 2), (4, 6, 7), (4, 7, 5), (0, 4, 5), (0, 5, 1),
          (2, 3, 7), (2, 7, 6), (0, 2, 6), (0, 6, 4), (1, 5, 7), (1, 7, 3)]


def _load(path):
    ms = ml.MeshSet()
    ms.load_new_mesh(path)
    m = ms.current_mesh()
    return (np.asarray(m.vertex_matrix(), np.float32),
            np.asarray(m.face_matrix(), np.int32))


def _repair(v, t, **kw):
    with tempfile.TemporaryDirectory(prefix='sutura-deep-') as tmp:
        return repair.repair_mesh_from_arrays(v, t, tmp, **kw)


def _same_output(a, b):
    rep_a, va, ta = a
    rep_b, vb, tb = b
    assert np.array_equal(va, vb) and np.array_equal(ta, tb)
    ra = {k: x for k, x in rep_a.items() if k != 'deep_repair'}
    rb = {k: x for k, x in rep_b.items() if k != 'deep_repair'}
    for d in (ra, rb):   # wall-clock fields differ between runs
        d.get('experimental_ftetwild') and d['experimental_ftetwild'].pop('time', None)
    assert json.dumps(ra, sort_keys=True, default=str) == \
        json.dumps(rb, sort_keys=True, default=str)


def _face_counter(verts, tris):
    """Reference multiset of faces by float32 corner coordinates, each face
    keyed by its smallest cyclic rotation."""
    from collections import Counter
    out = Counter()
    for f in np.asarray(verts, np.float32)[np.asarray(tris, np.int64)]:
        r = tuple(c.tobytes() for c in f)
        out[min(r, r[1:] + r[:1], r[2:] + r[:2])] += 1
    return out


def _sphere_with_hole(z_cut=0.95):
    """Sphere with the cap above ``z_cut`` removed: 0.95 leaves one
    16-edge hole (inside the local tier's scope), 0.8 a 36-edge hole."""
    ms = ml.MeshSet()
    ms.create_sphere(subdiv=3)
    m = ms.current_mesh()
    v = np.asarray(m.vertex_matrix(), np.float64)
    t = np.asarray(m.face_matrix(), np.int64)
    keep = v[t][:, :, 2].mean(axis=1) < z_cut
    return v, t[keep]


def test_full_equals_pre_ladder_output():
    v, t = _load(OPEN_SAMPLE)
    _same_output(_repair(v, t, ftetwild='auto'),
                 _repair(v, t, ftetwild='auto', deep_repair='full'))
    _same_output(_repair(v, t, ftetwild=False),
                 _repair(v, t, ftetwild=False, deep_repair='full'))


def test_full_equals_pre_ladder_with_fake_ftetwild():
    """The moved fTetWild block adopts a (fake) closed boundary exactly as
    before: same arrays and report with and without the ladder mode."""
    def fake_run(inter, out_obj):
        repair.write_obj(out_obj, np.asarray(CUBE_V, np.float32),
                         np.asarray(CUBE_T, np.int32))
        return {'ok': True, 'output_faces': 12}, True
    saved = repair.ftetwild_available, repair.run_ftetwild
    repair.ftetwild_available, repair.run_ftetwild = (lambda: True), fake_run
    try:
        v, t = _load(OPEN_SAMPLE)
        a = _repair(v, t, ftetwild='auto')
        b = _repair(v, t, ftetwild='auto', deep_repair='full')
    finally:
        repair.ftetwild_available, repair.run_ftetwild = saved
    assert a[0]['experimental_ftetwild']['adopted'], a[0]['experimental_ftetwild']
    _same_output(a, b)
    dr = b[0]['deep_repair']
    assert dr['tiers_run'] == ['ftetwild'] and dr['final_tier'] == 'ftetwild', dr
    assert dr['available'] is None


def test_off_reports_available():
    v, t = _load(OPEN_SAMPLE)
    rep, ov, ot = _repair(v, t, deep_repair='off')
    _same_output(_repair(v, t, ftetwild=False), (rep, ov, ot))
    dr = rep['deep_repair']
    assert dr['tiers_run'] == [] and dr['local'] is None and dr['ftetwild'] is None
    av = dr['available']
    s1 = rep['stage1']
    assert av and av['holes_remaining'] == s1['holes_remaining'] > 0, av
    assert av['nm_remaining'] == s1['non_manifold_edges_remaining']
    assert 'local' in av['tiers'] and av['estimate_s']['local'] > 0, av
    assert ('ftetwild' in av['tiers']) == repair.ftetwild_available()


def test_off_on_a_clean_mesh_offers_nothing():
    rep, _v, _t = _repair(np.asarray(CUBE_V, np.float32),
                          np.asarray(CUBE_T, np.int32), deep_repair='off')
    assert rep['deep_repair']['available'] is None
    assert rep['deep_repair']['final_tier'] == 'stage1'


def test_local_keeps_faces_outside_the_region():
    v, t = _sphere_with_hole()
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t.astype(np.int32)))
    topo = ms.apply_filter('get_topological_measures')
    assert repair.boundary_loop_stats(v, t)[0] == 1
    region, n = repair._damaged_region(t)
    assert n == 1 and 0 < region.sum() < len(t)
    out_ms, _topo, rep = repair._local_remesh_tier(ml, ms, topo)
    assert rep['adopted'] and rep['outside_unchanged'], rep
    assert rep['holes_after'] == 0 and rep['nm_after'] == 0, rep
    m = out_ms.current_mesh()
    have = _face_counter(m.vertex_matrix(), m.face_matrix())
    for key, c in _face_counter(v, t[~region]).items():
        assert have[key] >= c


def test_local_guard_rejects_moved_outside_faces():
    v, t = _sphere_with_hole()
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t.astype(np.int32)))
    topo = ms.apply_filter('get_topological_measures')
    saved = repair._umbrella_fair
    repair._umbrella_fair = lambda verts, tris, first, steps: verts * 1.01
    try:
        out_ms, _topo, rep = repair._local_remesh_tier(ml, ms, topo)
    finally:
        repair._umbrella_fair = saved
    assert not rep['adopted'] and not rep['outside_unchanged'], rep
    assert rep['reject_reason'] == 'faces outside the region changed'
    assert out_ms is ms


def test_local_scope_gate_skips_a_long_boundary_loop():
    v, t = _sphere_with_hole(0.8)
    assert repair.boundary_loop_stats(v, t)[1] > repair.LOCAL_MAX_LOOP_LEN
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t.astype(np.int32)))
    topo = ms.apply_filter('get_topological_measures')
    out_ms, _topo, rep = repair._local_remesh_tier(ml, ms, topo)
    assert not rep['adopted'] and out_ms is ms, rep
    assert rep['reject_reason'] == 'scope: boundary loop too long', rep


def test_faces_preserved_matches_the_counter_reference():
    rng = np.random.default_rng(1)
    v = rng.random((40, 3))
    for _ in range(100):
        t = rng.integers(0, 40, (25, 3))
        cand = np.vstack([t[rng.permutation(25)[:rng.integers(18, 26)]],
                          rng.integers(0, 40, (10, 3))])
        cand = np.roll(cand, int(rng.integers(0, 3)), axis=1)
        need = _face_counter(v, t)
        have = _face_counter(v, cand)
        ref = all(have[k] >= c for k, c in need.items())
        assert repair._faces_preserved(v, t, v, cand) == ref


def test_local_mode_through_the_ladder():
    v, t = _load(OPEN_SAMPLE)
    rep, _v, _t = _repair(v, t, deep_repair='local')
    dr = rep['deep_repair']
    assert dr['tiers_run'] == ['local'] and dr['ftetwild'] is None, dr
    lr = dr['local']
    assert lr['outside_unchanged'] or not lr['adopted'], lr
    assert dr['final_tier'] == ('local' if lr['adopted'] else 'stage1')
    assert rep['experimental_ftetwild'] is False


def test_mode_resolution():
    with tempfile.TemporaryDirectory(prefix='sutura-cfg-') as tmp:
        cfg = os.path.join(tmp, 'config.json')
        with open(cfg, 'w') as f:
            json.dump({'deep_repair': 'local'}, f)
        r = repair.resolve_deep_repair
        assert r(None, environ={}, config_path=os.path.join(tmp, 'none')) == 'full'
        assert r(None, environ={}, config_path=cfg) == 'local'
        assert r(None, environ={'SUTURA_DEEP_REPAIR': 'off'}, config_path=cfg) == 'off'
        assert r(None, environ={'SUTURA_DEEP_REPAIR': 'bogus'}, config_path=cfg) == 'local'
        assert r('full', environ={'SUTURA_DEEP_REPAIR': 'off'}, config_path=cfg) == 'full'
        f = repair.resolve_deep_repair_flags
        none = os.path.join(tmp, 'none')
        assert f(None, environ={}, config_path=none) == ('full', 'auto')
        assert f(None, no_fallback=True, environ={}, config_path=none) == ('off', False)
        assert f(None, experimental=True, environ={}, config_path=none) == ('full', True)
        assert f('local', environ={}, config_path=none) == ('local', False)
        assert f(None, environ={}, config_path=cfg) == ('local', False)


def test_cli_flag_and_human_line():
    py = sys.executable
    cli = os.path.join(SUTURA, 'repair.py')
    with tempfile.TemporaryDirectory(prefix='sutura-deepcli-') as tmp:
        bad = subprocess.run([py, cli, OPEN_SAMPLE, '--deep-repair', 'bogus'],
                             capture_output=True, text=True)
        assert bad.returncode != 0 and 'invalid choice' in bad.stderr
        out = subprocess.run([py, cli, OPEN_SAMPLE, '--deep-repair', 'off',
                              '--no-history', '--human',
                              '-o', os.path.join(tmp, 'o.stl')],
                             capture_output=True, text=True)
        assert out.returncode == 0, out.stderr
        assert 'Deep repair available' in out.stdout, out.stdout


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('deep-repair tests passed')
