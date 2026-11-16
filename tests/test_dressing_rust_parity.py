#!/usr/bin/env python3
"""Rust-core parity for the Dressing (#16) tier.

Compares the actual Python producer (``sutura_engine.dressing.dressing_coat``)
with the ``sutura_core_py.dressing_coat`` bridge, and the Python tier adoption
record with ``sutura_core_py.tier_dressing``.  The coat itself is produced by
``sutura_geom::dressing::dressing_coat`` on both sides; this test pins the
parameter resolution, cleanup, decimation, P-WELD and gate/report parity.

Requires both extensions (``sutura_geom`` and ``sutura_core_py``).  A missing
bridge FAILS instead of skipping, so the port cannot silently regress.

    <venv>/bin/python tests/test_dressing_rust_parity.py
"""
import json
import os
import sys

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
for p in (SUTURA, REPO):
    if p not in sys.path:
        sys.path.insert(0, p)

from sutura_engine import dressing  # noqa: E402
from sutura_engine.pipeline.tiers.dressing import dressing_tier  # noqa: E402

import sutura_core_py  # noqa: E402

# Open unit cube (one missing face): a small public-domain fixture.
V = np.array([(x, y, z) for x in (0, 1) for y in (0, 1) for z in (0, 1)],
             dtype=np.float64)
T = np.array([(0, 1, 3), (0, 3, 2), (4, 6, 7), (4, 7, 5), (0, 4, 5),
              (0, 5, 1), (2, 3, 7), (2, 7, 6), (0, 2, 6), (0, 6, 4),
              (1, 5, 7), (1, 7, 3)], dtype=np.int64)

OPTS = {'intensity': 'quick', 'drain': 'none'}


def _require_bridge():
    if not hasattr(sutura_core_py, 'dressing_coat'):
        raise AssertionError(
            'sutura_core_py.dressing_coat is required; build the Rust bridge')
    if not hasattr(sutura_core_py, 'tier_dressing'):
        raise AssertionError(
            'sutura_core_py.tier_dressing is required; build the Rust bridge')


def test_producer_matches_python_exactly():
    _require_bridge()
    pv, pt, prep = dressing.dressing_coat(
        V.copy(), T.copy(), intensity=OPTS['intensity'], drain=OPTS['drain'])
    rv, rt, rjson = sutura_core_py.dressing_coat(
        V, T, json.dumps(OPTS))
    rrep = json.loads(rjson)
    assert np.array_equal(np.asarray(pv), np.asarray(rv)), (
        'dressing vertices differ')
    assert np.array_equal(np.asarray(pt), np.asarray(rt)), (
        'dressing triangles differ')
    for key in ('engine', 'voxel', 'r_base', 'r_max', 'sigma',
                'faces_coat', 'faces_after', 'si_after', 'holes_after',
                'nm_after'):
        assert rrep.get(key) == prep.get(key), (key, prep.get(key), rrep.get(key))


def test_tier_gate_and_determinism_match_python():
    _require_bridge()
    recs = []
    arrays = []
    for _ in range(2):
        stats = {}
        pv, pt, _after = dressing_tier(
            V.copy(), T.copy(), V.copy(), T.copy(), None, stats, '/tmp',
            dressing=True, intensity=OPTS['intensity'], drain=OPTS['drain'])
        recs.append(stats['dressing'])
        arrays.append((np.asarray(pv), np.asarray(pt)))
    assert np.array_equal(arrays[0][0], arrays[1][0]), (
        'Python dressing tier is not deterministic')
    assert np.array_equal(arrays[0][1], arrays[1][1]), (
        'Python dressing tier is not deterministic')
    # Same tier for the Rust side, twice.
    rust_recs = []
    rust_arrays = []
    for _ in range(2):
        rv, rt, rjson = sutura_core_py.tier_dressing(
            V, T, V, T, OPTS['intensity'], json.dumps(OPTS))
        rust_recs.append(json.loads(rjson))
        rust_arrays.append((np.asarray(rv), np.asarray(rt)))
    assert np.array_equal(rust_arrays[0][0], rust_arrays[1][0])
    assert np.array_equal(rust_arrays[0][1], rust_arrays[1][1])
    for key in ('ran', 'adopted', 'holes', 'non_manifold', 'si_after',
                'fidelity_ok', 'reason'):
        assert rust_recs[0].get(key) == recs[0].get(key), (
            key, recs[0].get(key), rust_recs[0].get(key))
    # Forced adoption must also agree (the open cube trips the shape gates).
    stats = {}
    dressing_tier(V.copy(), T.copy(), V.copy(), T.copy(), None, stats, '/tmp',
                  dressing=True, intensity=OPTS['intensity'],
                  drain=OPTS['drain'], force_adopt=True)
    forced = dict(OPTS, force_adopt=True)
    _rv, _rt, rjson = sutura_core_py.tier_dressing(
        V, T, V, T, OPTS['intensity'], json.dumps(forced))
    rust_forced = json.loads(rjson)
    for key in ('ran', 'adopted', 'forced', 'holes', 'non_manifold',
                'si_after'):
        assert rust_forced.get(key) == stats['dressing'].get(key), (
            key, stats['dressing'].get(key), rust_forced.get(key))


if __name__ == '__main__':
    _require_bridge()
    fns = [v for k, v in sorted(globals().items()) if k.startswith('test_')]
    for fn in fns:
        fn()
        print('ok: %s' % fn.__name__)
    print('all %d dressing Rust-parity tests passed' % len(fns))
