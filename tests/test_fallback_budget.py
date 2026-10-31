#!/usr/bin/env python3
"""Deterministic fallback work-budget tests.

The estimate is a pure function of the geometry and the preset; no test here
reads or asserts a wall clock.  pytest-compatible and runnable directly.
"""
import json
import os
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402

from sutura_engine.pipeline import work_estimate as we  # noqa: E402


def test_damage_work_arithmetic():
    assert we.damage_work(0) == 0
    assert we.damage_work(100) == 100
    assert we.damage_work(100, non_manifold_edges=2) == 300
    assert we.damage_work(100, non_manifold_vertices=3, holes=1) == 500
    # Negative counters never reduce the estimate.
    assert we.damage_work(100, -5, -5, -5) == 100


def test_grid_work_is_ceil_cell_face():
    assert we.grid_work(1000, 1000) == 1000
    assert we.grid_work(1, 1) == 1
    assert we.grid_work(0, 10) == 0
    # A large mesh/grid combination is far above the default cap.
    large = we.grid_work(100_000, 1_000_000)
    small = we.grid_work(8_000, 80_000)
    assert large > we.DEFAULT_FALLBACK_WORK_BUDGET
    assert small < we.DEFAULT_FALLBACK_WORK_BUDGET


def test_grid_work_hole_factor():
    assert we.grid_work_with_holes(100_000, 1_000_000, 0) == 100_000_000
    assert we.grid_work_with_holes(100_000, 1_000_000, 99) == 100_000_000
    assert we.grid_work_with_holes(100_000, 1_000_000, 100) == 200_000_000
    assert we.grid_work_with_holes(100_000, 1_000_000, 609) == 700_000_000


def test_hole_factor_flips_graft_precheck_at_100_holes():
    """H1: the factor is neutral below 100 holes and doubles at 100."""
    raw = we.grid_work(70_000, 1_000_000)  # 70 M kilo cell-face units
    assert raw == 70_000_000
    budget = we.DEFAULT_GRAFT_GRID_WORK_BUDGET
    # 99 holes: the pre-gate would still admit the attempt.
    assert we.grid_work_with_holes(70_000, 1_000_000, 99) == raw
    assert we.fits_budget(raw, budget)
    # 100 holes: 140 M is above the 120 M calibrated cap.
    flipped = we.grid_work_with_holes(70_000, 1_000_000, 100)
    assert flipped == 2 * raw
    assert not we.fits_budget(flipped, budget)


def test_si_pair_work_arithmetic():
    assert we.si_pair_work(100) == 100
    assert we.si_pair_work(100, 2, 3) == 600
    assert we.si_pair_work(100, 2, 3, 4) == 1000
    assert we.si_pair_work(0, 5) == 0


def test_budget_env_opt_out_and_override():
    assert we.fallback_work_budget({}) == we.DEFAULT_FALLBACK_WORK_BUDGET
    assert we.fallback_work_budget({we.FALLBACK_BUDGET_ENV: '0'}) is None
    assert we.fallback_work_budget({we.FALLBACK_BUDGET_ENV: 'off'}) is None
    assert we.fallback_work_budget({we.FALLBACK_BUDGET_ENV: '1234'}) == 1234
    assert we.fallback_work_budget(
        {we.FALLBACK_BUDGET_ENV: 'bad'}) == we.DEFAULT_FALLBACK_WORK_BUDGET
    assert we.si_work_budget({we.SI_BUDGET_ENV: '0'}) is None
    assert we.si_work_budget({we.SI_BUDGET_ENV: '42'}) == 42
    assert (we.graft_grid_work_budget({})
            == we.DEFAULT_GRAFT_GRID_WORK_BUDGET)
    assert we.graft_grid_work_budget(
        {we.GRAFT_GRID_BUDGET_ENV: '0'}) is None


def test_candidate_work_uses_grid_for_morphology():
    assert we.candidate_work('graft', damage=10, grid=99) == 99
    assert we.candidate_work('dressing', damage=10, grid=99) == 99
    assert we.candidate_work('ftetwild', damage=10, grid=99) == 10
    # No grid estimate available: fall back to the damage count.
    assert we.candidate_work('graft', damage=10, grid=0) == 10


