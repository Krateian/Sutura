#!/usr/bin/env python3
"""Step 4 (flap) policy gate: the Flap tier constants come from
``[tiers.flap]`` / ``[tiers.flap.gates]``, equal the historical literals, and
keep their Python types (dict of str->int, ints stay ints, floats stay floats).

Usage: <venv>/bin/python tests/test_policy_flap.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import sutura_engine.pipeline.tiers.flap as flap  # noqa: E402
from sutura_engine.policy import gate_paths  # noqa: E402


def test_flap_values_match_history():
    assert flap.FLAP_WORK_BY_INTENSITY == {
        'quick': 0, 'balanced': 75_000_000, 'thorough': 150_000_000,
        'extreme': 300_000_000}
    assert flap.FLAP_DEFAULT_WORK == 75_000_000
    assert flap.FLAP_DEBRIS_VOLUME_EPS == 1e-3
    assert flap.FLAP_DEBRIS_VOLUME_REL == 1e-9
    assert flap.FLAP_DEBRIS_FLATNESS == 1e-4


def test_flap_types_are_preserved():
    assert isinstance(flap.FLAP_WORK_BY_INTENSITY, dict)
    for k, v in flap.FLAP_WORK_BY_INTENSITY.items():
        assert isinstance(k, str)
        assert isinstance(v, int) and not isinstance(v, bool), (k, type(v))
    assert isinstance(flap.FLAP_DEFAULT_WORK, int)
    assert not isinstance(flap.FLAP_DEFAULT_WORK, bool)
    for x in (flap.FLAP_DEBRIS_VOLUME_EPS, flap.FLAP_DEBRIS_VOLUME_REL,
              flap.FLAP_DEBRIS_FLATNESS):
        assert isinstance(x, float), type(x)


def test_flap_gate_registered():
    assert 'tiers.flap.gates' in gate_paths()


if __name__ == '__main__':
    test_flap_values_match_history()
    test_flap_types_are_preserved()
    test_flap_gate_registered()
    print('policy flap tests passed')
