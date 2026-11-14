#!/usr/bin/env python3
"""Regression tests for the CI verification verdicts and numeric shard log.

Covers the harness changes that keep a repair timeout separate from a crash
and from a generic no-output error (rc=124 -> TIMEOUT, rc<0 -> CRASHED, else
ERROR), and the numeric-only per-side shard log.
"""
import importlib.util
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
VERIFY_DIR = os.path.join(REPO, '.claude', 'skills', 'sutura-verify', 'scripts')
if VERIFY_DIR not in sys.path:
    sys.path.insert(0, VERIFY_DIR)


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_no_output_verdict_separates_timeout_crash_error():
    v = _load('verify_default_vs_optout',
              os.path.join(VERIFY_DIR, 'verify_default_vs_optout.py'))
    assert v.no_output_verdict([('baseline', 0), ('candidate', 124)], 900) == (
        'TIMEOUT', 'repair exceeded 900s (candidate)')
    assert v.no_output_verdict([('baseline', 0), ('candidate', -9)], 900) == (
        'CRASHED', 'crashed (signal 9, candidate)')
    verdict, reason = v.no_output_verdict(
        [('baseline', 1), ('candidate', 1)], 900)
    assert verdict == 'ERROR', (verdict, reason)
    assert 'baseline rc=1' in reason and 'candidate rc=1' in reason


def test_numeric_shard_log_has_no_names():
    s = _load('gh_mesh_shard', os.path.join(REPO, 'scripts', 'gh_mesh_shard.py'))
    items = [
        {'name': 'artec_secret-name.stl',
         'baseline': {'rc': 0, 'time_s': 1.234},
         'candidate': {'rc': 124, 'time_s': 900.0}},
        {'name': 'another/private.stl',
         'baseline': {'rc': 0},
         'candidate': {}},
    ]
    lines = s.numeric_log_lines(items)
    assert lines == ['1 0 0 1234', '1 1 124 900000', '2 0 0 -1', '2 1 -1 -1'], lines
    joined = '\n'.join(lines)
    assert '.stl' not in joined and '/' not in joined
    assert all(part.lstrip('-').isdigit()
               for line in lines for part in line.split())


def test_shard_row_keeps_numeric_rc_and_seconds():
    # The shard summary must retain rc/seconds even though the row's metric
    # payload is filtered to the judge keys.
    src = open(os.path.join(REPO, 'scripts', 'gh_mesh_shard.py')).read()
    assert "'rc': {side:" in src
    assert "'seconds': {side:" in src


if __name__ == '__main__':
    test_no_output_verdict_separates_timeout_crash_error()
    test_numeric_shard_log_has_no_names()
    test_shard_row_keeps_numeric_rc_and_seconds()
    print('ci verdict/log tests passed')
