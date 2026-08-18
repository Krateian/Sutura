#!/usr/bin/env python3
"""Regression test for sutura/classification.py (batch repair summary).

Checks that:
  1. classification.py is stdlib-only (importing it must NOT pull in
     numpy/pymeshlab/manifold3d), so the GUI and the PyMeshLab CLI venv can
     both import it regardless of interpreter.
  2. classify() produces the expected categories/issues for the documented
     scenarios (watertight, volume warning, stage 2 skipped, partial,
     malformed error, stage 2 error).
Usage: python3 tests/test_classification.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)


def test_stdlib_only():
    import classification  # noqa: F401
    for heavy in ('numpy', 'pymeshlab', 'manifold3d'):
        assert heavy not in sys.modules, (
            'classification import pulled in %s; it must stay stdlib-only' % heavy)


def test_watertight():
    from classification import classify
    data = {
        'stage1': {'two_manifold': True, 'holes_remaining': 0},
        'stage2': {'ok': True},
    }
    cat, issues, key = classify(data)
    assert cat == 'watertight', cat
    assert issues == []
    assert key == 'watertight'


def test_volume_warning():
    from classification import classify
    data = {
        'stage1': {'two_manifold': True, 'holes_remaining': 0,
                   'volume_warning': 'Volume changed by 20%'},
        'stage2': {'ok': True},
    }
    cat, issues, _ = classify(data)
    # a volume warning on an otherwise-watertight mesh stays watertight
    assert cat == 'watertight', cat


def test_stage2_skipped_is_warning():
    from classification import classify
    data = {
        'stage1': {'two_manifold': True, 'holes_remaining': 0},
        'stage2': {'error': 'Stage 2 skipped: manifold3d not available in this environment.'},
    }
    cat, issues, key = classify(data)
    assert cat == 'warning', cat
    assert 'stage2_skipped' in issues
    assert key == 'stage2_skipped'


def test_stage2_error():
    from classification import classify
    data = {
        'stage1': {'two_manifold': True, 'holes_remaining': 0},
        'stage2': {'error': 'manifold3d could not process the input'},
    }
    cat, issues, key = classify(data)
    assert cat == 'warning', cat
    assert 'stage2_error' in issues
    assert key == 'stage2_error'


def test_no_stage2_never_watertight():
    # a watertight-looking stage1 with no stage2 at all (bridge missing /
    # macOS in-process not available) must NOT be watertight
    from classification import classify
    data = {
        'stage1': {'two_manifold': True, 'holes_remaining': 0},
    }
    cat, issues, key = classify(data)
    assert cat == 'warning', cat
    assert 'stage2_skipped' in issues
    assert key == 'stage2_skipped'


def test_partial_holes():
    import classification
    from classification import classify
    data = {
        'stage1': {'two_manifold': True, 'holes_remaining': 3},
    }
    cat, issues, key = classify(data)
    assert cat == 'warning', cat
    assert 'partial' in issues
    assert key == 'holes'
    assert classification.summary_args(data) == (3,)


def test_malformed_error():
    from classification import classify
    data = {'input': 'x.stl', 'error': 'repair failed: malformed STL'}
    cat, issues, key = classify(data)
    assert cat == 'error', cat
    assert 'malformed' in issues
    assert key == 'error'


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('classification tests passed')


if __name__ == '__main__':
    main()
