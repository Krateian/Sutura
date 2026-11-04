#!/usr/bin/env python3
"""Opt-in auto-reorder (--auto-reorder): ordering + baseline safety.

Checks:
  1. ``triage._auto_reorder_recs`` orders the fallback recs by the suggestion
     list, keeps the same set, and appends methods the suggestions omit.
  2. The CLI accepts ``--auto-reorder`` and, on a clean mesh whose baseline is
     already watertight, the saved output is byte-identical to the run without
     the flag (the baseline pipeline is untouched).

Usage: <repair-venv>/bin/python tests/test_auto_reorder.py
"""
import hashlib
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
REPAIR_PY = os.path.join(SUTURA, 'repair.py')
sys.path.insert(0, SUTURA)

from sutura_engine import triage  # noqa: E402


class _Rec:
    def __init__(self, num):
        self.num = num
        self.id = str(num)
        self.name = 'm%d' % num
        self.score = 0.5


class _FakeMethods:
    @staticmethod
    def suggest_methods(analysis, top_n=3):
        return [{'num': 13}, {'num': 7}, {'num': 3}]


def test_reorder_follows_suggestions_and_keeps_set():
    orig = triage._methods_mod
    triage._methods_mod = lambda: _FakeMethods
    try:
        recs = [_Rec(3), _Rec(7), _Rec(13), _Rec(1)]
        out = triage._auto_reorder_recs(recs, object())
    finally:
        triage._methods_mod = orig
    # suggestion order first (13,7,3), omitted method 1 last
    assert [r.num for r in out] == [13, 7, 3, 1], [r.num for r in out]
    assert sorted(r.num for r in out) == [1, 3, 7, 13]


def test_reorder_never_raises():
    class Boom:
        @staticmethod
        def suggest_methods(*_a, **_k):
            raise RuntimeError('boom')
    orig = triage._methods_mod
    triage._methods_mod = lambda: Boom
    try:
        recs = [_Rec(3), _Rec(7)]
        assert [r.num for r in triage._auto_reorder_recs(recs, object())] == [3, 7]
    finally:
        triage._methods_mod = orig


def _sha(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        h.update(f.read())
    return h.hexdigest()


def test_cli_flag_is_baseline_safe(tmp):
    src = os.path.join(tmp, 'broken.stl')
    subprocess.run([sys.executable, os.path.join(REPO, 'tests', 'make_broken_stl.py'), src],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    env = dict(os.environ)
    env['SUTURA_DATA_DIR'] = os.path.join(tmp, 'data')
    env['XDG_CACHE_HOME'] = os.path.join(tmp, 'cache')
    a = os.path.join(tmp, 'a.stl')
    b = os.path.join(tmp, 'b.stl')
    for out, flag in ((a, []), (b, ['--auto-reorder'])):
        r = subprocess.run([sys.executable, REPAIR_PY, src, '-o', out,
                            '--no-history', '--json'] + flag,
                           capture_output=True, text=True, timeout=600, env=env)
        assert r.returncode == 0, (flag, r.stderr[-400:])
        assert os.path.isfile(out), (flag, r.stdout[-400:])
    assert _sha(a) == _sha(b), 'flag must not change a watertight baseline output'


def main():
    with tempfile.TemporaryDirectory(prefix='sutura-autoreorder-') as tmp:
        for name, fn in sorted(globals().items()):
            if name.startswith('test_') and callable(fn):
                if name == 'test_cli_flag_is_baseline_safe':
                    fn(tmp)
                else:
                    fn()
                print('ok  %s' % name)
    print('auto-reorder tests passed')


if __name__ == '__main__':
    main()
