#!/usr/bin/env python3
"""Graft G0 helper parity: Python `_auto_voxel`/`_gap_estimate`/`_r_ladder`
vs the pure Rust helpers. Public samples only."""
import os
import sys

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

from sutura_engine.graft import (  # noqa: E402
    _auto_voxel, _diag, _gap_estimate, _grid_cell_count, _median_edge_length,
    _r_ladder, resolve_grid_budget,
)


def test_graft_g0_helpers_match_python():
    try:
        import sutura_core_py
        from sutura_core_py import load_stl
    except Exception as exc:  # noqa: BLE001
        import pytest
        pytest.skip('bridge unavailable: %s' % exc)

    root = os.path.join(REPO, 'tests', 'real-world-samples')
    files = ['thingi10k_100173.stl', 'thingi10k_100045.stl',
             'artec_metal-nut.stl']
    budget = int(resolve_grid_budget('balanced'))
    for name in files:
        path = os.path.join(root, name)
        v, t = load_stl(path)
        v = np.asarray(v, dtype=np.float64)
        t = np.asarray(t, dtype=np.int64)
        diag = _diag(v)
        voxel = _auto_voxel(v, t, diag, budget)
        cells = _grid_cell_count(v, voxel)
        gap, info, _mask = _gap_estimate(v, t)
        ladder = _r_ladder(gap, voxel, diag, 12)
        got = sutura_core_py.graft_helpers_probe(v, t, budget)
        import json
        got = json.loads(got)
        assert got['voxel'] == voxel, (name, got['voxel'], voxel)
        assert got['cells'] == cells, (name, got['cells'], cells)
        assert got['gap'] == gap, (name, got['gap'], gap)
        assert got['diag'] == diag, (name, got['diag'], diag)
        assert got['median_edge'] == _median_edge_length(v, t), name
        assert got['ladder'] == ladder, (name, got['ladder'], ladder)
        assert got['info'] == info, (name, got['info'], info)


def test_graft_g0b_remesh_matches_python():
    try:
        import sutura_core_py
        from sutura_core_py import load_stl
    except Exception as exc:  # noqa: BLE001
        import pytest
        pytest.skip('bridge unavailable: %s' % exc)

    from sutura_engine.graft import (
        _collapse_short_edges, _isotropic_remesh, _smooth_tangential,
        _split_long_edges,
    )
    root = os.path.join(REPO, 'tests', 'real-world-samples')
    for name in ('thingi10k_100173.stl', 'thingi10k_100045.stl'):
        v, t = load_stl(os.path.join(root, name))
        v = np.asarray(v, dtype=np.float64)
        t = np.asarray(t, dtype=np.int64)
        target = _median_edge_length(v, t) * 1.2
        for step, fn in (
            ('split', _split_long_edges),
            ('collapse', _collapse_short_edges),
            ('isotropic', _isotropic_remesh),
        ):
            pv, pt = fn(v, t, target)
            rv, rt = sutura_core_py.graft_remesh_probe(v, t, target, step)
            # smooth/isotropic: numpy's second-iteration accumulation leaves a
            # <=7.1e-15 float-order difference (split/collapse/normals exact).
            # The final A2 mesh gate stays byte-exact.
            if step in ('smooth', 'isotropic'):
                assert np.allclose(pv, rv, rtol=0.0, atol=1e-12), (
                    name, step, 'verts')
            else:
                assert np.array_equal(pv, rv), (name, step, 'verts')
            assert np.array_equal(pt, rt), (name, step, 'tris')
        pv = _smooth_tangential(v, t, iterations=2)
        rv, _rt = sutura_core_py.graft_remesh_probe(v, t, target, 'smooth')
        assert np.allclose(pv, rv, rtol=0.0, atol=1e-12), (name, 'smooth')
