#!/usr/bin/env python3
"""Unit tests for the TRADE-SI verdict in verify_default_vs_optout.py.

A rise in SI-after with every shape metric equal or better is a deliberate
SI-for-shape trade: it must be its own verdict (reported, non-blocking), not
WORSE. Any shape regression must still be WORSE.

Needs numpy only; the script is loaded by path because the sutura-verify
scripts directory is not a package.
"""
import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, '..', '.claude', 'skills', 'sutura-verify',
                      'scripts', 'verify_default_vs_optout.py')


def _load():
    spec = importlib.util.spec_from_file_location('verify_default_vs_optout',
                                                  SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)
    return mod


V = _load()
TOL = 0.05


def _metrics(**kw):
    """A complete baseline/candidate metric record with sane defaults."""
    d = {
        'reload_holes': 0,
        'reload_non_manifold_edges': 0,
        'si_output': 0,
        'two_sided_max_mm': 0.1,
        'two_sided_p95_mm': 0.05,
        'volume_error_rel': 0.0,
        'orca_number_of_parts': 1,
        'components': 1,
        'closed_shells': 1,
        'leak_max_mm': 0.0,
        'leak_count': 0,
        'md5': 'a',
    }
    d.update(kw)
    return d


def test_si_rise_with_equal_shape_is_trade_si():
    b = _metrics(si_output=0)
    c = _metrics(si_output=6)
    verdict, reason = V.judge_quality(b, c, TOL)
    assert verdict == 'TRADE-SI', (verdict, reason)
    assert '+6' in reason, reason


def test_si_rise_with_better_shape_is_trade_si():
    # Visible two-sided distance improved while SI rose: still a trade, and
    # it must not be upgraded to BETTER (which would hide the SI loss).
    b = _metrics(si_output=0, two_sided_max_mm=0.4337)
    c = _metrics(si_output=6, two_sided_max_mm=0.2264)
    verdict, reason = V.judge_quality(b, c, TOL)
    assert verdict == 'TRADE-SI', (verdict, reason)


def test_si_rise_with_shape_regression_is_worse():
    b = _metrics(si_output=0)
    c = _metrics(si_output=6, reload_holes=1)
    verdict, reason = V.judge_quality(b, c, TOL)
    assert verdict == 'WORSE', (verdict, reason)
    assert 'holes' in reason, reason


def test_si_trade_does_not_mask_distance_regression():
    b = _metrics(si_output=0, two_sided_max_mm=0.1)
    c = _metrics(si_output=6, two_sided_max_mm=0.1 + TOL + 1e-6)
    verdict, reason = V.judge_quality(b, c, TOL)
    assert verdict == 'WORSE', (verdict, reason)
    assert 'two-sided max' in reason, reason


def test_si_trade_does_not_mask_new_leak():
    b = _metrics(si_output=0)
    c = _metrics(si_output=6, leak_count=3, leak_max_mm=0.8)
    verdict, reason = V.judge_quality(b, c, TOL)
    assert verdict == 'WORSE', (verdict, reason)
    assert 'leak' in reason, reason


def test_shape_regression_without_si_rise_is_worse():
    b = _metrics(si_output=0)
    c = _metrics(si_output=0, closed_shells=0)
    verdict, reason = V.judge_quality(b, c, TOL)
    assert verdict == 'WORSE', (verdict, reason)
    assert 'shells' in reason, reason


def test_si_drop_is_still_better():
    b = _metrics(si_output=6)
    c = _metrics(si_output=0)
    verdict, _reason = V.judge_quality(b, c, TOL)
    assert verdict == 'BETTER', verdict


def test_trade_si_is_not_a_failing_exit_code():
    # The runner's failure set is explicit; TRADE-SI must not be in it.
    src = open(SCRIPT).read()
    fail_line = next(l for l in src.splitlines()
                     if "'VISUAL-WORSE'" in l and 'bad =' in l)
    assert 'TRADE-SI' not in fail_line, fail_line


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('trade-si verdict tests passed')
