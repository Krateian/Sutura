#!/usr/bin/env python3
"""Step 6 sub-step 1 policy gate: the intensity presets come from
``[intensity]`` with unchanged values/types, the nullable cap uses the strict
"none" sentinel, and ``auto.generative_max_hausdorff_rel`` is a locked gate.

Usage: <venv>/bin/python tests/test_policy_intensity.py
"""
import os
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import sutura_engine.policy as pol  # noqa: E402
import triage  # noqa: E402
from sutura_engine.policy import gate_paths  # noqa: E402


def test_presets_match_history():
    q = triage.PRESETS['quick']
    assert q.ftetwild_enabled is False
    assert q.ftetwild_max_faces is None
    assert q.ftetwild_timeout == 180.0
    assert q.deep_repair == 'off'
    assert q.dense_target_ladder == ()

    b = triage.PRESETS['balanced']
    assert b.ftetwild_enabled is True
    assert b.ftetwild_max_faces == 300000
    assert b.graft_max_faces == 2000000
    assert b.ftetwild_timeout == 180.0
    assert b.graft_timeout == 300.0
    assert b.deep_repair == 'full'
    assert b.dense_target_ladder == (1.5, 3.0)
    assert b.dense_ratio == 4
    assert b.dense_min_faces == 20000
    assert b.ftetwild_hausdorff_samples == 200000

    t = triage.PRESETS['thorough']
    assert t.ftetwild_timeout == 600.0
    assert t.graft_timeout == 900.0
    assert t.dense_target_ladder == (1.5, 3.0, 'threshold')
    assert t.ftetwild_hausdorff_samples == 400000

    e = triage.PRESETS['extreme']
    assert e.ftetwild_max_faces is None
    assert e.graft_max_faces is None
    assert e.ftetwild_timeout == 1800.0
    assert e.graft_timeout == 1800.0
    assert e.ftetwild_optimize_retry_on_dense_fail is True
    assert e.dense_target_ladder == (1.5, 3.0, 'threshold')
    assert e.ftetwild_hausdorff_samples == 1000000

    assert triage.DEFAULT_INTENSITY == 'balanced'
    assert tuple(triage.PRESETS) == triage.INTENSITIES


def test_preset_types_preserved():
    b = triage.PRESETS['balanced']
    assert isinstance(b.ftetwild_max_faces, int) and not isinstance(
        b.ftetwild_max_faces, bool)
    assert isinstance(b.dense_ratio, int)
    assert isinstance(b.ftetwild_timeout, float)
    for name in triage.INTENSITIES:
        ladder = triage.PRESETS[name].dense_target_ladder
        assert isinstance(ladder, tuple), (name, type(ladder))
    assert isinstance(triage.PRESETS['quick'].ftetwild_enabled, bool)


def test_none_sentinel_strict():
    assert triage.PRESETS['quick'].ftetwild_max_faces is None
    text = open(pol.DEFAULT_POLICY_PATH, encoding='utf-8').read()
    bad = text.replace('ftetwild_max_faces = "none"', 'ftetwild_max_faces = "None"', 1)
    assert bad != text
    fd, path = tempfile.mkstemp(suffix='.toml')
    try:
        with os.fdopen(fd, 'w', encoding='utf-8') as f:
            f.write(bad)
        try:
            pol.load_policy(path)
            raised = False
        except ValueError:
            raised = True
        assert raised, 'a bad cap sentinel must be rejected'
    finally:
        os.unlink(path)


def test_auto_key_locked():
    assert 'auto.generative_max_hausdorff_rel' in gate_paths()


if __name__ == '__main__':
    test_presets_match_history()
    test_preset_types_preserved()
    test_none_sentinel_strict()
    test_auto_key_locked()
    print('policy intensity tests passed')
