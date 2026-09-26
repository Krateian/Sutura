#!/usr/bin/env python3
"""Triage Engine intensity-preset regression tests.

Checks that:
  1. sutura/triage.py is stdlib-only (importing it must not pull in numpy or
     pymeshlab).
  2. resolve_intensity honours the documented precedence
     CLI > SUTURA_INTENSITY > config.json > balanced, including invalid-value
     fall-through.
  3. The Balanced preset is the historical repair.py default: every field the
     module constants were derived from equals the constant (so
     --intensity balanced is byte-identical to the pre-triage behaviour).
  4. The preset table has the documented per-preset knobs (Quick has the
     fTetWild tier off and an empty ladder; Thorough/Extreme add the
     'threshold' rung; Extreme has no input-size cap and the optimising
     retry; no preset drops the Hausdorff sample count below 200000).
  5. The future user-profile hook overrides a base preset's fields.

Usage: ~/.local/share/sutura/venv/bin/python tests/test_triage.py
"""
import json
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import triage  # noqa: E402


def test_triage_is_stdlib_only():
    r = subprocess.run(
        [sys.executable, '-c',
         "import sys; sys.path.insert(0, %r); import triage; "
         "assert 'numpy' not in sys.modules, 'triage imported numpy'; "
         "assert 'pymeshlab' not in sys.modules, 'triage imported pymeshlab'; "
         "print('ok')" % SUTURA],
        capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    assert 'ok' in r.stdout, r.stdout


def test_resolve_precedence_and_fallthrough():
    with tempfile.TemporaryDirectory() as d:
        cfg = os.path.join(d, 'config.json')
        with open(cfg, 'w') as f:
            json.dump({'intensity': 'extreme'}, f)
        # config supplies the default
        assert triage.resolve_intensity(
            None, environ={}, config_path=cfg).name == 'extreme'
        # env beats config
        assert triage.resolve_intensity(
            None, environ={'SUTURA_INTENSITY': 'thorough'},
            config_path=cfg).name == 'thorough'
        # CLI beats env
        assert triage.resolve_intensity(
            'quick', environ={'SUTURA_INTENSITY': 'thorough'},
            config_path=cfg).name == 'quick'
        # invalid env falls through to config
        assert triage.resolve_intensity(
            None, environ={'SUTURA_INTENSITY': 'bogus'},
            config_path=cfg).name == 'extreme'
        # invalid config falls through to balanced
        with open(cfg, 'w') as f:
            json.dump({'intensity': 'nonsense'}, f)
        assert triage.resolve_intensity(
            None, environ={}, config_path=cfg).name == 'balanced'
        # missing config -> balanced
        assert triage.resolve_intensity(
            None, environ={}, config_path=os.path.join(d, 'nope.json')
        ).name == 'balanced'


def test_balanced_equals_historical_constants():
    import repair
    b = triage.PRESETS['balanced']
    assert repair.FTETWILD_TIMEOUT == b.ftetwild_timeout == 180
    assert repair.FTETWILD_MAX_FACES == b.ftetwild_max_faces == 300000
    assert repair.DENSE_RATIO == b.dense_ratio == 4
    assert repair.DENSE_MIN_FACES == b.dense_min_faces == 20000
    assert repair.DENSE_TARGET_LADDER == b.dense_target_ladder == (1.5, 3.0)
    assert repair.HAUSDORFF_SAMPLES == b.ftetwild_hausdorff_samples == 200000
    assert repair.DEEP_REPAIR_DEFAULT == b.deep_repair == 'full'
    assert b.ftetwild_enabled is True
    assert b.ftetwild_optimize_retry_on_dense_fail is False


def test_preset_table():
    q = triage.PRESETS['quick']
    assert q.ftetwild_enabled is False and q.deep_repair == 'off'
    assert q.dense_target_ladder == ()
    t = triage.PRESETS['thorough']
    assert t.dense_target_ladder == (1.5, 3.0, 'threshold')
    assert t.ftetwild_timeout == 600 and t.ftetwild_max_faces == 300000
    e = triage.PRESETS['extreme']
    assert e.ftetwild_max_faces is None
    assert e.dense_target_ladder == (1.5, 3.0, 'threshold')
    assert e.ftetwild_optimize_retry_on_dense_fail is True
    assert e.ftetwild_timeout == 1800
    for spec in triage.PRESETS.values():
        assert spec.ftetwild_hausdorff_samples >= 200000, spec.name
        assert spec.dense_ratio == 4 and spec.dense_min_faces == 20000


def test_user_profile_hook_overrides_fields():
    profiles = {'myfork': {'base': 'thorough', 'ftetwild_timeout': 42.0,
                           'ftetwild_max_faces': None}}
    s = triage.resolve_intensity('myfork', environ={}, user_profiles=profiles)
    assert s.name == 'myfork'
    assert s.ftetwild_timeout == 42.0 and s.ftetwild_max_faces is None
    # untouched fields come from the chosen base preset
    assert s.dense_target_ladder == (1.5, 3.0, 'threshold')


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('triage tests passed')


if __name__ == '__main__':
    main()
