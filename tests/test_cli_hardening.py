#!/usr/bin/env python3
"""CLI hardening regression tests (0.9.8 B4).

Covers the documented exit-code contract (0/1/2/130), the top-level error
guard (no Python traceback without --debug), human-mode error text instead of
JSON on stdout, and the English batch summary ("file(s)", not "dosya").

Usage: ~/.local/share/sutura/venv/bin/python tests/test_cli_hardening.py
"""
import json
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
REPAIR_PY = os.path.join(SUTURA, 'repair.py')


def _run(args):
    return subprocess.run([sys.executable, REPAIR_PY] + args,
                          capture_output=True, text=True, timeout=600,
                          stdin=subprocess.DEVNULL)


def _run_py(code):
    return subprocess.run([sys.executable, '-c', code],
                          capture_output=True, text=True, timeout=120,
                          stdin=subprocess.DEVNULL)


GUARD = r'''
import sys
sys.path.insert(0, {sutura!r})
import repair
repair.process_file = lambda *a, **k: (_ for _ in ()).throw(
    RuntimeError('boom'))
repair.main({args!r})
'''


def test_human_usage_error_is_text_exit_2():
    r = _run(['--human', '-o', 'out.stl', 'a.stl', 'b.stl'])
    assert r.returncode == 2, (r.returncode, r.stdout, r.stderr)
    assert '{' not in r.stdout and '"error"' not in r.stdout, r.stdout
    assert 'usage:' in r.stderr and 'error:' in r.stderr, r.stderr


def test_machine_usage_error_is_stderr_exit_2():
    r = _run(['-o', 'out.stl', 'a.stl', 'b.stl'])
    assert r.returncode == 2, (r.returncode, r.stdout, r.stderr)
    assert r.stdout.strip() == '', r.stdout
    assert 'usage:' in r.stderr, r.stderr


def test_unexpected_error_guard_machine():
    r = _run_py(GUARD.format(sutura=SUTURA, args=['fake.stl']))
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
    d = json.loads(r.stdout.strip().splitlines()[-1])
    assert d.get('error') == 'boom', d
    assert 'Traceback' not in r.stderr, r.stderr
    assert '--debug' in r.stderr, r.stderr


def test_unexpected_error_guard_human_debug():
    r = _run_py(GUARD.format(sutura=SUTURA,
                             args=['--human', '--debug', 'fake.stl']))
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
    assert r.stdout.strip() == '', r.stdout
    assert 'Traceback' in r.stderr and 'error: boom' in r.stderr, r.stderr


def test_keyboard_interrupt_exit_130():
    code = GUARD.replace("RuntimeError('boom')", "KeyboardInterrupt()")
    r = _run_py(code.format(sutura=SUTURA, args=['fake.stl']))
    assert r.returncode == 130, (r.returncode, r.stdout, r.stderr)
    assert 'interrupted' in r.stderr, r.stderr


def test_human_runtime_error_has_no_json(tmp):
    bad = os.path.join(tmp, 'garbage.stl')
    with open(bad, 'wb') as f:
        f.write(os.urandom(200))
    r = _run(['--human', bad])
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
    assert '"error"' not in r.stdout, r.stdout
    assert '{' not in r.stdout, r.stdout


def test_batch_human_summary_says_files(tmp):
    a = os.path.join(tmp, 'a.stl')
    b = os.path.join(tmp, 'b.stl')
    for p in (a, b):
        subprocess.run([sys.executable,
                        os.path.join(REPO, 'tests', 'make_broken_stl.py'), p],
                       check=True, stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL)
    r = _run(['--human', '--no-history', '--no-cache', a, b])
    assert 'file(s)' in r.stdout, r.stdout[-2000:]
    assert 'dosya' not in r.stdout, r.stdout[-2000:]


def test_subcommand_human_error_is_text():
    r = _run(['engines', 'bogus', '--human'])
    assert r.returncode == 2, (r.returncode, r.stdout, r.stderr)
    assert '{' not in r.stdout and '"error"' not in r.stdout, r.stdout
    assert 'error:' in r.stderr, r.stderr


def main():
    with tempfile.TemporaryDirectory(prefix='sutura-cli-') as tmp:
        for name, fn in sorted(globals().items()):
            if name.startswith('test_') and callable(fn):
                if fn.__code__.co_argcount:
                    fn(tmp)
                else:
                    fn()
                print('ok  %s' % name)


if __name__ == '__main__':
    main()
