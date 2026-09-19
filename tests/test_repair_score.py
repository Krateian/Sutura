#!/usr/bin/env python3
"""Regression test for sutura/repair_score.py (Repair Health / Repair Risk).

Checks that:
  1. repair_score.py is stdlib-only (no numpy/pymeshlab/manifold3d).
  2. The shipped config loads and normalizes (health weights sum to 100; the
     status lookup table has all 9 tier combinations).
  3. Health: a clean watertight result is 100; a mesh with remaining holes /
     non-manifold / self-intersections scores lower; missing metrics are
     skipped and their weight redistributed.
  4. Risk: small deltas score low, aggressive deltas + component loss score
     high.
  5. Status: all 9 (health-tier, risk-tier) combinations resolve to the
     correct status code via the config lookup table.
  6. Fail-silent: a malformed report (wrong types, NaN, missing fields)
     never raises and yields 'unavailable'.
  7. The 'caution' tier (mid health + high risk) is distinct from both
     'review' outcomes — the two axes do not collapse.

Usage: python3 tests/test_repair_score.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import repair_score as rs  # noqa: E402


def _cfg():
    return rs.load_config()


def _clean_report():
    return {
        'stage1': {
            'two_manifold': True,
            'holes_remaining': 0,
            'non_manifold_edges_remaining': 0,
            'self_intersections_remaining': 0,
            'faces_before': 1000, 'faces_after': 1020,
            'vertices_before': 600, 'vertices_after': 610,
            'components_before': 1, 'components': 1,
            'volume_change_percent': 1.2,
        },
        'stage2': {'ok': True},
    }


def test_stdlib_only():
    import subprocess
    code = (
        "import sys; sys.path.insert(0, %r); import repair_score; "
        "bad=[m for m in ('numpy','pymeshlab','manifold3d','trimesh','scipy') "
        "if m in sys.modules]; "
        "print('OK' if not bad else 'BAD:'+','.join(bad))" % SUTURA
    )
    out = subprocess.run([sys.executable, '-c', code],
                         capture_output=True, text=True)
    assert out.returncode == 0, out.stderr
    assert 'OK' in out.stdout, out.stdout + out.stderr


def test_config_loads_and_normalizes():
    cfg = _cfg()
    hw = cfg['health']['weights']
    assert sum(hw.values()) == 100, hw
    assert set(hw) == {'watertight', 'no_non_manifold',
                       'no_self_intersections', 'no_holes'}, hw
    # orientation factor must NOT exist (it was removed by design)
    assert 'orientation_consistent' not in hw
    # status table has all 9 tier combinations
    table = cfg['status']['table']
    tiers = [t for t in table if t != 'unavailable']
    assert len(tiers) == 9, table
    assert 'mid_high' in table and table['mid_high'] == 'caution'
    assert table['high_low'] == 'safe'
    assert table['low_high'] == 'failed'


def test_clean_report_high_health_low_risk():
    cfg = _cfg()
    r = rs.compute_scores(_clean_report(), cfg)
    assert r['health'] == 100, r
    assert r['health_factors'].get('watertight') == {'ok': True, 'weight': 39.0}, r
    assert r['risk'] < 40, r
    assert r['status_code'] == 'safe', r


def test_health_penalizes_each_defect():
    cfg = _cfg()
    base = _clean_report()

    # holes remaining: breaks BOTH watertight (39) and no_holes (17)
    rep = dict(base)
    rep['stage1'] = dict(base['stage1'], holes_remaining=3)
    r = rs.compute_scores(rep, cfg)
    assert r['health'] == 100 - 39 - 17, r  # nm + si still ok -> 44

    # non-manifold edges remaining: breaks only no_non_manifold (22)
    rep = dict(base)
    rep['stage1'] = dict(base['stage1'], non_manifold_edges_remaining=2)
    r = rs.compute_scores(rep, cfg)
    assert r['health'] == 100 - 22, r

    # self-intersections remaining: breaks only no_self_intersections (22)
    rep = dict(base)
    rep['stage1'] = dict(base['stage1'], self_intersections_remaining=4)
    r = rs.compute_scores(rep, cfg)
    assert r['health'] == 100 - 22, r

    # stage 2 skipped -> not watertight even if stage 1 closed it (39 off)
    rep = dict(base)
    rep['stage2'] = {'error': 'Stage 2 skipped: manifold3d not available.'}
    r = rs.compute_scores(rep, cfg)
    assert r['health'] == 100 - 39, r
    assert r['status_code'] == 'review' or r['status_code'] == 'caution', r


def test_health_missing_metric_redistributes():
    cfg = _cfg()
    base = _clean_report()
    # drop self_intersections_remaining -> weight redistributed to others
    rep = dict(base)
    s1 = dict(base['stage1'])
    del s1['self_intersections_remaining']
    rep['stage1'] = s1
    r = rs.compute_scores(rep, cfg)
    # watertight(39)+nm(22)+holes(17) = 78/78 = 100
    assert r['health'] == 100, r
    assert 'no_self_intersections' not in r['health_factors'], r


def test_risk_small_delta_low():
    cfg = _cfg()
    r = rs.compute_scores(_clean_report(), cfg)
    assert r['risk'] < 40, r  # 2% faces, 1.7% verts, 0 comp change, 1.2% vol


def test_risk_aggressive_delta_and_component_loss_high():
    cfg = _cfg()
    base = _clean_report()
    rep = dict(base)
    rep['stage1'] = dict(base['stage1'],
                         faces_before=1000, faces_after=200,
                         vertices_before=600, vertices_after=150,
                         components_before=4, components=1,
                         volume_change_percent=40.0)
    r = rs.compute_scores(rep, cfg)
    assert r['risk'] >= 40, r


def test_status_all_nine_combinations():
    cfg = _cfg()
    table = cfg['status']['table']
    ht = {'high': 80, 'mid': 60}
    rt = {'high': 70, 'mid': 40}
    # representative scores per tier
    h = {'high': 90, 'mid': 70, 'low': 30}
    rk = {'low': 10, 'mid': 50, 'high': 90}
    for hname, hval in h.items():
        for rname, rval in rk.items():
            rep = _clean_report()
            # force health by removing factors until score matches tier
            s = rs.compute_scores(rep, cfg)
            # simpler: drive via a synthetic report through _health/_risk
            # is private; instead build a report that yields the tiers by
            # constructing the expected code directly from the table and
            # checking _tier/_status agree.
            from repair_score import _tier, _status
            th = _tier(hval, ht)
            tr = _tier(rval, rt)
            assert th == hname, (hval, th, hname)
            assert tr == rname, (rval, tr, rname)
            code, label = _status(hval, rval, cfg)
            expected = table['%s_%s' % (hname, rname)]
            assert code == expected, (hname, rname, code, expected)
            assert label == cfg['status']['labels'][expected], (code, label)


def test_status_axes_do_not_collapse():
    cfg = _cfg()
    # high health + low risk -> safe; mid health + high risk -> caution
    r1 = rs.compute_scores(
        {'stage1': {'two_manifold': True, 'holes_remaining': 0,
                    'non_manifold_edges_remaining': 0,
                    'self_intersections_remaining': 0,
                    'faces_before': 1000, 'faces_after': 1010,
                    'vertices_before': 600, 'vertices_after': 605,
                    'components_before': 1, 'components': 1,
                    'volume_change_percent': 1.0},
         'stage2': {'ok': True}}, cfg)
    r2 = rs.compute_scores(
        {'stage1': {'two_manifold': True, 'holes_remaining': 0,
                    'non_manifold_edges_remaining': 0,
                    'self_intersections_remaining': 0,
                    'faces_before': 1000, 'faces_after': 100,
                    'vertices_before': 600, 'vertices_after': 60,
                    'components_before': 5, 'components': 1,
                    'volume_change_percent': 50.0},
         # stage 2 never confirmed -> not watertight -> health 61 (mid)
         'stage2': {'error': 'Stage 2 skipped: manifold3d not available.'}}, cfg)
    assert r1['status_code'] == 'safe', r1
    assert r2['status_code'] == 'caution', r2
    assert r1['status_code'] != r2['status_code']


def test_fail_silent_malformed_report():
    cfg = _cfg()
    # wrong types / NaN / missing fields -> no exception, unavailable
    cases = [
        {'stage1': {'holes_remaining': 'oops'}},
        {'stage1': {'faces_before': float('nan'), 'faces_after': 100}},
        {},
        {'stage1': None},
        None,
    ]
    for rep in cases:
        r = rs.compute_scores(rep, cfg)
        assert r['health'] is None and r['risk'] is None, (rep, r)
        assert r['status_code'] == 'unavailable', (rep, r)


def test_config_corrupt_file_falls_back():
    import json
    import tempfile
    with tempfile.NamedTemporaryFile('w', suffix='.json', delete=False) as f:
        f.write('{ not valid json')
        path = f.name
    try:
        cfg = rs.load_config(path)
        assert cfg['health']['weights']['watertight'] == 39, cfg
    finally:
        os.unlink(path)


if __name__ == '__main__':
    fns = [v for k, v in sorted(globals().items()) if k.startswith('test_')]
    for fn in fns:
        fn()
        print('ok  %s' % fn.__name__)
    print('repair_score tests passed')