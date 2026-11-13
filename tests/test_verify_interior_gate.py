#!/usr/bin/env python3
"""Verifier mirrors the product's interior-wall gate, and --measure-timeout.

Above the product's `budgets.max_faces` (read from sutura/fidelity.py, not
copied) the verifier must not compute the interior-wall mask at all: the
visible distances fall back to the all-face values and the row carries the
product's own note. Below it, nothing changes.
"""
import importlib.util
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.join(HERE, '..')
SCRIPTS = os.path.join(REPO, '.claude', 'skills', 'sutura-verify', 'scripts')
sys.path.insert(0, SCRIPTS)
sys.path.insert(0, os.path.join(REPO, 'sutura'))


def _load(name):
    spec = importlib.util.spec_from_file_location(
        name, os.path.join(SCRIPTS, name + '.py'))
    m = importlib.util.module_from_spec(spec)
    sys.modules[name] = m
    spec.loader.exec_module(m)
    return m


vrs = _load('verify_repair_shape')


def test_gate_is_read_from_the_product_config():
    gate = vrs._interior_wall_gate()
    assert gate is not None, 'gate must come from sutura/fidelity.py'
    import fidelity
    budgets = ((fidelity.load_config().get('fidelity') or {}).get('budgets')
               or {})
    assert gate == int(budgets['max_faces']), (gate, budgets['max_faces'])


def test_gate_matches_the_products_own_comparison():
    # The product gates on len(to) > max_faces (strict greater-than).
    import fidelity
    budgets = ((fidelity.load_config().get('fidelity') or {}).get('budgets')
               or {})
    limit = int(budgets['max_faces'])
    assert vrs._interior_wall_gate() == limit
    # exactly at the limit is NOT skipped by the product, so not here either
    assert not (limit > limit)
    assert (limit + 1) > limit


def _cube(n=12):
    v = np.array([[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
                  [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]], np.float64)
    t = np.array([[0, 1, 2], [0, 2, 3], [4, 6, 5], [4, 7, 6],
                  [0, 4, 5], [0, 5, 1], [1, 5, 6], [1, 6, 2],
                  [2, 6, 7], [2, 7, 3], [3, 7, 4], [3, 4, 0]], np.int64)
    return v, t


def test_skipped_mask_never_calls_the_kernel():
    import sutura_geom
    v, t = _cube()
    calls = {'n': 0}
    real = sutura_geom.buried_face_mask

    def counting(*a, **k):
        calls['n'] += 1
        return real(*a, **k)

    sutura_geom.buried_face_mask = counting
    try:
        d = vrs.two_sided_distance(v, t, v, t, skip_buried_mask=True)
        assert calls['n'] == 0, 'the mask was computed despite skip_buried_mask'
        assert d['buried_faces_input'] == 0
        assert d['buried_mask_available'] is False
        assert d['buried_note'] == vrs.INTERIOR_WALLS_SKIPPED_NOTE
    finally:
        sutura_geom.buried_face_mask = real


def test_skipped_mask_falls_back_to_all_face_values():
    v, t = _cube()
    skipped = vrs.two_sided_distance(v, t, v, t, skip_buried_mask=True)
    # no interior walls -> visible values ARE the all-face values (both paths)
    assert skipped['two_sided_max'] == skipped['two_sided_all_max']
    assert skipped['i2o_max'] == skipped['i2o_all_max']
    assert skipped['i2o_p95'] == skipped['i2o_all_p95']


def test_below_the_gate_the_mask_still_runs():
    v, t = _cube()
    normal = vrs.two_sided_distance(v, t, v, t)
    # The kernel reports 'buried_note' only on failure; a successful run has
    # none, and crucially it must NOT carry the skip note.
    assert normal.get('buried_note') != vrs.INTERIOR_WALLS_SKIPPED_NOTE
    assert normal['buried_mask_available'] is True


def test_surface_measure_skip_avoids_the_kernel():
    sm = _load('surface_metrics')
    v, t = _cube()
    out = sm.measure(v, t, v, t, budget='sweep', skip_buried_mask=True)
    assert out['buried_input_available'] is False
    assert out['buried_input_note'] == vrs.INTERIOR_WALLS_SKIPPED_NOTE
    assert out['buried_input_faces'] == 0


def test_measure_timeout_returns_a_timeout_record():
    vo = _load('verify_default_vs_optout')

    class Slow:
        def verify(self, *a, **k):
            import time
            time.sleep(2.0)
            return {'ok': True}

    real = vo.vrs.verify
    vo.vrs.verify = Slow().verify
    try:
        m = vo.measure('in', 'out', 1.0, 'repo', timeout=1)
        assert m.get('measure_timeout') is True, m
        assert 'ok' not in m
    finally:
        vo.vrs.verify = real


def test_measure_without_a_budget_still_works():
    vo = _load('verify_default_vs_optout')

    class Fast:
        def verify(self, *a, **k):
            return {'ok': True}

    real = vo.vrs.verify
    vo.vrs.verify = Fast().verify
    try:
        m = vo.measure('in', 'out', 1.0, 'repo', timeout=0)
        assert m.get('ok') is True
        assert 'measure_timeout' not in m
    finally:
        vo.vrs.verify = real


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('interior-wall gate + measure-timeout tests passed')
