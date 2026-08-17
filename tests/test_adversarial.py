#!/usr/bin/env python3
"""Regression test for file-level-broken (adversarial) inputs.

Each generated input must be rejected with a non-zero exit code and an
'error' entry in the JSON report. A silent "success" (exit 0) for a
malformed input is the failure mode this guards against: previously the
tool returned exit 0 on every failure, and NaN/Inf triangles were silently
dropped and reported as a clean repair.

Usage: test_adversarial.py  (expects the installed `sutura` CLI)
"""
import json
import os
import struct
import subprocess
import sys
import tempfile

SUTURA = os.environ.get('SUTURA', os.path.expanduser('~/.local/bin/sutura'))

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
        r = subprocess.run([SUTURA, path], capture_output=True, text=True, timeout=120)
    except subprocess.TimeoutExpired:
        return 124, {}   # hung, not a clean rejection -> treated as a failure
    try:
        rep = json.loads(r.stdout.strip().splitlines()[-1])
    except Exception:
        return r.returncode, {}
    return r.returncode, rep


def main():
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
                print('ok  %-11s exit=%d  %s' % (name, code, rep['error'].splitlines()[0][:70]))
        if fails:
            print('FAIL:')
            for f in fails:
                print('  - %s' % f)
            return 1
        print('PASS: all adversarial inputs rejected with a clear error')
        return 0


if __name__ == '__main__':
    sys.exit(main())