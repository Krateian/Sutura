#!/usr/bin/env python3
# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
"""repeat.py reads its 3x3 eigendecompositions from the shared core.

`np.linalg.eigh` is LAPACK and no portable implementation reproduces it bit for
bit, so `repeat.py` calls `sutura_geom.eigh3` instead. Two things must hold and
are checked here without touching the corpus:

  1. `_eigh3` returns what LAPACK returns to within numerical noise (eigenvalues
     ascending, eigenvectors as columns) and does not silently fall back when
     `SUTURA_REQUIRE_RUST=1`.
  2. The detector's score is a function of the *axis line*, not of the
     eigenvector's sign. Rotating by +theta about +ax is a different point set
     than about -ax, so a solver that flips a sign must not move the score.

Runtime target: < 30 s.  Usage: python3 tests/test_repeat_eigh3.py
"""

import os
import sys

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, 'sutura'))

import sutura_geom  # noqa: E402
from sutura_engine.methods import repeat as rpt  # noqa: E402


def _cylinder(radius=10.0, height=30.0, segments=64):
    """A closed cylinder: exactly n-fold rotationally symmetric about z."""
    ang = np.linspace(0.0, 2.0 * np.pi, segments, endpoint=False)
    ring = np.column_stack([radius * np.cos(ang), radius * np.sin(ang)])
    bottom = np.column_stack([ring, np.zeros(len(ring))])
    top = np.column_stack([ring, np.full(len(ring), height)])
    verts = np.vstack([bottom, top])
    tris = []
    for i in range(segments):
        j = (i + 1) % segments
        tris.append([i, j, segments + j])
        tris.append([i, segments + j, segments + i])
    # Caps as fans around two extra centre vertices.
    centre_b = len(verts)
    centre_t = centre_b + 1
    verts = np.vstack([verts, [[0.0, 0.0, 0.0], [0.0, 0.0, height]]])
    for i in range(segments):
        j = (i + 1) % segments
        tris.append([centre_b, j, i])
        tris.append([centre_t, segments + i, segments + j])
    return verts, np.asarray(tris, dtype=np.int64)


def test_eigh3_is_exposed():
    assert hasattr(sutura_geom, 'eigh3'), (
        'sutura_geom.eigh3 is missing; repeat.py would fall back to LAPACK')


def test_eigh3_matches_lapack():
    rng = np.random.default_rng(0)
    worst_value = 0.0
    worst_angle = 0.0
    for k in range(400):
        m = rng.normal(size=(3, 3))
        a = (m + m.T) / 2.0
        if k % 5 == 0:
            a = np.diag([1.0, 1.0, 3.0])  # repeated eigenvalue
        if k % 7 == 0:
            a = np.zeros((3, 3))
        values, vectors = sutura_geom.eigh3(a)
        ref_values, ref_vectors = np.linalg.eigh(a)
        assert np.all(np.diff(values) >= 0), values
        denom = max(float(np.abs(ref_values).max()), 1e-300)
        worst_value = max(worst_value, float(np.abs(values - ref_values).max() / denom))
        cosines = np.abs((vectors * ref_vectors).sum(axis=0))
        worst_angle = max(
            worst_angle,
            float(np.degrees(np.arccos(np.clip(cosines, -1.0, 1.0))).max()))
    assert worst_value < 1e-12, worst_value
    assert worst_angle < 1e-3, worst_angle


def test_eigh3_eigenvectors_are_canonical():
    rng = np.random.default_rng(1)
    for _ in range(200):
        m = rng.normal(size=(3, 3))
        _, vectors = sutura_geom.eigh3((m + m.T) / 2.0)
        for col in range(3):
            dominant = int(np.argmax(np.abs(vectors[:, col])))
            assert vectors[dominant, col] > 0.0, (dominant, vectors)


def test_repeat_uses_the_core_not_lapack():
    """With the binding present, `_eigh3` must not consult LAPACK at all."""
    original = np.linalg.eigh

    def boom(*args, **kwargs):
        raise AssertionError('repeat._eigh3 fell back to np.linalg.eigh')

    np.linalg.eigh = boom
    try:
        values, vectors = rpt._eigh3(np.diag([3.0, 1.0, 2.0]))
    finally:
        np.linalg.eigh = original
    assert np.allclose(values, [1.0, 2.0, 3.0])
    assert vectors.shape == (3, 3)


def test_canonical_axis_sign():
    assert rpt._canonical_axis_sign(np.array([0.1, -0.9, 0.2])) == -1.0
    assert rpt._canonical_axis_sign(np.array([-0.1, 0.9, 0.2])) == 1.0
    assert rpt._canonical_axis_sign(np.array([1.0, 0.0, 0.0])) == 1.0


def test_repeat_score_is_axis_sign_invariant():
    """Flipping every eigenvector sign must not move any result.

    This is the regression guard for the measured failure: with LAPACK the
    eigenvector signs differed from the core's and the score moved on 16 of 226
    corpus meshes, flipping `pattern_type` on two of them.
    """
    verts, tris = _cylinder()
    original = rpt._eigh3

    def flipped(cov):
        values, vectors = original(cov)
        return values, -vectors

    baseline = rpt.detect_repetition(verts, tris)
    try:
        rpt._eigh3 = flipped
        mirrored = rpt.detect_repetition(verts, tris)
    finally:
        rpt._eigh3 = original

    assert baseline[0] == mirrored[0], (baseline[0], mirrored[0])
    assert baseline[1].get('pattern_type') == mirrored[1].get('pattern_type')
    assert baseline[1].get('order') == mirrored[1].get('order')
    # A cylinder really is n-fold about z, so the detector should find it.
    assert baseline[1].get('pattern_type') in ('rotational', 'helical'), baseline[1]


def main():
    import time
    t0 = time.time()
    tests = [
        test_eigh3_is_exposed,
        test_eigh3_matches_lapack,
        test_eigh3_eigenvectors_are_canonical,
        test_repeat_uses_the_core_not_lapack,
        test_canonical_axis_sign,
        test_repeat_score_is_axis_sign_invariant,
    ]
    for test in tests:
        test()
        print(f'  ok  {test.__name__}')
    elapsed = time.time() - t0
    print(f'\nAll {len(tests)} tests passed in {elapsed:.2f} s (< 30 s target).')
    assert elapsed < 30.0, f'Tests exceeded 30 s limit: {elapsed:.2f} s'


if __name__ == '__main__':
    main()
