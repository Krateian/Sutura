#!/usr/bin/env python3
"""Step 5 (b) policy gate: the six adoption-guard numbers come from the
non-overridable ``[gates.adoption]`` with unchanged values/types, and the SI
policy enum/map (``SI_POLICIES``, ``_TIER_POLICIES``) stay in code.

Usage: <venv>/bin/python tests/test_policy_adoption.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import sutura_engine.pipeline.adopt_guard as ag  # noqa: E402
from sutura_engine.policy import gate_paths  # noqa: E402


def test_adoption_values_match_history():
    assert ag.FACE_RATIO == 4.0
    assert ag.VERTEX_RATIO == 4.0
    assert ag.MIN_SLACK == 256
    assert ag.LOOP_FACE_SLACK == 16
    assert ag.LOOP_VERTEX_SLACK == 16
    assert ag.FTETWILD_ABS_FLOOR == 50000


def test_adoption_types_are_preserved():
    assert isinstance(ag.FACE_RATIO, float)
    assert isinstance(ag.VERTEX_RATIO, float)
    for name in ('MIN_SLACK', 'LOOP_FACE_SLACK', 'LOOP_VERTEX_SLACK',
                 'FTETWILD_ABS_FLOOR'):
        value = getattr(ag, name)
        assert isinstance(value, int) and not isinstance(value, bool), name


def test_adoption_gate_registered():
    assert 'gates.adoption' in gate_paths()


def test_si_enums_stay_in_code():
    assert ag.SI_POLICIES == ('non_rise', 'strict_zero', 'flag', 'improved_flag', 'off')
    assert ag._TIER_POLICIES['graft'] == 'improved_flag'
    assert ag._TIER_POLICIES['dressing'] == 'strict_zero'
    assert ag.policy_for('ftetwild').si_policy == 'improved_flag'
    assert ag.policy_for('unknown-tier').si_policy == 'flag'


if __name__ == '__main__':
    test_adoption_values_match_history()
    test_adoption_types_are_preserved()
    test_adoption_gate_registered()
    test_si_enums_stay_in_code()
    print('policy adoption-guard tests passed')
