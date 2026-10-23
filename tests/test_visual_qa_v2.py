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


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
    print('Visual QA V2 identical-pair test passed')
