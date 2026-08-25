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

For the interactive viewer, ``render`` is factored into
``prepare_render`` (one-time: normals, lighting LUTs, defect/healed masks,
deviation ramp) + ``draw_frame`` (per-frame: project + z-sort + QPainter),
so a drag loop only re-runs the cheap per-frame half. ``render`` remains a
wrapper around both and keeps its exact behaviour and signature. An optional
per-vertex ``deviation`` array (surface distance) drives a separate colour
ramp ("surface deviation" view); defect faces always win over the ramp.
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


def defect_camera(center, target, up_hint=None):
    """Build a defect-facing camera basis for the before/after comparison.

    ``center`` is the mesh's world-space bbox centre and ``target`` the world-
    space centroid of the worst defect. Returns a (3,3) row-based orthonormal
    rotation matrix with the same structure as ``_ISOMETRIC`` (rows =
    [right, up, forward], right-handed, det = +1), where ``forward`` points
    from the mesh centre toward the defect so the defect faces the camera.
    Returns None when ``target`` is effectively at ``center`` (norm below a
    small epsilon) so the caller can fall back to ``_ISOMETRIC``.

    ``up_hint`` is reserved for a future explicit up preference and is
    currently unused (the least-aligned world axis, tie-broken toward +Y,
    is chosen automatically).
    """
    d = np.asarray(target, dtype=np.float64) - np.asarray(center, dtype=np.float64)
    norm = np.linalg.norm(d)
    if norm < 1e-9:
        return None
    forward = d / norm
    # least-aligned world axis for the up; axis order [+Y, +Z, +X] so ties
    # break toward +Y, and the +Y gimbal pole (forward == (0,1,0)) drops to
    # +Z automatically (first zero-dot axis after +Y itself).
    axes = np.array([[0.0, 1.0, 0.0],
                     [0.0, 0.0, 1.0],
                     [1.0, 0.0, 0.0]])
    up0 = axes[int(np.argmin(np.abs(axes @ forward)))]
    right = np.cross(up0, forward)
    right = right / np.linalg.norm(right)
    up = np.cross(forward, right)
    return np.array([right, up, forward], dtype=np.float64)


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
# healed faces use the same vivid modulation so the green stays bright/exaggerated
_HEALED_MOD = _RED_MOD
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


def _project(verts, w, h, pad, frame=None, rotation=None):
    """Orthographic projection of centered verts through the isometric view.

    ``frame`` is an optional ``(center, scale)`` tuple that forces the SAME
    camera for multiple meshes (used by the before/after comparison, so the
    original and the repaired mesh frame identically). When None, the camera
    auto-fits this mesh alone. ``rotation`` is an optional 3x3 row-based
    camera basis (right/up/forward); when None the fixed isometric
    ``_ISOMETRIC`` is used.

    Returns (px, py, z) screen pixel coordinates and view-space depth.
    """
    rot = _ISOMETRIC if rotation is None else rotation
    if frame is not None:
        center, s = frame
        v = (verts - center) @ rot.T
        x, y, z = v[:, 0], v[:, 1], v[:, 2]
        px = w * 0.5 + x * s
        py = h * 0.5 - y * s   # flip y (image origin top-left)
        return px, py, z
    lo = verts.min(axis=0)
    hi = verts.max(axis=0)
    center = (lo + hi) * 0.5
    v = (verts - center) @ rot.T
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


