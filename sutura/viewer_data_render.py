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
import sutura_geom

from defects import detect, defect_type_colors
from heatmap_render import load_mesh
import topology
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

    Native (Faz 4 W3a): uses ``topology.cluster_decimate`` (the Rust
    vertex-clustering pass replacing PyMeshLab's ``meshing_decimation_clustering``).
    Returns (lverts, ltris) in the same dtypes as the input. Meshes already
    at or below the target are returned unchanged. Tries the threshold steps
    in ascending order (larger cell size -> fewer triangles) and picks the
    result that lands AT OR BELOW the target and is CLOSEST to it, so the LOD
    never stays above the target and never over-collapses when a step jumps
    far below it. If no step reaches the target, the most aggressive
    (smallest) result is used. Never raises for empty input.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    if len(tris) == 0 or len(tris) <= target:
        return verts, tris
    diag = 0.0
    if len(verts):
        diag = float(np.linalg.norm(verts.max(axis=0) - verts.min(axis=0)))
    if diag <= 0.0:
        return verts, tris
    best_below = None   # (face_count, lverts, ltris) closest to target from below
    fallback = None     # (face_count, lverts, ltris) smallest result overall
    for pct in (1.0, 1.5, 2.0, 3.0, 5.0, 8.0):
        try:
            lv, lt, _ = topology.cluster_decimate(
                verts, tris, diag * (pct / 100.0))
        except Exception:  # noqa: BLE001 - fall back to a coarser step
            continue
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

    Uses the Rust BVH nearest-surface query, so the two meshes need not share
    topology. Returns a (len(measure_verts),) float64 array."""
    _q, d, _f = sutura_geom.closest_points(
        np.asarray(ref_verts, np.float64), np.asarray(ref_tris, np.int32),
        np.asarray(measure_verts, np.float64))
    return np.asarray(d, dtype=np.float64)


def _hausdorff(verts, tris, rverts, rtris):
    """Global one-sided Hausdorff distance of original -> repaired.

    Same native sampler as the repair-path fidelity metric (vertices plus
    area-weighted surface samples), so the two numbers stay comparable."""
    from sutura_engine.pipeline.utils import one_sided_hausdorff

    mx, _mn = one_sided_hausdorff(
        verts, tris, rverts, rtris, samples=100000)
    return float(mx) if mx is not None else 0.0


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