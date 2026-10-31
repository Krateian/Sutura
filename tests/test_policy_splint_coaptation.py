#!/usr/bin/env python3
"""Step 6 sub-step 3 policy gate: the splint/coaptation tier knobs come from
``[tiers.splint]`` / ``[tiers.coaptation]`` and the flap/tamponade enable
switches from their tier sections, with unchanged values/types; the derived
MAX_FACES caps stay derived.

Usage: <venv>/bin/python tests/test_policy_splint_coaptation.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import sutura_engine.pipeline.presets as presets  # noqa: E402

SPLINT_WORK = {'quick': 0, 'balanced': 4_000_000, 'thorough': 8_000_000,
               'extreme': 16_000_000}
COAPTATION_WORK = {'quick': 0, 'balanced': 1_000_000, 'thorough': 2_000_000,
                   'extreme': 4_000_000}


def test_values_match_history():
    assert presets.SPLINT_RES == 128
    assert presets.SPLINT_WORK_BY_INTENSITY == SPLINT_WORK
    assert presets.SPLINT_DEFAULT_WORK == 4_000_000
    assert presets.SPLINT_DEFAULT_ENABLED is False
    assert presets.COAPTATION_SEAM_ULP_K == 1
    assert presets.COAPTATION_WORK_BY_INTENSITY == COAPTATION_WORK
    assert presets.COAPTATION_DEFAULT_WORK == 1_000_000
    assert presets.COAPTATION_DEFAULT_ENABLED is True
    assert presets.FLAP_DEFAULT_ENABLED is True
    assert presets.TAMPONADE_DEFAULT_ENABLED is True


def test_types_are_preserved():
    assert isinstance(presets.SPLINT_RES, int) and not isinstance(
        presets.SPLINT_RES, bool)
    assert isinstance(presets.COAPTATION_SEAM_ULP_K, int)
    for table in (presets.SPLINT_WORK_BY_INTENSITY,
                  presets.COAPTATION_WORK_BY_INTENSITY):
        for k, v in table.items():
            assert isinstance(k, str)
            assert isinstance(v, int) and not isinstance(v, bool)
    for name in ('SPLINT_DEFAULT_ENABLED', 'COAPTATION_DEFAULT_ENABLED',
                 'FLAP_DEFAULT_ENABLED', 'TAMPONADE_DEFAULT_ENABLED'):
        assert isinstance(getattr(presets, name), bool), name


def test_derived_caps_stay_derived():
    balanced = presets._BALANCED_INTENSITY
    assert presets.SPLINT_MAX_FACES == balanced.ftetwild_max_faces
    assert presets.COAPTATION_MAX_FACES == balanced.ftetwild_max_faces


if __name__ == '__main__':
    test_values_match_history()
    test_types_are_preserved()
    test_derived_caps_stay_derived()
    print('policy splint/coaptation tests passed')
