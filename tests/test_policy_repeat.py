#!/usr/bin/env python3
"""Step 4 (repeat) policy gate: the Repeat tier's Hausdorff guard comes from
``[tiers.repeat.gates]`` and equals the historical literal with its type.

Usage: <venv>/bin/python tests/test_policy_repeat.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import sutura_engine.pipeline.tiers.repeat as repeat  # noqa: E402
from sutura_engine.policy import gate_paths  # noqa: E402


def test_repeat_value_matches_history():
    assert repeat.REPEAT_MAX_HAUSDORFF_OUTSIDE == 0.01


def test_repeat_type_is_preserved():
    value = repeat.REPEAT_MAX_HAUSDORFF_OUTSIDE
    assert isinstance(value, float) and not isinstance(value, bool)


def test_repeat_gate_registered():
    assert 'tiers.repeat.gates' in gate_paths()


if __name__ == '__main__':
    test_repeat_value_matches_history()
    test_repeat_type_is_preserved()
    test_repeat_gate_registered()
    print('policy repeat tests passed')
