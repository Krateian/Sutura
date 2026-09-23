#!/usr/bin/env python3
"""Smoke test for the sutura_geom Python extension (Phase A foundation crate,
rust/sutura-geom).

Checks that the PyO3 wrapper exposing robust orient3d / insphere is importable
and returns the expected signs on known cases, including degenerate/coplanar
cases that must be exactly zero.

Usage: needs the venv with the extension installed via maturin:
    maturin develop --interpreter <venv>/bin/python
    <venv>/bin/python tests/test_sutura_geom.py
"""
import os
import sys

import numpy as np  # noqa: E402

import sutura_geom  # noqa: E402


def test_module_exposes_predicates():
    assert hasattr(sutura_geom, 'orient3d')
    assert hasattr(sutura_geom, 'insphere')
    assert isinstance(sutura_geom.__version__, str)


def test_orient3d_positive():
    # pd below the (pa, pb, pc) plane -> positive (robust convention).
    assert sutura_geom.orient3d((0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, -1)) > 0


def test_orient3d_negative():
    assert sutura_geom.orient3d((0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, 1)) < 0


def test_orient3d_coplanar_is_exactly_zero():
    assert sutura_geom.orient3d((0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, 0)) == 0
    assert sutura_geom.orient3d((0, 0, 0), (1, 0, 0), (0, 1, 0), (0.25, 0.25, 0.0)) == 0


def test_orient3d_accepts_numpy_arrays():
    pa = np.array([0.0, 0.0, 0.0])
    pb = np.array([1.0, 0.0, 0.0])
    pc = np.array([0.0, 1.0, 0.0])
    pd = np.array([0.0, 0.0, -1.0])
    assert sutura_geom.orient3d(pa, pb, pc, pd) > 0


def test_insphere_center_inside_is_positive():
    # Positively-oriented regular tetrahedron (ORIGIN, X, Y, -Z) (side sqrt(2));
    # circumcenter at (0.25, 0.25, -0.25) lies inside the circumsphere.
    assert sutura_geom.insphere((0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, -1),
                                (0.25, 0.25, -0.25)) > 0


def test_insphere_outside_is_negative():
    assert sutura_geom.insphere((0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, -1),
                                (10.0, 10.0, 10.0)) < 0


def test_insphere_vertex_on_sphere_is_exactly_zero():
    assert sutura_geom.insphere((0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, -1),
                                (0, 0, 0)) == 0
    assert sutura_geom.insphere((0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, -1),
                                (1, 0, 0)) == 0


def test_bad_point_length_raises():
    try:
        sutura_geom.orient3d((0, 0), (1, 0, 0), (0, 1, 0), (0, 0, 1))
    except (ValueError, TypeError):
        pass
    else:
        raise AssertionError('expected an error for a length-2 point')


if __name__ == '__main__':
    fns = [v for k, v in sorted(globals().items()) if k.startswith('test_')]
    for fn in fns:
        fn()
        print('ok: %s' % fn.__name__)
    print('all %d sutura_geom smoke tests passed' % len(fns))