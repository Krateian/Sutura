#!/usr/bin/env python3
"""Step 4 (dressing) policy gate: the dressing names move from presets.py into
``[tiers.dressing]`` / ``[tiers.dressing.gates]`` with unchanged values and
types (tuple stays a tuple, bool stays a bool, int stays an int); the
step-6 constants in presets.py are untouched.

Usage: <venv>/bin/python tests/test_policy_dressing.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import sutura_engine.pipeline.presets as presets  # noqa: E402
from sutura_engine.policy import gate_paths  # noqa: E402


def test_dressing_values_match_history():
    assert presets.DRESSING_DEFAULT_ENABLED is False
    assert presets.DRESSING_MAX_VOLUME_DELTA == 0.10
    assert presets.DRESSING_MAX_PARTS_SLACK == 1
    assert presets.DRESSING_MAX_NORMAL_ANGLE_P95 == 30.0
    assert presets.CAD_LIKENESS_THRESHOLD == 0.5


def test_dressing_defect_sets_stay_in_code():
    # Structural enum mirroring sutura_engine.dressing.DEFECT_SETS, not policy.
    assert presets.DRESSING_DEFECT_SETS == ('all', 'holes_nm')
    assert isinstance(presets.DRESSING_DEFECT_SETS, tuple)


def test_dressing_types_are_preserved():
    assert isinstance(presets.DRESSING_DEFAULT_ENABLED, bool)
    slack = presets.DRESSING_MAX_PARTS_SLACK
    assert isinstance(slack, int) and not isinstance(slack, bool)
    for name in ('DRESSING_MAX_VOLUME_DELTA', 'DRESSING_MAX_NORMAL_ANGLE_P95',
                 'CAD_LIKENESS_THRESHOLD'):
        assert isinstance(getattr(presets, name), float), name


def test_dressing_gate_registered():
    assert 'tiers.dressing.gates' in gate_paths()


def test_step6_constants_untouched():
    # The rest of presets.py stays in code for step 6 / 4b.
    assert presets.MECH_TUNE_GATE == 0.75
    assert presets.ORG_TUNE_GATE == 0.70
    assert presets.FLAP_DEFAULT_ENABLED is True
    assert presets.TAMPONADE_DEFAULT_ENABLED is True
    assert presets.SPLINT_RES == 128


if __name__ == '__main__':
    test_dressing_values_match_history()
    test_dressing_defect_sets_stay_in_code()
    test_dressing_types_are_preserved()
    test_dressing_gate_registered()
    test_step6_constants_untouched()
    print('policy dressing tests passed')
