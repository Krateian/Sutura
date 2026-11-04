#!/usr/bin/env python3
"""Tests for the shared crash log (sutura/diagnostics.py).

Covers: the log lives in the runtime user-data directory, is bounded to its
tail, faulthandler is enabled into it, its tail is scrubbed, and the
--diagnostics archive carries it (only when present) with a registry-derived
method count.

Usage: python3 tests/test_crashlog.py (stdlib only; a temp HOME/DATA_DIR
isolates the run from real user data).
"""
import getpass
import os
import sys
import tempfile
import zipfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, 'sutura'))

# Isolate every path BEFORE importing diagnostics (paths.DEFAULT_DATA_DIR is
# computed at import time from HOME).
_TMP = tempfile.mkdtemp(prefix='sutura-crashlog-')
os.environ['HOME'] = _TMP
os.environ['USER'] = 'testuser'
os.environ['LOGNAME'] = 'testuser'
os.environ['SUTURA_DATA_DIR'] = os.path.join(_TMP, 'data')

import diagnostics  # noqa: E402

_HOME = os.path.expanduser('~')


def _reset_log():
    p = diagnostics.crash_log_path()
    if os.path.exists(p):
        os.remove(p)
    return p


def _write_log(text):
    p = diagnostics.crash_log_path()
    os.makedirs(os.path.dirname(p), exist_ok=True)
    with open(p, 'w', encoding='utf-8') as f:
        f.write(text)
    return p


def test_crash_log_path_respects_env():
    assert diagnostics.crash_log_path() == \
        os.path.join(_TMP, 'data', 'crash.log')


def test_trim_keeps_tail_and_bounds():
    p = _write_log('OLD\n' + ('x' * 1000) + '\nNEWEST\n')
    diagnostics._trim_crash_log(p, max_bytes=100)
    with open(p, encoding='utf-8') as f:
        data = f.read()
    assert 'NEWEST' in data
    assert 'OLD' not in data
    # bounded: tail + one short marker line
    assert os.path.getsize(p) <= 100 + 120


def test_enable_crash_logging_writes_marker():
    _reset_log()
    path = diagnostics.enable_crash_logging()
    assert path == diagnostics.crash_log_path()
    with open(path, encoding='utf-8') as f:
        text = f.read()
    assert 'sutura start (pid' in text
    import faulthandler
    assert faulthandler.is_enabled()


def test_crash_log_tail_sanitizes():
    user = getpass.getuser()
    _write_log('crash while reading %s (user %s)\n'
               % (os.path.join(_HOME, 'models', 'part.stl'), user))
    tail = diagnostics.crash_log_tail()
    assert tail is not None
    assert _HOME not in tail, tail
    assert '~' in tail, tail
    if user and user != '~':
        assert user not in tail, tail


def test_crash_log_tail_none_when_absent():
    _reset_log()
    assert diagnostics.crash_log_tail() is None


def test_diagnostics_zip_includes_sanitized_crash_log():
    _write_log('trace: %s/models/o.stl\n' % _HOME)
    out = os.path.join(_TMP, 'diag.zip')
    diagnostics.create_diagnostics_zip(out)
    with zipfile.ZipFile(out) as zf:
        names = zf.namelist()
        assert 'crash_log.txt' in names
        body = zf.read('crash_log.txt').decode('utf-8')
        readme = zf.read('README.txt').decode('utf-8')
    assert _HOME not in body
    assert '~' in body
    assert 'crash_log.txt' in readme


def test_diagnostics_zip_omits_crash_log_when_absent():
    _reset_log()
    out = os.path.join(_TMP, 'diag2.zip')
    diagnostics.create_diagnostics_zip(out)
    with zipfile.ZipFile(out) as zf:
        assert 'crash_log.txt' not in zf.namelist()
        readme = zf.read('README.txt').decode('utf-8')
    assert 'crash_log.txt' not in readme


def test_readme_method_count_is_registry_derived():
    _reset_log()
    out = os.path.join(_TMP, 'diag3.zip')
    diagnostics.create_diagnostics_zip(out)
    with zipfile.ZipFile(out) as zf:
        readme = zf.read('README.txt').decode('utf-8')
    count = sum(1 for m in diagnostics.collect_methods_availability()
                if 'num' in m)
    assert 'all %d methods' % count in readme, readme
    assert '1-17' not in readme


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('crash log tests passed')


if __name__ == '__main__':
    main()
