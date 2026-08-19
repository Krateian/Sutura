"""Offscreen defect heatmap renderer (numpy + Qt raster paint engine).

Projects the mesh with an auto-fit orthographic isometric camera, depth-sorts
the faces (painter's algorithm) and fills each face with QPainter on a CPU
QImage: red when the face touches a defect vertex (hole rim / non-manifold
region), neutral grey otherwise. The caller (the GUI) passes the defect
index lists produced by ``defects.detect(..., with_indices=True)``.

Why software raster instead of OpenGL: the GUI runs PySide6, and offscreen
OpenGL draw calls (``glDrawArrays``/``glDrawElements``) crash on headless
systems (e.g. NVIDIA without a display) and can be unavailable in the
AppImage build or macOS CI. Rendering here goes through Qt's CPU raster
engine, which works everywhere and never crashes the GUI -- it is the
always-available, guaranteed fallback the heatmap plan requires.

This module intentionally imports no pymeshlab/trimesh: the caller already
has ``verts``/``tris`` arrays, so it stays importable and unit-testable
anywhere (same rule as defects.py / classification.py).
"""
import numpy as np
from PySide6.QtCore import Qt, QPoint
from PySide6.QtGui import QImage, QPainter, QColor, QPolygon

# Fixed isometric view: look toward the origin from direction (1,1,1) with a
# consistent up, so every mesh is framed identically regardless of rotation.
_ISOMETRIC = np.array([
    [0.70710678, 0.70710678, 0.0],
    [-0.40824829, 0.40824829, 0.81649658],
    [0.57735027, -0.57735027, 0.57735027],
], dtype=np.float64)


def _project(verts, w, h, pad):
    """Orthographic projection of centered verts through the isometric view.

    Returns (px, py, z) screen pixel coordinates and view-space depth.
    """
    lo = verts.min(axis=0)
    hi = verts.max(axis=0)
    center = (lo + hi) * 0.5
    v = (verts - center) @ _ISOMETRIC.T
    x, y, z = v[:, 0], v[:, 1], v[:, 2]
    x0, x1 = float(x.min()), float(x.max())
    y0, y1 = float(y.min()), float(y.max())
    span_x = (x1 - x0) or 1.0
    span_y = (y1 - y0) or 1.0
    s = min((w - 2 * pad) / span_x, (h - 2 * pad) / span_y)
    cx, cy = (x0 + x1) * 0.5, (y0 + y1) * 0.5
    px = w * 0.5 + (x - cx) * s
    py = h * 0.5 - (y - cy) * s   # flip y (image origin top-left)
    return px, py, z


def _defect_vertex_set(holes, non_manifold):
    """Union of all defect vertex indices (hole rims + non-manifold regions)."""
    dv = set()
    for h in holes or []:
        dv.update(h.get('verts_idx') or [])
    for nm in non_manifold or []:
        dv.update(nm.get('verts_idx') or [])
    return dv


def render(verts, tris, holes=None, non_manifold=None, w=240, h=180,
           pad=24, bg=(18, 22, 26), mesh=(178, 186, 194), defect=(235, 60, 70)):
    """Render a mesh heatmap to a QImage.

    verts: (N,3) float array; tris: (M,3) int array. ``holes``/``non_manifold``
    are the defect dict lists from ``defects.detect(..., with_indices=True)``;
    their ``verts_idx`` entries mark which vertices (and thus faces) are drawn
    red. Returns a QImage (RGB32). Never raises for empty/degenerate input:
    a blank image is returned so callers can fall back gracefully.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    img = QImage(w, h, QImage.Format_RGB32)
    img.fill(QColor(*bg))
    if len(verts) == 0 or len(tris) == 0:
        return img

    try:
        px, py, z = _project(verts, w, h, pad)
    except Exception:
        return img

    dv = _defect_vertex_set(holes, non_manifold)
    is_defect = np.zeros(len(verts), dtype=bool)
    if dv:
        is_defect[list(dv)] = True
    is_defect_face = is_defect[tris].any(axis=1)

    # painter's algorithm: sort far -> near by mean view-space depth
    face_z = z[tris].mean(axis=1)
    order = np.argsort(face_z)

    mesh_c = QColor(*mesh)
    defect_c = QColor(*defect)
    p = QPainter(img)
    p.setPen(Qt.NoPen)
    for i in order:
        t = tris[i]
        poly = QPolygon([
            QPoint(int(px[t[0]]), int(py[t[0]])),
            QPoint(int(px[t[1]]), int(py[t[1]])),
            QPoint(int(px[t[2]]), int(py[t[2]])),
        ])
        p.setBrush(defect_c if is_defect_face[i] else mesh_c)
        p.drawPolygon(poly)
    p.end()
    return img
