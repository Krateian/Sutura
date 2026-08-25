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
from heatmap import (_ISOMETRIC, defect_camera, focus_frame, render,  # noqa: E402
                     shared_frame)
from PySide6.QtGui import QImage  # noqa: E402


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


def _flat_mesh(n=12, extent=10.0):
    """A subdivided flat grid in the XY plane (all z=0) used by the
    rotation-dependent scale test: its view-space extent changes with the
    camera rotation while its world-space bbox centre is fixed."""
    xs = np.linspace(-extent / 2, extent / 2, n)
    ys = np.linspace(-extent / 2, extent / 2, n)
    gx, gy = np.meshgrid(xs, ys)
    verts = np.stack([gx.ravel(), gy.ravel(),
                      np.zeros(gx.size, dtype=np.float64)], axis=1)
    tris = []
    for i in range(n - 1):
        for j in range(n - 1):
            a = i * n + j
            tris.append((a, a + n, a + 1))
            tris.append((a + 1, a + n, a + n + 1))
    return verts, np.asarray(tris, dtype=np.int64)


def _img_bytes(img):
    """Raw RGB bytes of a QImage, so two images can be compared exactly."""
    img = img.convertToFormat(QImage.Format_RGB888)
    ptr = img.constBits()
    if isinstance(ptr, memoryview):
        ptr = ptr.tobytes()
    return ptr


def _red_count(img):
    """Count clearly red-dominant pixels (the defect colour, however the
    shading modulates it). Uses RGB888 to avoid the RGB32 BGR in-memory byte
    order. A pixel is 'defect-red' when red clearly dominates green and blue
    (rotation-independent, immune to the renderer's shade modulation)."""
    img = img.convertToFormat(QImage.Format_RGB888)
    w, h = img.width(), img.height()
    ptr = img.constBits()
    if isinstance(ptr, memoryview):
        ptr = ptr.tobytes()
    a = np.frombuffer(ptr, dtype=np.uint8).reshape((h, w, 3)).astype(np.int16)
    r, g, b = a[:, :, 0], a[:, :, 1], a[:, :, 2]
    return int(((r - g > 40) & (r - b > 40)).sum())


def test_render_with_identity_rotation_equals_default():
    verts, tris = _grid_mesh()
    w, h, pad = 240, 180, 24
    a = render(verts, tris, w=w, h=h, pad=pad)
    b = render(verts, tris, w=w, h=h, pad=pad, rotation=_ISOMETRIC)
    assert _img_bytes(a) == _img_bytes(b)


def test_shared_frame_scale_depends_on_rotation():
    verts, _ = _flat_mesh()
    w, h, pad = 200, 200, 20
    center_eye, scale_eye = shared_frame([verts], w, h, pad, rotation=np.eye(3))
    center_iso, scale_iso = shared_frame([verts], w, h, pad, rotation=_ISOMETRIC)
    assert np.allclose(center_eye, center_iso, atol=1e-9)
    assert not np.isclose(scale_eye, scale_iso), (scale_eye, scale_iso)


def test_focus_frame_empty_forwards_rotation():
    verts, _ = _grid_mesh()
    w, h, pad = 240, 180, 24
    R = np.eye(3)
    f1 = focus_frame(verts, [], w, h, pad, rotation=R)
    f2 = shared_frame([verts], w, h, pad, rotation=R)
    assert np.allclose(f1[0], f2[0], atol=1e-9)
    assert np.isclose(f1[1], f2[1])


def test_render_custom_rotation_changes_visible_defect():
    # a cube with the defect (hole) on the +Y top face
    verts = np.array([
        [1, 0, 0], [1, 0, 1], [1, 1, 0], [1, 1, 1],
        [0, 0, 0], [0, 0, 1], [0, 1, 0], [0, 1, 1],
    ], dtype=np.float64)
    quads = [
        (0, 1, 3, 2), (4, 6, 7, 5), (0, 2, 6, 4),
        (1, 5, 7, 3), (0, 4, 5, 1), (2, 3, 7, 6),
    ]
    tris = []
    for a, b, c, d in quads:
        tris += [(a, b, c), (a, c, d)]
    tris = [t for i, t in enumerate(tris) if i not in (4, 5)]  # drop +Y top face
    tris = np.asarray(tris, dtype=np.int64)

    # defect = the +Y top face vertices (indices 2,3,6,7)
    defect_idx = [2, 3, 6, 7]
    holes = [{'centroid': list(verts[np.asarray(defect_idx)].mean(axis=0)),
              'verts_idx': defect_idx}]
    w, h, pad = 240, 180, 24

    # manual camera looking straight down from +Y: forward row = (0,1,0),
    # up0 = least-aligned world axis (+Z), giving a proper right-handed basis
    R_down = np.array([[-1.0, 0.0, 0.0],
                       [0.0, 0.0, 1.0],
                       [0.0, 1.0, 0.0]], dtype=np.float64)
    assert abs(np.linalg.det(R_down) - 1.0) < 1e-9  # proper rotation

    red_iso = _red_count(render(verts, tris, holes=holes, w=w, h=h, pad=pad))
    red_down = _red_count(render(verts, tris, holes=holes, w=w, h=h, pad=pad,
                                 rotation=R_down))
    assert red_down > red_iso, (red_iso, red_down)


