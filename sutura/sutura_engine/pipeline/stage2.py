# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Stage 2 (manifold3d solid rebuild) and pinch splitting (P1a split).

Runs the manifold3d bridge in-process or via the venv311 subprocess, gates
Stage 2 behind the strict-watertight verdict, and splits pinched vertices on a
closed Stage-1 result. Moved verbatim from sutura/repair.py; no logic change.
"""
import json
import os
import subprocess
import sys
import tempfile
import time

import numpy as np

import topology
from sutura_engine import health as method_health
from sutura_engine.pipeline.mesh_io import BRIDGE, read_obj, write_obj
from sutura_engine.pipeline.presets import VENV311
from sutura_engine.pipeline.utils import boundary_loop_stats


def run_stage2(inter, out_obj):
    """Run stage 2. Returns (report, ok); reports a skip, never silence."""
    # 1) primary: the fixed Linux two-venv layout (python3.11 + manifold3d)
    if os.path.exists(VENV311) and os.path.exists(BRIDGE):
        r = subprocess.run(
            [VENV311, BRIDGE, inter, out_obj],
            capture_output=True, text=True, timeout=600,
        )
        if r.returncode == 0:
            try:
                report = json.loads(r.stdout.strip().splitlines()[-1])
            except Exception:
                report = {'error': 'unparseable manifold output'}
            if 'error' not in report:
                return report, True
        # fall through to the in-process attempt if the venv failed

    # 2) single-environment installs (macOS/conda): call the bridge in-process
    try:
        import importlib.util
        spec = importlib.util.spec_from_file_location('sutura_manifold_bridge', BRIDGE)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        report = module.run_bridge(inter, out_obj)
        if 'error' not in report:
            return report, True
    except Exception:
        pass

    # 3) unavailable: report it explicitly, never silently
    return {'error': 'Stage 2 skipped: manifold3d not available in this environment.'}, False


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
        t_topo = trial.apply_filter('get_topological_measures')
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
    """Run stage 2 (manifold3d) when the mesh is watertight and the bridge is
    available; shared by the single-mesh path and per-object 3MF repair.

    The gate mirrors the historical single-mesh behaviour: stage 2 only
    applies to a mesh that stage 1 closed (two-manifold with no remaining
    holes) AND the bridge exists. ``run_stage2`` itself handles the fixed
    venv subprocess, the in-process fallback, or an explicit "skipped"
    report. Sets ``report['stage2']`` when stage 2 was attempted and returns
    ``(new_verts, new_tris)`` — the manifold3d-rebuilt mesh when it produced
    output, the input arrays unchanged otherwise."""
    if (report.get('stage1', {}).get('two_manifold')
            and report.get('stage1', {}).get('holes_remaining', 0) == 0
            and os.path.exists(BRIDGE)):
        inter = os.path.join(tmpdir, 'stage1.obj')
        out_obj = os.path.join(tmpdir, 'stage2.obj')
        write_obj(inter, verts, tris)
        _t_s2 = time.perf_counter()
        mrep, _ = run_stage2(inter, out_obj)
        report.setdefault('timings', {})['stage2'] = round(
            time.perf_counter() - _t_s2, 3)
        if mrep is not None:
            report['stage2'] = mrep
            if 'error' not in mrep and os.path.exists(out_obj):
                return read_obj(out_obj)
    return verts, tris
