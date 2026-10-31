#!/usr/bin/env python3
"""Step 2 policy gate: the auto-escalation constants come from ``[auto]`` in the
policy and equal the historical ``triage.py`` literals.

Usage: <venv>/bin/python tests/test_policy_auto.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import sutura_engine.triage as triage  # noqa: E402
from sutura_engine.policy import AUTO  # noqa: E402

# The historical literals (pre-policy-file).
HIST = {
    'AUTO_MAX_ATTEMPTS': 3,
    'AUTO_FALLBACK_WATCHDOG_S': 3600.0,
    'AUTO_FALLBACK_BUDGET_FACTOR': 2.0,
    'AUTO_FALLBACK_MIN_S': 30.0,
    'AUTO_FALLBACK_MAX_S': 120.0,
    'AUTO_INVENT_MIN_SCORE': 0.8,
    'GENERATIVE_MAX_HAUSDORFF_REL': 0.01,
}


def test_auto_constants_match_history():
    for name, value in HIST.items():
        assert getattr(triage, name) == value, (name, getattr(triage, name), value)


def test_auto_is_the_source():
    assert AUTO['max_attempts'] == 3
    assert AUTO['fallback_watchdog_s'] == 3600.0
    assert AUTO['invent_min_score'] == 0.8
    assert AUTO['generative_max_hausdorff_rel'] == 0.01


if __name__ == '__main__':
    test_auto_constants_match_history()
    test_auto_is_the_source()
    print('policy auto tests passed')
