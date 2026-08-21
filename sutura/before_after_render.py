"""CLI entry for the on-demand before/after mesh comparison render.

Runs as a SEPARATE PROCESS spawned by the GUI (``BeforeAfterWorker``), for the
same reason as heatmap_render.py: using pymeshlab inside a Qt worker thread
while a QMainWindow exists corrupts the heap at interpreter shutdown (PySide6
6.11 + Python 3.14). This subprocess loads the original and the repaired mesh
with pymeshlab, renders BOTH with the same shared isometric camera frame (so
they are directly comparable), and writes two PNG files.

Usage: python before_after_render.py INPUT REPAIRED BEFORE_PNG AFTER_PNG WIDTH HEIGHT
Exit 0 on success, non-zero if either mesh could not be loaded or rendered.
"""
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from heatmap import render, shared_frame
from heatmap_render import load_mesh


def main():
    if len(sys.argv) != 7:
        print('usage: before_after_render.py INPUT REPAIRED BEFORE_PNG AFTER_PNG WIDTH HEIGHT',
              file=sys.stderr)
        return 2
    path, repaired = sys.argv[1], sys.argv[2]
    out_before, out_after = sys.argv[3], sys.argv[4]
    w, h = int(sys.argv[5]), int(sys.argv[6])

    verts, tris = load_mesh(path)
    if verts is None or len(verts) == 0 or len(tris) == 0:
        print('before_after: could not load original %s' % path, file=sys.stderr)
        return 1
    rverts, rtris = load_mesh(repaired)
    if rverts is None or len(rverts) == 0 or len(rtris) == 0:
        print('before_after: could not load repaired %s' % repaired, file=sys.stderr)
        return 1
    try:
        frame = shared_frame([verts, rverts], w, h, pad=24)
        before = render(verts, tris, w=w, h=h, frame=frame)
        after = render(rverts, rtris, w=w, h=h, frame=frame)
    except Exception as e:  # noqa: BLE001
        print('before_after: render failed: %s' % e, file=sys.stderr)
        return 1
    ok = before.save(out_before, 'PNG') and after.save(out_after, 'PNG')
    return 0 if ok else 1


if __name__ == '__main__':
    sys.exit(main())