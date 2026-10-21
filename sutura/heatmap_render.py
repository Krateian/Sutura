"""CLI entry for the on-demand defect heatmap render.

Runs as a SEPARATE PROCESS spawned by the GUI (``HeatmapWorker``). The GUI
process never imports pymeshlab, because using pymeshlab inside a Qt worker
thread while a QMainWindow exists corrupts the heap at interpreter shutdown
(observed with PySide6 6.11 + Python 3.14 + offscreen/headless). Doing the
load + detect + raster in a subprocess isolates pymeshlab entirely and keeps
the GUI responsive and crash-free.

Usage: python heatmap_render.py INPUT OUTPUT_PNG WIDTH HEIGHT
Exit 0 on success, non-zero if the mesh could not be loaded or rendered.
"""
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import numpy as np

from defects import detect
from heatmap import render


def load_mesh(path):
    """Load an input mesh as plain (verts, tris) numpy arrays.

    STL/OBJ/3MF load natively without PyMeshLab. 3MF multi-object files return
    the FIRST object's mesh, matching the defect panel's first-object behaviour.
    Returns (None, None) on failure.
    """
    try:
        from sutura_engine.pipeline.mesh_io import load_mesh as _load_mesh
        v, t = _load_mesh(path)
        return (np.asarray(v, dtype=np.float64),
                np.asarray(t, dtype=np.int64))
    except Exception:
        return None, None


def main():
    if len(sys.argv) != 5:
        print('usage: heatmap_render.py INPUT OUTPUT WIDTH HEIGHT', file=sys.stderr)
        return 2
    path, out, w, h = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
    verts, tris = load_mesh(path)
    if verts is None or len(verts) == 0 or len(tris) == 0:
        print('heatmap: could not load mesh %s' % path, file=sys.stderr)
        return 1
    try:
        report = detect(verts, tris, with_indices=True)
        img = render(verts, tris, report['holes'], report['non_manifold'], w=w, h=h)
    except Exception as e:  # noqa: BLE001
        print('heatmap: render failed: %s' % e, file=sys.stderr)
        return 1
    return 0 if img.save(out, 'PNG') else 1


if __name__ == '__main__':
    sys.exit(main())
