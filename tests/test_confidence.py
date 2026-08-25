#!/usr/bin/env python3
"""Regression test for sutura/confidence.py (repair confidence scoring).

Checks that:
  1. confidence.py is stdlib-only (importing it must NOT pull in
     numpy/pymeshlab/manifold3d), so the GUI and the PyMeshLab CLI venv can
     both import it regardless of interpreter.
  2. repair_confidence() produces high/medium/low scores for synthetic
     reports (watertight, watertight-with-caveats, partial, error) and forces
     0/low on hard errors (malformed input, extreme_removed_object).
  3. estimate_confidence_pre_repair() produces sensible estimates from
     pre-repair signals and never crashes on missing keys (validate-style
     partial signal dicts).
Usage: python3 tests/test_confidence.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)


def test_stdlib_only():
    # import confidence in a fresh subprocess and assert it does not pull in
    # any heavy third-party library (stdlib+numpy rule); must run in a
    # subprocess because this module's own tests import repair (which pulls
    # numpy) for the gate-consistency checks.
    import subprocess
    code = (
        "import sys; sys.path.insert(0, %r); import confidence; "
        "bad=[m for m in ('numpy','pymeshlab','manifold3d') if m in sys.modules]; "
        "print('OK' if not bad else 'BAD:'+','.join(bad))" % SUTURA
    )
    out = subprocess.run([sys.executable, '-c', code],
                         capture_output=True, text=True)
    assert out.returncode == 0, out.stderr
    assert 'OK' in out.stdout, out.stdout + out.stderr


def test_high_watertight():
    from confidence import repair_confidence
    report = {
        'stage1': {'two_manifold': True, 'holes_remaining': 0},
        'stage2': {'ok': True},
        'detected_type': 'mechanical',
        'detected_confidence': 0.95,
        'tuning_applied': True,
        'repair_mode': 'auto',
        'defects': {'holes': []},
    }
    r = repair_confidence(report)
    assert r['score'] >= 80, r
    assert r['label'] == 'high', r
    assert 'stage2_watertight' in r['factors'], r


def test_medium_watertight_with_caveats():
    # watertight (stage 2 confirmed) but with a volume warning, aggressive
    # mode and input holes that were closed: lands in medium, not high.
    from confidence import repair_confidence
    report = {
        'stage1': {'two_manifold': True, 'holes_remaining': 0,
                   'volume_warning': 'Volume changed by 20%'},
        'stage2': {'ok': True},
        'detected_type': 'mechanical',
        'detected_confidence': 0.9,
        'tuning_applied': True,
        'repair_mode': 'aggressive',
        'defects': {'holes': [{'x': 1}, {'x': 2}]},
    }
    r = repair_confidence(report)
    assert 50 <= r['score'] < 80, r
    assert r['label'] == 'medium', r
    assert 'volume_warning' in r['factors'], r
    assert 'aggressive_mode' in r['factors'], r


def test_low_partial():
    from confidence import repair_confidence
    report = {
        'stage1': {'two_manifold': True, 'holes_remaining': 5},
        'detected_type': 'unknown',
        'detected_confidence': 0.4,
        'tuning_applied': False,
        'repair_mode': 'extreme',
        'defects': {'holes': [{'x': 1}] * 8},
    }
    r = repair_confidence(report)
    assert r['score'] < 50, r
    assert r['label'] == 'low', r
    assert 'holes_remaining' in r['factors'], r


def test_malformed_error_forced_low():
    from confidence import repair_confidence
    report = {'input': 'x.stl', 'error': 'repair failed: malformed STL'}
    r = repair_confidence(report)
    assert r['score'] == 0, r
    assert r['label'] == 'low', r
    assert 'category_error' in r['factors'], r


def test_extreme_removed_object_forced_low():
    from confidence import repair_confidence
    report = {'error': 'Extreme mode removed all geometry',
              'extreme_removed_object': True}
    r = repair_confidence(report)
    assert r['score'] == 0, r
    assert r['label'] == 'low', r


def test_stage2_skipped_drops_score():
    from confidence import repair_confidence
    report = {
        'stage1': {'two_manifold': True, 'holes_remaining': 0},
        'stage2': {'error': 'Stage 2 skipped: manifold3d not available in this environment.'},
        'detected_type': 'mechanical',
        'detected_confidence': 0.9,
        'tuning_applied': True,
        'repair_mode': 'auto',
        'defects': {'holes': []},
    }
    r = repair_confidence(report)
    assert r['label'] == 'low', r
    assert 'stage2_skipped' in r['factors'], r
    # a stage-1-closed but unconfirmed mesh must score far below watertight
    assert r['score'] < 50, r


def test_pre_repair_clean_high():
    from confidence import estimate_confidence_pre_repair
    signals = {
        'detected_type': 'mechanical',
        'detected_confidence': 0.9,
        'tuning_applied': True,
        'mode': 'auto',
        'holes': 0,
        'non_manifold': 0,
        'self_intersections': 0,
        'stage2_bridge_available': True,
    }
    r = estimate_confidence_pre_repair(signals)
    assert r['score'] >= 80, r
    assert r['label'] == 'high', r


def test_pre_repair_broken_low():
    from confidence import estimate_confidence_pre_repair
    signals = {
        'detected_type': 'unknown',
        'detected_confidence': 0.3,
        'tuning_applied': False,
        'mode': 'extreme',
        'holes': 8,
        'non_manifold': 2,
        'self_intersections': 5,
        'stage2_bridge_available': False,
    }
    r = estimate_confidence_pre_repair(signals)
    assert r['score'] < 50, r
    assert r['label'] == 'low', r
    assert 'stage2_unavailable' in r['factors'], r


def test_pre_repair_missing_keys_ok():
    # validate-style partial dict (no mode/tuning) and even an empty dict
    # must not crash: missing signals are simply ignored.
    from confidence import estimate_confidence_pre_repair
    partial = {
        'detected_type': 'organic',
        'detected_confidence': 0.6,
        'holes': 2,
        'self_intersections': 0,
        'stage2_bridge_available': True,
    }
    r = estimate_confidence_pre_repair(partial)
    assert 50 <= r['score'] <= 100, r
    assert r['label'] in ('high', 'medium'), r

    empty = estimate_confidence_pre_repair({})
    assert empty['score'] == 70, empty
    assert empty['label'] == 'medium', empty


def test_gates_match_repair():
    # regression: confidence.py must source the class-specific tuning gates
    # from repair.py (single source of truth). A stale hardcoded copy here
    # silently diverged before (organic 0.55 vs repair's 0.70) and made
    # dry-run/validate disagree with the real repair on 'below_confidence_gate';
    # this test makes that impossible again.
    from confidence import _tuning_gates
    from repair import MECH_TUNE_GATE, ORG_TUNE_GATE
    mech_gate, org_gate = _tuning_gates()
    assert mech_gate == MECH_TUNE_GATE, (mech_gate, MECH_TUNE_GATE)
    assert org_gate == ORG_TUNE_GATE, (org_gate, ORG_TUNE_GATE)


def test_pre_repair_uses_repair_gates():
    # behavioral: the below-gate penalty must kick in exactly at repair.py's
    # ORG_TUNE_GATE (not at some stale hardcoded value).
    from confidence import estimate_confidence_pre_repair
    from repair import ORG_TUNE_GATE
    below = estimate_confidence_pre_repair({
        'detected_type': 'organic',
        'detected_confidence': ORG_TUNE_GATE - 0.01})
    assert 'below_confidence_gate' in below['factors'], below
    at = estimate_confidence_pre_repair({
        'detected_type': 'organic',
        'detected_confidence': ORG_TUNE_GATE})
    assert 'below_confidence_gate' not in at['factors'], at


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('confidence tests passed')


if __name__ == '__main__':
    main()