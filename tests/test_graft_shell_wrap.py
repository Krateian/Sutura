#!/usr/bin/env python3
# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

"""Graft G1 parity: whole-mesh Python `shell_wrap` vs the Rust loop.

Public samples only. The guard is disabled so the NumPy-RNG coverage terms
(G3) are not part of this slice; the produced mesh must still be bit-exact.
"""
import json
import os
import sys

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, 'sutura'))


def _case(name, voxel, r, **kwargs):
    try:
        import sutura_core_py
        from sutura_core_py import load_stl
    except Exception as exc:  # noqa: BLE001
        import pytest
        pytest.skip('bridge unavailable: %s' % exc)

    from sutura_engine.graft import shell_wrap

    v, t = load_stl(os.path.join(REPO, 'tests', 'real-world-samples', name))
    v = np.asarray(v, dtype=np.float64)
    t = np.asarray(t, dtype=np.int64)
    params = dict(kwargs)
    params.setdefault('local', False)
    params.setdefault('remesh', False)
    params.setdefault('guard', False)
    params.setdefault('si_check', True)
    params.setdefault('grid_budget', 300_000)
    params.setdefault('hybrid', False)
    pv, pt, prep = shell_wrap(v, t, r=r, voxel=voxel, **params)
    rv, rt, rrep = sutura_core_py.graft_shell_wrap_probe(
        v, t, r, voxel, json.dumps(params))
    rrep = json.loads(rrep)
    assert np.array_equal(pv, rv), (name, 'verts')
    assert np.array_equal(pt, rt), (name, 'tris')
    keys = ['r_used', 'mode', 'holes', 'non_manifold', 'faces_after',
            'si_before', 'si_after', 'projected_fraction', 'remeshed',
            'detail_max_mm', 'detail_mean_mm', 'voxel']
    keys += ['hausdorff_healthy', 'fidelity_ok']
    if params['guard']:
        keys += ['hausdorff_one_sided', 'hausdorff_healthy_behind',
                 'input_coverage', 'coverage_ok']
    for key in keys:
        assert prep[key] == rrep[key], (name, key, prep[key], rrep[key])


def test_shell_wrap_g1_whole_mesh():
    _case('thingi10k_100173.stl', 8.0, 4.0, remesh=True)


def test_shell_wrap_g1_whole_mesh_no_remesh():
    _case('thingi10k_100045.stl', 0.5, 1.0)


def test_shell_wrap_g2_local_stitch():
    _case('thingi10k_100077.stl', 0.101, 0.304, local=True)


def test_shell_wrap_g2_hybrid_close():
    _case('thingi10k_100045.stl', 0.5, 1.5, hybrid=True, guard=True)


def test_shell_wrap_g3_rng_coverage():
    _case('thingi10k_100827.stl', 1.354, 2.031, guard=True)


def test_shell_wrap_g4_engine_bridge():
    try:
        import sutura_core_py
        from sutura_core_py import load_stl
    except Exception as exc:  # noqa: BLE001
        import pytest
        pytest.skip('bridge unavailable: %s' % exc)

    from sutura_engine.graft import shell_wrap

    v, t = load_stl(os.path.join(
        REPO, 'tests', 'real-world-samples', 'thingi10k_100045.stl'))
    v = np.asarray(v, dtype=np.float64)
    t = np.asarray(t, dtype=np.int64)
    pv, pt, prep = shell_wrap(v, t, hybrid=True, intensity='balanced')
    rv, rt, rrep = sutura_core_py.graft_shell_wrap(v, t, 'balanced', None)
    rrep = json.loads(rrep)
    assert np.array_equal(pv, rv)
    assert np.array_equal(pt, rt)
    for key in ('r_used', 'mode', 'input_coverage', 'coverage_ok',
                'hausdorff_healthy', 'faces_after'):
        assert prep[key] == rrep[key], (key, prep[key], rrep[key])
