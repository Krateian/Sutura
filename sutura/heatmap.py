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

# --- Three-point lighting model (world space, camera fixed at direction 1,1,1) ---
# Light directions point from the surface toward the light source (so the
# Lambertian term is dot(normal, dir), clamped at 0).
_LIGHT_KEY = np.array([0.57735027, 0.57735027, 0.57735027])   # camera dir, bright
_LIGHT_FILL = np.array([-0.7, 0.3, 0.65])                      # camera-left, low
_LIGHT_RIM = np.array([-0.55, 0.55, -0.62])                    # behind/above, silhouette
_LIGHT_FILL = _LIGHT_FILL / np.linalg.norm(_LIGHT_FILL)
_LIGHT_RIM = _LIGHT_RIM / np.linalg.norm(_LIGHT_RIM)
_LIGHT_KEY_W = 0.62
_LIGHT_FILL_W = 0.22
_LIGHT_RIM_W = 0.16
_AMBIENT = 0.30
# defect faces get a partial shading blend so the "hot" region stays clearly red
_RED_MOD = 0.38   # 38% lighting modulation over the base red
_SHADE_LEVELS = 64
_GREY_MIN = 0.18   # fraction of the base colour preserved in full shadow


def _lighting_shade(normals):
    """Vectorized Lambertian 3-point diffuse per face normal -> shade in [0,1].

    normals: (M,3) unit face normals. Returns a (M,) float array of total
    diffuse illumination (ambient + weighted key/fill/rim), clipped to [0,1].
    """
    s = np.full(len(normals), _AMBIENT, dtype=np.float64)
    for d, w in ((_LIGHT_KEY, _LIGHT_KEY_W),
                 (_LIGHT_FILL, _LIGHT_FILL_W),
                 (_LIGHT_RIM, _LIGHT_RIM_W)):
        s += w * np.maximum(normals @ d, 0.0)
    return np.clip(s, 0.0, 1.0)


def _face_normals(verts, tris):
    """Unit face normals via vectorized cross products (degenerate -> +z)."""
    a, b, c = verts[tris[:, 0]], verts[tris[:, 1]], verts[tris[:, 2]]
    n = np.cross(b - a, c - a)
    ln = np.linalg.norm(n, axis=1)
    ln[ln == 0] = 1.0
    return n / ln[:, None]


def _shade_lut(base):
    """Quantized colour LUT (per level -> QColor) for a base RGB colour.

    For a given face shade ``v`` the channel value is
    ``base * (GREY_MIN + (1-GREY_MIN) * v)``; the LUT is indexed by a quantised
    shade so the draw loop allocates no per-face QColor objects.
    """
    levels = _SHADE_LEVELS
    lut = []
    for i in range(levels + 1):
        v = _GREY_MIN + (1 - _GREY_MIN) * (i / levels)
        lut.append(QColor(
            int(round(base[0] * v)),
            int(round(base[1] * v)),
            int(round(base[2] * v)),
        ))
    return lut


def _shade_index(shade):
    """Map a shade in [0,1] to an integer LUT index (clamped)."""
    idx = np.rint(shade * _SHADE_LEVELS).astype(np.int64)
    return np.clip(idx, 0, _SHADE_LEVELS)


def _project(verts, w, h, pad, frame=None):
    """Orthographic projection of centered verts through the isometric view.

    ``frame`` is an optional ``(center, scale)`` tuple that forces the SAME
    camera for multiple meshes (used by the before/after comparison, so the
    original and the repaired mesh frame identically). When None, the camera
    auto-fits this mesh alone.

    Returns (px, py, z) screen pixel coordinates and view-space depth.
    """
    if frame is not None:
        center, s = frame
        v = (verts - center) @ _ISOMETRIC.T
        x, y, z = v[:, 0], v[:, 1], v[:, 2]
        px = w * 0.5 + x * s
        py = h * 0.5 - y * s   # flip y (image origin top-left)
        return px, py, z
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


def shared_frame(verts_list, w, h, pad):
    """A (center, scale) camera frame that fits ALL meshes in ``verts_list``
    with the same projection, so before/after renders are directly
    comparable. Uses the combined bounding box and the combined view-space
    extent for the scale."""
    lo = np.asarray(verts_list[0]).min(axis=0)
    hi = np.asarray(verts_list[0]).max(axis=0)
    for vs in verts_list[1:]:
        lo = np.minimum(lo, np.asarray(vs).min(axis=0))
        hi = np.maximum(hi, np.asarray(vs).max(axis=0))
    center = (lo + hi) * 0.5
    xs, ys = [], []
    for vs in verts_list:
        v = (np.asarray(vs) - center) @ _ISOMETRIC.T
        xs.extend([float(v[:, 0].min()), float(v[:, 0].max())])
        ys.extend([float(v[:, 1].min()), float(v[:, 1].max())])
    span_x = (max(xs) - min(xs)) or 1.0
    span_y = (max(ys) - min(ys)) or 1.0
    s = min((w - 2 * pad) / span_x, (h - 2 * pad) / span_y)
    return center, s


def _defect_vertex_set(holes, non_manifold):
    """Union of all defect vertex indices (hole rims + non-manifold regions)."""
    dv = set()
    for h in holes or []:
        dv.update(h.get('verts_idx') or [])
    for nm in non_manifold or []:
        dv.update(nm.get('verts_idx') or [])
    return dv


def render(verts, tris, holes=None, non_manifold=None, w=240, h=180,
           pad=24, bg=(18, 22, 26), mesh=(178, 186, 194), defect=(235, 60, 70),
           frame=None):
    """Render a mesh heatmap to a QImage.

    verts: (N,3) float array; tris: (M,3) int array. ``holes``/``non_manifold``
    are the defect dict lists from ``defects.detect(..., with_indices=True)``;
    their ``verts_idx`` entries mark which vertices (and thus faces) are drawn
    red. ``frame`` is an optional ``(center, scale)`` from ``shared_frame`` to
    force the same camera as another render. Returns a QImage (RGB32). Never
    raises for empty/degenerate input: a blank image is returned so callers
    can fall back gracefully.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    img = QImage(w, h, QImage.Format_RGB32)
    img.fill(QColor(*bg))
    if len(verts) == 0 or len(tris) == 0:
        return img

    try:
        px, py, z = _project(verts, w, h, pad, frame=frame)
    except Exception:
        return img

    dv = _defect_vertex_set(holes, non_manifold)
    is_defect = np.zeros(len(verts), dtype=bool)
    if dv:
        is_defect[list(dv)] = True
    is_defect_face = is_defect[tris].any(axis=1)

    # three-point lighting: face normals -> per-face shade, split by defect
    normals = _face_normals(verts, tris)
    shade = _lighting_shade(normals)
    grey_shade = _shade_index(shade)
    red_shade = _shade_index(_RED_MOD + (1 - _RED_MOD) * shade)

    # painter's algorithm: sort far -> near by mean view-space depth
    face_z = z[tris].mean(axis=1)
    order = np.argsort(face_z)

    grey_lut = _shade_lut(mesh)
    red_lut = _shade_lut(defect)
    p = QPainter(img)
    p.setPen(Qt.NoPen)
    for i in order:
        t = tris[i]
        poly = QPolygon([
            QPoint(int(px[t[0]]), int(py[t[0]])),
            QPoint(int(px[t[1]]), int(py[t[1]])),
            QPoint(int(px[t[2]]), int(py[t[2]])),
        ])
        if is_defect_face[i]:
            p.setBrush(red_lut[red_shade[i]])
        else:
            p.setBrush(grey_lut[grey_shade[i]])
        p.drawPolygon(poly)
    p.end()
    return img
