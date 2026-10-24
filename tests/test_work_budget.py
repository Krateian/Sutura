#!/usr/bin/env python3
"""Deterministic work-budget regression tests.

Checks the pure ``WorkBudget`` helper and the source-level contract: the Flap
core and its tier must not read a wall clock in a decision.  pytest-compatible;
runnable directly with the repo venv or any Python with numpy.
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
for p in (SUTURA, REPO):
    if p not in sys.path:
        sys.path.insert(0, p)

from sutura_engine.pipeline.work_budget import WorkBudget  # noqa: E402


def test_work_budget_arithmetic():
    b = WorkBudget(limit=10, unit='cells')
    assert b.fits(10)
    assert not b.fits(11)
    assert b.consume(4)
    assert b.spent == 4
    assert b.remaining == 6
    assert not b.exhausted
    assert b.stop_before(7)
    assert not b.stop_before(6)
    assert b.as_report()['budget_limited_by'] is None
    assert b.consume(6)
    assert b.exhausted
    assert b.as_report()['budget_limited_by'] == 'work'
    assert b.as_report()['work_units'] == 10
    assert b.as_report()['work_budget'] == 10


def test_work_budget_unlimited_and_zero():
    unlimited = WorkBudget(limit=None)
    assert unlimited.fits(10**18)
    assert unlimited.consume(10**18)
    assert not unlimited.exhausted
    assert unlimited.remaining is None

    # A non-positive limit disables the consumer; callers check <= 0 directly.
    zero = WorkBudget(limit=0)
    assert zero.exhausted
    assert not zero.fits(1)
    assert zero.stop_before(0) is False


def test_flap_core_has_no_wall_clock_decision():
    rust = open(os.path.join(REPO, 'rust', 'sutura-geom', 'src', 'flap',
                             'mod.rs'), encoding='utf-8').read()
    assert 'Instant::now' not in rust
    assert 'Duration' not in rust
    assert 'time_budget' not in rust
    assert 'work_budget' in rust
    assert 'report.work_units = work_used' in rust


def test_tamponade_and_melt_are_work_budgeted():
    tamp = open(os.path.join(REPO, 'rust', 'sutura-geom', 'src',
                             'tamponade.rs'), encoding='utf-8').read()
    assert 'work_budget_cells' in tamp
    assert 'throat_cells(&rim, verts, h)' in tamp
    assert 'time_budget_s' not in tamp
    melt = open(os.path.join(REPO, 'rust', 'sutura-geom', 'src',
                             'melt.rs'), encoding='utf-8').read()
    assert 'MELT_MAX_SECONDS' not in melt
    assert 't0.elapsed().as_secs_f64() > b' not in melt
    assert 'max_layers' in melt


def test_splint_and_coaptation_use_work_fields():
    for name in ('splint.py', 'coaptation.py'):
        src = open(os.path.join(REPO, 'sutura', 'sutura_engine', 'pipeline',
                                'tiers', name), encoding='utf-8').read()
        assert "rec['work_units']" in src, name
        assert 'work_budget' in src, name
        assert 'timeout' not in src, name


def test_flap_tier_uses_work_units_not_time():
    tier = open(os.path.join(REPO, 'sutura', 'sutura_engine', 'pipeline',
                             'tiers', 'flap.py'), encoding='utf-8').read()
    assert "rec['work_units']" in tier
    assert 'FLAP_WORK_BY_INTENSITY' in tier
    assert 'FLAP_TIMEOUT_BY_INTENSITY' not in tier
    # The only remaining time read is the measurement of ``time_s``.
    assert "rec['time_s']" in tier
    assert 'work_budget=budget' in tier
    assert '_flap_cache_key(base_v, base_t, budget)' in tier


if __name__ == '__main__':
    test_work_budget_arithmetic()
    test_work_budget_unlimited_and_zero()
    test_flap_core_has_no_wall_clock_decision()
    test_tamponade_and_melt_are_work_budgeted()
    test_splint_and_coaptation_use_work_fields()
    test_flap_tier_uses_work_units_not_time()
    print('ok test_work_budget')
