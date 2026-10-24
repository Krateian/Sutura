#!/usr/bin/env python3
"""Regression tests for the deterministic coplanar shell work budget.

Two coincident triangle grids form two shells on one plane.  A 24x24 grid
exceeds the 1024-face plane cap; shell_analyze must return the input unchanged
with budget_exceeded=True instead of entering the exact coplanar CDT.  A small
grid stays below the caps.

Usage:
    <venv>/bin/python tests/test_shell_budget.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, 'sutura'))

import numpy as np  # noqa: E402

import sutura_geom  # noqa: E402


def _grid(n):
    xs = np.linspace(0.0, 1.0, n + 1)
    verts = np.array([[x, y, 0.0] for y in xs for x in xs], dtype=np.float64)
    tris = []
    for j in range(n):
        for i in range(n):
            a = j * (n + 1) + i
            b = a + 1
            c = a + (n + 1) + 1
            d = a + (n + 1)
            tris.append([a, b, c])
            tris.append([a, c, d])
    return verts, np.asarray(tris, dtype=np.int32)


def _two_copies(n):
    v1, t1 = _grid(n)
    v2 = v1.copy()
    t2 = t1 + len(v1)
    return np.vstack([v1, v2]), np.vstack([t1, t2])


def test_large_coplanar_plane_hits_budget():
    v, t = _two_copies(24)  # 2304 plane faces -> above the 1024 plane cap
    ov, ot, info = sutura_geom.shell_analyze(
        v, t, split_intersections=False)
    assert info.get('budget_exceeded') is True, info
    assert info.get('reason') == 'budget_exceeded', info
    assert info.get('max_plane_faces') == 1024, info
    assert info.get('max_coplanar_cluster_faces') == 512, info
    assert info.get('max_coplanar_constraints') == 1024, info
    assert np.array_equal(np.asarray(ov), v), 'budget must keep the input vertices'
    assert np.array_equal(np.asarray(ot), t), 'budget must keep the input triangles'
    print('ok  large coplanar plane hits budget')


def test_small_coplanar_plane_stays_below_budget():
    v, t = _two_copies(8)  # 256 plane faces
    ov, ot, info = sutura_geom.shell_analyze(
        v, t, split_intersections=False)
    assert info.get('budget_exceeded') is False, info
    assert len(ov) > 0 and len(ot) > 0
    print('ok  small coplanar plane stays below budget')


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
    print('shell budget tests passed')
