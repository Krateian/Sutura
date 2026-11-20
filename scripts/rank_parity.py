#!/usr/bin/env python3
# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
"""Ranking parity: Rust `rank_probe` vs Python `rank_methods`.

For each mesh the analysis fields are computed by the Python analyzer, then
both engines rank on the same numbers. Learning triage is disabled so the run
is deterministic (the bonus itself is covered by config tests).

Usage:
    ~/sutura-bench/venv/bin/python scripts/rank_parity.py RANK_PROBE [LIMIT]
"""
import json
import os
import pathlib
import subprocess
import sys

# Resolve the sibling `sutura` package from this file, so the script works in
# any checkout instead of one machine's worktree.
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / 'sutura'))
os.environ.setdefault('SUTURA_LEARNING_TRIAGE', '0')

from sutura_engine.diagnosis import analyze_file  # noqa: E402
from sutura_engine import methods as method_registry  # noqa: E402

BENCH = os.path.expanduser('~/sutura-bench')
MAIN = os.path.expanduser('~/Sutura')


def corpus(limit=0):
    out = []
    for setname, base in [('samples40', MAIN), ('oracle36', BENCH),
                          ('core150', BENCH)]:
        with open(os.path.join(BENCH, 'sets', setname + '.txt')) as f:
            for line in f:
                rel = line.strip()
                if rel:
                    path = os.path.join(base, rel)
                    if os.path.exists(path):
                        out.append((setname, path))
    return out[:limit] if limit else out


def fields(analysis):
    return {
        'mesh_type': analysis.mesh_type,
        'type_confidence': float(analysis.type_confidence or 0.0),
        'boundary_loops': float(analysis.boundary_loops or 0),
        'non_manifold_edges': float(analysis.non_manifold_edges or 0),
        'self_intersections': float(analysis.self_intersections or 0),
        'components': float(analysis.components or 1),
        'largest_loop_ratio': float(analysis.largest_loop_ratio or 0.0),
        'open_area_ratio': float(analysis.open_area_ratio or 0.0),
        'single_side_score': float(analysis.single_side_score or 0.0),
        'relief_score': float(analysis.relief_score or 0.0),
        'repetition_score': float(analysis.repetition_score or 0.0),
    }


def main():
    probe = sys.argv[1]
    limit = int(sys.argv[2]) if len(sys.argv) > 2 else 0
    meshes = corpus(limit)
    fails = 0
    for setname, path in meshes:
        objs = analyze_file(path)
        if not objs:
            continue
        analysis = objs[0]
        py = [{'num': r.num, 'id': r.id, 'name': r.name, 'score': r.score,
               'reason': r.reason, 'template': r.template,
               'reason_key': r.reason_key,
               'reason_args': list(r.reason_args)}
              for r in method_registry.rank_methods(analysis)]
        payload = json.dumps(fields(analysis))
        out = subprocess.run([probe, path, payload], capture_output=True, text=True)
        if out.returncode != 0:
            print('ERROR', path, out.stderr[-200:], flush=True)
            fails += 1
            continue
        rust = json.loads(out.stdout)
        if rust != py:
            fails += 1
            print('DIFF', setname, os.path.basename(path), flush=True)
            for a, b in zip(py, rust):
                if a != b:
                    print('  py ', json.dumps(a)[:220], flush=True)
                    print('  rust', json.dumps(b)[:220], flush=True)
            if len(py) != len(rust):
                print('  len', len(py), len(rust), flush=True)
    print('fails', fails, 'meshes', len(meshes), flush=True)
    return 1 if fails else 0


if __name__ == '__main__':
    sys.exit(main())
