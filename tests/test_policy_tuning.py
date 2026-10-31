#!/usr/bin/env python3
"""Step 6 sub-step 4 policy gate: HAUSDORFF_FLOOR, DECIMATE_HD_MARGIN and
FTETWILD_MAX_HAUSDORFF_REL come from the non-overridable ``[gates.tuning]``
with unchanged values/types.

Usage: <venv>/bin/python tests/test_policy_tuning.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import sutura_engine.pipeline.presets as presets  # noqa: E402
import triage  # noqa: E402
from sutura_engine.policy import GATES, gate_paths  # noqa: E402


def test_values_match_history():
    assert triage.HAUSDORFF_FLOOR == 200000
    assert presets.DECIMATE_HD_MARGIN == 0.005
    assert presets.FTETWILD_MAX_HAUSDORFF_REL == 0.01


def test_types_are_preserved():
    assert isinstance(triage.HAUSDORFF_FLOOR, int)
    assert not isinstance(triage.HAUSDORFF_FLOOR, bool)
    assert isinstance(presets.DECIMATE_HD_MARGIN, float)
    assert isinstance(presets.FTETWILD_MAX_HAUSDORFF_REL, float)


def test_locked():
    paths = gate_paths()
    assert 'gates.tuning' in paths
    assert 'auto.generative_max_hausdorff_rel' in paths
    assert GATES['tuning']['hausdorff_floor'] == 200000
    assert GATES['tuning']['decimate_hd_margin'] == 0.005
    assert GATES['tuning']['ftetwild_max_hausdorff_rel'] == 0.01


if __name__ == '__main__':
    test_values_match_history()
    test_types_are_preserved()
    test_locked()
    print('policy tuning-gate tests passed')
