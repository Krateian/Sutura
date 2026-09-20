"""CLI entry that produces the interactive-viewer dataset (one .npz).

Runs as a SEPARATE PROCESS spawned by the GUI the first time the user
switches the before/after dialog to the interactive view (lazy). Same
isolation rule as heatmap_render.py / before_after_render.py: pymeshlab
never runs inside the GUI process.

Loads the original and repaired meshes with pymeshlab, detects defects on
both, computes the healed per-face mask, the per-vertex surface-deviation
distance (repaired -> original) plus the global Hausdorff max, decimates
both meshes to an interactive LOD, and writes ONE npz with everything the
interactive MeshViewport needs: full + LOD meshes, defect masks, healed
masks, distances, the shared before/after camera and the initial
defect-facing rotation.

Usage: python viewer_data_render.py INPUT REPAIRED OUT_NPZ [WIDTH HEIGHT]
Exit 0 on success, non-zero if either mesh could not be loaded/processed.
"""
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import numpy as np

import pymeshlab as ml

from defects import detect, defect_type_colors
from heatmap_render import load_mesh
from viewer_common import (defect_vertex_set,
                           healed_face_mask, initial_view_camera)

# Interactive-LOD triangle target for the drag preview. Clustering
# decimation with a percentage threshold is non-linear and mesh-dependent
# (larger pct -> fewer triangles; on a 2.5M-face mesh 2% -> ~5.5k, 3% -> ~2.4k,
# 5% -> ~3.4k), so _decimate_lod tries ascending steps and picks the result
# AT OR BELOW the target that is closest to it (never above). Meshes already
# at or below the target are used as-is (LOD == full).
LOD_TARGET = 3500


def _defect_vertex_list(defect_report):
    """Sorted vertex index list of all defect regions (holes + non-manifold)."""
    return sorted(defect_vertex_set(defect_report))


