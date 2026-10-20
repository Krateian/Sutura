#!/usr/bin/env python3
"""Regression test for file-level-broken inputs.

Each generated input must be rejected with a non-zero exit code and an
'error' in the JSON report. A silent exit-0 success on malformed input is
the failure mode this guards (the tool once returned 0 on every failure).

The CLI defaults to the in-repo entry point (this interpreter running
``sutura/repair.py``) so the working tree is exercised; set ``SUTURA`` to use
an installed CLI or launcher instead. Run with an interpreter that can import
``repair.py``'s dependencies (the venv with pymeshlab).
Usage: test_adversarial.py
"""
import json
import os
import struct
import subprocess
import sys
import tempfile

import _cli

CUBE = [
    ((-1, -1, -1), (1, -1, -1), (1, 1, -1)), ((-1, -1, -1), (1, 1, -1), (-1, 1, -1)),
    ((-1, -1, 1), (1, -1, 1), (1, 1, 1)),    ((-1, -1, 1), (1, 1, 1), (-1, 1, 1)),
    ((-1, -1, -1), (-1, -1, 1), (-1, 1, 1)), ((-1, -1, -1), (-1, 1, 1), (-1, 1, -1)),
    ((1, -1, -1), (1, -1, 1), (1, 1, 1)),    ((1, -1, -1), (1, 1, 1), (1, 1, -1)),
    ((-1, 1, -1), (1, 1, -1), (1, 1, 1)),    ((-1, 1, -1), (1, 1, 1), (-1, 1, 1)),
    ((-1, -1, -1), (1, -1, -1), (1, -1, 1)), ((-1, -1, -1), (1, -1, 1), (-1, -1, 1)),
]


def write_stl(path, tris):
    with open(path, 'wb') as f:
        f.write(b'adversarial'.ljust(80, b'\0'))
        f.write(struct.pack('<I', len(tris)))
        for a, b, c in tris:
            f.write(struct.pack('<3f', 0, 0, 0))
            f.write(struct.pack('<3f', *a))
            f.write(struct.pack('<3f', *b))
            f.write(struct.pack('<3f', *c))
            f.write(struct.pack('<H', 0))


def build(tmp):
    cases = {}

    p = os.path.join(tmp, 'truncated.stl')
    write_stl(p, CUBE)
    data = open(p, 'rb').read()
    open(p, 'wb').write(data[:len(data) // 2])
    cases['truncated'] = p

    p = os.path.join(tmp, 'wrongcount.stl')
    with open(p, 'wb') as f:
        f.write(b'wrong count'.ljust(80, b'\0'))
        f.write(struct.pack('<I', 1000))
        for a, b, c in CUBE:
            f.write(struct.pack('<3f', 0, 0, 0))
            f.write(struct.pack('<3f', *a))
            f.write(struct.pack('<3f', *b))
            f.write(struct.pack('<3f', *c))
            f.write(struct.pack('<H', 0))
    cases['wrongcount'] = p

    p = os.path.join(tmp, 'degenerate.stl')
    write_stl(p, [((0, 0, 0), (1, 0, 0), (1, 1, 0)), ((0, 0, 0), (1, 1, 0), (0, 1, 0))])
    cases['degenerate'] = p

    p = os.path.join(tmp, 'all_zero_area.stl')
    write_stl(p, [((0, 0, 0), (1, 0, 0), (2, 0, 0)), ((0, 0, 0), (0, 1, 0), (0, 2, 0))])
    cases['all_zero_area'] = p

    p = os.path.join(tmp, 'nan_inf.stl')
    write_stl(p, [
        ((-1, -1, -1), (float('nan'), -1, -1), (1, 1, -1)),
        ((1, -1, -1), (1, -1, 1), (float('inf'), float('inf'), float('inf'))),
    ] + CUBE[2:])
    cases['nan_inf'] = p

    p = os.path.join(tmp, 'empty.stl')
    write_stl(p, [])
    cases['empty'] = p

    p = os.path.join(tmp, 'obj_as_stl.stl')
    with open(p, 'w') as f:
        f.write('v -1 -1 -1\nv 1 -1 -1\nv 1 1 -1\nf 1 2 3\n')
    cases['wrong_ext'] = p

    return cases


def run_sutura(path):
    try:
        r = subprocess.run(_cli.cli_command([path]), capture_output=True,
                           text=True, timeout=120)
    except subprocess.TimeoutExpired:
        return 124, {}   # hung, not a clean rejection -> treated as a failure
    try:
        rep = json.loads(r.stdout.strip().splitlines()[-1])
    except Exception:
        return r.returncode, {}
    return r.returncode, rep


def test_all_degenerate_faces_repair_pipeline():
    """Verify entirely zero-area faces are rejected with the canonical error and no crash."""
    with tempfile.TemporaryDirectory() as tmp:
        p = os.path.join(tmp, 'zero_area.stl')
        write_stl(p, [((0, 0, 0), (1, 0, 0), (2, 0, 0))])
        code, rep = run_sutura(p)
        assert code == 1, f"Expected exit code 1, got {code}"
        assert rep.get('error') == 'repair failed: all faces are degenerate; nothing to repair', rep
        assert rep.get('category') == 'error', rep
        assert rep.get('issues') == ['malformed'], rep


def main():
    test_all_degenerate_faces_repair_pipeline()
    with tempfile.TemporaryDirectory() as tmp:
        cases = build(tmp)
        fails = []
        for name, path in cases.items():
            code, rep = run_sutura(path)
            if code == 0:
                fails.append('%s: silent success (exit 0)' % name)
            elif 'error' not in rep:
                fails.append('%s: failed without an error report' % name)
            else:
                print('ok  %-13s exit=%d  %s' % (name, code, rep['error'].splitlines()[0][:70]))
        if fails:
            print('FAIL:')
            for f in fails:
                print('  - %s' % f)
            return 1
        print('PASS: all adversarial inputs rejected with a clear error')
        return 0


if __name__ == '__main__':
    sys.exit(main())