def test_candidate_work_is_per_method():
    # A cheap topology tier is charged its pipeline face passes, so it is not
    # skipped by the global damage estimate (H2) but a multi-million-face
    # rerun is still refused.
    assert we.candidate_work('local_mend', damage=6_100_000, grid=0,
                             faces=100_000) == 800_000
    assert we.candidate_work('join', damage=6_100_000, grid=0,
                             faces=100_000) == 800_000
    assert we.candidate_work('join', damage=0, grid=0,
                             faces=2_643_020) > we.DEFAULT_FALLBACK_WORK_BUDGET
    assert we.candidate_work('join', damage=12_000_000, grid=0,
                             faces=0) == 12_000_000
    # A global rebuild keeps the weighted damage estimate.
    assert we.candidate_work('coaptation', damage=6_100_000, grid=0,
                             faces=100_000) == 6_100_000
    assert we.work_class('graft') == 'grid'
    assert we.work_class('local_mend') == 'faces'
    assert we.work_class('unknown') == 'damage'


def test_over_budget_candidate_still_checks_si():
    """H3: the exact SI reject is not bypassed above the final-scan cap."""
    import types
    from sutura_engine import triage as t
    from sutura_engine.pipeline import adopt_guard

    orig_units = t._si_pair_work_units
    orig_count = adopt_guard.exact_si_count
    orig_budget = we.si_work_budget
    try:
        t._si_pair_work_units = lambda objs: 10 ** 9
        we.si_work_budget = lambda environ=None: 1

        def _count(vertices, faces):
            # The input is clean, the candidate introduces an intersection.
            return 0 if len(faces) == 1 else 5

        adopt_guard.exact_si_count = _count
        v = np.zeros((3, 3), dtype=np.float64)
        t_in = np.array([[0, 1, 2]], dtype=np.int32)
        t_out = np.array([[0, 1, 2], [0, 1, 2]], dtype=np.int32)
        in_objs = [(None, v, t_in)]
        out_objs = [(None, v, t_out)]
        method = types.SimpleNamespace(id='splint')
        guard = t._adopt_guard_for_method({}, in_objs, out_objs, method, 0, 0)
        assert guard.fields['si_checked'] is True
        assert guard.fields.get('si_candidate_over_budget') is True
        assert guard.ok is False
        assert 'self-intersections' in (guard.reason or '')
    finally:
        t._si_pair_work_units = orig_units
        adopt_guard.exact_si_count = orig_count
        we.si_work_budget = orig_budget
        t._BASE_SI_CACHE.clear()


def test_deterministic_estimate():
    args = (98_398, 0, 133, 609)
    first = we.damage_work(*args)
    for _ in range(5):
        assert we.damage_work(*args) == first
    assert first > we.DEFAULT_FALLBACK_WORK_BUDGET


def test_trace_flushes_before_finish():
    from sutura_engine import trace
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, 'trace.json')
        tracer = trace.begin_trace(trace_path=path, input_file='input.stl')
        try:
            v = np.zeros((3, 3), dtype=np.float64)
            t = np.array([[0, 1, 2]], dtype=np.int32)
            tracer.record_stage('unit', 'test', v, t, v, t, elapsed=0.0)
            # The file exists and is valid JSON before finish_trace().
            with open(path) as f:
                data = json.load(f)
            assert len(data['stages']) == 1
            assert data['stages'][0]['stage'] == 'unit'
        finally:
            trace.finish_trace()


if __name__ == '__main__':
    test_damage_work_arithmetic()
    test_grid_work_is_ceil_cell_face()
    test_grid_work_hole_factor()
    test_hole_factor_flips_graft_precheck_at_100_holes()
    test_si_pair_work_arithmetic()
    test_budget_env_opt_out_and_override()
    test_candidate_work_uses_grid_for_morphology()
    test_candidate_work_is_per_method()
    test_over_budget_candidate_still_checks_si()
    test_deterministic_estimate()
    test_trace_flushes_before_finish()
    print('ok test_fallback_budget')