def shared_frame(verts_list, w, h, pad, rotation=None):
    """A (center, scale) camera frame that fits ALL meshes in ``verts_list``
    with the same projection, so before/after renders are directly
    comparable. Uses the combined bounding box and the combined view-space
    extent for the scale. ``rotation`` is an optional 3x3 row-based camera
    basis (right/up/forward); when None the fixed isometric ``_ISOMETRIC`` is
    used (the center stays the world-space bbox centre, independent of the
    rotation)."""
    rot = _ISOMETRIC if rotation is None else rotation
    lo = np.asarray(verts_list[0]).min(axis=0)
    hi = np.asarray(verts_list[0]).max(axis=0)
    for vs in verts_list[1:]:
        lo = np.minimum(lo, np.asarray(vs).min(axis=0))
        hi = np.maximum(hi, np.asarray(vs).max(axis=0))
    center = (lo + hi) * 0.5
    xs, ys = [], []
    for vs in verts_list:
        v = (np.asarray(vs) - center) @ rot.T
        xs.extend([float(v[:, 0].min()), float(v[:, 0].max())])
        ys.extend([float(v[:, 1].min()), float(v[:, 1].max())])
    span_x = (max(xs) - min(xs)) or 1.0
    span_y = (max(ys) - min(ys)) or 1.0
    s = min((w - 2 * pad) / span_x, (h - 2 * pad) / span_y)
    return center, s


def focus_frame(verts, verts_idx, w, h, pad, fill=0.75, rotation=None):
    """A (center, scale) camera frame zoomed in on a defect region.

    ``verts_idx`` lists the defect's vertices (from ``defects.detect`` with
    ``with_indices=True``). The frame centers on their mean and scales so the
    defect's view-space bounding box occupies ``fill`` of the viewport, so a
    ``render(..., frame=...)`` call with it frames exactly that region. A
    defect with no vertices falls back to the mesh auto-fit camera.
    ``rotation`` is an optional 3x3 row-based camera basis; when None the
    fixed isometric ``_ISOMETRIC`` is used (and forwarded to the fallback)."""
    verts = np.asarray(verts, dtype=np.float64)
    vs = verts[np.asarray(verts_idx, dtype=np.int64)]
    if len(vs) == 0:
        return shared_frame([verts], w, h, pad, rotation=rotation)
    rot = _ISOMETRIC if rotation is None else rotation
    center = vs.mean(axis=0)
    v = (vs - center) @ rot.T
    xs, ys = v[:, 0], v[:, 1]
    span_x = float(xs.max() - xs.min()) or 1.0
    span_y = float(ys.max() - ys.min()) or 1.0
    s = min((w - 2 * pad) * fill / span_x,
            (h - 2 * pad) * fill / span_y)
    return center, s


def _defect_vertex_set(holes, non_manifold):
    """Union of all defect vertex indices (hole rims + non-manifold regions)."""
    dv = set()
    for h in holes or []:
        dv.update(h.get('verts_idx') or [])
    for nm in non_manifold or []:
        dv.update(nm.get('verts_idx') or [])
    return dv


class RenderContext:
    """Prepared per-mesh render state, reused across interactive frames.

    Created once by ``prepare_render``; ``draw_frame`` does the per-frame
    half (project + z-sort + QPainter) using it. Holds the mesh, viewport
    geometry, per-face defect/healed masks, precomputed lighting-shade LUT
    indices, the per-face deviation-ramp indices (None outside deviation
    mode) and the colour LUTs.
    """

    __slots__ = ('verts', 'tris', 'w', 'h', 'pad', 'frame', 'bg',
                 'is_defect_face', 'is_healed_face',
                 'grey_shade', 'red_shade', 'green_shade', 'dev_idx',
                 'grey_lut', 'red_lut', 'green_lut', 'dev_lut')


def deviation_quantile_index(distances, qlo=0.01, qhi=0.99, n_levels=256):
    """Map a distance array to colour-ramp indices in [0, n_levels].

    Quantile-normalized so the ramp is robust to a few large outliers: the
    ``[qlo, qhi]`` quantile range spans the full ramp, everything below qlo
    maps to 0 and above qhi to n_levels. Returns an int array of the same
    shape as ``distances``; an empty input returns an empty array."""
    d = np.asarray(distances, dtype=np.float64)
    if d.size == 0:
        return np.zeros(0, dtype=np.int64)
    lo = float(np.quantile(d, qlo))
    hi = float(np.quantile(d, qhi))
    span = hi - lo
    if span <= 0:
        return np.zeros(d.shape, dtype=np.int64)
    idx = np.floor((d - lo) / span * n_levels)
    return np.clip(idx, 0, n_levels).astype(np.int64)