def _decimate_lod(verts, tris, target=LOD_TARGET):
    """Cluster-decimate to AT MOST ``target`` triangles (fast, topology-free).

    Returns (lverts, ltris) in the same dtypes as the input. Meshes already
    at or below the target are returned unchanged. Tries the threshold steps
    in ascending order (larger pct -> fewer triangles) and picks the result
    that lands AT OR BELOW the target and is CLOSEST to it, so the LOD never
    stays above the target and never over-collapses when a step jumps far
    below it. If no step reaches the target, the most aggressive (smallest)
    result is used. Never raises for empty input.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    if len(tris) == 0 or len(tris) <= target:
        return verts, tris
    best_below = None   # (face_count, lverts, ltris) closest to target from below
    fallback = None     # (face_count, lverts, ltris) smallest result overall
    for pct in (1.0, 1.5, 2.0, 3.0, 5.0, 8.0):
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=verts, face_matrix=tris.astype(np.int32)))
        ms.apply_filter('meshing_decimation_clustering',
                        threshold=ml.PercentageValue(pct))
        m = ms.current_mesh()
        lv = np.asarray(m.vertex_matrix(), dtype=np.float64)
        lt = np.asarray(m.face_matrix(), dtype=np.int64)
        n = len(lt)
        if n == 0:
            continue
        if fallback is None or n < fallback[0]:
            fallback = (n, lv, lt)
        if n <= target:
            if best_below is None or abs(n - target) < abs(best_below[0] - target):
                best_below = (n, lv, lt)
    if best_below is not None:
        return best_below[1], best_below[2]
    if fallback is not None:
        return fallback[1], fallback[2]
    return verts, tris


def _surface_distance(measure_verts, measure_tris, ref_verts, ref_tris):
    """Per-vertex unsigned distance from the measure mesh to the ref surface.

    Uses pymeshlab's nearest-surface-point filter (unsigned), so the two
    meshes need not share topology. Returns a (len(measure_verts),) float64
    array."""
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(measure_verts, np.float64),
                        face_matrix=np.asarray(measure_tris, np.int32)), 'measure')
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(ref_verts, np.float64),
                        face_matrix=np.asarray(ref_tris, np.int32)), 'ref')
    ms.apply_filter('compute_scalar_by_distance_from_another_mesh_per_vertex',
                    measuremesh=0, refmesh=1, signeddist=False)
    return np.asarray(ms.mesh(0).vertex_scalar_array(), dtype=np.float64)


def _hausdorff(verts, tris, rverts, rtris):
    """Global one-sided Hausdorff distance of original -> repaired (max of
    the sampled-mesh vertex qualities)."""
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float64),
                        face_matrix=np.asarray(tris, np.int32)), 'a')
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(rverts, np.float64),
                        face_matrix=np.asarray(rtris, np.int32)), 'b')
    ms.apply_filter('get_hausdorff_distance', sampledmesh=0, targetmesh=1,
                    samplenum=100000)
    q = np.asarray(ms.mesh(0).vertex_scalar_array(), dtype=np.float64)
    return float(q.max()) if len(q) else 0.0


def main():
    if len(sys.argv) not in (4, 6):
        print('usage: viewer_data_render.py INPUT REPAIRED OUT_NPZ [WIDTH HEIGHT]',
              file=sys.stderr)
        return 2
    path, repaired, out_npz = sys.argv[1], sys.argv[2], sys.argv[3]
    w, h = (int(sys.argv[4]), int(sys.argv[5])) if len(sys.argv) == 6 else (720, 540)

    verts, tris = load_mesh(path)
    if verts is None or len(verts) == 0 or len(tris) == 0:
        print('viewer_data: could not load original %s' % path, file=sys.stderr)
        return 1
    rverts, rtris = load_mesh(repaired)
    if rverts is None or len(rverts) == 0 or len(rtris) == 0:
        print('viewer_data: could not load repaired %s' % repaired, file=sys.stderr)
        return 1

    try:
        orig_defects = detect(verts, tris, with_indices=True)
        rep_defects = detect(rverts, rtris, with_indices=True)

        # healed mask on the full repaired mesh (authoritative).
        healed = healed_face_mask(rverts, rtris, orig_defects, verts,
                                  rep_defects=rep_defects)
        # per-vertex surface deviation: repaired -> original surface.
        distance = _surface_distance(rverts, rtris, verts, tris)
        hausdorff = _hausdorff(verts, tris, rverts, rtris)

        # interactive LOD + per-LOD masks (drag preview; rep_defects skipped
        # on the LOD so the full-res release frame stays authoritative).
        lverts, ltris = _decimate_lod(verts, tris)
        rlverts, rltris = _decimate_lod(rverts, rtris)
        lorig_defects = detect(lverts, ltris, with_indices=True)
        lrep_defects = detect(rlverts, rltris, with_indices=True)
        lhealed = healed_face_mask(rlverts, rltris, orig_defects, verts,
                                   rep_defects=None)
        ldistance = _surface_distance(rlverts, rltris, verts, tris)

        # shared before/after camera (same as the static view opens with).
        R, center, scale = initial_view_camera(verts, rverts, orig_defects, w, h, pad=24)

        np.savez(out_npz,
                 verts=verts, tris=tris, rverts=rverts, rtris=rtris,
                 lverts=lverts, ltris=ltris, rlverts=rlverts, rltris=rltris,
                 defect_vidx=np.asarray(_defect_vertex_list(orig_defects), np.int64),
                 broken_vidx=np.asarray(_defect_vertex_list(rep_defects), np.int64),
                 ldefect_vidx=np.asarray(_defect_vertex_list(lorig_defects), np.int64),
                 lbroken_vidx=np.asarray(_defect_vertex_list(lrep_defects), np.int64),
                 healed=healed.astype(np.bool_), lhealed=lhealed.astype(np.bool_),
                 distance=distance, ldistance=ldistance,
                 hausdorff=hausdorff,
                 defect_colors=defect_type_colors(verts, tris),
                 ldefect_colors=defect_type_colors(lverts, ltris),
                 frame_center=center, frame_scale=scale,
                 initial_rotation=(R if R is not None else np.zeros((3, 3), np.float64)),
                 viewport_w=w, viewport_h=h)
    except Exception as e:  # noqa: BLE001
        print('viewer_data: processing failed: %s' % e, file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())