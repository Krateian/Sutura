#!/usr/bin/env python3
"""Unit tests for the surface-damage metrics.

Run with a Python that has numpy + sutura_geom, e.g.:
  PYTHONPATH=sutura python tests/test_surface_metrics.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, 'sutura'))
sys.path.insert(0, os.path.join(REPO, '.claude', 'skills', 'sutura-verify',
                                'scripts'))

import numpy as np  # noqa: E402
import surface_metrics as sm  # noqa: E402
from defects import detect  # noqa: E402


def _box(size=10.0, n=4, open_top=False):
    """Axis-aligned box with n x n quads per face, outward oriented."""
    s = float(size) / 2.0
    verts = []
    tris = []
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
                b = a + 1
                c = a + (n + 1)
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
    a, b, c = uniq[tris[:, 0]], uniq[tris[:, 1]], uniq[tris[:, 2]]
    nrm = np.cross(b - a, c - a)
    centre = uniq.mean(axis=0)
    outward = np.einsum('ij,ij->i', nrm, (a + b + c) / 3.0 - centre) > 0.0
    tris[~outward] = tris[~outward][:, [0, 2, 1]]
    return uniq.astype(np.float64), tris.astype(np.int64)


def test_identity_recall_and_zero_fill():
    v, t = _box()
    rep = detect(v, t, with_indices=True)
    m = sm.measure(v, t, v, t, rep)
    assert m['feature_input_sharp_edges'] > 0, m
    assert abs(m['feature_recall'] - 1.0) < 1e-12, m
    assert m['healthy_fill_mm3'] == 0.0, m
    assert m['healthy_removed_mm3'] == 0.0, m
    print('identity: recall=%.6f fill=%.6f removed=%.6f edges=%d' % (
        m['feature_recall'], m['healthy_fill_mm3'],
        m['healthy_removed_mm3'], m['feature_input_sharp_edges']))


def test_budget_profiles():
    v, t = _box()
    rep = detect(v, t, with_indices=True)
    c = v.mean(axis=0)
    grow = c + (v - c) * 1.05
    sw = sm.measure(v, t, grow, t, rep)
    ga = sm.measure(v, t, grow, t, rep, budget='gate')
    cu = sm.measure(v, t, grow, t, rep, area_samples=1234, layers=6)
    assert sw['healthy_budget'] == 'sweep', sw
    assert (sw['healthy_area_samples'], sw['healthy_offset_layers']) == (4000, 10)
    assert ga['healthy_budget'] == 'gate', ga
    assert (ga['healthy_area_samples'], ga['healthy_offset_layers']) == (10000, 16)
    assert cu['healthy_budget'] == 'custom', cu
    assert (cu['healthy_area_samples'], cu['healthy_offset_layers']) == (1234, 6)
    assert sw['feature_samples_per_edge'] == 8, sw
    print('budgets: sweep=%sx%s gate=%sx%s custom=%sx%s' % (
        sw['healthy_area_samples'], sw['healthy_offset_layers'],
        ga['healthy_area_samples'], ga['healthy_offset_layers'],
        cu['healthy_area_samples'], cu['healthy_offset_layers']))


def test_open_box_cap_is_not_counted():
    vo, to = _box(open_top=True)
    vc, tc = _box()
    rep = detect(vo, to, with_indices=True)
    mask = sm.defect_face_mask(to, rep)
    assert mask.any(), 'open box must report a boundary'
    m = sm.measure(vo, to, vc, tc, rep)
    assert m['healthy_fill_mm3'] < 1e-6, m
    print('open-box cap: fill=%.8f (masked faces %d/%d)' % (
        m['healthy_fill_mm3'], int(mask.sum()), len(mask)))


def test_growth_and_shrink():
    v, t = _box()
    rep = detect(v, t, with_indices=True)
    c = v.mean(axis=0)
    grow = c + (v - c) * 1.05
    shrink = c + (v - c) * 0.95
    g = sm.measure(v, t, grow, t, rep)
    s = sm.measure(v, t, shrink, t, rep)
    assert g['healthy_fill_mm3'] > 1.0, g
    assert g['healthy_removed_mm3'] < 1e-6, g
    assert s['healthy_removed_mm3'] > 1.0, s
    assert s['healthy_fill_mm3'] < 1e-6, s
    assert g['feature_recall'] < 1.0, g
    print('growth: fill=%.3f recall=%.4f | shrink: removed=%.3f recall=%.4f' % (
        g['healthy_fill_mm3'], g['feature_recall'],
        s['healthy_removed_mm3'], s['feature_recall']))


def test_translated_hole_loses_recall():
    v, t = _box()
    rep = detect(v, t, with_indices=True)
    out = v + np.array([1.0, 0.0, 0.0])
    m = sm.measure(v, t, out, t, rep)
    assert m['feature_recall'] < 0.5, m
    print('translated: recall=%.4f fill=%.3f' % (
        m['feature_recall'], m['healthy_fill_mm3']))


if __name__ == '__main__':
    test_identity_recall_and_zero_fill()
    test_budget_profiles()
    test_open_box_cap_is_not_counted()
    test_growth_and_shrink()
    test_translated_hole_loses_recall()
    print('OK')
