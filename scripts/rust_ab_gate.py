#!/usr/bin/env python3
# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
"""Rust-vs-Rust byte gate for the P10 removal.

The product no longer has a Python runtime to compare against, so the gate's
baseline is the release CLI built from the previous Rust commit and the
candidate is the CLI built from this commit. Both run with the same arguments in
separate data directories; a mesh is SAME only when both exit 0 and write
identical bytes. No Sutura Python is imported or executed.

Usage:
    scripts/rust_ab_gate.py --baseline-bin OLD/sutura --candidate-bin NEW/sutura \
        [--flags "--json --no-history --no-cache"] [--timeout 1800] MESH...

Exit status is 1 when any mesh is DIFF, RC or TIMEOUT.
"""
import argparse
import hashlib
import os
import shutil
import subprocess
import sys
import tempfile
import time


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b''):
            digest.update(chunk)
    return digest.hexdigest()


def run_side(binary, mesh, flags, timeout):
    work = tempfile.mkdtemp(prefix='rust-ab-')
    try:
        out = os.path.join(work, 'out.stl')
        env = dict(os.environ)
        env['SUTURA_DATA_DIR'] = os.path.join(work, 'data')
        env['SUTURA_NO_CACHE'] = '1'
        started = time.time()
        try:
            proc = subprocess.run([binary, mesh, '-o', out] + flags,
                                  capture_output=True, text=True,
                                  timeout=timeout, env=env)
            rc, err = proc.returncode, proc.stderr[-300:]
        except subprocess.TimeoutExpired:
            rc, err = 'TIMEOUT', ''
        elapsed = time.time() - started
        sha = sha256_file(out) if rc == 0 and os.path.exists(out) else None
        return {'rc': rc, 'sha256': sha, 'seconds': round(elapsed, 3),
                'error': None if rc == 0 else err}
    finally:
        shutil.rmtree(work, ignore_errors=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline-bin', required=True)
    parser.add_argument('--candidate-bin', required=True)
    parser.add_argument('--flags', default='--json --no-history --no-cache')
    parser.add_argument('--timeout', type=float, default=1800)
    parser.add_argument('meshes', nargs='+')
    args = parser.parse_args()
    flags = args.flags.split()
    failed = 0
    for mesh in args.meshes:
        base = run_side(args.baseline_bin, mesh, flags, args.timeout)
        cand = run_side(args.candidate_bin, mesh, flags, args.timeout)
        if base['rc'] != 0 or cand['rc'] != 0:
            verdict = 'TIMEOUT' if 'TIMEOUT' in (base['rc'], cand['rc']) else 'RC'
        elif base['sha256'] == cand['sha256']:
            verdict = 'SAME'
        else:
            verdict = 'DIFF'
        failed += verdict != 'SAME'
        print('%-32s %-7s rc %s/%s sha %s/%s' % (
            os.path.basename(mesh), verdict, base['rc'], cand['rc'],
            (base['sha256'] or '-')[:12], (cand['sha256'] or '-')[:12]),
            flush=True)
    print('SUMMARY meshes=%d failed=%d' % (len(args.meshes), failed))
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
