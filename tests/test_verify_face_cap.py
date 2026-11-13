#!/usr/bin/env python3
"""Face-cap SI skip in the quality judge.

When exact SI-after is unavailable on BOTH sides because the mesh exceeds
sutura_geom.SI_MAX_FACES, the judge skips the SI criterion, judges every other
metric as usual, and marks the row 'SI n/a (face cap)'. One capped side, or a
missing count for any other reason, stays ERROR.

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
    spec = importlib.util.spec_from_file_location('verify_default_vs_optout_cap',
                                                  SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)
    return mod


V = _load()
TOL = 0.05
CAP_NOTE = 'skipped (face cap)'


def _metrics(**kw):
    d = {
        'reload_holes': 0,
        'reload_non_manifold_edges': 0,
        'si_output': 0,
        'si_output_note': None,
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


def _capped(**kw):
    return _metrics(si_output=None, si_output_note=CAP_NOTE, **kw)


def test_both_capped_is_judged_without_si():
    b = _capped()
    c = _capped()
    verdict, reason = V.judge_quality(b, c, TOL)
    assert verdict == 'SAME', (verdict, reason)
    assert V.SI_FACE_CAP_MARKER in reason, reason


def test_both_capped_still_reports_worse():
    # Shape regression must win over the SI skip.
    b = _capped()
    c = _capped(reload_holes=3)
    verdict, reason = V.judge_quality(b, c, TOL)
    assert verdict == 'WORSE', (verdict, reason)
    assert 'holes' in reason, reason


def test_both_capped_still_reports_better():
    b = _capped(two_sided_max_mm=0.4)
    c = _capped(two_sided_max_mm=0.1)
    verdict, reason = V.judge_quality(b, c, TOL)
    assert verdict == 'BETTER', (verdict, reason)
    assert V.SI_FACE_CAP_MARKER in reason, reason


def test_both_capped_never_reports_trade_si():
    # No SI delta is available, so TRADE-SI is not a possible verdict.
    b = _capped()
    c = _capped(two_sided_max_mm=0.09)
    verdict, _reason = V.judge_quality(b, c, TOL)
    assert verdict != 'TRADE-SI', verdict


def test_both_capped_changed_is_still_changed():
    b = _capped(md5='a')
    c = _capped(md5='b')
    verdict, _reason = V.judge_quality(b, c, TOL)
    assert verdict == 'CHANGED', verdict


def test_one_sided_cap_is_error():
    b = _capped()
    c = _metrics(si_output=3)          # candidate measured fine
    verdict, reason = V.judge_quality(b, c, TOL)
    assert verdict == 'ERROR', (verdict, reason)
    assert 'unavailable' in reason, reason


def test_other_capped_side_is_error():
    b = _metrics(si_output=3)
    c = _capped()
    verdict, reason = V.judge_quality(b, c, TOL)
    assert verdict == 'ERROR', (verdict, reason)


def test_other_reason_for_missing_si_is_error():
    # Both sides missing, but NOT because of the face cap.
    b = _metrics(si_output=None, si_output_note='si failed: boom')
    c = _metrics(si_output=None, si_output_note='si failed: boom')
    verdict, reason = V.judge_quality(b, c, TOL)
    assert verdict == 'ERROR', (verdict, reason)
    assert 'hard fail' in reason, reason


def test_one_capped_one_other_reason_is_error():
    b = _capped()
    c = _metrics(si_output=None, si_output_note='si failed: boom')
    verdict, reason = V.judge_quality(b, c, TOL)
    assert verdict == 'ERROR', (verdict, reason)


def test_note_must_match_exactly():
    b = _metrics(si_output=None, si_output_note='skipped (face cap) extra')
    c = _capped()
    verdict, _reason = V.judge_quality(b, c, TOL)
    assert verdict == 'ERROR', verdict


def test_no_note_at_all_is_error():
    b = _metrics(si_output=None, si_output_note=None)
    c = _metrics(si_output=None, si_output_note=None)
    verdict, _reason = V.judge_quality(b, c, TOL)
    assert verdict == 'ERROR', verdict


def test_ordinary_runs_are_unaffected():
    # Metric present on both sides: unchanged behaviour.
    verdict, _ = V.judge_quality(_metrics(si_output=0), _metrics(si_output=0), TOL)
    assert verdict == 'SAME', verdict
    verdict, reason = V.judge_quality(_metrics(si_output=0),
                                      _metrics(si_output=6), TOL)
    assert verdict == 'TRADE-SI', (verdict, reason)


def test_helper_only_skips_when_both_sides_capped():
    assert V.si_judged_without(_capped(), _capped()) == 'face cap'
    assert V.si_judged_without(_capped(), _metrics()) is None
    assert V.si_judged_without(_metrics(), _metrics()) is None


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('face-cap SI skip tests passed')
