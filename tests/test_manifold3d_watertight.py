#!/usr/bin/env python3
"""Regression test for the optional manifold3d cross-validation check
(sutura/manifold_bridge.watertight_check, added in FAZ10).

The check is an INDEPENDENT verdict that sits next to the pymeshlab-based
defects.detect() strict watertight test; it never replaces it. A mesh is
watertight iff a Manifold constructs with Error.NoError and is non-empty.

Usage: needs the venv (manifold3d):
    ~/.local/share/sutura/venv/bin/python tests/test_manifold3d_watertight.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402

import manifold_bridge  # noqa: E402


def cube():
    v = np.array([[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
                  [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]], dtype=np.float32)
    def quad(a, b, c, d):
        return [[a, b, c], [a, c, d]]
    t = np.array(quad(0, 2, 6, 4) + quad(1, 5, 7, 3) + quad(0, 1, 3, 2) +
                 quad(4, 6, 7, 5) + quad(0, 4, 5, 1) + quad(2, 3, 7, 6),
                 dtype=np.int32)
    return v, t


def test_closed_cube_watertight():
    v, t = cube()
    ok, status = manifold_bridge.watertight_check(v, t)
    assert ok is True, (ok, status)
    assert 'NoError' in status, status


def test_open_quad_not_watertight():
    v = np.array([[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0]], dtype=np.float32)
    t = np.array([[0, 1, 2], [0, 2, 3]], dtype=np.int32)
    ok, status = manifold_bridge.watertight_check(v, t)
    assert ok is False, (ok, status)


def test_single_triangle_not_watertight():
    v = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0]], dtype=np.float32)
    t = np.array([[0, 1, 2]], dtype=np.int32)
    ok, _ = manifold_bridge.watertight_check(v, t)
    assert ok is False, ok


def test_empty_input():
    ok, status = manifold_bridge.watertight_check(
        np.zeros((0, 3), dtype=np.float32), np.zeros((0, 3), dtype=np.int32))
    assert ok is False, (ok, status)


def test_never_raises_on_garbage():
    # NaN vertices must return False, never raise
    v = np.array([[0, 0, 0], [np.nan, 0, 0], [1, 1, 0]], dtype=np.float32)
    t = np.array([[0, 1, 2]], dtype=np.int32)
    ok, status = manifold_bridge.watertight_check(v, t)
    assert ok is False, (ok, status)
    assert isinstance(status, str), status


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('manifold3d watertight_check tests passed')