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

# Cap the picker mesh for responsiveness; a repeated element is a local feature,
# so a decimated surface is enough to click on.
LOD_TARGET = 20000


def decimate(verts, tris, target=LOD_TARGET):
    """Cluster-decimate to at or below ``target`` faces (no-op below it)."""
    if len(tris) <= target:
        return verts, tris
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float64),
                        face_matrix=np.asarray(tris, np.int32)))
    best = None
    for threshold in (0.5, 1.0, 1.5, 2.0, 3.0, 5.0, 8.0):
        try:
            ms2 = ml.MeshSet()
            ms2.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float64),
                                 face_matrix=np.asarray(tris, np.int32)))
            ms2.apply_filter('meshing_decimation_clustering',
                             threshold=ml.PureValue(threshold))
            cur = ms2.current_mesh()
            cv = np.asarray(cur.vertex_matrix(), dtype=np.float64)
            ct = np.asarray(cur.face_matrix(), dtype=np.int64)
            if len(ct) and len(ct) <= target:
                best = (cv, ct)
                break
            if best is None:
                best = (cv, ct)
        except Exception:  # noqa: BLE001 - fall back to a coarser step
            continue
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