def test_defect_camera_plus_x():
    center = np.array([0.0, 0.0, 0.0])
    R = defect_camera(center, center + np.array([1.0, 0.0, 0.0]))
    assert R is not None
    forward = R[2]
    assert np.allclose(forward, [1.0, 0.0, 0.0], atol=1e-9)
    # right-handed: right x up == forward
    assert np.allclose(np.cross(R[0], R[1]), R[2], atol=1e-9)
    assert abs(np.linalg.det(R) - 1.0) < 1e-9


def test_defect_camera_plus_y_gimbal():
    center = np.array([0.0, 0.0, 0.0])
    R = defect_camera(center, center + np.array([0.0, 1.0, 0.0]))
    assert R is not None
    assert np.all(np.isfinite(R))
    forward = R[2]
    assert np.allclose(forward, [0.0, 1.0, 0.0], atol=1e-9)
    # up0 falls to +Z, so right/up are horizontal (no (0,1,0) component)
    assert abs(R[0, 1]) < 1e-9 and abs(R[1, 1]) < 1e-9
    assert np.allclose(np.cross(R[0], R[1]), R[2], atol=1e-9)
    assert abs(np.linalg.det(R) - 1.0) < 1e-9


def test_defect_camera_near_center_returns_none():
    center = np.array([5.0, -2.0, 3.0])
    assert defect_camera(center, center) is None
    assert defect_camera(center, center + np.array([1e-12, 0.0, 0.0])) is None


def test_defect_camera_orthonormal_various_directions():
    center = np.array([0.0, 0.0, 0.0])
    dirs = [np.array([1.0, 0.0, 0.0]),
            np.array([-1.0, 0.0, 0.0]),
            np.array([0.0, 0.0, 1.0]),
            np.array([0.0, 0.0, -1.0]),
            np.array([1.0, 1.0, 1.0]) / np.sqrt(3.0),
            np.array([1.0, -2.0, 3.0])]
    for d in dirs:
        R = defect_camera(center, center + d)
        assert R is not None
        assert np.all(np.isfinite(R))
        assert np.allclose(R @ R.T, np.eye(3), atol=1e-9), d
        assert abs(np.linalg.det(R) - 1.0) < 1e-9, d
        assert np.allclose(R[2], d / np.linalg.norm(d), atol=1e-9), d


def test_prepare_draw_matches_render():
    # regression gate: render() == draw_frame(prepare_render(...)) byte-for-
    # byte, so the interactive split never changes what the static path drew.
    from heatmap import (prepare_render, draw_frame, render,
                         deviation_quantile_index, _deviation_lut)
    verts, tris = _grid_mesh()
    holes = [{'verts_idx': [0, 1, 2]}]
    healed = np.zeros(len(tris), dtype=bool)
    healed[3] = True
    rots = [None, _ISOMETRIC, np.eye(3),
            np.array([[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]])]
    for R in rots:
        a = render(verts, tris, holes=holes, healed=healed, w=240, h=180,
                   pad=24, rotation=R)
        ctx = prepare_render(verts, tris, holes=holes, healed=healed,
                             w=240, h=180, pad=24)
        b = draw_frame(ctx, rotation=R)
        assert _img_bytes(a) == _img_bytes(b), R
    # deviation mode must be identical through both paths too
    dev = np.abs(verts[:, 0]) + np.abs(verts[:, 2])
    for R in rots:
        a = render(verts, tris, deviation=dev, w=240, h=180, pad=24, rotation=R)
        ctx = prepare_render(verts, tris, deviation=dev, w=240, h=180, pad=24)
        b = draw_frame(ctx, rotation=R)
        assert _img_bytes(a) == _img_bytes(b), R


