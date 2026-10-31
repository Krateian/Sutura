#!/usr/bin/env python3
"""Step 3 policy gate: the SI escalation constants come from ``[si_escalation]``
and equal the historical ``triage.py`` literals.

Usage: <venv>/bin/python tests/test_policy_si.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import sutura_engine.triage as triage  # noqa: E402
from sutura_engine.policy import SI_ESCALATION, gate_paths  # noqa: E402

# The historical literals (pre-policy-file).
HIST = {
    'SI_ESCALATION_METHOD_NUM': 10,
    'SI_ESCALATION_CANDIDATES': (10,),
    'SI_ESCALATION_VOL_REL_TOL': 0.005,
    'SI_ESCALATION_DIST_TOL_MIN_MM': 0.0125,
    'SI_ESCALATION_DIST_TOL_MAX_MM': 0.05,
    'SI_ESCALATION_DIST_TOL_DIV': 10000.0,
}


def test_si_constants_match_history():
    for name, value in HIST.items():
        assert getattr(triage, name) == value, (name, getattr(triage, name), value)


def test_si_is_the_source():
    assert SI_ESCALATION['method_num'] == 10
    assert SI_ESCALATION['dist_tol_min_mm'] == 0.0125
    assert SI_ESCALATION['dist_tol_max_mm'] == 0.05
    assert SI_ESCALATION['dist_tol_div'] == 10000.0
    # The candidates tuple stays derived from the single method number.
    assert triage.SI_ESCALATION_CANDIDATES == (SI_ESCALATION['method_num'],)
    # The whole section is a locked gate (the auto-SI never-worse rule).
    assert 'si_escalation' in gate_paths()


if __name__ == '__main__':
    test_si_constants_match_history()
    test_si_is_the_source()
    print('policy si-escalation tests passed')
