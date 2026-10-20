#!/usr/bin/env python3
"""Splint core (steps 1-8) PyO3 regression.

Usage:
    <venv>/bin/python tests/test_splint.py

Runs on a synthetic cube only, so it is CI-safe and needs no corpus files.
The 5-mesh spike equivalence is a separate local harness.
"""
import json
import os
import subprocess
import sys
import tempfile

import numpy as np

import sutura_geom


def cube():
    h = 0.5
    v = np.array([
        [-h, -h, -h], [h, -h, -h], [h, h, -h], [-h, h, -h],
        [-h, -h, h], [h, -h, h], [h, h, h], [-h, h, h],
    ], dtype=np.float64)
    t = np.array([
        [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
        [0, 1, 5], [0, 5, 4], [3, 7, 6], [3, 6, 2],
        [0, 4, 7], [0, 7, 3], [1, 2, 6], [1, 6, 5],
    ], dtype=np.int32)
    return v, t


def test_entry_point_and_report():
    v, t = cube()
    ov, ot, rep = sutura_geom.splint_cut(v, t, 32)
    assert rep['ok'] is True, rep
    assert rep['res'] == 32
    assert rep['input_vertices'] == 8
    assert rep['input_faces'] == 12
    assert rep['proxy_vertices'] > 0
    assert rep['tets'] > 0
    assert rep['output_vertices'] == len(ov)
    assert rep['output_faces'] == len(ot)
    assert rep['output_parts'] >= 1
    assert rep['fix_islands'] is True
    assert rep['soft_band'] is False
    assert rep['out_manifold'] in (True, False)
    assert rep['delaunay_hash'] != 0

    # Two direct calls must produce the same output arrays.
    ov2, ot2, _ = sutura_geom.splint_cut(v, t, 32)
    assert np.array_equal(ov, ov2)
    assert np.array_equal(ot, ot2)


def test_invalid_res_returns_an_error_report():
    v, t = cube()
    ov, ot, rep = sutura_geom.splint_cut(v, t, 0)
    assert rep['ok'] is False
    assert 'res' in rep['error']
    assert len(ov) == 0 and len(ot) == 0


def test_rayon_thread_counts_are_byte_identical():
    v, t = cube()
    with tempfile.TemporaryDirectory() as td:
        inp = os.path.join(td, 'in.npz')
        np.savez(inp, v=v, t=t)
        results = []
        for n in (1, 2, 8):
            out = os.path.join(td, 'out_%d.npz' % n)
            code = r'''
import json, sys
import numpy as np
import sutura_geom
d = np.load(sys.argv[1])
ov, ot, rep = sutura_geom.splint_cut(d['v'], d['t'], 32)
np.savez(sys.argv[2], v=ov, t=ot)
drop = ('seconds', 'secs_udf', 'secs_proxy', 'secs_delaunay', 'secs_cut')
print(json.dumps({k: rep[k] for k in rep if k not in drop}, sort_keys=True))
'''
            env = dict(os.environ)
            env['RAYON_NUM_THREADS'] = str(n)
            p = subprocess.run(
                [sys.executable, '-c', code, inp, out],
                env=env, capture_output=True, text=True, check=True)
            results.append((np.load(out), json.loads(p.stdout)))
        base, base_rep = results[0]
        for data, rep in results[1:]:
            assert np.array_equal(base['v'], data['v'])
            assert np.array_equal(base['t'], data['t'])
            # Diagnostic volumes are the only tolerance (float sum order).
            for key in base_rep:
                if key in ('dropped_inside_vol', 'filled_outside_vol'):
                    assert abs(base_rep[key] - rep[key]) <= 1e-12
                else:
                    assert base_rep[key] == rep[key], (key, base_rep[key], rep[key])


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('Splint core tests passed')
