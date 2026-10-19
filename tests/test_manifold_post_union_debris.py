#!/usr/bin/env python3
"""Regression test for the F-06 debris filters in the Rust Stage-2 rebuild.

The pre-union filter drops flat/collapsed shells; the post-union filter drops
micro-slivers isolated by the boolean union. Both live in
``sutura_geom.stage2_rebuild`` now (they were in ``manifold_bridge``).

Usage:
    python3 tests/test_manifold_post_union_debris.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402

import sutura_geom  # noqa: E402


def _cube(center=(0.0, 0.0, 0.0), size=10.0):
    c = np.asarray(center, dtype=np.float32)
    h = size / 2.0
    v = np.array([[-h, -h, -h], [h, -h, -h], [h, h, -h], [-h, h, -h],
                  [-h, -h, h], [h, -h, h], [h, h, h], [-h, h, h]],
                 dtype=np.float32) + c

    def quad(a, b, c_, d):
        return [[a, b, c_], [a, c_, d]]

    t = np.array([[0, 2, 1], [0, 3, 2],
                  [4, 5, 6], [4, 6, 7],
                  [0, 1, 5], [0, 5, 4],
                  [3, 7, 6], [3, 6, 2],
                  [0, 4, 7], [0, 7, 3],
                  [1, 2, 6], [1, 6, 5]], dtype=np.int32)
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


def test_pre_union_drops_micro_sliver():
    _, _, rep = _run([_cube(), _cube((20, 20, 20), 0.01)])
    assert rep['ok'] is True, rep
    assert rep.get('debris_parts_dropped') == 1, rep
    assert rep['shells'] == 1, rep
    assert abs(rep['volume_after'] - 1000.0) < 1e-3, rep


def test_preserves_multi_part_assembly():
    ov, ot, rep = _run([_cube(), _cube((15, 0, 0))])
    assert rep['ok'] is True, rep
    assert rep.get('debris_parts_dropped', 0) == 0, rep
    assert rep.get('post_union_debris_dropped', 0) == 0, rep
    assert rep['shells'] == 2, rep
    assert abs(rep['volume_after'] - 2000.0) < 1e-3, rep


def test_never_drops_the_largest_part():
    _, _, rep = _run([_cube((0, 0, 0), 0.01),
                      _cube((5, 5, 5), 0.01),
                      _cube((9, 9, 9), 0.01)])
    assert rep['ok'] is True, rep
    # All three are debris, so the pre-union filter keeps them (never drop
    # everything) and the post-union filter keeps only the largest.
    assert rep.get('debris_parts_dropped') == 3, rep
    assert rep.get('post_union_debris_dropped') == 2, rep
    assert rep['shells'] == 1, rep
    assert not np.isnan(rep['volume_after'])


def test_single_part_is_untouched():
    _, _, rep = _run([_cube()])
    assert rep['ok'] is True, rep
    assert rep.get('debris_parts_dropped', 0) == 0, rep
    assert rep.get('post_union_debris_dropped', 0) == 0, rep
    assert rep['shells'] == 1, rep
    assert abs(rep['volume_after'] - 1000.0) < 1e-3, rep


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('Manifold post-union debris tests passed')
