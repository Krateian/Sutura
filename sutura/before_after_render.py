"""CLI entry for the on-demand before/after mesh comparison render.

Runs as a SEPARATE PROCESS spawned by the GUI (``BeforeAfterWorker``), for the
same reason as heatmap_render.py: using pymeshlab inside a Qt worker thread
while a QMainWindow exists corrupts the heap at interpreter shutdown (PySide6
6.11 + Python 3.14). This subprocess loads the original and the repaired mesh
with pymeshlab, detects defects on BOTH, and renders four PNGs:

  * before  - original mesh, defect regions red on the neutral grey body
  * after   - repaired mesh in the tri-state scheme: grey where nothing was
              ever broken, vivid green where an ORIGINAL defect used to be and
              is now healthy, orange where a defect REMAINS
  * detail_before / detail_after - close-ups of the WORST defect region of the
    original, both framed with the SAME zoomed camera so they are comparable.

The original and repaired meshes have DIFFERENT vertex/face indices (repair
changes topology), so "did this region heal?" is answered spatially: a
repaired face is "healed" when it lies within a halo of an original defect's
centroid (its real extent from ``verts_idx``) and the repaired mesh's own
detect() reports no defect there (see ``healed_face_mask``).

When the original has no defects, the detail views simply mirror the main
views (no zoom, no crash).

Usage: python before_after_render.py INPUT REPAIRED BEFORE_PNG AFTER_PNG \
              DETAIL_BEFORE_PNG DETAIL_AFTER_PNG WIDTH HEIGHT
Exit 0 on success, non-zero if either mesh could not be loaded or rendered.
"""
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import numpy as np

from defects import detect
from heatmap import render, shared_frame, focus_frame
from heatmap_render import load_mesh

# Tri-state colour scheme for the after view. Red (235,60,70) stays the
# historical defect colour used by the BEFORE view (render's default).
ORANGE = (255, 140, 60)   # still-broken regions in the after view
GREEN = (46, 204, 113)    # healed regions (render's default healed_color)

# Only the ``cap`` largest original defects drive the healed (green)
# classification, so a scan mesh with thousands of micro-cracks stays fast and
# only notable fixed regions are highlighted. The still-broken (orange)
# classification is NOT capped: it comes from the repaired mesh's own
# detect(), which always runs at full resolution.
HEALED_CAP = 256
# Halo factor applied to each defect's real extent (max |v - centroid|): a
# repaired face within radius*halo of an old defect centre is "near it".
HEALED_HALO = 1.5


def _defect_vertex_set(defect_report):
    """Union of all defect vertex indices (hole rims + non-manifold regions)."""
    dv = set()
    for h in (defect_report.get('holes') or []):
        dv.update(h.get('verts_idx') or [])
    for nm in (defect_report.get('non_manifold') or []):
        dv.update(nm.get('verts_idx') or [])
    return dv


def _broken_face_mask(verts, tris, defect_report):
    """(M,) bool mask over ``tris``: True where a face touches a defect
    vertex of ``defect_report`` (the repaired mesh's own detect())."""
    dv = _defect_vertex_set(defect_report)
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
        near_old = near_old & ~_broken_face_mask(rverts, rtris, rep_defects)
    return near_old


def _defect_diagonal(verts, defect):
    """Physical bounding-box diagonal of a defect's vertices, in mesh units.

    Both hole loops and non-manifold regions carry ``verts_idx`` when
    detected with with_indices=True, so they are compared on the same metric."""
    idx = defect.get('verts_idx')
    if idx:
        pts = verts[np.asarray(idx, dtype=np.int64)]
        return float(np.linalg.norm(pts.max(axis=0) - pts.min(axis=0)))
    return float(defect.get('diameter', 0.0))


def _worst_defect(verts, defect_report):
    """The single worst defect region by physical bounding-box diagonal.
    Returns the defect dict, or None when the mesh has no defects."""
    best, best_diag = None, -1.0
    for d in (defect_report.get('holes') or []) + (defect_report.get('non_manifold') or []):
        diag = _defect_diagonal(verts, d)
        if diag > best_diag:
            best, best_diag = d, diag
    return best


def main():
    if len(sys.argv) != 9:
        print('usage: before_after_render.py INPUT REPAIRED BEFORE_PNG AFTER_PNG '
              'DETAIL_BEFORE_PNG DETAIL_AFTER_PNG WIDTH HEIGHT', file=sys.stderr)
        return 2
    path, repaired = sys.argv[1], sys.argv[2]
    out_before, out_after = sys.argv[3], sys.argv[4]
    out_d_before, out_d_after = sys.argv[5], sys.argv[6]
    w, h = int(sys.argv[7]), int(sys.argv[8])

    verts, tris = load_mesh(path)
    if verts is None or len(verts) == 0 or len(tris) == 0:
        print('before_after: could not load original %s' % path, file=sys.stderr)
        return 1
    rverts, rtris = load_mesh(repaired)
    if rverts is None or len(rverts) == 0 or len(rtris) == 0:
        print('before_after: could not load repaired %s' % repaired, file=sys.stderr)
        return 1

    try:
        orig_defects = detect(verts, tris, with_indices=True)
        rep_defects = detect(rverts, rtris, with_indices=True)

        frame = shared_frame([verts, rverts], w, h, pad=24)
        # before: original defects red on the neutral grey body (unchanged).
        before = render(verts, tris, orig_defects['holes'], orig_defects['non_manifold'],
                        w=w, h=h, frame=frame)
        # after: healed regions green, still-broken defects orange, everything
        # else grey (mesh keeps its default colour - no more all-teal body).
        healed = healed_face_mask(rverts, rtris, orig_defects, verts,
                                  rep_defects=rep_defects)
        after = render(rverts, rtris, rep_defects['holes'], rep_defects['non_manifold'],
                       w=w, h=h, frame=frame, defect=ORANGE, healed=healed,
                       healed_color=GREEN)

        # detail close-up of the worst original defect, SAME zoomed camera for
        # both views so the before/after comparison is meaningful.
        worst = _worst_defect(verts, orig_defects)
        if worst is not None:
            dframe = focus_frame(verts, worst['verts_idx'], w, h, pad=24)
            d_before = render(verts, tris, orig_defects['holes'],
                              orig_defects['non_manifold'], w=w, h=h, frame=dframe)
            d_after = render(rverts, rtris, rep_defects['holes'],
                             rep_defects['non_manifold'], w=w, h=h, frame=dframe,
                             defect=ORANGE, healed=healed, healed_color=GREEN)
        else:
            # no defects to zoom: the detail views mirror the main views
            d_before, d_after = before, after
    except Exception as e:  # noqa: BLE001
        print('before_after: render failed: %s' % e, file=sys.stderr)
        return 1

    ok = (before.save(out_before, 'PNG') and after.save(out_after, 'PNG')
          and d_before.save(out_d_before, 'PNG') and d_after.save(out_d_after, 'PNG'))
    return 0 if ok else 1


if __name__ == '__main__':
    sys.exit(main())