#!/usr/bin/env python3
"""Step 5 (a) policy gate: the ranking knobs and score-template data come from
``[ranking]`` / ``[templates]`` with unchanged values and types (ints stay ints,
the preferred orders load back as tuples of int), and the ``_score`` weights
stay in code.

Usage: <venv>/bin/python tests/test_policy_ranking.py
"""
import inspect
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import sutura_engine.diagnosis as diagnosis  # noqa: E402
import sutura_engine.learning as learning  # noqa: E402
from sutura_engine import methods  # noqa: E402
from sutura_engine.policy import RANKING, TEMPLATES_CFG  # noqa: E402

PREFS = {
    'mechanical': (1, 3, 4, 5, 2),
    'organic': (1, 2, 3, 13, 7, 5),
    'single_side_scan': (8, 13, 7, 3, 2),
    'relief': (9, 10, 3, 8),
    'repeated_pattern': (11, 12, 1),
    'dense_scan_heavy_si': (5, 6, 13, 7, 3),
}


def test_ranking_knobs_match_history():
    assert methods.PREFERENCE_BOOST == 1.25
    assert methods.RANKING_TOP_N == 6
    assert learning.LEARNING_MAX_BONUS == 0.05
    assert learning.LEARNING_PRIOR == 2.0


def test_ranking_knobs_types():
    assert isinstance(methods.PREFERENCE_BOOST, float)
    assert isinstance(methods.RANKING_TOP_N, int) and not isinstance(
        methods.RANKING_TOP_N, bool)
    assert isinstance(learning.LEARNING_MAX_BONUS, float)
    assert isinstance(learning.LEARNING_PRIOR, float)


def test_top_n_signature_unchanged():
    assert inspect.signature(methods.rank_methods).parameters['top_n'].default == 6


def test_template_thresholds_match_history():
    assert diagnosis.OPEN_AREA_SCAN == 0.02
    assert diagnosis.OPEN_AREA_RELIEF == 0.08
    assert diagnosis.LARGE_LOOP_RATIO == 0.25
    assert diagnosis.MANY_LOOPS == 4
    assert diagnosis.SI_MODERATE == 20
    assert diagnosis.SI_HEAVY == 200
    for name in ('MANY_LOOPS', 'SI_MODERATE', 'SI_HEAVY'):
        value = getattr(diagnosis, name)
        assert isinstance(value, int) and not isinstance(value, bool), name
    for name in ('OPEN_AREA_SCAN', 'OPEN_AREA_RELIEF', 'LARGE_LOOP_RATIO'):
        assert isinstance(getattr(diagnosis, name), float), name


def test_template_preferred_orders():
    got = {t.id: t.preferred for t in diagnosis.all_templates()}
    for tid, want in PREFS.items():
        assert got[tid] == want, (tid, got[tid], want)
        assert isinstance(got[tid], tuple)
        assert all(isinstance(x, int) and not isinstance(x, bool) for x in got[tid])


def test_policy_is_the_source():
    assert RANKING['preference_boost'] == 1.25
    assert RANKING['top_n'] == 6
    assert TEMPLATES_CFG['preferred']['organic'] == [1, 2, 3, 13, 7, 5]


if __name__ == '__main__':
    test_ranking_knobs_match_history()
    test_ranking_knobs_types()
    test_top_n_signature_unchanged()
    test_template_thresholds_match_history()
    test_template_preferred_orders()
    test_policy_is_the_source()
    print('policy ranking tests passed')
