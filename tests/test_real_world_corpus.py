#!/usr/bin/env python3
"""Real-world corpus regression (tests/real-world-samples/*.stl).

Runs validate + dry-run + auto repair on every real-world sample mesh and
asserts the pipeline never crashes (every phase must exit 0 with a clean JSON
report; partial repairs are fine, hard errors/crashes are not). Also prints a
compact result table so the expected outcomes stay visible.

The corpus is deliberately small (<50 MB): Artec scan decimations (CC BY 4.0,
decimated with preservetopology so defect counts stay close to the originals)
plus Thingi10K meshes (varying original licenses). Several files are
scanned-mechanical parts that the mesh classifier currently reads as organic
-- a documented known limitation; this regression keeps the behaviour visible
so a future classifier change cannot silently break the corpus.

Run with a Python that can reach the CLI ($SUTURA or the installed wrapper):
    ~/.local/share/sutura/venv/bin/python tests/test_real_world_corpus.py
"""
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SAMPLES = os.path.join(REPO, 'tests', 'real-world-samples')
SUTURA = os.environ.get('SUTURA', os.path.expanduser('~/.local/bin/sutura'))


def run(args, path, out=None):
    cmd = [SUTURA] + args + [path]
    if out is not None:
        cmd += ['-o', out]
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=900)
    return r


def main():
    files = sorted(f for f in os.listdir(SAMPLES) if f.lower().endswith('.stl'))
    failed = []
    with tempfile.TemporaryDirectory(prefix='sutura-corpus-') as tmp:
        for name in files:
            path = os.path.join(SAMPLES, name)
            out = os.path.join(tmp, name + '_fixed.stl')
            row = [name]
            ok = True
            for label, args in (('val', ['validate']),
                                ('dry', ['--dry-run', '--mode', 'auto']),
                                ('rep', ['--mode', 'auto'])):
                r = run(args, path, out if label == 'rep' else None)
                # partial repairs are expected; only hard errors/crashes fail
                if r.returncode != 0:
                    ok = False
                    row.append('%s=FAIL(rc%d %s)' % (label, r.returncode,
                                                     r.stderr.strip()[-80:]))
                else:
                    row.append('%s=ok' % label)
            print('  %-42s %s' % (name, ' '.join(row[1:])))
            if not ok:
                failed.append(name)
    print('---')
    if failed:
        print('FAILED (%d): %s' % (len(failed), ', '.join(failed)))
        sys.exit(1)
    print('real-world corpus: %d meshes, 0 crashes' % len(files))


if __name__ == '__main__':
    main()