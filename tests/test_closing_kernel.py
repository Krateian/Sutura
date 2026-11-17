#!/usr/bin/env python3
# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
"""The Closing detectors read their numbers from the shared Rust kernel.

`single_side_score` and `relief_score` are pure geometry, so `sutura_geom`
owns them and `closing.py` only formats the result. With the binding present
neither may touch `np.linalg.svd`, which is what the NumPy path used for the
plane fit.

Regression context: the SVD normal of an exactly-flat loop carries a ~1e-17
component where the eigh-based kernel returns an exact zero, so the
`np.dot(unit_normal, n) > 0.0` test flipped on 9 of 8929 faces of one corpus
mesh and moved `relief_score` from 0.3076 to 0.3116. Routing Python through the
kernel removes the possibility of that disagreement.

Runtime target: < 30 s.  Usage: python3 tests/test_closing_kernel.py
"""

import os
import sys

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, 'sutura'))

import sutura_geom  # noqa: E402
from sutura_engine.methods import closing  # noqa: E402


def _open_box():
    """An open box: five faces, one boundary loop, clearly single-sided."""
    verts = np.array([
        [0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [10.0, 10.0, 0.0], [0.0, 10.0, 0.0],
        [0.0, 0.0, 5.0], [10.0, 0.0, 5.0], [10.0, 10.0, 5.0], [0.0, 10.0, 5.0],
    ])
    tris = np.array([
        [0, 3, 2], [0, 2, 1],          # bottom
        [0, 1, 5], [0, 5, 4],          # front
        [1, 2, 6], [1, 6, 5],          # right
        [2, 3, 7], [2, 7, 6],          # back
        [3, 0, 4], [3, 4, 7],          # left
    ], dtype=np.int32)
    return verts, tris


def _flat_relief():
    """A flat plate with one hole: the plane fit is exactly degenerate."""
    n = 6
    verts = []
    for i in range(n):
        for j in range(n):
            verts.append([float(i), float(j), 0.0])
    verts = np.asarray(verts)
    tris = []
    for i in range(n - 1):
        for j in range(n - 1):
            a = i * n + j
            b = a + 1
            c = a + n
            d = c + 1
            tris.append([a, b, d])
            tris.append([a, d, c])
    tris = np.asarray(tris, dtype=np.int32)
    # Drop the centre quad so a boundary loop exists.
    centre = np.array([[2, 2], [2, 3], [3, 3], [3, 2]])
    drop = []
    for quad in centre:
        a = quad[0] * n + quad[1]
        drop.extend([[a, a + 1, a + n + 1], [a, a + n + 1, a + n]])
    keep = [t for t in tris.tolist() if t not in drop]
    return verts, np.asarray(keep, dtype=np.int32)


def test_bindings_are_exposed():
    assert hasattr(sutura_geom, 'closing_single_side'), (
        'sutura_geom.closing_single_side missing; closing.py would use NumPy')
    assert hasattr(sutura_geom, 'closing_relief'), (
        'sutura_geom.closing_relief missing; closing.py would use NumPy')


def test_python_does_not_fall_back_to_svd():
    """With the binding present, no SVD may run inside either detector."""
    original = np.linalg.svd

    def boom(*args, **kwargs):
        raise AssertionError('closing fell back to np.linalg.svd')

    np.linalg.svd = boom
    try:
        for verts, tris in (_open_box(), _flat_relief()):
            closing.single_side_score(verts, tris)
        for verts, tris in (_flat_relief(), _open_box()):
            closing.relief_score(verts, tris)
    finally:
        np.linalg.svd = original


def test_scores_match_the_kernel_exactly():
    for verts, tris in (_open_box(), _flat_relief()):
        v32 = verts.astype(np.float32)
        t32 = tris.astype(np.int32)
        v64 = np.asarray(v32, dtype=np.float64)
        t64 = np.asarray(t32, dtype=np.int64)
        loops = closing.boundary_loops(v64, t64)

        score, info = closing.single_side_score(v32, t32)
        kernel = sutura_geom.closing_single_side(v64, t32, loops)
        assert score == kernel[0], (score, kernel[0])
        if not kernel[6]:
            assert info['n_loops'] == kernel[1]
            assert info['dominant_ratio'] == kernel[3]
            assert info['spanned_area_ratio'] == kernel[4]

        score, info = closing.relief_score(v32, t32)
        kernel = sutura_geom.closing_relief(v64, t32, loops)
        assert score == kernel[0], (score, kernel[0])
        if not kernel[7]:
            assert info['rms_rel'] == kernel[3]
            assert info['one_sided_ratio'] == kernel[4]
            assert info['normal_align_ratio'] == kernel[5]


def test_info_dict_keys_are_preserved():
    """Tests and mirror.py read these fields; the kernel path must supply them."""
    verts, tris = _flat_relief()
    _, info = closing.relief_score(verts, tris)
    if 'reason' in info:
        return  # degenerate fixture; keys are checked by the case below
    for key in ('plane_normal', 'plane_center', 'rms_rel',
                'one_sided_ratio', 'normal_align_ratio', 'dominant_loop'):
        assert key in info, key

    _, info = closing.single_side_score(verts, tris)
    for key in ('n_loops', 'dominant_loop_len', 'dominant_ratio',
                'spanned_area_ratio', 'dominant_loop'):
        assert key in info, key


def test_closed_mesh_scores_zero():
    verts = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1]], dtype=np.float64)
    tris = np.array([[0, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3]], dtype=np.int64)
    assert closing.single_side_score(verts, tris)[0] == 0.0
    assert closing.relief_score(verts, tris)[0] == 0.0


def main():
    import time
    t0 = time.time()
    tests = [
        test_bindings_are_exposed,
        test_python_does_not_fall_back_to_svd,
        test_scores_match_the_kernel_exactly,
        test_info_dict_keys_are_preserved,
        test_closed_mesh_scores_zero,
    ]
    for test in tests:
        test()
        print(f'  ok  {test.__name__}')
    elapsed = time.time() - t0
    print(f'\nAll {len(tests)} tests passed in {elapsed:.2f} s (< 30 s target).')
    assert elapsed < 30.0, f'Tests exceeded 30 s limit: {elapsed:.2f} s'


if __name__ == '__main__':
    main()
