#!/usr/bin/env python3
"""Regression tests for the deep-repair ladder (`repair.deep_repair_ladder`).

- 'full' produces exactly the pre-ladder output (the fTetWild block moved
  into the ladder unchanged), with and without a (fake) fTetWild tier.
- 'off' runs no tier and reports `deep_repair.available` (remaining holes /
  non-manifold edges, tiers, estimate) when something is left.
- 'local' never changes a face outside the deleted region, and its guard
  rejects a result that does.
- A wastefully dense fTetWild boundary is decimated (target ladder, strict
  watertight + Hausdorff-margin acceptance) or falls back to the undecimated
  result, below-the-ratio outputs are left alone.
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
    """Without fTetWild: fTetWild is not deterministic, so with the extra
    installed two real 'auto' runs can differ. The fTetWild path is covered
    with a fake bridge below."""
    saved = repair.ftetwild_available
    repair.ftetwild_available = lambda: False
    try:
        v, t = _load(OPEN_SAMPLE)
        _same_output(_repair(v, t, ftetwild='auto'),
                     _repair(v, t, ftetwild='auto', deep_repair='full'))
        _same_output(_repair(v, t, ftetwild=False),
                     _repair(v, t, ftetwild=False, deep_repair='full'))
    finally:
        repair.ftetwild_available = saved


def test_full_equals_pre_ladder_with_fake_ftetwild():
    """The moved fTetWild block adopts a (fake) closed boundary exactly as
    before: same arrays and report with and without the ladder mode."""
    def fake_run(inter, out_obj, params=None, timeout=None):
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


def test_ftetwild_shape_change_is_flagged_not_rejected():
    """A closed fTetWild boundary far from the input (a unit cube for
    thingi10k_100827) is adopted, flagged shape_changed and reported with
    the issue code 'shape_changed' (the category is not downgraded)."""
    import classification

    def fake_run(inter, out_obj, params=None, timeout=None):
        repair.write_obj(out_obj, np.asarray(CUBE_V, np.float32),
                         np.asarray(CUBE_T, np.int32))
        return {'ok': True, 'output_faces': 12}, True
    saved = repair.ftetwild_available, repair.run_ftetwild
    repair.ftetwild_available, repair.run_ftetwild = (lambda: True), fake_run
    try:
        v, t = _load(OPEN_SAMPLE)
        rep, _ov, ot = _repair(v, t, ftetwild='auto', deep_repair='full')
    finally:
        repair.ftetwild_available, repair.run_ftetwild = saved
    ft = rep['experimental_ftetwild']
    assert ft['ran'] and ft['adopted'] and ft['shape_changed'], ft
    assert ft['hausdorff_rel'] > repair.FTETWILD_MAX_HAUSDORFF_REL, ft
    assert 'reject_reason' not in ft, ft
    assert rep['shape_changed'] is True and len(ot) == 12
    assert rep['deep_repair']['final_tier'] == 'ftetwild'
    _cat, issues, _key = classification.classify(rep)
    assert 'shape_changed' in issues, issues


def _cube_with_fin():
    """Closed cube plus one extra face on an existing edge: a boundary that
    has a non-manifold edge (fails the holes/nm guard on a nm-free mesh)."""
    v = np.asarray(CUBE_V + [(0.5, -1.0, 0.5)], np.float32)
    t = np.asarray(CUBE_T + [(0, 1, 8)], np.int32)
    return v, t


def _run_with_fake_ftetwild(fake_run, **patch):
    """Repair thingi10k_100827 with a faked fTetWild bridge and no stage-2
    post-process (so a fake boundary is judged as returned). Intensity knobs
    are passed as a Balanced spec override (dense_min_faces / ftetwild_timeout
    / ftetwild_max_faces / dense_target_ladder); other keys patch repair."""
    import dataclasses
    spec_over = {}
    for key in ('dense_min_faces', 'ftetwild_timeout', 'ftetwild_max_faces',
                'dense_target_ladder', 'ftetwild_hausdorff_samples'):
        if key in patch:
            spec_over[key] = patch.pop(key)
    spec = dataclasses.replace(repair.triage.PRESETS['balanced'], **spec_over)
    names = ['ftetwild_available', 'run_ftetwild', 'run_stage2'] + list(patch)
    saved = {n: getattr(repair, n) for n in names}
    repair.ftetwild_available = lambda: True
    repair.run_ftetwild = fake_run
    repair.run_stage2 = lambda inter, out: ({'error': 'disabled in test'}, False)
    for n, val in patch.items():
        setattr(repair, n, val)
    try:
        v, t = _load(OPEN_SAMPLE)
        return _repair(v, t, ftetwild='auto', deep_repair='full',
                       triage_spec=spec)[0]
    finally:
        for n, val in saved.items():
            setattr(repair, n, val)


def test_ftetwild_retries_with_the_optimisation_on():
    calls = []

    def fake_run(inter, out_obj, params=None, timeout=None):
        calls.append((params, timeout))
        cv, ct = (_cube_with_fin() if params is None
                  else (np.asarray(CUBE_V, np.float32), np.asarray(CUBE_T, np.int32)))
        repair.write_obj(out_obj, cv, ct)
        return {'ok': True, 'output_faces': len(ct), 'time': 0.1}, True
    rep = _run_with_fake_ftetwild(fake_run)
    ft = rep['experimental_ftetwild']
    assert [c[0] for c in calls] == [None, repair.FTETWILD_RETRY_PARAMS], calls
    assert 0 < calls[1][1] <= repair.FTETWILD_TIMEOUT, calls
    assert ft['adopted'] and ft['adopted_attempt'] == 'optimize', ft
    assert [a['attempt'] for a in ft['attempts']] == ['default', 'optimize']
    assert ft['attempts'][0]['reject_reason'] == 'holes_nm'
    assert 'reject_reason' not in ft and ft['output_non_manifold'] == 0, ft


def test_ftetwild_no_retry_after_success_or_timeout():
    calls = []

    def ok_run(inter, out_obj, params=None, timeout=None):
        calls.append(params)
        repair.write_obj(out_obj, np.asarray(CUBE_V, np.float32),
                         np.asarray(CUBE_T, np.int32))
        return {'ok': True, 'output_faces': 12}, True
    ft = _run_with_fake_ftetwild(ok_run)['experimental_ftetwild']
    assert calls == [None] and ft['adopted_attempt'] == 'default', (calls, ft)

    calls.clear()

    def timeout_run(inter, out_obj, params=None, timeout=None):
        calls.append(params)
        return {'error': 'timeout'}, False
    ft = _run_with_fake_ftetwild(timeout_run)['experimental_ftetwild']
    assert calls == [None] and not ft['adopted'] and ft['error'] == 'timeout', ft


def test_ftetwild_retry_needs_budget():
    calls = []

    def fake_run(inter, out_obj, params=None, timeout=None):
        calls.append(params)
        cv, ct = _cube_with_fin()
        repair.write_obj(out_obj, cv, ct)
        return {'ok': True, 'output_faces': len(ct)}, True
    ft = _run_with_fake_ftetwild(fake_run, ftetwild_timeout=5)['experimental_ftetwild']
    assert calls == [None], calls
    assert not ft['adopted'] and ft['reject_reason'] == 'holes_nm', ft
    assert ft['attempts'][0]['retry_skipped'] == 'budget', ft


def test_ftetwild_skipped_for_large_inputs():
    calls = []

    def fake_run(inter, out_obj, params=None, timeout=None):
        calls.append(params)
        return {'error': 'must not run'}, False
    rep = _run_with_fake_ftetwild(fake_run, ftetwild_max_faces=10)
    ft = rep['experimental_ftetwild']
    assert calls == [], calls
    assert not ft['ran'] and ft['reject_reason'] == 'too_large', ft
    assert ft['input_faces'] > ft['max_faces'] == 10, ft
    assert rep['deep_repair']['tiers_run'] == []
    # the off-mode offer does not list a tier that would be skipped
    import dataclasses
    off_spec = dataclasses.replace(repair.triage.PRESETS['balanced'],
                                   ftetwild_max_faces=10)
    saved = repair.ftetwild_available
    repair.ftetwild_available = lambda: True
    try:
        v, t = _load(OPEN_SAMPLE)
        off = _repair(v, t, deep_repair='off', triage_spec=off_spec)[0]
    finally:
        repair.ftetwild_available = saved
    assert off['deep_repair']['available']['tiers'] == ['local'], off['deep_repair']


def _dense_sphere(subdiv=4):
    """A closed, dense fake fTetWild boundary (5,120 faces at subdiv=4)."""
    ms = ml.MeshSet()
    ms.create_sphere(subdiv=subdiv)
    m = ms.current_mesh()
    return (np.asarray(m.vertex_matrix(), np.float64),
            np.asarray(m.face_matrix(), np.int64))


def _fake_hd(raw_value=0.02, dec_value=0.02):
    """Deterministic stand-in for ``repair._hausdorff_rel``: a raw/undecimated
    boundary (many faces) reports ``raw_value``, a decimated candidate reports
    ``dec_value``. Returns ``(max, mean)`` like the real function."""
    def _hd(_ml, _in_v, _in_t, _out_v, out_t, samples=None):
        return ((raw_value if len(out_t) > 1000 else dec_value), 0.0)
    return _hd


def test_dense_ftetwild_output_is_decimated():
    """A fake dense boundary triggers the decimation ladder: decimation is
    recorded, the final face count is inside the chosen target, the adopted
    result is strictly watertight and both Hausdorff measurements are kept."""
    v, t = _dense_sphere(4)
    raw_faces = len(t)

    def fake_run(inter, out_obj, params=None, timeout=None):
        repair.write_obj(out_obj, v, t)
        return {'ok': True, 'output_faces': raw_faces}, True
    rep = _run_with_fake_ftetwild(fake_run, dense_min_faces=100,
                                  _hausdorff_rel=_fake_hd(0.02, 0.02))
    ft = rep['experimental_ftetwild']
    assert ft['adopted'] and ft['ftetwild_decimated'], ft
    assert ft['ftetwild_faces_raw'] == raw_faces, ft
    assert ft['ftetwild_decimate_target'] == max(int(round(1.5 * 71)), 100), ft
    assert ft['ftetwild_faces_final'] <= ft['ftetwild_decimate_target'], ft
    assert ft['ftetwild_hausdorff_raw'] == 0.02, ft
    assert ft['ftetwild_hausdorff_decimated'] == 0.02, ft
    assert rep['stage1']['holes_remaining'] == 0, rep['stage1']
    assert rep['stage1']['non_manifold_edges_remaining'] == 0, rep['stage1']


def test_ftetwild_output_below_the_ratio_is_not_decimated():
    v = np.asarray(CUBE_V, np.float32)
    t = np.asarray(CUBE_T, np.int32)

    def fake_run(inter, out_obj, params=None, timeout=None):
        repair.write_obj(out_obj, v, t)
        return {'ok': True, 'output_faces': len(t)}, True
    rep = _run_with_fake_ftetwild(fake_run, dense_min_faces=100)
    ft = rep['experimental_ftetwild']
    assert ft['adopted'] and not ft['ftetwild_decimated'], ft
    assert ft['ftetwild_faces_raw'] == ft['ftetwild_faces_final'] == len(t), ft
    assert 'ftetwild_decimate_target' not in ft, ft


def test_decimation_failure_falls_back_to_the_undecimated_result():
    v, t = _dense_sphere(4)
    raw_faces = len(t)

    def fake_run(inter, out_obj, params=None, timeout=None):
        repair.write_obj(out_obj, v, t)
        return {'ok': True, 'output_faces': raw_faces}, True

    def failing_decimate(_ml, _cv, _ct, _target):
        return None
    rep = _run_with_fake_ftetwild(fake_run, dense_min_faces=100,
                                  _decimate_boundary=failing_decimate,
                                  _hausdorff_rel=_fake_hd(0.02, 0.02))
    ft = rep['experimental_ftetwild']
    assert ft['adopted'] and not ft['ftetwild_decimated'], ft
    assert ft['ftetwild_faces_final'] == raw_faces, ft
    assert ft['ftetwild_decimate_fallback'] == 'decimation_failed', ft
    assert rep['stage1']['holes_remaining'] == 0, rep['stage1']


def test_decimation_worse_than_raw_beyond_the_margin_is_rejected():
    """The ladder must not make the shape measurably worse than the raw
    fTetWild boundary: when every target exceeds
    ``hd_raw + DECIMATE_HD_MARGIN`` it falls through to the undecimated
    boundary, which passes the holes/non-manifold guard as before."""
    v, t = _dense_sphere(4)
    raw_faces = len(t)

    def fake_run(inter, out_obj, params=None, timeout=None):
        repair.write_obj(out_obj, v, t)
        return {'ok': True, 'output_faces': raw_faces}, True
    # raw 0.02, decimated 0.05 > 0.02 + DECIMATE_HD_MARGIN -> both rejected
    rep = _run_with_fake_ftetwild(fake_run, dense_min_faces=100,
                                  _hausdorff_rel=_fake_hd(0.02, 0.05))
    ft = rep['experimental_ftetwild']
    assert ft['adopted'] and not ft['ftetwild_decimated'], ft
    assert ft['ftetwild_faces_final'] == raw_faces, ft
    assert ft['ftetwild_decimate_fallback'] == 'hausdorff', ft
    assert len(ft['ftetwild_decimate_attempts']) == 2, ft
    assert [a.get('reason') for a in ft['ftetwild_decimate_attempts']] == \
        ['hausdorff', 'hausdorff'], ft
    assert rep['stage1']['holes_remaining'] == 0, rep['stage1']


def test_decimation_crossing_the_shape_threshold_is_rejected():
    """thingi10k_78968: the raw boundary is inside FTETWILD_MAX_HAUSDORFF_REL
    (0.0087 <= 0.01) but a decimated candidate would cross it (0.0126), even
    though it stays inside hd_raw + DECIMATE_HD_MARGIN. It must be rejected,
    so decimation never turns an unflagged fTetWild result into a flagged one;
    the undecimated boundary (0.0087, unflagged) is adopted instead."""
    v, t = _dense_sphere(4)
    raw_faces = len(t)
    raw, dec = 0.0087, 0.0126
    assert raw <= repair.FTETWILD_MAX_HAUSDORFF_REL < dec, (raw, dec)
    assert dec <= raw + repair.DECIMATE_HD_MARGIN, (raw, dec)

    def fake_run(inter, out_obj, params=None, timeout=None):
        repair.write_obj(out_obj, v, t)
        return {'ok': True, 'output_faces': raw_faces}, True
    rep = _run_with_fake_ftetwild(fake_run, dense_min_faces=100,
                                  _hausdorff_rel=_fake_hd(raw, dec))
    ft = rep['experimental_ftetwild']
    assert ft['adopted'] and not ft['ftetwild_decimated'], ft
    assert ft['ftetwild_faces_final'] == raw_faces, ft
    assert ft['ftetwild_decimate_fallback'] == 'hausdorff', ft
    attempts = ft['ftetwild_decimate_attempts']
    assert len(attempts) == 2, ft
    assert [a.get('reason') for a in attempts] == ['hausdorff', 'hausdorff'], ft
    assert ft['shape_changed'] is False, ft
    assert rep['stage1']['holes_remaining'] == 0, rep['stage1']


def test_hausdorff_rel_is_not_clipped_above_five_percent():
    """The filter's maxdist cap must never clip a real distance: a true
    one-sided Hausdorff distance clearly above 5 % of the diagonal has to be
    reported as such. A 5 %-of-diagonal clip would hide how bad a result is
    and make the raw/decimated comparison meaningless above 5 %. The sample
    below straddles the 1 % FTETWILD_MAX_HAUSDORFF_REL guard as well."""
    v = np.asarray(CUBE_V, np.float64)
    t = np.asarray(CUBE_T, np.int64)
    diag = float(np.linalg.norm(v.max(0) - v.min(0)))
    hd = None
    for rel in (0.008, 0.02, 0.06, 0.3):
        ov = v.copy()
        ov[:, 0] += rel * diag
        hd, _mean = repair._hausdorff_rel(ml, v, t, ov, t)
        assert abs(hd - rel) < 1e-4, (rel, hd)
    assert hd > 0.05, hd


def test_hausdorff_rel_is_zero_for_identical_meshes():
    v, t = _sphere_with_hole()
    hd_max, hd_mean = repair._hausdorff_rel(ml, v, t, v, t)
    assert hd_max is not None and hd_max < 1e-6 and hd_mean < 1e-6
    assert repair._hausdorff_rel(ml, v, t, v, t[:0]) == (None, None)
    # unreferenced vertices (the fTetWild bridge writes interior tet
    # vertices too) must not be sampled
    ms = ml.MeshSet()
    ms.create_sphere(subdiv=3)
    m = ms.current_mesh()
    sv, st = np.asarray(m.vertex_matrix()), np.asarray(m.face_matrix())
    inner = np.random.default_rng(0).random((200, 3)) * 0.6 - 0.3
    hd_max, _mean = repair._hausdorff_rel(ml, sv, st, np.vstack([sv, inner]), st)
    assert hd_max < 1e-6, hd_max


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
