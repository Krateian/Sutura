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
from heatmap import render, focus_frame
from heatmap_render import load_mesh
from viewer_common import (ORANGE, GREEN, healed_face_mask,
                           worst_defect as _worst_defect, initial_view_camera)


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

        # Shared before/after camera (same as the interactive viewer opens
        # with): defect-facing rotation aimed at the worst ORIGINAL defect
        # (None for a clean mesh / defect at the bbox centre -> render falls
        # back to the isometric view), and the (center, scale) frame fitted
        # to that rotation so the mesh is not clipped or left with excessive
        # margin.
        R, center, scale = initial_view_camera(verts, rverts, orig_defects, w, h, pad=24)
        frame = (center, scale)
        worst = _worst_defect(verts, orig_defects)
        # before: original defects red on the neutral grey body (unchanged).
        before = render(verts, tris, orig_defects['holes'], orig_defects['non_manifold'],
                        w=w, h=h, frame=frame, rotation=R)
        # after: healed regions green, still-broken defects orange, everything
        # else grey (mesh keeps its default colour - no more all-teal body).
        healed = healed_face_mask(rverts, rtris, orig_defects, verts,
                                  rep_defects=rep_defects)
        after = render(rverts, rtris, rep_defects['holes'], rep_defects['non_manifold'],
                       w=w, h=h, frame=frame, defect=ORANGE, healed=healed,
                       healed_color=GREEN, rotation=R)

        # detail close-up of the worst original defect, SAME zoomed camera for
        # both views so the before/after comparison is meaningful.
        if worst is not None:
            dframe = focus_frame(verts, worst['verts_idx'], w, h, pad=24, rotation=R)
            d_before = render(verts, tris, orig_defects['holes'],
                              orig_defects['non_manifold'], w=w, h=h, frame=dframe,
                              rotation=R)
            d_after = render(rverts, rtris, rep_defects['holes'],
                             rep_defects['non_manifold'], w=w, h=h, frame=dframe,
                             defect=ORANGE, healed=healed, healed_color=GREEN,
                             rotation=R)
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