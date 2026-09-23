#!/usr/bin/env python3
"""Smoke test for the arrangement_lite Python binding (Phase B4).

Checks that two crossing triangles are split into a mesh with no proper
self-intersections remaining (SI=0), and that the report dict has the
expected keys.

Usage: needs the venv with the extension installed via maturin:
    maturin develop --interpreter <venv>/bin/python
    <venv>/bin/python tests/test_sutura_geom_arrangement.py
"""
import numpy as np  # noqa: E402

import sutura_geom  # noqa: E402


def test_module_exposes_arrangement_lite():
    assert hasattr(sutura_geom, 'arrangement_lite')


def _crossing_pair():
    # T1 in the XY plane, T2 vertical, piercing T1 along a proper segment.
    verts = np.array([
        [0.0, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [0.0, 2.0, 0.0],
        [1.0, -1.0, -1.0],
        [1.0, 1.0, -1.0],
        [1.0, 1.0, 1.0],
    ], dtype=np.float64)
    tris = np.array([[0, 1, 2], [3, 4, 5]], dtype=np.int32)
    return verts, tris


def _count_proper_si(verts, tris):
    """Count proper-intersection pairs by re-running arrangement_lite."""
    _, _, report = sutura_geom.arrangement_lite(verts, tris)
    return report['si_pairs_detected']


def test_two_crossing_triangles_split_to_si_zero():
    verts, tris = _crossing_pair()
    out_v, out_t, report = sutura_geom.arrangement_lite(verts, tris)

    assert report['input_faces'] == 2
    assert report['si_pairs_detected'] >= 1
    assert report['output_faces'] > report['input_faces']
    assert report['converged'] is True
    assert isinstance(report['degenerate_cases'], dict)

    # The output must be a valid mesh: no out-of-range indices.
    assert out_t.min() >= 0
    assert out_t.max() < out_v.shape[0]
    assert out_v.shape[1] == 3 and out_t.shape[1] == 3

    # Re-running arrangement_lite on the split output finds no proper
    # intersections left: SI = 0.
    assert _count_proper_si(out_v, out_t) == 0


def test_clean_pair_has_no_intersections():
    # Two separated triangles: no intersections detected, mesh unchanged.
    verts = np.array([
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 5.0],
        [1.0, 0.0, 5.0],
        [0.0, 1.0, 5.0],
    ], dtype=np.float64)
    tris = np.array([[0, 1, 2], [3, 4, 5]], dtype=np.int32)

    out_v, out_t, report = sutura_geom.arrangement_lite(verts, tris)
    assert report['si_pairs_detected'] == 0
    assert report['output_faces'] == 2


def test_bad_shape_rejected():
    try:
        sutura_geom.arrangement_lite(
            np.zeros((3, 2)), np.zeros((1, 3), dtype=np.int32))
    except (ValueError, TypeError):
        pass
    else:
        raise AssertionError('expected an error for a non-Nx3 verts array')


if __name__ == '__main__':
    fns = [v for k, v in sorted(globals().items()) if k.startswith('test_')]
    for fn in fns:
        fn()
        print('ok: %s' % fn.__name__)
    print('all %d arrangement_lite smoke tests passed' % len(fns))