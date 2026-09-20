#!/usr/bin/env python3
"""Regression tests for viewer_data_render.py (interactive-viewer npz).

Runs the real viewer_data_render.py as a subprocess (the same isolation the
GUI uses) on a half-face-defect cube pair and a clean cube pair, then checks
the produced npz:

  1. all expected keys are present;
  2. the interactive LOD is decimated to at most LOD_TARGET triangles (and
     the full mesh is untouched);
  3. the per-vertex distance arrays line up with the repaired mesh vertex
     counts and are finite, the global Hausdorff max is >= 0, the shared
     camera frame is present, and the initial rotation is 3x3 (zeros when
     the mesh is clean -> isometric fallback);
  4. the defect cube carries a non-empty defect_vidx / empty broken_vidx
     (the repaired mesh is the closed cube).

The mesh pair builders are shared with test_before_after_render.py.

Run with the sutura venv (subprocess needs pymeshlab), with
QT_QPA_PLATFORM=offscreen so no display is needed.
Usage: QT_QPA_PLATFORM=offscreen ~/.local/share/sutura/venv/bin/python tests/test_viewer_data.py
"""
import os
import subprocess
import sys
import tempfile

os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
TESTS = os.path.join(REPO, 'tests')
for p in (SUTURA, TESTS):
    if p not in sys.path:
        sys.path.insert(0, p)

import numpy as np  # noqa: E402

from test_before_after_render import _cube_pair  # noqa: E402

EXPECTED_KEYS = {
    'verts', 'tris', 'rverts', 'rtris',
    'lverts', 'ltris', 'rlverts', 'rltris',
    'defect_vidx', 'broken_vidx', 'ldefect_vidx', 'lbroken_vidx',
    'healed', 'lhealed', 'distance', 'ldistance',
    'defect_colors', 'ldefect_colors',
    'hausdorff', 'frame_center', 'frame_scale', 'initial_rotation',
    'viewport_w', 'viewport_h',
}
LOD_TARGET = 8000


def _render(tmpdir, defect_stl, repaired_stl):
    """Run viewer_data_render.py as a subprocess; returns (npz_path, rc)."""
    renderer = os.path.join(SUTURA, 'viewer_data_render.py')
    out = os.path.join(tmpdir, 'viewer.npz')
    proc = subprocess.run(
        [sys.executable, renderer, defect_stl, repaired_stl, out, '360', '270'],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=180)
    return out, proc.returncode


def _load(out, rc):
    assert rc == 0, rc
    assert os.path.getsize(out) > 0, out
    with np.load(out) as d:
        return {k: d[k] for k in d.files}


def _check_structure(data):
    assert set(data) == EXPECTED_KEYS, (set(data) ^ EXPECTED_KEYS)
    assert len(data['ltris']) > 0 and len(data['ltris']) <= LOD_TARGET
    assert len(data['rltris']) > 0 and len(data['rltris']) <= LOD_TARGET
    # full meshes are preserved (LOD is a separate decimated copy)
    assert len(data['tris']) >= len(data['ltris'])
    assert len(data['rtris']) >= len(data['rltris'])
    # per-vertex distances line up with the repaired mesh
    assert len(data['distance']) == len(data['rverts'])
    assert len(data['ldistance']) == len(data['rlverts'])
    assert np.all(np.isfinite(data['distance']))
    assert np.all(np.isfinite(data['ldistance']))
    # healed masks line up with the repaired faces
    assert len(data['healed']) == len(data['rtris'])
    assert len(data['lhealed']) == len(data['rltris'])
    # camera + global metric
    assert float(data['hausdorff']) >= 0.0
    assert data['frame_center'].shape == (3,)
    assert float(data['frame_scale']) > 0.0
    assert data['initial_rotation'].shape == (3, 3)
    assert int(data['viewport_w']) > 0 and int(data['viewport_h']) > 0


def test_defect_cube_pair_structure():
    with tempfile.TemporaryDirectory(prefix='sutura-vd-') as tmp:
        _cube_pair('plusx', 0, tmp)  # +X face, half open
        out, rc = _render(tmp, os.path.join(tmp, 'plusx.stl'),
                          os.path.join(tmp, 'plusx_repaired.stl'))
        data = _load(out, rc)
        _check_structure(data)
        # the broken cube has a defect rim, the repaired (closed) cube none
        assert len(data['defect_vidx']) > 0, data['defect_vidx']
        assert len(data['broken_vidx']) == 0, data['broken_vidx']
        # the repaired mesh's healed mask must mark the fixed region
        assert data['healed'].any(), 'closed cube should heal the original defect'


def test_clean_cube_pair_no_defects_no_rotation():
    with tempfile.TemporaryDirectory(prefix='sutura-vd-') as tmp:
        _cube_pair('clean', 0, tmp)
        closed = os.path.join(tmp, 'clean_repaired.stl')
        out, rc = _render(tmp, closed, closed)
        data = _load(out, rc)
        _check_structure(data)
        assert len(data['defect_vidx']) == 0, data['defect_vidx']
        assert len(data['broken_vidx']) == 0, data['broken_vidx']
        # clean mesh / defect at bbox centre -> isometric fallback (zeros)
        assert not data['initial_rotation'].any()
        # identical surfaces -> all distances ~0
        assert float(data['hausdorff']) < 1e-6, data['hausdorff']
        assert data['distance'].max() < 1e-6, data['distance'].max()


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('viewer_data tests passed')


if __name__ == '__main__':
    main()