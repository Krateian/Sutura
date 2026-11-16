# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

"""Proxy producer parity: Python `proxy_template_repair` vs the Rust bridge.

Public shapes only (a small open cube built with numpy). The gate is the
watertight verdict, deterministic output, empty/degenerate report and
np.array_equal mesh parity on public real-world samples.
"""
import json
import os
import sys

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, 'sutura'))


def _open_cube():
    verts = np.array([
        [0.0, 0.0, 0.0], [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0], [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0], [1.0, 0.0, 1.0],
        [1.0, 1.0, 1.0], [0.0, 1.0, 1.0],
    ], dtype=np.float64)
    faces = np.array([
        [0, 2, 1], [0, 3, 2],
        [0, 1, 5], [0, 5, 4],
        [1, 2, 6], [1, 6, 5],
        [2, 3, 7], [2, 7, 6],
        [3, 0, 4], [3, 4, 7],
    ], dtype=np.int64)
    return verts, faces


def _skip_if_unavailable():
    try:
        import sutura_core_py  # noqa: F401
        import sutura_geom  # noqa: F401
    except Exception as exc:  # noqa: BLE001
        import pytest
        pytest.skip('bridge/geometry unavailable: %s' % exc)


def test_open_cube_watertight_and_deterministic():
    _skip_if_unavailable()
    import sutura_core_py
    from sutura_engine.methods.proxy import proxy_template_repair as py_proxy

    v, t = _open_cube()
    pv, pt, prep = py_proxy(v, t)
    rv, rt, rj = sutura_core_py.proxy_template_repair(v, t)
    rrep = json.loads(rj)
    rv = np.asarray(rv)
    rt = np.asarray(rt)

    assert prep['watertight'] == rrep['watertight']
    if prep.get('watertight'):
        assert rrep['watertight'] is True
        assert rv.shape[1] == 3 and rt.shape[1] == 3
        assert len(rt) >= 4

    rv2, rt2, rj2 = sutura_core_py.proxy_template_repair(v, t)
    assert np.array_equal(rv, np.asarray(rv2))
    assert np.array_equal(rt, np.asarray(rt2))
    assert rj == rj2


def test_empty_and_degenerate_report():
    _skip_if_unavailable()
    import sutura_core_py

    ev = np.zeros((0, 3), dtype=np.float64)
    et = np.zeros((0, 3), dtype=np.int64)
    rv, rt, rj = sutura_core_py.proxy_template_repair(ev, et)
    rep = json.loads(rj)
    assert np.asarray(rv).shape == (0, 3)
    assert np.asarray(rt).shape == (0, 3)
    assert rep['watertight'] is False
    assert rep['output_faces'] == 0
    assert rep['proxy_faces'] == 0
    assert rep['error']

    dv = np.zeros((3, 3), dtype=np.float64)
    dt = np.array([[0, 1, 2]], dtype=np.int64)
    rv, rt, rj = sutura_core_py.proxy_template_repair(dv, dt)
    rep = json.loads(rj)
    assert rep['watertight'] is False
    assert rep['error']
    assert np.asarray(rt).shape == (1, 3)


def _load_public(name):
    import sutura_core_py
    from sutura_core_py import load_stl
    v, t = load_stl(os.path.join(
        REPO, 'tests', 'real-world-samples', name))
    return np.asarray(v, dtype=np.float64), np.asarray(t, dtype=np.int64)


def test_proxy_real_samples_bit_exact():
    _skip_if_unavailable()
    import sutura_core_py
    from sutura_engine.methods import proxy as px

    for name in ('artec_bovine-heart.stl', 'artec_copper-key.stl'):
        v, t = _load_public(name)
        pv, pt, prep = px.proxy_template_repair(v, t)
        rv, rt, rj = sutura_core_py.proxy_template_repair(v, t, None)
        rrep = json.loads(rj)
        assert prep['watertight'] is True, name
        assert rrep['watertight'] is True, name
        assert np.array_equal(pv, np.asarray(rv)), name
        assert np.array_equal(pt, np.asarray(rt)), name
        r2 = sutura_core_py.proxy_template_repair(v, t, None)
        assert np.array_equal(np.asarray(rv), np.asarray(r2[0])), name
        assert np.array_equal(np.asarray(rt), np.asarray(r2[1])), name
