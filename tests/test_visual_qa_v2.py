#!/usr/bin/env python3
"""Regression test for the Visual QA V2 metrics (identical-pair invariant).

The V2 metrics are outside measurements: comparing a mesh against itself must
produce no V2 flags and zero deltas for every channel (silhouette, X-ray leak
and layers, Z-slice area/loops, sharp edges, 3D deviation and clusters).

Usage:
    <repair-venv>/bin/python tests/test_visual_qa_v2.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCRIPTS = os.path.join(REPO, '.claude', 'skills', 'sutura-verify', 'scripts')
if SCRIPTS not in sys.path:
    sys.path.insert(0, SCRIPTS)

import numpy as np  # noqa: E402

try:
    import verify_visual_qa as vvq  # noqa: E402
except ImportError as exc:  # pragma: no cover
    print('skip: cannot import verify_visual_qa (%s)' % exc)
    raise SystemExit(0)

MESHES = [
    'artec_copper-key',
    'artec_metal-nut',
    'thingi10k_100281',
    'thingi10k_100827',
    'thingi10k_46012',
]


def _tolerance(verts):
    diag = float(np.linalg.norm(verts.max(axis=0) - verts.min(axis=0)))
    return min(0.05, max(0.0125, diag / 10000.0))


def test_identical_pair_has_no_v2_flags():
    if not vvq.HAS_PYSIDE6:
        print('skip: PySide6 is not installed')
        return
    for name in MESHES:
        path = os.path.join(REPO, 'tests', 'real-world-samples', name + '.stl')
        if not os.path.exists(path):
            print('skip: missing %s' % name)
            continue
        verts, tris, normals, _ = vvq.load_mesh(path)
        r = vvq.evaluate_v2(verts, tris, normals, verts, tris, normals,
                            _tolerance(verts))
        assert r['flags'] == [], (name, r['flags'], r['reason'])
        assert abs(r['silhouette']['min_iou'] - 1.0) < 1e-9, (name, r['silhouette'])
        assert r['xray']['leak_delta_px'] == 0, (name, r['xray'])
        assert r['xray']['mean_layer_delta'] == 0.0, (name, r['xray'])
        assert r['slices']['max_area_rel'] == 0.0, (name, r['slices'])
        assert r['slices']['max_loop_delta'] == 0, (name, r['slices'])
        assert abs(r['sharp_edges']['ratio'] - 1.0) < 1e-12, (name, r['sharp_edges'])
        assert r['deviation']['max'] == 0.0, (name, r['deviation'])
        assert r['clusters'] == [], (name, r['clusters'])
        print('ok  %s' % name)


def test_xray_binding_available_when_expected():
    geom = getattr(vvq, 'sutura_geom', None)
    if not getattr(vvq, 'HAS_SUTURA_GEOM', False):
        print('skip: sutura_geom is not installed')
        return
    if not hasattr(geom, 'raycast_points'):
        print('note: installed sutura_geom predates raycast_points; '
              'the X-ray channel reports ran=False')
        return
    print('ok  sutura_geom.raycast_points present')


def _cube(offset=(0.0, 0.0, 0.0)):
    verts = np.array([[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
                      [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]], dtype=np.float64)
    verts = verts + np.asarray(offset, dtype=np.float64)
    tris = np.array([[0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
                     [0, 1, 5], [0, 5, 4], [3, 7, 6], [3, 6, 2],
                     [0, 4, 7], [0, 7, 3], [1, 2, 6], [1, 6, 5]],
                    dtype=np.int32)
    return verts, tris


def test_xray_overlap_layers_are_visible_in_reference():
    if not getattr(vvq, 'HAS_SUTURA_GEOM', False):
        print('skip: sutura_geom is not installed')
        return
    if not hasattr(vvq.sutura_geom, 'raycast_points'):
        print('skip: installed sutura_geom lacks raycast_points')
        return
    a, ta = _cube()
    b, tb = _cube((0.5, 0.0, 0.5))
    verts = np.concatenate([a, b], axis=0)
    tris = np.concatenate([ta, tb + 8], axis=0)
    center = (verts.min(axis=0) + verts.max(axis=0)) * 0.5
    grid = vvq._ray_grid_for_view([verts], np.eye(3), center)
    stats = vvq._xray_counts(verts, tris, grid)
    assert int(stats['layers'].max()) >= 2, 'overlap must produce >= 2 layers'
    assert int(stats['leak'].sum()) == 0, 'closed cubes must have no odd rays'
    img = vvq._xray_image(stats)
    orange = np.array([255, 140, 60], dtype=np.uint8)
    assert np.any(np.all(img == orange, axis=2)), 'reference map must show overlap layers'
    print('ok  xray overlap layers on reference')


def test_two_sided_deviation_sees_missing_material():
    if not getattr(vvq, 'HAS_SUTURA_GEOM', False):
        print('skip: sutura_geom is not installed')
        return
    a, ta = _cube()
    b, tb = _cube((0.5, 0.0, 0.0))
    dev = vvq.surface_deviation(a, ta, b, tb, 0.01)
    if dev['method'] is None:
        print('skip: no closest-point backend')
        return
    assert dev['candidate_to_reference'] is not None
    assert dev['reference_to_candidate'] is not None
    assert dev['max'] > 0.4
    assert dev['max'] == max(dev['candidate_to_reference']['max'],
                             dev['reference_to_candidate']['max'])
    print('ok  two-sided deviation')


def test_oblique_replaces_flat_plate_sliver():
    # A flat plate (10 x 10 x 0.1) is edge-on in the profile/cross-section views.
    verts = np.array([[x, y, z] for x in (0.0, 10.0) for y in (0.0, 10.0)
                      for z in (0.0, 0.1)], dtype=np.float64)
    _, evecs = vvq.compute_pca_axes(verts)
    cams = vvq.get_pca_cameras(evecs, verts)
    assert any('oblique' in name for name, _ in cams), 'flat plate must get an oblique view'
    for name, rot in cams:
        assert vvq._projected_aspect(verts, rot) <= 8.0 + 1e-6, (name, 'still a sliver')
    print('ok  oblique plate views')


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
    print('Visual QA V2 identical-pair test passed')
