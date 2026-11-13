#!/usr/bin/env python3
"""Unit tests for the core150 judge fixes in verify_default_vs_optout.py.

Rule 1: skip the volume-error criterion on a self-intersecting input or a
degenerate input GWN volume, and say so in the reason.
Rule 2: "fewer closed shells" is only a regression when the closed-shell
volume missing from the candidate is more than 0.1 % of the input volume.
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
        'tolerance_mm': 0.01,
    }
    d.update(kw)
    return d


def test_volume_error_skipped_on_si_input():
    b = _metrics(input_si=10, volume_error_rel=2.0)
    c = _metrics(volume_error_rel=3.0)
    assert not any('volume error' in r for r in V._shape_regressions(b, c, TOL))
    verdict, reason = V.judge_quality(b, c, TOL)
    assert verdict == 'SAME', (verdict, reason)
    assert 'volume error skipped (input self-intersections' in reason, reason


def test_volume_error_skipped_on_degenerate_gwn():
    b = _metrics(input_si=0, input_gwn_volume_mm3=1e-9,
                 tolerance_mm=0.01, volume_error_rel=2.0)
    c = _metrics(volume_error_rel=3.0)
    assert not any('volume error' in r for r in V._shape_regressions(b, c, TOL))
    verdict, reason = V.judge_quality(b, c, TOL)
    assert verdict == 'SAME', (verdict, reason)
    assert 'GWN volume degenerate' in reason, reason


def test_fewer_shells_real_lost_part_is_worse():
    b = _metrics(closed_shells=2, shell_volumes=[100.0, 10.0],
                 input_gwn_volume_mm3=1000.0)
    c = _metrics(closed_shells=1, shell_volumes=[100.0])
    regs = V._shape_regressions(b, c, TOL)
    assert any('fewer closed shells' in r and '0.1%' in r for r in regs), regs
    assert V.judge_quality(b, c, TOL)[0] == 'WORSE'


def test_fewer_shells_small_artifact_is_not_worse():
    b = _metrics(closed_shells=2, shell_volumes=[100.0, 0.5],
                 input_gwn_volume_mm3=1000.0)
    c = _metrics(closed_shells=1, shell_volumes=[100.0])
    assert not any('fewer closed shells' in r
                   for r in V._shape_regressions(b, c, TOL))
    verdict, reason = V.judge_quality(b, c, TOL)
    assert verdict == 'SAME', (verdict, reason)
    assert 'fewer closed shells accepted' in reason, reason


def test_fewer_shells_merged_volume_is_not_worse():
    b = _metrics(closed_shells=2, shell_volumes=[100.0, 10.0],
                 input_gwn_volume_mm3=1000.0)
    c = _metrics(closed_shells=1, shell_volumes=[110.0])
    assert not any('fewer closed shells' in r
                   for r in V._shape_regressions(b, c, TOL))


def test_fewer_shells_summary_fallback_uses_volume_error():
    b = _metrics(closed_shells=2, volume_error_rel=0.01)
    c = _metrics(closed_shells=1, volume_error_rel=0.0)
    assert not any('fewer closed shells' in r
                   for r in V._shape_regressions(b, c, TOL))
    c2 = _metrics(closed_shells=1, volume_error_rel=0.02)
    assert any('fewer closed shells' in r
               for r in V._shape_regressions(b, c2, TOL))


def test_zero_shells_still_worse_without_volumes():
    b = _metrics(closed_shells=1)
    c = _metrics(closed_shells=0)
    assert V.judge_quality(b, c, TOL)[0] == 'WORSE'


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok ', name)
    print('judge core150 tests passed')
