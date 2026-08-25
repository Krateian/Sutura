#!/usr/bin/env python3
"""Unit tests for sutura/gui.py's mode_suggestion_keys().

Checks the design rules of the mode-suggestion system:
  1. low-confidence suggestion only fires while the mode is still
     low/medium/auto (no point suggesting a step up when already on
     aggressive/extreme).
  2. the extreme caveat fires only when extreme was suggested AND the mesh
     is small enough (< 20 faces) that extreme could actually delete it
     (the extreme_removed_object risk); a missing face count (e.g. a
     multi-object 3MF aggregate) never fires it.
  3. at most three suggestions, plus optionally one caveat as a 4th line.
  4. no matching signal -> empty list (no noise).

Run with a Python that has PySide6 (e.g. the sutura venv), with
QT_QPA_PLATFORM=offscreen so no display is needed.
Usage: QT_QPA_PLATFORM=offscreen ~/.local/share/sutura/venv/bin/python tests/test_suggestions.py
"""
import os
import sys

os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

from gui import mode_suggestion_keys  # noqa: E402


def _analysis(**kw):
    """Minimal analysis dict; caller overrides the fields it cares about."""
    base = {
        'repair_mode': 'auto',
        'detected_type': 'mechanical',
        'detected_confidence': 0.9,
        'tuning_applied': True,
        'holes_found': 0,
        'self_intersections': 0,
        'non_manifold_regions': 0,
        'connected_components': 1,
        'validation_faces': 1000,
        'watertight': False,
        'estimated_confidence_label': 'high',
    }
    base.update(kw)
    return base


def test_no_signals_is_empty():
    assert mode_suggestion_keys(_analysis()) == []


def test_low_conf_fires_on_gentle_modes():
    for mode in ('low', 'medium', 'auto'):
        keys = mode_suggestion_keys(_analysis(estimated_confidence_label='low',
                                              repair_mode=mode))
        assert 'sug_low_conf' in keys, mode


def test_low_conf_silent_on_aggressive_modes():
    for mode in ('aggressive', 'extreme'):
        keys = mode_suggestion_keys(_analysis(estimated_confidence_label='low',
                                              repair_mode=mode))
        assert 'sug_low_conf' not in keys, mode


def test_low_conf_fires_when_mode_missing():
    a = _analysis(estimated_confidence_label='low')
    del a['repair_mode']
    assert 'sug_low_conf' in mode_suggestion_keys(a)


def test_caveat_requires_small_mesh():
    # extreme suggested (self-intersections) on a large mesh: no caveat
    keys = mode_suggestion_keys(_analysis(self_intersections=5,
                                          validation_faces=5000))
    assert 'sug_si_few' in keys
    assert 'sug_extreme_caveat' not in keys


def test_caveat_fires_on_small_mesh():
    keys = mode_suggestion_keys(_analysis(self_intersections=5,
                                          validation_faces=13))
    assert 'sug_extreme_caveat' in keys


def test_caveat_fires_without_other_suggestion_when_small():
    # holes_many also flags extreme; a small mesh still gets the caveat
    keys = mode_suggestion_keys(_analysis(holes_found=12,
                                          validation_faces=13))
    assert 'sug_holes_many' in keys
    assert 'sug_extreme_caveat' in keys


def test_caveat_needs_extreme_fired():
    # holes_few does NOT suggest extreme; caveat must not fire even on a
    # tiny mesh
    keys = mode_suggestion_keys(_analysis(holes_found=4,
                                          validation_faces=13))
    assert 'sug_holes_few' in keys
    assert 'sug_extreme_caveat' not in keys


def test_caveat_silent_when_face_count_missing():
    # multi-object 3MF aggregate has no validation_faces: stay silent
    a = _analysis(self_intersections=3)
    del a['validation_faces']
    keys = mode_suggestion_keys(a)
    assert 'sug_si_few' in keys
    assert 'sug_extreme_caveat' not in keys


def test_capped_at_three_suggestions():
    # every non-extreme signal fires: still only three keys
    keys = mode_suggestion_keys(_analysis(
        holes_found=5, non_manifold_regions=4, connected_components=3,
        watertight=False))
    assert len(keys) == 3


def test_caveat_is_fourth_line():
    keys = mode_suggestion_keys(_analysis(
        self_intersections=12, holes_found=5, non_manifold_regions=4,
        connected_components=3, validation_faces=13))
    assert len(keys) == 4
    assert keys[-1] == 'sug_extreme_caveat'


def test_priority_order():
    # self-intersection and holes come before the lower-priority signals
    keys = mode_suggestion_keys(_analysis(
        self_intersections=2, holes_found=5, non_manifold_regions=4))
    assert keys == ['sug_si_few', 'sug_holes_few', 'sug_nm']


def test_watertight_suggestion():
    keys = mode_suggestion_keys(_analysis(watertight=True))
    assert keys == ['sug_watertight']


def test_unknown_type_suggestion():
    keys = mode_suggestion_keys(_analysis(detected_type='unknown',
                                          tuning_applied=False))
    assert 'sug_unknown' in keys


if __name__ == '__main__':
    tests = [(n, f) for n, f in sorted(globals().items())
             if n.startswith('test_') and callable(f)]
    failed = 0
    for name, fn in tests:
        try:
            fn()
            print('ok ', name)
        except AssertionError as e:
            failed += 1
            print('FAIL %s: %s' % (name, e))
    if failed:
        print('%d/%d tests failed' % (failed, len(tests)))
        sys.exit(1)
    print('all %d tests passed' % len(tests))