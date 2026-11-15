#!/usr/bin/env python3
# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

"""Mirror producer parity: Python `mirror_close` vs Rust.

Public samples only. Both mirror meshes below exercise a different stitch
path: thingi10k_224108 needs the outer-shell union, thingi10k_1038439 goes
through the same union plus topology repair. A closed cube checks the
low-confidence Poisson fallback report.
"""
import json
import os
import sys

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, 'sutura'))

MIRROR_MESHES = ('thingi10k_224108.stl', 'thingi10k_1038439.stl')


def _load(name):
    from sutura_core_py import load_stl
    v, t = load_stl(os.path.join(REPO, 'tests', 'real-world-samples', name))
    return np.asarray(v, dtype=np.float64), np.asarray(t, dtype=np.int64)


def _core():
    try:
        import sutura_core_py
    except Exception as exc:  # noqa: BLE001
        import pytest
        pytest.skip('bridge unavailable: %s' % exc)
    if not hasattr(sutura_core_py, 'mirror_close'):
        import pytest
        pytest.skip('sutura_core_py.mirror_close is missing')
    return sutura_core_py


def _cube():
    v = np.array([[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
                  [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]],
                 dtype=np.float64)
    t = np.array([[0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
                  [0, 1, 5], [0, 5, 4], [3, 7, 6], [3, 6, 2],
                  [0, 4, 7], [0, 7, 3], [1, 2, 6], [1, 6, 5]],
                 dtype=np.int64)
    return v, t


def test_mirror_close_bit_exact():
    core = _core()
    import mirror_repair as MR

    for name in MIRROR_MESHES:
        v, t = _load(name)
        pv, pt, prep = MR.mirror_close(v, t)
        rv, rt, rj = core.mirror_close(v, t)
        rrep = json.loads(rj)

        assert prep['mode'] == 'mirror', (name, prep)
        assert prep['watertight'] is True, (name, prep)
        assert rrep['mode'] == 'mirror', (name, rrep)
        assert rrep['watertight'] is True, (name, rrep)
        assert np.array_equal(pv, np.asarray(rv)), name
        assert np.array_equal(pt, np.asarray(rt)), name
        assert prep['faces'] == rrep['faces'], name
        assert prep['mirror_score'] == rrep['mirror_score'], name

        rv2, rt2, rj2 = core.mirror_close(v, t)
        assert np.array_equal(np.asarray(rv), np.asarray(rv2)), name
        assert np.array_equal(np.asarray(rt), np.asarray(rt2)), name
        assert rj == rj2, name


def test_low_score_poisson_fallback_report_equal():
    core = _core()
    import mirror_repair as MR

    v, t = _cube()
    pv, pt, prep = MR.mirror_close(v, t)
    rv, rt, rj = core.mirror_close(v, t)
    rrep = json.loads(rj)

    assert prep['mode'] == 'poisson_fallback', prep
    assert rrep['mode'] == 'poisson_fallback', rrep
    assert np.array_equal(pv, np.asarray(rv))
    assert np.array_equal(pt, np.asarray(rt))
    assert prep == rrep, (prep, rrep)

    rv2, rt2, rj2 = core.mirror_close(v, t)
    assert np.array_equal(np.asarray(rv), np.asarray(rv2))
    assert np.array_equal(np.asarray(rt), np.asarray(rt2))
    assert rj == rj2


import pytest


@pytest.mark.xfail(
    reason='open: mirror plane normal waits for the shared 3x3 eigh core '
           '(cmd p10/cli (c)); no second SVD/Jacobi is allowed')
def test_mirror_100173_shared_eigh():
    _core()
    from sutura_engine.methods import closing as cl
    from sutura_engine import mirror as mir

    v, t = _load('thingi10k_100173.stl')
    pv, pt, prep = mir.mirror_close(v, t)
    rv, rt, rj = _core().mirror_close(v, t, None)
    rrep = json.loads(rj)
    assert prep['watertight'] == rrep['watertight']
    assert prep['mode'] == rrep['mode']
    assert np.array_equal(pv, np.asarray(rv))
    assert np.array_equal(pt, np.asarray(rt))
