#!/usr/bin/env python3
"""Regression test for the F-06 post-union debris filter in manifold_bridge.

The boolean union can cut intersecting input shells and isolate microscopic,
disconnected slivers that no slicer should print. ``_drop_post_union_debris``
sits after ``batch_boolean``, keeps the largest part unconditionally and drops
only parts that ``_is_debris_part`` classifies as flat/zero-volume debris.

Usage (needs manifold3d + trimesh, i.e. the stage-2 venv):
    ./sutura/venv311/bin/python tests/test_manifold_post_union_debris.py
"""
import os
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402
import manifold3d as m3d  # noqa: E402

import manifold_bridge  # noqa: E402


def _big_cube(translate=(0.0, 0.0, 0.0)):
    return m3d.Manifold.cube([10.0, 10.0, 10.0]).translate(list(translate))


def _micro_sliver(translate=(0.0, 0.0, 0.0)):
    return m3d.Manifold.cube([0.01, 0.01, 0.01]).translate(list(translate))


def test_post_union_drops_micro_sliver():
    combined = m3d.Manifold.compose([_big_cube(), _micro_sliver((20, 20, 20))])
    assert len(combined.decompose()) == 2
    report = {}
    result, dropped = manifold_bridge._drop_post_union_debris(combined, report)
    assert dropped == 1, dropped
    assert report['post_union_debris_dropped'] == 1, report
    assert len(result.decompose()) == 1
    assert abs(float(result.volume()) - 1000.0) < 1e-3


def test_post_union_preserves_multi_part_assembly():
    combined = m3d.Manifold.compose([_big_cube(), _big_cube((15, 0, 0))])
    assert len(combined.decompose()) == 2
    report = {}
    result, dropped = manifold_bridge._drop_post_union_debris(combined, report)
    assert dropped == 0, dropped
    assert 'post_union_debris_dropped' not in report, report
    assert len(result.decompose()) == 2
    assert abs(float(result.volume()) - 2000.0) < 1e-3


def test_never_drops_the_largest_part():
    # Every part is debris: the largest must still survive untouched.
    combined = m3d.Manifold.compose([
        _micro_sliver(), _micro_sliver((5, 5, 5)), _micro_sliver((9, 9, 9))])
    assert len(combined.decompose()) == 3
    report = {}
    result, dropped = manifold_bridge._drop_post_union_debris(combined, report)
    assert dropped == 2, dropped
    assert len(result.decompose()) == 1
    assert not result.is_empty()


def test_single_part_is_untouched():
    report = {}
    result, dropped = manifold_bridge._drop_post_union_debris(_big_cube(), report)
    assert dropped == 0
    assert report == {}
    assert len(result.decompose()) == 1


def test_run_bridge_keeps_legitimate_parts():
    # End-to-end: two disjoint solid cubes must survive Stage 2 intact.
    combined = m3d.Manifold.compose([_big_cube(), _big_cube((15, 0, 0))])
    out = combined.to_mesh()
    verts = np.asarray(out.vert_properties)[:, :3].astype(np.float64)
    tris = np.asarray(out.tri_verts).astype(np.int64)
    with tempfile.TemporaryDirectory() as tmp:
        src = os.path.join(tmp, 'in.obj')
        dst = os.path.join(tmp, 'out.obj')
        manifold_bridge.write_obj(src, verts, tris)
        report = manifold_bridge.run_bridge(src, dst)
    assert report['ok'] is True, report
    assert report.get('post_union_debris_dropped', 0) == 0, report
    assert report['shells'] == 2, report


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('manifold post-union debris tests passed')
