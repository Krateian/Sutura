#!/usr/bin/env python3
"""Step 6 sub-step 2 policy gate: Stage-1 modes/profiles/type thresholds and the
advanced tuning gates come from the policy with unchanged values/types, and the
structural name tuples stay in code.

Usage: <venv>/bin/python tests/test_policy_stage1.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import sutura_engine.pipeline.presets as presets  # noqa: E402

MODES = {
    'low': {'mincomponentsize': 8, 'maxholesize': 200},
    'medium': {'mincomponentsize': 8, 'maxholesize': 1000},
    'aggressive': {'mincomponentsize': 12, 'maxholesize': 3000},
    'extreme': {'mincomponentsize': 20, 'maxholesize': 10000},
}
PROFILES = {
    'mechanical': {'mincomponentsize': 8, 'maxholesize': 300},
    'organic': {'mincomponentsize': 12, 'maxholesize': 1000},
    'scan': {'mincomponentsize': 4, 'maxholesize': 10000},
    'miniature': {'mincomponentsize': 1, 'maxholesize': 50},
    'fast': {'mincomponentsize': 8, 'maxholesize': 200},
}
TYPES = {
    'mechanical': {'mincomponentsize': 8, 'maxholesize': 300},
    'organic': {'mincomponentsize': 12, 'maxholesize': 1000},
    'unknown': {'mincomponentsize': 8, 'maxholesize': 1000},
}


def test_values_match_history():
    assert presets.MODE_PARAMS == MODES
    assert presets.PROFILES == PROFILES
    assert presets._TYPE_PARAMS == TYPES
    assert presets.MECH_TUNE_GATE == 0.75
    assert presets.ORG_TUNE_GATE == 0.70


def test_types_are_preserved():
    for table in (presets.MODE_PARAMS, presets.PROFILES, presets._TYPE_PARAMS):
        for spec in table.values():
            assert set(spec) == {'mincomponentsize', 'maxholesize'}
            for value in spec.values():
                assert isinstance(value, int) and not isinstance(value, bool)
    assert isinstance(presets.MECH_TUNE_GATE, float)
    assert isinstance(presets.ORG_TUNE_GATE, float)


def test_structural_tuples_stay_in_code():
    assert presets.REPAIR_MODES == ('low', 'medium', 'auto', 'aggressive', 'extreme')
    assert presets.REPAIR_PROFILES == ('mechanical', 'organic', 'scan',
                                       'miniature', 'fast')
    assert set(MODES) == set(presets.REPAIR_MODES) - {'auto'}


if __name__ == '__main__':
    test_values_match_history()
    test_types_are_preserved()
    test_structural_tuples_stay_in_code()
    print('policy stage1 tests passed')
