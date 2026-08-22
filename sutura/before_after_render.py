"""CLI entry for the on-demand before/after mesh comparison render.

Runs as a SEPARATE PROCESS spawned by the GUI (``BeforeAfterWorker``), for the
same reason as heatmap_render.py: using pymeshlab inside a Qt worker thread
while a QMainWindow exists corrupts the heap at interpreter shutdown (PySide6
6.11 + Python 3.14). This subprocess loads the original and the repaired mesh
with pymeshlab, detects defects on BOTH, and renders four PNGs:

  * before  - original mesh, defect regions red (the historical heatmap look)
  * after   - repaired mesh in the brand teal #14b8a6 (fixed/healthy), any
              remaining defects (holes / non-manifold) red
  * detail_before / detail_after - close-ups of the WORST defect region of the
    original, both framed with the SAME zoomed camera so they are comparable.

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

# Brand teal #14b8a6 - the "repaired/healthy" base colour for the after view
# (same tone as the GUI Repair button / progress bar). Defect red stays the
# historical (235, 60, 70) default of render().
TEAL = (20, 184, 166)


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
        # before: original defects red on the neutral grey body; after: teal
        # healthy body, residual defects (if any) red.
        before = render(verts, tris, orig_defects['holes'], orig_defects['non_manifold'],
                        w=w, h=h, frame=frame)
        after = render(rverts, rtris, rep_defects['holes'], rep_defects['non_manifold'],
                       w=w, h=h, frame=frame, mesh=TEAL)

        # detail close-up of the worst original defect, SAME zoomed camera for
        # both views so the before/after comparison is meaningful.
        worst = _worst_defect(verts, orig_defects)
        if worst is not None:
            dframe = focus_frame(verts, worst['verts_idx'], w, h, pad=24)
            d_before = render(verts, tris, orig_defects['holes'],
                              orig_defects['non_manifold'], w=w, h=h, frame=dframe)
            d_after = render(rverts, rtris, rep_defects['holes'],
                             rep_defects['non_manifold'], w=w, h=h, frame=dframe,
                             mesh=TEAL)
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