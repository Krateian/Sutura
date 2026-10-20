# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Stage 2 (Manifold solid rebuild) and pinch splitting (P1a split).

Runs the Manifold kernel in the Rust core (`sutura_geom.stage2_rebuild`) on
the arrays already in memory, gates Stage 2 behind the strict-watertight
verdict, and splits pinched vertices on a closed Stage-1 result. Moved
verbatim from sutura/repair.py; no logic change.
"""
import time

import numpy as np

import topology
from sutura_engine.pipeline.mesh_io import read_obj, write_obj
from sutura_engine.pipeline.utils import boundary_loop_stats


def run_stage2_arrays(verts, tris):
    """Rebuild arrays as a Manifold solid. Returns ``(verts, tris, report)``."""
    import sutura_geom  # lazy so health.check_required_geometry owns the error
    return sutura_geom.stage2_rebuild(
        np.asarray(verts, dtype=np.float64),
        np.asarray(tris, dtype=np.int32),
    )


def run_stage2(inter, out_obj):
    """File-based wrapper kept for the fTetWild post-process."""
    verts, tris = read_obj(inter)
    out_v, out_t, report = run_stage2_arrays(verts, tris)
    if 'error' not in report:
        write_obj(out_obj, out_v, out_t)
    return report, 'error' not in report


PINCH_SPLIT_MAX_PASSES = 5


def _split_pinched_vertices(ml, ms, topo):
    """Split pinched vertices of a closed mesh so it becomes two-manifold.

    Applies only when the mesh has no hole, no non-manifold edge and at least
    one non-manifold vertex. ``meshing_repair_non_manifold_vertices`` is
    repeated (at most ``PINCH_SPLIT_MAX_PASSES`` times, one pass can leave
    some pinches) until the mesh is two-manifold. The vertices are only
    duplicated, the geometry is unchanged; the result is kept only when it
    still has no hole and no non-manifold edge.

    Returns ``(meshset, topo, report)`` where report is False when the step
    did not apply, else a dict with ``before``/``after`` (pinched vertices),
    ``passes`` and ``adopted``.
    """
    pinched = topo.get('non_two_manifold_vertices', 0)
    m = ms.current_mesh()
    if (pinched == 0 or topo.get('is_mesh_two_manifold')
            or topo.get('non_two_manifold_edges', 0) != 0
            or boundary_loop_stats(m.vertex_matrix(), m.face_matrix())[0] != 0):
        return ms, topo, False
    trial = ml.MeshSet()
    trial.add_mesh(ml.Mesh(vertex_matrix=np.asarray(m.vertex_matrix(), np.float64),
                           face_matrix=np.asarray(m.face_matrix(), np.int32)))
    t_topo, passes = topo, 0
    while passes < PINCH_SPLIT_MAX_PASSES:
        passes += 1
        trial.apply_filter('meshing_repair_non_manifold_vertices')
        t_topo = topology.mesh_topology(trial)
        if t_topo.get('is_mesh_two_manifold'):
            break
    tm = trial.current_mesh()
    holes = boundary_loop_stats(tm.vertex_matrix(), tm.face_matrix())[0]
    adopted = bool(holes == 0 and t_topo.get('non_two_manifold_edges', 0) == 0
                   and t_topo.get('non_two_manifold_vertices', 0) < pinched)
    report = {'before': int(pinched),
              'after': int(t_topo.get('non_two_manifold_vertices', 0)),
              'passes': passes, 'adopted': adopted}
    if adopted:
        return trial, t_topo, report
    return ms, topo, report


def maybe_run_stage2(report, verts, tris, tmpdir):
    """Run stage 2 when the mesh is watertight and the Rust core is present.

    Shared by the single-mesh path and per-object 3MF repair. The gate mirrors
    the historical single-mesh behaviour: stage 2 only applies to a mesh that
    stage 1 closed (two-manifold with no remaining holes). Sets
    ``report['stage2']`` when stage 2 was attempted and returns
    ``(new_verts, new_tris)`` — the rebuilt mesh when it produced output, the
    input arrays unchanged otherwise.
    """
    del tmpdir  # arrays go straight to the Rust core; kept for callers
    if (report.get('stage1', {}).get('two_manifold')
            and report.get('stage1', {}).get('holes_remaining', 0) == 0):
        _t_s2 = time.perf_counter()
        new_v, new_t, mrep = run_stage2_arrays(verts, tris)
        report.setdefault('timings', {})['stage2'] = round(
            time.perf_counter() - _t_s2, 3)
        if mrep is not None:
            report['stage2'] = mrep
            if 'error' not in mrep and len(new_t) > 0:
                return new_v, new_t
    return verts, tris
