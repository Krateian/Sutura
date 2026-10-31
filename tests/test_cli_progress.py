#!/usr/bin/env python3
"""C8 progress/cancel unit tests (no repair, no heavy work)."""
import io
import os
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

from sutura_engine import progress  # noqa: E402


class _TTY(io.StringIO):
    def isatty(self):
        return True


def test_human_is_silent_without_tty():
    old = progress.sys.stderr
    buf = io.StringIO()
    progress.sys.stderr = buf
    try:
        p = progress.HumanProgress('model.stl', 1, 2)
        p.stage(3, 'stage 1')
        p.end_file()
    finally:
        progress.sys.stderr = old
    assert buf.getvalue() == '', buf.getvalue()


def test_human_rewrites_on_tty():
    old = progress.sys.stderr
    buf = _TTY()
    progress.sys.stderr = buf
    try:
        p = progress.HumanProgress('model.stl', 1, 2)
        p.stage(3, 'stage 1', 'closing holes')
        p.end_file()
    finally:
        progress.sys.stderr = old
    out = buf.getvalue()
    assert '1/2' in out and '3/8' in out and 'stage 1' in out, out
    assert '\r' in out and out.endswith('\r\033[K'), repr(out)


def test_ndjson_is_parseable():
    old = progress.sys.stderr
    buf = io.StringIO()
    progress.sys.stderr = buf
    try:
        p = progress.JsonProgress('model.stl', 1, 1)
        p.start_file()
        p.stage(3, 'stage 1', 'closing holes')
        p.end_file()
    finally:
        progress.sys.stderr = old
    import json
    rows = [json.loads(line) for line in buf.getvalue().splitlines()]
    assert [r['event'] for r in rows] == ['start', 'stage', 'end'], rows
    assert rows[1]['stage'] == 'stage 1' and rows[1]['step'] == 3, rows
    assert rows[1]['step_total'] == 8, rows


def test_cancel_token():
    t = progress.CancelToken()
    assert not t.is_cancelled()
    t.cancel()
    assert t.is_cancelled()
    try:
        progress.raise_if_cancelled(t)
    except progress.CancelledError:
        pass
    else:
        raise AssertionError('CancelToken did not raise')


def test_progress_json_requires_json():
    r = subprocess.run([sys.executable, os.path.join(SUTURA, 'repair.py'),
                        '--progress-json', 'fake.stl'],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 2, (r.returncode, r.stdout, r.stderr)
    assert '--progress-json requires --json' in r.stderr, r.stderr
    assert r.stdout.strip() == '', r.stdout


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok ', name)


if __name__ == '__main__':
    main()
