"""CLI entry for the method-12 repeated-element picker dataset.

Runs as a SEPARATE PROCESS spawned by the GUI (``RepeatPickerWorker``): the
GUI process must never import pymeshlab (using it inside a Qt worker thread
while a QMainWindow exists corrupts the heap at interpreter shutdown). This
entry loads the input mesh, decimates it to an interactive LOD and writes an
npz with plain ``verts``/``tris`` arrays; the dialog then rasterises it and maps
clicks to faces with ``heatmap`` (pure numpy + QPainter) in the GUI process.

Usage: python repeat_picker_render.py INPUT OUTPUT_NPZ
Exit 0 on success, non-zero when the mesh could not be loaded.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import numpy as np

from heatmap_render import load_mesh
import topology

# Cap the picker mesh for responsiveness; a repeated element is a local feature,
# so a decimated surface is enough to click on.
LOD_TARGET = 20000


def decimate(verts, tris, target=LOD_TARGET):
    """Cluster-decimate to at or below ``target`` faces (no-op below it).

    Native (Faz 4 W3a): ``topology.cluster_decimate`` replaces PyMeshLab's
    ``meshing_decimation_clustering``; the threshold steps are absolute model
    units (VCG ``PureValue``) and the search stops at the first step at or
    below the target.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    if len(tris) <= target:
        return verts, tris
    best = None
    for threshold in (0.5, 1.0, 1.5, 2.0, 3.0, 5.0, 8.0):
        try:
            cv, ct, _ = topology.cluster_decimate(verts, tris, float(threshold))
        except Exception:  # noqa: BLE001 - fall back to a coarser step
            continue
        if len(ct) and len(ct) <= target:
            best = (cv, ct)
            break
        if best is None:
            best = (cv, ct)
    return best if best is not None else (verts, tris)


def main():
    if len(sys.argv) != 3:
        print('usage: repeat_picker_render.py INPUT OUTPUT.npz', file=sys.stderr)
        return 2
    path, out = sys.argv[1], sys.argv[2]
    verts, tris = load_mesh(path)
    if verts is None or len(verts) == 0 or len(tris) == 0:
        print('repeat-picker: could not load mesh %s' % path, file=sys.stderr)
        return 1
    verts, tris = decimate(verts, tris)
    np.savez(out, verts=np.asarray(verts, dtype=np.float64),
             tris=np.asarray(tris, dtype=np.int64))
    return 0


if __name__ == '__main__':
    sys.exit(main())