def _deviation_lut(n=256):
    """Surface-deviation colour ramp navy -> cyan -> yellow -> red (n+1
    levels), indexed by ``deviation_quantile_index``."""
    stops = np.array([[10, 20, 80], [0, 180, 200],
                      [240, 220, 60], [230, 40, 40]], dtype=np.float64)
    pos = np.linspace(0.0, 1.0, len(stops))
    xs = np.linspace(0.0, 1.0, n + 1)
    rgb = np.stack([np.interp(xs, pos, stops[:, i]) for i in range(3)], axis=1)
    return [QColor(int(round(r)), int(round(g)), int(round(b)))
            for r, g, b in rgb]


def prepare_render(verts, tris, holes=None, non_manifold=None, healed=None,
                   deviation=None, w=240, h=180, pad=24, frame=None,
                   bg=(18, 22, 26), mesh=(178, 186, 194), defect=(235, 60, 70),
                   healed_color=(46, 204, 113)):
    """One-time render preparation: build a ``RenderContext``.

    Computes everything that does NOT depend on the camera: face normals,
    the lighting-shade LUT indices, the defect/healed masks and (when
    ``deviation`` is given, a per-VERTEX surface-distance array) the
    deviation-ramp indices. ``render`` is equivalent to
    ``draw_frame(prepare_render(...), rotation=...)``.

    Parameters mirror ``render``; ``deviation`` with a length that does not
    match ``len(verts)`` is ignored (no crash). Never raises for
    empty/degenerate input.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    ctx = RenderContext()
    ctx.verts, ctx.tris = verts, tris
    ctx.w, ctx.h, ctx.pad, ctx.frame, ctx.bg = w, h, pad, frame, bg

    if len(verts) == 0 or len(tris) == 0:
        ctx.is_defect_face = np.zeros(len(tris), dtype=bool)
        ctx.is_healed_face = np.zeros(len(tris), dtype=bool)
        ctx.grey_shade = np.zeros(len(tris), dtype=np.int64)
        ctx.red_shade = np.zeros(len(tris), dtype=np.int64)
        ctx.green_shade = np.zeros(len(tris), dtype=np.int64)
        ctx.dev_idx = None
        ctx.grey_lut = _shade_lut(mesh)
        ctx.red_lut = _shade_lut(defect)
        ctx.green_lut = _shade_lut(healed_color)
        ctx.dev_lut = None
        return ctx

    dv = _defect_vertex_set(holes, non_manifold)
    is_defect = np.zeros(len(verts), dtype=bool)
    if dv:
        is_defect[list(dv)] = True
    ctx.is_defect_face = is_defect[tris].any(axis=1)

    if healed is None:
        ctx.is_healed_face = np.zeros(len(tris), dtype=bool)
    else:
        ctx.is_healed_face = np.asarray(healed, dtype=bool)

    # three-point lighting: face normals -> per-face shade, split by region
    normals = _face_normals(verts, tris)
    shade = _lighting_shade(normals)
    ctx.grey_shade = _shade_index(shade)
    ctx.red_shade = _shade_index(_RED_MOD + (1 - _RED_MOD) * shade)
    ctx.green_shade = _shade_index(_HEALED_MOD + (1 - _HEALED_MOD) * shade)

    ctx.grey_lut = _shade_lut(mesh)
    ctx.red_lut = _shade_lut(defect)
    ctx.green_lut = _shade_lut(healed_color)

    if deviation is not None and len(np.asarray(deviation)) == len(verts):
        d_face = np.asarray(deviation, dtype=np.float64)[tris].mean(axis=1)
        ctx.dev_idx = deviation_quantile_index(d_face)
        ctx.dev_lut = _deviation_lut()
    else:
        ctx.dev_idx = None
        ctx.dev_lut = None
    return ctx


def draw_frame(ctx, rotation=None, scale=None):
    """Per-frame rasterise of a ``RenderContext`` (from ``prepare_render``).

    ``rotation`` is the 3x3 row-based camera basis (right/up/forward); when
    None the fixed isometric ``_ISOMETRIC`` camera is used. ``scale``
    optionally overrides the context frame's scale (interactive zoom); the
    frame centre is unchanged. Precedence per face: defect colour first,
    then the deviation ramp (when in deviation mode), then healed, then the
    neutral mesh colour. Returns a QImage (RGB32); never raises for
    empty/degenerate input (blank image).
    """
    img = QImage(ctx.w, ctx.h, QImage.Format_RGB32)
    img.fill(QColor(*ctx.bg))
    if len(ctx.tris) == 0:
        return img

    frame = ctx.frame
    if scale is not None and frame is not None:
        frame = (frame[0], scale)
    try:
        px, py, z = _project(ctx.verts, ctx.w, ctx.h, ctx.pad,
                             frame=frame, rotation=rotation)
    except Exception:
        return img

    # painter's algorithm: sort far -> near by mean view-space depth
    face_z = z[ctx.tris].mean(axis=1)
    order = np.argsort(face_z)

    p = QPainter(img)
    p.setPen(Qt.NoPen)
    for i in order:
        t = ctx.tris[i]
        poly = QPolygon([
            QPoint(int(px[t[0]]), int(py[t[0]])),
            QPoint(int(px[t[1]]), int(py[t[1]])),
            QPoint(int(px[t[2]]), int(py[t[2]])),
        ])
        if ctx.is_defect_face[i]:
            p.setBrush(ctx.red_lut[ctx.red_shade[i]])
        elif ctx.dev_idx is not None:
            p.setBrush(ctx.dev_lut[ctx.dev_idx[i]])
        elif ctx.is_healed_face[i]:
            p.setBrush(ctx.green_lut[ctx.green_shade[i]])
        else:
            p.setBrush(ctx.grey_lut[ctx.grey_shade[i]])
        p.drawPolygon(poly)
    p.end()
    return img


def render(verts, tris, holes=None, non_manifold=None, w=240, h=180,
           pad=24, bg=(18, 22, 26), mesh=(178, 186, 194), defect=(235, 60, 70),
           healed=None, healed_color=(46, 204, 113), frame=None, rotation=None,
           deviation=None):
    """Render a mesh heatmap to a QImage.

    Equivalent to ``draw_frame(prepare_render(...), rotation=rotation)``.
    verts: (N,3) float array; tris: (M,3) int array. ``holes``/``non_manifold``
    are the defect dict lists from ``defects.detect(..., with_indices=True)``;
    their ``verts_idx`` entries mark which vertices (and thus faces) are drawn
    in the ``defect`` colour. ``healed`` is an optional (M,) bool mask (one
    entry per face); faces marked healed that are NOT defect faces are drawn
    in ``healed_color``. ``deviation`` is an optional per-vertex surface-
    distance array switching the non-defect faces to the deviation colour
    ramp ("surface deviation" view). Precedence: defect faces first, then
    deviation ramp (if given), then healed, then the neutral ``mesh`` colour.
    ``frame`` is an optional ``(center, scale)`` from ``shared_frame`` to
    force the same camera as another render. ``rotation`` is an optional 3x3
    row-based camera basis (right/up/forward); when None (or not given) the
    fixed isometric ``_ISOMETRIC`` camera is used. Returns a QImage (RGB32).
    Never raises for empty/degenerate input: a blank image is returned so
    callers can fall back gracefully.
    """
    ctx = prepare_render(verts, tris, holes=holes, non_manifold=non_manifold,
                         healed=healed, deviation=deviation, w=w, h=h, pad=pad,
                         frame=frame, bg=bg, mesh=mesh, defect=defect,
                         healed_color=healed_color)
    return draw_frame(ctx, rotation=rotation)
