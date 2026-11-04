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
    assert r['repair_fidelity'] < 100, r
    assert r['repair_verdict_code'] in ('changed', 'low_confidence'), r
    assert r['repair_fidelity_factors']['healthy_distance']['penalty'] > 0


def test_small_hole_keeps_high_fidelity():
    v, t = _box(hole=True)
    vf, tf = _box()
    r = fidelity.compute((v, t), (vf, tf), _REPORT)
    assert r['repair_fidelity'] is not None and r['repair_fidelity'] >= 80, r
    assert r['repair_verdict_low_confidence'] is False, r


def test_big_hole_is_low_confidence():
    v, t = _box(open_top=True)
    vf, tf = _box()
    r = fidelity.compute((v, t), (vf, tf), _REPORT)
    assert r['repair_verdict_low_confidence'] is True, r
    assert r['repair_verdict_code'] == 'low_confidence', r
    assert r['repair_verdict_areas'] >= 1
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
    m = sm.measure(v, t, v, t, _REPORT)
    assert abs(m['feature_recall'] - 1.0) < 1e-12, m
    assert abs(m['healthy_fill_mm3']) < 1e-9, m
    assert abs(m['healthy_removed_mm3']) < 1e-9, m
    r = fidelity.compute((v, t), (v, t), _REPORT)
    assert r['repair_fidelity_metrics']['feature_recall'] == 1.0
    assert r['repair_fidelity_metrics']['healthy_fill_fraction'] == 0.0
    assert r['repair_fidelity_metrics']['healthy_removed_fraction'] == 0.0


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


def main():
    tests = [v for k, v in sorted(globals().items())
             if k.startswith('test_') and callable(v)]
    for fn in tests:
        fn()
        print('ok', fn.__name__)
    print('%d fidelity tests passed' % len(tests))


if __name__ == '__main__':
    main()
