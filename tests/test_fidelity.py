#!/usr/bin/env python3
"""Regression tests for sutura/fidelity.py (Repair Fidelity).

Run with a Python that has numpy + sutura_geom:
  PYTHONPATH=sutura python tests/test_fidelity.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, 'sutura'))

import numpy as np  # noqa: E402
import fidelity  # noqa: E402

_REPORT = {
    'stage1': {
        'two_manifold': True, 'holes_remaining': 0,
        'non_manifold_edges_remaining': 0,
        'self_intersections_remaining': 0,
    },
    'stage2': {'ok': True},
}


def _box(size=10.0, n=4, open_top=False, hole=False):
    s = float(size) / 2.0
    verts, tris = [], []
    faces = [
        ((s, 0, 0), (0, 1, 0), (0, 0, 1)),
        ((-s, 0, 0), (0, 1, 0), (0, 0, 1)),
        ((0, s, 0), (1, 0, 0), (0, 0, 1)),
        ((0, -s, 0), (1, 0, 0), (0, 0, 1)),
        ((0, 0, s), (1, 0, 0), (0, 1, 0)),
        ((0, 0, -s), (1, 0, 0), (0, 1, 0)),
    ]
    for origin, u, v in faces:
        o = np.asarray(origin, float)
        u = np.asarray(u, float) * size
        v = np.asarray(v, float) * size
        base = len(verts)
        for i in range(n + 1):
            for j in range(n + 1):
                verts.append(o + u * (i / n - 0.5) + v * (j / n - 0.5))
        for i in range(n):
            for j in range(n):
                a = base + i * (n + 1) + j
                b, c = a + 1, a + (n + 1)
                d = c + 1
                tris.append((a, b, d))
                tris.append((a, d, c))
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    flat = verts.reshape(-1, 3)
    uniq, inv = np.unique(np.round(flat, 12), axis=0, return_inverse=True)
    tris = inv[tris].reshape(-1, 3).astype(np.int64)
    if open_top:
        top = np.all(np.isclose(uniq[tris][:, :, 2], s), axis=1)
        tris = tris[~top]
    if hole:
        top = np.all(np.isclose(uniq[tris][:, :, 2], s), axis=1)
        cen = uniq[tris].mean(axis=1)
        keep = ~(top & (np.abs(cen[:, 0]) < s * 0.30)
                 & (np.abs(cen[:, 1]) < s * 0.30))
        tris = tris[keep]
    a, b, c = uniq[tris[:, 0]], uniq[tris[:, 1]], uniq[tris[:, 2]]
    nrm = np.cross(b - a, c - a)
    centre = uniq.mean(axis=0)
    outward = np.einsum('ij,ij->i', nrm, (a + b + c) / 3.0 - centre) > 0.0
    tris[~outward] = tris[~outward][:, [0, 2, 1]]
    return uniq.astype(np.float64), tris.astype(np.int64)


def _sphere(radius=5.0, nu=32, nv=16):
    """Smooth welded UV sphere (no sharp edges above 30 degrees)."""
    verts = [(0.0, 0.0, radius)]
    for j in range(1, nv):
        phi = np.pi * j / nv
        for i in range(nu):
            th = 2.0 * np.pi * i / nu
            verts.append((radius * np.sin(phi) * np.cos(th),
                          radius * np.sin(phi) * np.sin(th),
                          radius * np.cos(phi)))
    verts.append((0.0, 0.0, -radius))
    tris = []
    for i in range(nu):
        tris.append((0, 1 + i, 1 + (i + 1) % nu))
    for j in range(nv - 2):
        ring0 = 1 + j * nu
        ring1 = 1 + (j + 1) * nu
        for i in range(nu):
            a = ring0 + i
            b = ring0 + (i + 1) % nu
            c = ring1 + i
            d = ring1 + (i + 1) % nu
            tris.append((a, c, b))
            tris.append((b, c, d))
    last = len(verts) - 1
    ring = 1 + (nv - 2) * nu
    for i in range(nu):
        tris.append((last, ring + (i + 1) % nu, ring + i))
    return (np.asarray(verts, dtype=np.float64),
            np.asarray(tris, dtype=np.int64))


def test_identity_is_100():
    v, t = _box()
    r = fidelity.compute((v, t), (v, t), _REPORT)
    assert r['repair_fidelity'] == 100, r
    assert r['repair_verdict_code'] == 'preserved', r
    assert r['repair_verdict_low_confidence'] is False
    assert all(f['penalty'] == 0.0 for f in r['repair_fidelity_factors'].values())


def test_scaled_shape_scores_lower():
    v, t = _box()
    r = fidelity.compute((v, t), (v * 1.01, t), _REPORT)
    assert r['repair_fidelity'] is not None
    assert r['repair_fidelity'] == 33, r
    # A 1 % size change stays within the calibrated 1 %-of-diagonal gate: the
    # score drops but the verdict remains preserved (no cry-wolf).
    assert r['repair_verdict_code'] == 'preserved', r
    assert r['repair_verdict_areas'] == 0, r
    assert r['repair_fidelity_factors']['healthy_distance']['penalty'] > 0


def test_scaled_shape_above_gate_is_changed():
    v, t = _box()
    r = fidelity.compute((v, t), (v * 1.06, t), _REPORT)
    assert r['repair_fidelity'] == 0, r
    assert r['repair_verdict_code'] == 'low_confidence', r
    assert r['repair_verdict_areas'] == 1, r
    assert r['repair_fidelity_metrics']['healthy_distance_max_pct'] > 1.0, r


def test_small_hole_keeps_high_fidelity():
    v, t = _box(hole=True)
    vf, tf = _box()
    r = fidelity.compute((v, t), (vf, tf), _REPORT)
    assert r['repair_fidelity'] == 98, r
    assert r['repair_verdict_code'] == 'preserved', r
    assert r['repair_verdict_areas'] == 2, r
    assert r['repair_verdict_low_confidence'] is False, r


def test_big_hole_is_low_confidence():
    v, t = _box(open_top=True)
    vf, tf = _box()
    r = fidelity.compute((v, t), (vf, tf), _REPORT)
    assert r['repair_fidelity'] == 90, r
    assert r['repair_verdict_low_confidence'] is True, r
    assert r['repair_verdict_code'] == 'low_confidence', r
    assert r['repair_verdict_areas'] == 1, r
    assert 'invented_area' in r['repair_verdict_reasons']


def test_smooth_mesh_drops_recall_factor():
    v, t = _sphere()
    r = fidelity.compute((v, t), (v, t), _REPORT)
    assert r['repair_fidelity'] == 100, r
    assert 'feature_recall' not in r['repair_fidelity_factors'], r


def test_deterministic():
    v, t = _box(hole=True)
    vf, tf = _box()
    a = fidelity.compute((v, t), (vf, tf), _REPORT)
    b = fidelity.compute((v, t), (vf, tf), _REPORT)
    assert a == b, (a, b)


def test_fail_silent_on_bad_input():
    r = fidelity.compute(([], []), ([], []), _REPORT)
    assert r['repair_fidelity'] is None
    assert r['repair_verdict_code'] == 'unavailable'
    r = fidelity.compute('bad', 'bad', _REPORT)
    assert r['repair_fidelity'] is None


def test_unavailable_never_reports_area_count():
    cfg = fidelity.load_config()
    # No measurement: the verdict is 'unavailable' and carries no area count,
    # even when changed areas were detected (regression: 99 areas next to
    # 'unavailable').
    code, areas = fidelity._verdict(None, _REPORT, False, [], 99, cfg,
                                    max_pct=None)
    assert (code, areas) == ('unavailable', 0), (code, areas)
    # A real low-confidence guess still reports its areas.
    code, areas = fidelity._verdict(None, _REPORT, True, ['invented_area'], 3,
                                    cfg, max_pct=None)
    assert (code, areas) == ('low_confidence', 3), (code, areas)


def test_parity_with_verify_surface_metrics():
    """The lean metric must agree with the verification definition on identity."""
    sys.path.insert(0, os.path.join(REPO, '.claude', 'skills', 'sutura-verify',
                                    'scripts'))
    try:
        import surface_metrics as sm
    except Exception:
        print('surface_metrics unavailable; skipping parity')
        return
    v, t = _box()
    from defects import detect
    vo = v + np.array([0.02, 0.0, 0.0])
    m = sm.measure(v, t, vo, t, detect(v, t, with_indices=True))
    r = fidelity.compute((v, t), (vo, t), _REPORT)
    assert abs(m['feature_recall'] - r['repair_fidelity_metrics'][
        'feature_recall']) < 1e-6, (m, r)
    # The lean fill uses a smaller lattice; require agreement within 1 mm3
    # (surface_metrics reports 0.6 mm3 on this shift, the lean pass 0.0).
    assert abs((m['healthy_fill_mm3'] or 0.0)) < 1.0, m
    assert abs((m['healthy_removed_mm3'] or 0.0)) < 1.0, m


def test_aggregate_worst_object():
    good = {'repair_fidelity': 95, 'repair_verdict_code': 'preserved',
            'repair_verdict_areas': 0, 'repair_verdict_low_confidence': False,
            'repair_verdict_reasons': [], 'repair_fidelity_metrics': {
                'changed_areas': 0, 'low_confidence_areas': 0}}
    bad = {'repair_fidelity': 40, 'repair_verdict_code': 'low_confidence',
           'repair_verdict_areas': 2, 'repair_verdict_low_confidence': True,
           'repair_verdict_reasons': ['invented_area'],
           'repair_fidelity_metrics': {'changed_areas': 2,
                                       'low_confidence_areas': 2}}
    agg = fidelity._aggregate([good, bad])
    assert agg['repair_fidelity'] == 40, agg
    assert agg['repair_verdict_code'] == 'low_confidence', agg
    assert agg['repair_verdict_areas'] == 2, agg
    assert agg['repair_verdict_low_confidence'] is True, agg
    # A 0 score is a real worst value, not a missing one (regression: ``or
    # 100`` used to skip it and report a healthier object's verdict).
    zero = {'repair_fidelity': 0, 'repair_verdict_code': 'low_confidence',
            'repair_verdict_areas': 1, 'repair_verdict_low_confidence': True,
            'repair_verdict_reasons': ['invented_area'],
            'repair_fidelity_metrics': {'changed_areas': 1,
                                        'low_confidence_areas': 1}}
    agg = fidelity._aggregate([good, zero, bad])
    assert agg['repair_fidelity'] == 0, agg
    assert agg['repair_verdict_code'] == 'low_confidence', agg
    assert agg['repair_fidelity_metrics']['worst_object'] == 1, agg


def test_fully_defective_input_is_unavailable():
    vi = np.array([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]])
    ti = np.array([[0, 1, 2]])
    vo, to = _box()
    r = fidelity.compute((vi, ti), (vo, to), _REPORT)
    assert r['repair_fidelity'] is None, r
    # There is no healthy baseline, but the invented/new surface is a guess:
    # low_confidence is the honest headline, not a fabricated score.
    assert r['repair_verdict_code'] == 'low_confidence', r
    assert r['repair_verdict_low_confidence'] is True, r


def test_invalid_closest_indices_are_dropped():
    v, t = _box()
    geom = fidelity.sutura_geom
    orig = geom.closest_points
    def fake(vi, ti, pts):
        return pts, np.zeros(len(pts)), np.full(len(pts), -1)
    geom.closest_points = fake
    try:
        r = fidelity.compute((v, t), (v, t), _REPORT)
    finally:
        geom.closest_points = orig
    assert r['repair_fidelity'] is None, r
    assert r['repair_verdict_code'] == 'unavailable', r
    assert r['repair_verdict_areas'] == 0, r


def test_area_samples_are_not_capped_by_face_count():
    v, t = _box()
    pts, _n, _w, idx = fidelity._area_samples(v, t, 4000)
    assert len(pts) == 4000 and len(idx) == 4000
    mask = np.zeros(len(t), dtype=bool)
    mask[::2] = True
    pts2, _n2, _w2, idx2 = fidelity._area_samples(v, t, 4000, face_mask=mask)
    assert len(pts2) == 4000 and len(idx2) == 4000
    assert not mask[idx2].any()


def test_large_mesh_skips_healthy_mask():
    v, t = _box()
    cfg = fidelity.load_config({'fidelity': {'budgets': {'max_faces': 1}}})
    r = fidelity.compute((v, t), (v, t), _REPORT, config=cfg)
    assert r['repair_fidelity'] is None, r
    assert 'healthy mask skipped (large mesh)' in (r['repair_fidelity_note'] or ''), r


def test_fill_basis_is_translation_invariant():
    v, t = _box()
    vo = v + np.array([0.2, 0.0, 0.0])
    mask = np.zeros(len(t), dtype=bool)
    b = fidelity._DEFAULTS['fidelity']['budgets']
    tol = fidelity._detail_tol(v)
    a = fidelity._healthy_fill(v, t, vo, t, mask, tol, b, 600, 6)
    sh = np.array([100.0, 100.0, 100.0])
    c = fidelity._healthy_fill(v + sh, t, vo + sh, t, mask, tol, b, 600, 6)
    assert a.get('fill_fraction') is not None, a
    assert abs(a['fill_fraction'] - c['fill_fraction']) < 1e-9, (a, c)
    assert abs(a['removed_fraction'] - c['removed_fraction']) < 1e-9, (a, c)


def test_aggregate_semantic_verdict_and_low_confidence():
    failed = {'repair_fidelity': 90, 'repair_verdict_code': 'not_repaired',
              'repair_verdict_areas': 0, 'repair_verdict_low_confidence': False,
              'repair_verdict_reasons': [],
              'repair_fidelity_metrics': {'changed_areas': 0,
                                          'low_confidence_areas': 0}}
    good = {'repair_fidelity': 70, 'repair_verdict_code': 'preserved',
            'repair_verdict_areas': 0, 'repair_verdict_low_confidence': False,
            'repair_verdict_reasons': [],
            'repair_fidelity_metrics': {'changed_areas': 0,
                                        'low_confidence_areas': 0}}
    agg = fidelity._aggregate([good, failed])
    assert agg['repair_verdict_code'] == 'not_repaired', agg
    assert agg['repair_fidelity'] is None, agg
    changed = {'repair_fidelity': 65, 'repair_verdict_code': 'changed',
               'repair_verdict_areas': 2, 'repair_verdict_low_confidence': False,
               'repair_verdict_reasons': [],
               'repair_fidelity_metrics': {'changed_areas': 2,
                                           'low_confidence_areas': 0}}
    guessed = {'repair_fidelity': 75, 'repair_verdict_code': 'low_confidence',
               'repair_verdict_areas': 1, 'repair_verdict_low_confidence': True,
               'repair_verdict_reasons': ['invented_area'],
               'repair_fidelity_metrics': {'changed_areas': 1,
                                           'low_confidence_areas': 1}}
    agg = fidelity._aggregate([changed, guessed])
    assert agg['repair_verdict_code'] == 'low_confidence', agg
    assert agg['repair_verdict_low_confidence'] is True, agg


def test_no_fabricated_area_when_reduced():
    from sutura_engine.pipeline.report import fidelity_verdict_text
    code, areas = fidelity._verdict(
        50.0, _REPORT, False, [], 0, fidelity.load_config(), max_pct=2.0)
    assert code == 'changed' and areas == 0, (code, areas)
    assert fidelity_verdict_text('changed', 0) == (
        'Repaired, shape changed - check')
    assert fidelity_verdict_text('low_confidence', 0) == (
        'Repaired, shape may be rebuilt - check')


def test_detail_tol_scales_both_ways():
    v, t = _box()
    tol = fidelity._detail_tol(v)
    assert abs(tol - 0.002 * fidelity._bbox_diag(v)) < 1e-12
    vs = v * 0.02
    assert abs(fidelity._detail_tol(vs) - 0.002 * fidelity._bbox_diag(vs)) < 1e-12
    vl = v * 100.0
    assert abs(fidelity._detail_tol(vl) - 0.1) < 1e-12


def test_human_report_headline():
    from sutura_engine.pipeline.report import human_report, fidelity_verdict_text
    r = dict(_REPORT)
    r.update({
        'input': 'in.stl', 'output': 'out.stl',
        'repair_health': 100, 'repair_risk': 10,
        'repair_fidelity': 87, 'repair_verdict_code': 'preserved',
        'repair_verdict_areas': 0, 'repair_verdict_low_confidence': False,
        'repair_status': 'Safe to inspect',
    })
    out = human_report(r)
    assert 'Fidelity: 87/100' in out, out
    assert 'Repaired, shape preserved' in out, out
    assert 'Repair status: Safe to inspect' in out, out
    assert fidelity_verdict_text('changed', 2) == (
        'Repaired, shape changed in 2 area(s) - check')
    assert fidelity_verdict_text('low_confidence', 3) == (
        'Repaired, shape changed in 3 guessed area(s) - check')


def test_human_report_unavailable_scores():
    """An unavailable score prints 'n/a', never 'n/a/100'."""
    from sutura_engine.pipeline.report import human_report
    r = dict(_REPORT)
    r.update({
        'input': 'in.stl', 'output': 'out.stl',
        'repair_health': 100, 'repair_risk': 100,
        'repair_fidelity': None, 'repair_verdict_code': 'unavailable',
    })
    out = human_report(r)
    assert 'Health: 100/100' in out, out
    assert 'Risk: 100/100' in out, out
    assert 'Fidelity: n/a' in out, out
    assert 'n/a/100' not in out, out

    r.update({'repair_health': None, 'repair_risk': None,
              'repair_fidelity': 87, 'repair_verdict_code': 'preserved'})
    out = human_report(r)
    assert 'Health: n/a' in out, out
    assert 'Risk: n/a' in out, out
    assert 'Fidelity: 87/100' in out, out
    assert 'n/a/100' not in out, out


def test_changed_area_distance_gate():
    """A changed area counts only beyond the configured healthy-distance gate."""
    cfg = fidelity.load_config()
    # area present, max healthy distance at/below the 1.0 % gate -> preserved
    code, _ = fidelity._verdict(90, {}, False, [], 2, cfg, max_pct=0.9)
    assert code == 'preserved', code
    # clearly above the gate -> changed
    code, _ = fidelity._verdict(90, {}, False, [], 2, cfg, max_pct=1.5)
    assert code == 'changed', code
    # config-driven: a tighter gate makes the same distance count as changed
    cfg2 = fidelity.load_config(
        {'fidelity': {'changed_min_healthy_distance_pct': 0.2}})
    code, _ = fidelity._verdict(90, {}, False, [], 2, cfg2, max_pct=0.3)
    assert code == 'changed', code


def main():
    tests = [v for k, v in sorted(globals().items())
             if k.startswith('test_') and callable(v)]
    for fn in tests:
        fn()
        print('ok', fn.__name__)
    print('%d fidelity tests passed' % len(tests))


if __name__ == '__main__':
    main()
