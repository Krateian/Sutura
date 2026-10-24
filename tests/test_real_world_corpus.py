#!/usr/bin/env python3
"""Real-world corpus regression (tests/real-world-samples/*.stl).

Runs validate + dry-run + auto repair on every real-world sample mesh and
asserts the pipeline never crashes (every phase must exit 0 with a clean JSON
report; partial repairs are fine, hard errors/crashes are not). Also prints a
compact result table so the expected outcomes stay visible.

The untagged auto repair includes the ranked fallback, which is bounded by the
deterministic candidate count in ``methods._auto`` (``AUTO_MAX_ATTEMPTS``), so
the suite exercises the real default path without a wall-clock-dependent
method selection.  A very high watchdog remains as a batch safety net only.

The corpus is deliberately bounded (97 MB, 40 meshes): Artec scan decimations (CC BY 4.0,
decimated with preservetopology so defect counts stay close to the originals)
plus Thingi10K meshes (varying original licenses). Several files are
scanned-mechanical parts that the mesh classifier currently reads as organic
-- a documented known limitation; this regression keeps the behaviour visible
so a future classifier change cannot silently break the corpus.

The CLI is the in-repo entry point (this interpreter running
``sutura/repair.py``) unless the ``SUTURA`` environment variable overrides it,
so the suite always exercises the working tree, never a stale global install.
Run it with an interpreter that can import ``repair.py``'s dependencies (the
venv with pymeshlab):
    ~/.local/share/sutura/venv/bin/python tests/test_real_world_corpus.py

The per-mesh subprocess timeout (seconds) is configurable via
``SUTURA_CORPUS_TIMEOUT`` (default 900) so a loaded machine or a stricter CI
runner budget can bound a single mesh without editing the suite.
"""
import os
import subprocess
import sys
import tempfile

import _cli

REPO = _cli.REPO
SAMPLES = os.path.join(REPO, 'tests', 'real-world-samples')
PER_MESH_TIMEOUT = float(os.environ.get('SUTURA_CORPUS_TIMEOUT', '900'))


def run(args, path, out=None):
    cmd = _cli.cli_command(args + [path])
    if out is not None:
        cmd += ['-o', out]
    env = dict(os.environ)
    env.setdefault('PYTHONFAULTHANDLER', '1')
    r = subprocess.run(cmd, capture_output=True, text=True,
                       timeout=PER_MESH_TIMEOUT, env=env)
    return r


def main():
    # Only the canonical fixtures: a stray `*_fixed.stl` output left in the
    # directory (from a manual run) must not be treated as a corpus sample.
    files = sorted(f for f in os.listdir(SAMPLES)
                   if f.lower().endswith('.stl')
                   and '_fixed' not in f.lower())
    failed = []
    with tempfile.TemporaryDirectory(prefix='sutura-corpus-') as tmp:
        for name in files:
            path = os.path.join(SAMPLES, name)
            out = os.path.join(tmp, name + '_fixed.stl')
            row = [name]
            ok = True
            # The default untagged path (auto): the ranked fallback that runs
            # on a non-watertight result is bounded by a wall-clock budget
            # (methods._auto), so the real default path is exercised here.
            for label, args in (('val', ['validate']),
                                ('dry', ['--dry-run', '--mode', 'auto']),
                                ('rep', ['--mode', 'auto'])):
                r = run(args, path, out if label == 'rep' else None)
                # partial repairs are expected; only hard errors/crashes fail
                if r.returncode != 0:
                    ok = False
                    row.append('%s=FAIL(rc%d %s)' % (label, r.returncode,
                                                     r.stderr.strip()[-80:]))
                    print('    %s stderr:\n%s' % (label, r.stderr))
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