#!/usr/bin/env python3
# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

"""Closing producer parity: Python `poisson_close`/`flat_back_close` vs Rust.

Public samples only. Poisson is bit-exact (both use the same Rust resample);
flat_back may choose a different plane basis, so its gate is shape quality.
"""
import json
import os
import sys

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, 'sutura'))


def _load(name):
    import sutura_core_py
    from sutura_core_py import load_stl
    v, t = load_stl(os.path.join(REPO, 'tests', 'real-world-samples', name))
    return np.asarray(v, dtype=np.float64), np.asarray(t, dtype=np.int64)


def _relief(nx=10, ny=10, amplitude=0.2):
    xs = np.linspace(-1.0, 1.0, nx)
    ys = np.linspace(-1.0, 1.0, ny)
    xx, yy = np.meshgrid(xs, ys)
    zz = amplitude * np.cos(np.pi * xx / 2.0) * np.cos(np.pi * yy / 2.0)
    verts = np.column_stack([xx.ravel(), yy.ravel(), zz.ravel()])
    faces = []
    for i in range(ny - 1):
        for j in range(nx - 1):
            v0 = i * nx + j
            v1 = i * nx + (j + 1)
            v2 = (i + 1) * nx + j
            v3 = (i + 1) * nx + (j + 1)
            faces.append([v0, v1, v2])
            faces.append([v1, v3, v2])
    return np.asarray(verts, dtype=np.float64), np.asarray(faces, dtype=np.int64)


def test_poisson_close_bit_exact():
    try:
        import sutura_core_py
    except Exception as exc:  # noqa: BLE001
        import pytest
        pytest.skip('bridge unavailable: %s' % exc)
    from sutura_engine.methods import closing as cl

    v, t = _load('thingi10k_100173.stl')
    pv, pt, prep = cl.poisson_close(v, t, depth=8)
    rv, rt, rj = sutura_core_py.closing_poisson(v, t, 8)
    rrep = json.loads(rj)
    assert np.array_equal(pv, np.asarray(rv))
    assert np.array_equal(pt, np.asarray(rt))
    assert prep['watertight'] == rrep['watertight']
    r2 = sutura_core_py.closing_poisson(v, t, 8)
    assert np.array_equal(np.asarray(rv), np.asarray(r2[0]))
    assert np.array_equal(np.asarray(rt), np.asarray(r2[1]))


def test_flat_back_relief_quality():
    try:
        import sutura_core_py
    except Exception as exc:  # noqa: BLE001
        import pytest
        pytest.skip('bridge unavailable: %s' % exc)
    from sutura_engine.methods import closing as cl

    v, t = _relief()
    pv, pt, prep = cl.flat_back_close(v, t)
    rv, rt, rj = sutura_core_py.closing_flat_back(v, t, None)
    rrep = json.loads(rj)
    assert prep['watertight'] is True
    assert rrep['watertight'] is True
    assert cl._check_validity(np.asarray(rv), np.asarray(rt))[0]
    assert len(rt) == len(pt)
    assert abs(prep['thickness'] - rrep['thickness']) < 1e-12
    r2 = sutura_core_py.closing_flat_back(v, t, None)
    assert np.array_equal(np.asarray(rv), np.asarray(r2[0]))
    assert np.array_equal(np.asarray(rt), np.asarray(r2[1]))


def test_flat_back_closed_mesh_is_noop():
    try:
        import sutura_core_py
    except Exception as exc:  # noqa: BLE001
        import pytest
        pytest.skip('bridge unavailable: %s' % exc)
    v = np.array([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0],
                  [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]])
    t = np.array([[0, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3]],
                 dtype=np.int64)
    rv, rt, rj = sutura_core_py.closing_flat_back(v, t, None)
    assert np.array_equal(v, np.asarray(rv))
    assert np.array_equal(t, np.asarray(rt))
    assert json.loads(rj)['watertight'] is True
