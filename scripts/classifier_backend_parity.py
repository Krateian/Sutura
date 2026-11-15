#!/usr/bin/env python3
# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
"""Backend parity: `SUTURA_CLASSIFIER_BACKEND=python` vs `rust`.

Same 226-mesh corpus as the classifier gate. For both engines (classic and
experimental) the whole result dict must match: type, confidence, every
metric, and `feature_engine`.

Usage:
    python scripts/classifier_backend_parity.py [LIMIT]
"""
import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__))), 'sutura'))

import numpy as np  # noqa: E402

from sutura_engine import rust_bridge  # noqa: E402
from sutura_engine.pipeline.mesh_io import load_mesh  # noqa: E402

BENCH = os.path.expanduser('~/sutura-bench')
MAIN = os.path.expanduser('~/Sutura')


def corpus(limit=0):
    out = []
    for setname, base in [('samples40', MAIN), ('oracle36', BENCH),
                          ('core150', BENCH)]:
        with open(os.path.join(BENCH, 'sets', setname + '.txt')) as f:
            for line in f:
                rel = line.strip()
                if rel and os.path.exists(os.path.join(base, rel)):
                    out.append((setname, os.path.join(base, rel)))
    return out[:limit] if limit else out


def canonical(result):
    """JSON text of a result with numpy scalars normalised."""
    def enc(value):
        if isinstance(value, dict):
            return {k: enc(v) for k, v in value.items()}
        if isinstance(value, (list, tuple)):
            return [enc(v) for v in value]
        if isinstance(value, (np.integer,)):
            return int(value)
        if isinstance(value, (np.floating,)):
            return float(value)
        return value
    return json.dumps(enc(result), sort_keys=True)


def main():
    limit = int(sys.argv[1]) if len(sys.argv) > 1 else 0
    os.environ['SUTURA_CLASSIFIER_BACKEND'] = 'python'
    import mesh_classifier
    import mesh_classifier_v2
    from sutura_engine import classifier_rust

    meshes = corpus(limit)
    fails = 0
    checked = 0
    engines = 0
    for setname, path in meshes:
        verts, tris = load_mesh(path)
        if len(tris) == 0:
            continue
        for engine, mod in [('classic', mesh_classifier),
                            ('experimental', mesh_classifier_v2)]:
            os.environ['SUTURA_CLASSIFIER_BACKEND'] = 'python'
            rust_bridge.reset()
            py = mod.classify_mesh(verts, tris)
            os.environ['SUTURA_CLASSIFIER_BACKEND'] = 'rust'
            rust_bridge.reset()
            try:
                got = classifier_rust.classify_mesh(verts, tris, engine)[0]
            except Exception as exc:  # noqa: BLE001
                print('ERROR', engine, path, repr(exc)[:200], flush=True)
                fails += 1
                continue
            engines += 1
            if canonical(py) != canonical(got):
                fails += 1
                print('DIFF', engine, setname, os.path.basename(path),
                      flush=True)
                a = json.loads(canonical(py))
                b = json.loads(canonical(got))
                for key in sorted(set(a) | set(b)):
                    if a.get(key) != b.get(key):
                        print(' ', key, b.get(key), '!=', a.get(key),
                              flush=True)
        checked += 1
    print('meshes', checked, 'results', engines, 'fails', fails, flush=True)
    return 1 if fails else 0


if __name__ == '__main__':
    sys.exit(main())