def test_deviation_quantile_monotonic():
    # larger distance -> not-smaller ramp index; flat/empty arrays -> 0.
    from heatmap import deviation_quantile_index
    d = np.linspace(0.0, 1.0, 11)
    idx = deviation_quantile_index(d, qlo=0.0, qhi=1.0, n_levels=10)
    assert idx.tolist() == [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10], idx
    assert np.all(np.diff(idx) >= 0)
    assert deviation_quantile_index(np.zeros(5)).tolist() == [0] * 5
    assert deviation_quantile_index(np.array([])).size == 0
    # a single huge outlier must not blow up the ramp (quantile-robust)
    idx2 = deviation_quantile_index(np.array([0.0, 0.1, 0.2, 50.0]))
    assert idx2.max() <= 256 and idx2.min() >= 0


def test_deviation_ramp_endpoints_and_length():
    # the ramp covers navy at index 0 and red at the top, all colours valid.
    from heatmap import _deviation_lut
    lut = _deviation_lut(64)
    assert len(lut) == 65
    c0, clast = lut[0], lut[-1]
    assert (c0.red(), c0.green(), c0.blue()) == (10, 20, 80), c0
    assert (clast.red(), clast.green(), clast.blue()) == (230, 40, 40), clast
    for c in lut:
        assert 0 <= c.red() <= 255 and 0 <= c.green() <= 255 and 0 <= c.blue() <= 255


def test_deviation_defect_wins():
    # defect faces must be drawn in the defect colour even in deviation mode:
    # use a CONSTANT deviation (ramp collapses to navy everywhere) plus a
    # defect cluster -> the cluster must still render defect-red.
    from heatmap import render
    verts, tris = _grid_mesh()
    n = int(round(len(verts) ** 0.5))
    defect_idx = [i for i in range(len(verts))
                  if verts[i, 0] > 4.5 and verts[i, 2] > 4.5]
    holes = [{'verts_idx': defect_idx}]
    dev = np.full(len(verts), 5.0)   # constant -> all ramp index 0 (navy)
    w, h, pad = 240, 180, 24
    with_d = render(verts, tris, holes=holes, deviation=dev, w=w, h=h, pad=pad)
    without = render(verts, tris, deviation=dev, w=w, h=h, pad=pad)
    assert _red_count(without) == 0, 'constant deviation must be all-navy'
    assert _red_count(with_d) > 0, 'defect colour must win over the ramp'


def test_prepare_render_accepts_numpy_verts_idx():
    # Regression: gui.py's interactive viewer passes np.asarray(vidx) as a
    # hole's verts_idx (MeshViewport._build_contexts). heatmap._defect_vertex_set
    # used ``vs or []``, which raises ValueError on a multi-element numpy
    # array (ambiguous truth value) — so prepare_render crashed silently in
    # the viewer, _build_contexts aborted, and the interactive view never
    # rendered. prepare_render must accept plain lists AND numpy arrays.
    from heatmap import prepare_render, _defect_vertex_set
    verts, tris = _grid_mesh()
    n = int(round(len(verts) ** 0.5))
    multi = np.asarray([i for i in range(len(verts))
                        if verts[i, 0] > 3.0 and verts[i, 2] > 3.0],
                       dtype=np.int64)
    assert multi.size > 1, 'test needs a multi-element defect cluster'
    # multi-element numpy array (the gui.py case) must not raise
    ctx = prepare_render(verts, tris, holes=[{'verts_idx': multi}],
                         w=240, h=180, pad=24)
    assert _defect_vertex_set([{'verts_idx': multi}], None) == set(multi.tolist())
    assert ctx.is_defect_face.any()
    # non_manifold with a numpy array must be tolerated too
    assert _defect_vertex_set(None, [{'verts_idx': multi}]) == set(multi.tolist())
    # single-element numpy array (never raised, but keep it covered)
    one = np.asarray([0], dtype=np.int64)
    assert _defect_vertex_set([{'verts_idx': one}], None) == {0}
    ctx1 = prepare_render(verts, tris, holes=[{'verts_idx': one}],
                          w=240, h=180, pad=24)
    assert ctx1.is_defect_face.any()
    # plain Python lists still work (the historical before/after path)
    assert _defect_vertex_set([{'verts_idx': [0, 1, 2]}], None) == {0, 1, 2}


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('heatmap tests passed')


if __name__ == '__main__':
    main()