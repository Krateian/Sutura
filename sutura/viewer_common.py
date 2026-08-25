"""Shared pure helpers for the before/after mesh comparison renderers.

Used by both ``before_after_render.py`` (the static PNG pair) and
``viewer_data_render.py`` (the interactive viewer npz), so the healed/defect
classification and the initial camera never diverge between the two.

numpy + Qt only (imports ``heatmap`` for the camera helpers); no pymeshlab —
callers pass plain ``verts``/``tris`` arrays and the defect dicts from
``defects.detect``, so this module is importable in the GUI process as well
as in the subprocess entry points.
"""
import numpy as np

from heatmap import defect_camera, shared_frame

# Tri-state colour scheme for the after view. Red (235,60,70) stays the
# historical defect colour used by the BEFORE view (heatmap.render's default).
ORANGE = (255, 140, 60)   # still-broken regions in the after view
GREEN = (46, 204, 113)    # healed regions (heatmap.render's default healed_color)

# Only the ``cap`` largest original defects drive the healed (green)
# classification, so a scan mesh with thousands of micro-cracks stays fast and
# only notable fixed regions are highlighted. The still-broken (orange)
# classification is NOT capped: it comes from the repaired mesh's own
# detect(), which always runs at full resolution.
HEALED_CAP = 256
# Halo factor applied to each defect's real extent (max |v - centroid|): a
# repaired face within radius*halo of an old defect centre is "near it".
HEALED_HALO = 1.5


def defect_vertex_set(defect_report):
    """Union of all defect vertex indices (hole rims + non-manifold regions)."""
    dv = set()
    for h in (defect_report.get('holes') or []):
        dv.update(h.get('verts_idx') or [])
    for nm in (defect_report.get('non_manifold') or []):
        dv.update(nm.get('verts_idx') or [])
    return dv


def broken_face_mask(verts, tris, defect_report):
    """(M,) bool mask over ``tris``: True where a face touches a defect
    vertex of ``defect_report`` (the repaired mesh's own detect())."""
    dv = defect_vertex_set(defect_report)
    is_defect = np.zeros(len(verts), dtype=bool)
    if dv:
        is_defect[list(dv)] = True
    return is_defect[tris].any(axis=1)


def healed_face_mask(rverts, rtris, old_defects, overts, rep_defects=None,
                     cap=HEALED_CAP, halo=HEALED_HALO):
    """Per-face "healed" classification of the repaired mesh. Returns (M,) bool.

    A repaired face is *healed* (green) when it lies within ``halo * radius``
    of an ORIGINAL defect's centroid — where ``radius`` is the defect's real
    extent from its ``verts_idx`` (max |v - centroid|), NOT the bbox
    diameter — and the repaired mesh has no defect there (``rep_defects``).
    Faces far from any original defect are untouched/never-broken and stay
    grey; faces the repaired mesh still reports as defective are excluded
    from healed (they render orange).

    ``old_defects``/``rep_defects`` are the dicts from
    ``defects.detect(..., with_indices=True)`` on the original / repaired
    mesh. ``overts`` are the ORIGINAL mesh's vertices (used to measure each
    defect's extent; both meshes share the same coordinate frame). Only the
    ``cap`` largest defects (by radius) are considered.

    Pure numpy: loops over the (few) defects, vectorizes over faces, never
    materializes an (M, k, 3) distance array."""
    rverts = np.asarray(rverts, dtype=np.float32)
    rtris = np.asarray(rtris, dtype=np.int64)
    overts = np.asarray(overts, dtype=np.float32)

    centers = []
    radii = []
    for d in (old_defects.get('holes') or []) + (old_defects.get('non_manifold') or []):
        idx = d.get('verts_idx')
        if not idx:
            continue
        idx = np.asarray(idx, dtype=np.int64)
        centroid = np.asarray(d['centroid'], dtype=np.float32)
        radius = float(np.linalg.norm(overts[idx] - centroid, axis=1).max())
        centers.append(centroid)
        radii.append(radius * halo)
    if not centers:
        return np.zeros(len(rtris), dtype=bool)

    centers = np.asarray(centers, dtype=np.float32)
    radii = np.asarray(radii, dtype=np.float32)
    if len(centers) > cap:
        top = np.argsort(radii)[::-1][:cap]
        centers = centers[top]
        radii = radii[top]

    fc = rverts[rtris].mean(axis=1)              # (M,3) face centroids
    # Squared-distance nearest-neighbour over the (few) defect centres with
    # buffer reuse: avoids allocating an (M,3) temp per defect, ~5x faster on
    # million-face meshes than the naive (fc - c) ** 2 form.
    m = len(fc)
    best_d2 = np.full(m, np.inf, dtype=np.float32)
    best_r2 = np.zeros(m, dtype=np.float32)
    diff = np.empty((m, 3), dtype=np.float32)
    d2 = np.empty(m, dtype=np.float32)
    for c, r2 in zip(centers, radii.astype(np.float32) ** 2):
        np.subtract(fc, c, out=diff)
        np.multiply(diff, diff, out=diff)
        d2[:] = diff[:, 0] + diff[:, 1] + diff[:, 2]
        near = d2 < best_d2
        best_d2[near] = d2[near]
        best_r2[near] = r2
    near_old = best_d2 <= best_r2

    if rep_defects is not None:
        near_old = near_old & ~broken_face_mask(rverts, rtris, rep_defects)
    return near_old


def defect_diagonal(verts, defect):
    """Physical bounding-box diagonal of a defect's vertices, in mesh units.

    Both hole loops and non-manifold regions carry ``verts_idx`` when
    detected with with_indices=True, so they are compared on the same metric."""
    idx = defect.get('verts_idx')
    if idx:
        pts = verts[np.asarray(idx, dtype=np.int64)]
        return float(np.linalg.norm(pts.max(axis=0) - pts.min(axis=0)))
    return float(defect.get('diameter', 0.0))


def worst_defect(verts, defect_report):
    """The single worst defect region by physical bounding-box diagonal.
    Returns the defect dict, or None when the mesh has no defects."""
    best, best_diag = None, -1.0
    for d in (defect_report.get('holes') or []) + (defect_report.get('non_manifold') or []):
        diag = defect_diagonal(verts, d)
        if diag > best_diag:
            best, best_diag = d, diag
    return best


def initial_view_camera(verts, rverts, orig_defects, w, h, pad=24):
    """The shared before/after camera used by both the static and interactive
    views, so the interactive viewer opens where the static comparison left
    off.

    Returns ``(rotation, center, scale)``: ``rotation`` is the defect-facing
    3x3 camera basis (``heatmap.defect_camera``) aimed at the worst ORIGINAL
    defect, or None when the mesh is clean or the defect sits at the bbox
    centre (callers fall back to ``_ISOMETRIC``); ``center``/``scale`` is the
    ``shared_frame`` tuple recomputed with that rotation so the scale fits
    the rotation's view axes.
    """
    center = shared_frame([verts, rverts], w, h, pad=pad)[0]
    worst = worst_defect(verts, orig_defects)
    R = defect_camera(center, worst['centroid']) if worst is not None else None
    center, scale = shared_frame([verts, rverts], w, h, pad=pad, rotation=R)
    return R, center, scale