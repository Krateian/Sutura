#!/usr/bin/env python3
"""Classifier parity: Rust `classify_probe` vs the Python engines.

Covers samples40 + oracle36 + core150 for both `classic` and `experimental`
(type, confidence and every metric, compared as parsed JSON). Run through sq
when the corpus is large; the Rust probe is built by the caller.
"""
import json
import os
import pathlib
import subprocess
import sys

# The `sutura` package sits beside `scripts/` in this checkout; resolving it
# from `__file__` keeps the script runnable from any clone.
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / 'sutura'))
from sutura_engine.pipeline.mesh_io import load_mesh  # noqa: E402
import mesh_classifier  # noqa: E402
import mesh_classifier_v2  # noqa: E402

BENCH = os.path.expanduser('~/sutura-bench')
MAIN = os.path.expanduser('~/Sutura')


def corpus(limit=0):
    out = []
    for setname, base in [('samples40', MAIN), ('oracle36', BENCH),
                          ('core150', BENCH)]:
        with open(os.path.join(BENCH, 'sets', setname + '.txt')) as f:
            for line in f:
                rel = line.strip()
                if not rel:
                    continue
                path = os.path.join(base, rel)
                if os.path.exists(path):
                    out.append((setname, path))
    return out[:limit] if limit else out


def main():
    cli = sys.argv[1]
    limit = int(sys.argv[2]) if len(sys.argv) > 2 else 0
    meshes = corpus(limit)
    fails = 0
    for setname, path in meshes:
        v, t = load_mesh(path)
        for engine, mod in [('classic', mesh_classifier),
                            ('experimental', mesh_classifier_v2)]:
            want = json.loads(json.dumps(mod.classify_mesh(v, t), sort_keys=True))
            out = subprocess.run([cli, path, engine], capture_output=True,
                                 text=True)
            if out.returncode != 0:
                print('ERROR', engine, path, out.stderr[-200:], flush=True)
                fails += 1
                continue
            got = json.loads(out.stdout)
            if got != want:
                fails += 1
                print('DIFF', engine, setname, os.path.basename(path), flush=True)
                for key in sorted(set(want) | set(got)):
                    if got.get(key) != want.get(key):
                        print(' ', key, got.get(key), '!=', want.get(key),
                              flush=True)
        if fails == 0 and meshes.index((setname, path)) % 20 == 0:
            print('ok', setname, os.path.basename(path), flush=True)
    print('fails', fails, 'meshes', len(meshes), flush=True)
    return 1 if fails else 0


if __name__ == '__main__':
    sys.exit(main())
