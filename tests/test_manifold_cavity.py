#!/usr/bin/env python3
"""Regression test for the Stage-2 cavity-preserving fallback.

An intentional internal void (a CAD pocket / magnet or insert hole) appears in
the loaded mesh as an inner shell whose faces are inverted (negative signed
volume). ``Manifold::from_mesh`` keeps it as a cavity, but the decomposed
``Add`` union reorients every part outward and fills it. ``stage2_rebuild`` now
keeps the whole oriented solid when the union would grow the cavity-aware
volume, so such a void survives; a buried *positive* shell (no cavity) and the
debris filters are unchanged.

Usage:
    python3 tests/test_manifold_cavity.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402

import sutura_geom  # noqa: E402


def _cube(center=(0.0, 0.0, 0.0), size=10.0, invert=False):
    c = np.asarray(center, dtype=np.float32)
    h = size / 2.0
    v = np.array([[-h, -h, -h], [h, -h, -h], [h, h, -h], [-h, h, -h],
                  [-h, -h, h], [h, -h, h], [h, h, h], [-h, h, h]],
                 dtype=np.float32) + c
    t = np.array([[0, 2, 1], [0, 3, 2],
                  [4, 5, 6], [4, 6, 7],
                  [0, 1, 5], [0, 5, 4],
                  [3, 7, 6], [3, 6, 2],
                  [0, 4, 7], [0, 7, 3],
                  [1, 2, 6], [1, 6, 5]], dtype=np.int32)
    if invert:
        t = t[:, ::-1].copy()
    return v, t


def _compose(parts):
    verts = []
    tris = []
    offset = 0
    for v, t in parts:
        verts.append(v)
        tris.append(t + offset)
        offset += len(v)
    return np.vstack(verts), np.vstack(tris)


def _run(parts):
    v, t = _compose(parts)
    ov, ot, rep = sutura_geom.stage2_rebuild(v, t)
    return np.asarray(ov), np.asarray(ot), rep


def test_internal_cavity_is_preserved():
    # Outer solid 10x10x10 with a 2x2x2 internal void (inverted inner shell).
    ov, ot, rep = _run([_cube(), _cube(size=2.0, invert=True)])
    assert rep['ok'] is True, rep
    assert rep.get('cavity_preserved') is True, rep
    # Whole oriented solid: 1000 (outer) - 8 (void) == cavity-aware volume_before.
    assert abs(rep['volume_before'] - 992.0) < 1e-3, rep
    assert abs(rep['volume_after'] - 992.0) < 1e-3, rep
    # The union would have filled it (1000): the fallback must not.
    assert abs(rep['volume_after_union'] - 1000.0) < 1e-3, rep
    wt, _ = sutura_geom.watertight_check(ov, ot)
    assert wt is True, rep


def test_buried_positive_shell_is_unchanged():
    # A positively-oriented solid buried in material (37890 comp3): the union
    # only drops its redundant inner boundary, volume does not grow.
    ov, ot, rep = _run([_cube(), _cube(size=2.0)])
    assert rep['ok'] is True, rep
    assert 'cavity_preserved' not in rep, rep
    assert abs(rep['volume_before'] - 1008.0) < 1e-3, rep
    assert abs(rep['volume_after'] - 1000.0) < 1e-3, rep


def test_cavity_with_debris_keeps_debris_behaviour():
    # A cavity plus a pre-union debris sliver: the debris filter must still run.
    # The fallback is deliberately skipped here (a kept whole solid would
    # resurrect the dropped sliver), so the cavity stays filled for now.
    ov, ot, rep = _run([_cube(), _cube(size=2.0, invert=True),
                        _cube(center=(100.0, 0.0, 0.0), size=0.01)])
    assert rep['ok'] is True, rep
    assert rep.get('debris_parts_dropped') == 1, rep
    assert 'cavity_preserved' not in rep, rep
    assert abs(rep['volume_after'] - 1000.0) < 1e-3, rep


def test_parallel_inner_shell_is_filled():
    # A large inverted inner shell is a near-copy of the body (its bbox
    # diagonal is 0.6 of the body's), not a meaningful pocket.  It must be
    # filled like the union (39344 interior), independently of the SI guard.
    ov, ot, rep = _run([_cube(), _cube(size=6.0, invert=True)])
    assert rep['ok'] is True, rep
    assert 0.55 <= rep.get('cavity_bbox_ratio', 0.0) <= 0.65, rep
    assert rep.get('cavity_rejected_bbox') is True, rep
    assert rep.get('cavity_preserved') is not True, rep
    # The union filled the void: 1000, not the cavity-aware 784.
    assert abs(rep['volume_after'] - 1000.0) < 1e-3, rep
    wt, _ = sutura_geom.watertight_check(ov, ot)
    assert wt is True, rep


def test_cavity_crossing_the_host_is_rejected():
    # An inverted inner shell that crosses the outer wall is not a clean void:
    # the exact SI of the whole is higher than the union's, so the union must
    # win (44041).  Part count alone is not a criterion.
    ov, ot, rep = _run([_cube(), _cube(center=(4.0, 0.0, 0.0), size=4.0,
                                       invert=True)])
    assert rep['ok'] is True, rep
    assert rep.get('cavity_si_whole', 0) > rep.get('cavity_si_union', 0), rep
    assert rep.get('cavity_preserved') is not True, rep
    assert rep.get('cavity_rejected_si') is True, rep
    wt, _ = sutura_geom.watertight_check(ov, ot)
    assert wt is True, rep


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('Manifold cavity-preservation tests passed')
