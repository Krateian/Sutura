#!/usr/bin/env python3
"""Unit tests for sutura/heatmap.py's camera frame helpers.

Checks that:
  1. focus_frame() centers on the given defect vertices' mean and returns a
     scale that frames the defect (the defect's view-space span fits within
     the image bounds when rendered with that frame).
  2. focus_frame() falls back to the mesh auto-fit camera for an empty
     verts_idx (no crash, a valid (center, scale)).
  3. shared_frame() produces a frame that fits all given meshes.

Run with any Python that has numpy + PySide6 (e.g. the sutura venv), with
QT_QPA_PLATFORM=offscreen so no display is needed.
Usage: QT_QPA_PLATFORM=offscreen ~/.local/share/sutura/venv/bin/python tests/test_heatmap.py
"""
import os
import sys

os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402
from heatmap import _ISOMETRIC, focus_frame, render, shared_frame  # noqa: E402


def _grid_mesh(n=12, extent=10.0):
    """A subdivided flat grid in the XZ plane, with the defect cluster at
    the far corner so a zoomed frame is measurably different from the whole."""
    xs = np.linspace(-extent / 2, extent / 2, n)
    zs = np.linspace(-extent / 2, extent / 2, n)
    gx, gz = np.meshgrid(xs, zs)
    verts = np.stack([gx.ravel(), np.zeros(gx.size, dtype=np.float64),
                      gz.ravel()], axis=1)
    tris = []
    for i in range(n - 1):
        for j in range(n - 1):
            a = i * n + j
            tris.append((a, a + n, a + 1))
            tris.append((a + 1, a + n, a + n + 1))
    return verts, np.asarray(tris, dtype=np.int64)


def _corner_verts(verts, corner_idx):
    return list(corner_idx)


def test_focus_frame_centers_on_defect():
    verts, _ = _grid_mesh()
    # the defect = the corner vertex cluster (last rows/cols)
    n = int(round(len(verts) ** 0.5))
    defect_idx = [i for i in range(len(verts))
                  if verts[i, 0] > 4.5 and verts[i, 2] > 4.5]
    center, scale = focus_frame(verts, defect_idx, w=240, h=180, pad=24)
    exp_center = verts[np.asarray(defect_idx)].mean(axis=0)
    assert np.allclose(center, exp_center, atol=1e-9), (center, exp_center)
    assert scale > 0, scale
    # the zoomed frame must be meaningfully tighter than the full-mesh frame
    _, full_scale = shared_frame([verts], 240, 180, 24)
    assert scale > full_scale * 1.5, (scale, full_scale)


def test_focus_frame_fits_defect_in_view():
    verts, _ = _grid_mesh()
    n = int(round(len(verts) ** 0.5))
    defect_idx = [i for i in range(len(verts))
                  if verts[i, 0] > 4.5 and verts[i, 2] > 4.5]
    w, h, pad = 240, 180, 24
    frame = focus_frame(verts, defect_idx, w, h, pad)
    img = render(verts, np.zeros((0, 3), dtype=np.int64), w=w, h=h, frame=frame)
    assert img.width() == w and img.height() == h
    # project the defect verts through the frame; all must land inside the image
    v = (verts[np.asarray(defect_idx)] - frame[0]) @ _ISOMETRIC.T
    px = w * 0.5 + v[:, 0] * frame[1]
    py = h * 0.5 - v[:, 1] * frame[1]
    assert px.min() >= 0 and px.max() <= w, (px.min(), px.max())
    assert py.min() >= 0 and py.max() <= h, (py.min(), py.max())


def test_focus_frame_empty_falls_back():
    verts, _ = _grid_mesh()
    center, scale = focus_frame(verts, [], w=240, h=180, pad=24)
    assert scale > 0, scale
    # identical to the auto-fit frame for the same mesh
    exp_center, exp_scale = shared_frame([verts], 240, 180, 24)
    assert np.allclose(center, exp_center, atol=1e-9)
    assert np.isclose(scale, exp_scale)


def test_shared_frame_fits_all():
    v1, _ = _grid_mesh(n=8, extent=4.0)
    v2, _ = _grid_mesh(n=8, extent=12.0)
    w, h, pad = 200, 200, 20
    center, scale = shared_frame([v1, v2], w, h, pad)
    assert scale > 0, scale
    for vs in (v1, v2):
        v = (vs - center) @ _ISOMETRIC.T
        assert v[:, 0].max() - v[:, 0].min() <= (w - 2 * pad) / scale + 1e-6
        assert v[:, 1].max() - v[:, 1].min() <= (h - 2 * pad) / scale + 1e-6


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('heatmap tests passed')


if __name__ == '__main__':
    main()