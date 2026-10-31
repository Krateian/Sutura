#!/usr/bin/env python3
"""Step 4 (tamponade) policy gate: the Tamponade tier constants come from
``[tiers.tamponade]`` / ``[tiers.tamponade.gates]``, equal the historical
literals with their types, and the open-shell thresholds stay exactly 1.0/0.10.

Usage: <venv>/bin/python tests/test_policy_tamponade.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import sutura_engine.pipeline.tiers.tamponade as tamp  # noqa: E402
from sutura_engine.policy import gate_paths  # noqa: E402


def test_tamponade_values_match_history():
    assert tamp.TAMP_COLLAPSE_BBOX_FRAC == 0.5
    assert tamp.TAMP_LEAK_TRIGGER_FRAC == 0.01
    assert tamp.TAMP_LEAK_MAX_FACES == 150000
    assert tamp.TAMP_SHAPE_COMPACTNESS == 0.10
    assert tamp.TAMP_SHAPE_OPEN_AREA == 0.10
    assert tamp.TAMP_SHAPE_THIN == 0.30
    assert tamp.TAMP_SHAPE_THICKNESS == 0.004
    assert tamp.TAMP_SHAPE_DEPTH_CELLS == 32768
    assert tamp.TAMP_SHAPE_PERIMETER_RATIO == 1.0
    assert tamp.TAMP_SHAPE_SOLID_AREA == 0.10
    assert tamp.TAMP_SHAPE_WINDING == 0.90
    assert tamp.TAMP_SHAPE_SIDE_SAMPLES == 20000
    assert tamp.TAMP_SHAPE_SIDE_EPS == 1e-4


def test_tamponade_types_are_preserved():
    for name in ('TAMP_LEAK_MAX_FACES', 'TAMP_SHAPE_DEPTH_CELLS',
                 'TAMP_SHAPE_SIDE_SAMPLES'):
        value = getattr(tamp, name)
        assert isinstance(value, int) and not isinstance(value, bool), (name, type(value))
    for name in ('TAMP_COLLAPSE_BBOX_FRAC', 'TAMP_LEAK_TRIGGER_FRAC',
                 'TAMP_SHAPE_COMPACTNESS', 'TAMP_SHAPE_OPEN_AREA',
                 'TAMP_SHAPE_THIN', 'TAMP_SHAPE_THICKNESS',
                 'TAMP_SHAPE_PERIMETER_RATIO', 'TAMP_SHAPE_SOLID_AREA',
                 'TAMP_SHAPE_WINDING', 'TAMP_SHAPE_SIDE_EPS'):
        assert isinstance(getattr(tamp, name), float), (name, type(getattr(tamp, name)))


def test_open_shell_thresholds_exact():
    assert tamp.TAMP_SHAPE_PERIMETER_RATIO == 1.0
    assert tamp.TAMP_SHAPE_SOLID_AREA == 0.10


def test_tamponade_gate_registered():
    assert 'tiers.tamponade.gates' in gate_paths()


if __name__ == '__main__':
    test_tamponade_values_match_history()
    test_tamponade_types_are_preserved()
    test_open_shell_thresholds_exact()
    test_tamponade_gate_registered()
    print('policy tamponade tests passed')
