# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Splint tier (method #18): last-resort exact solid rebuild.

Splint runs the Rust ``sutura_geom.splint_cut`` core (proxy surface + Delaunay
tetrahedralization + tet-surface cut) on the ORIGINAL input arrays, like the
Graft and fTetWild tiers.  It is opt-in (``SPLINT_DEFAULT_ENABLED`` is False
until the 40-mesh corpus decision) and sits in the deep-repair ladder AFTER
Dressing and BEFORE fTetWild, so a Splint that rebuilds the mesh strictly
watertight supersedes fTetWild.

The adaptive sampling mode is fixed to ``'retry'`` (hardened pass first; a
broader rerun only when the hardened result still has too many shells) and the
resolution is capped at ``SPLINT_RES`` (128).  The candidate is adopted only
when it is strict-watertight after the save/reload-equivalent weld (holes and
non-manifold edges equal to the best possible zero), which also guarantees it is
no worse than the result the ladder already holds.  Never raises.
"""
import time

import numpy as np

import topology
from defects import detect as detect_defects
from sutura_engine import health as method_health
from sutura_engine.pipeline.p_weld import (
    _repair_welded_topology, weld_reload_equivalent,
)
from sutura_engine.pipeline.presets import (
    SPLINT_MAX_FACES, SPLINT_RES, SPLINT_TIMEOUT,
)


def splint_tier(ml, ms, after, stats, v, t, tmpdir, splint=False,
                intensity=None, spec=None, adaptive=None):
    """Exact solid-rebuild tier for registry method 18 (Splint).

    Runs ``sutura_geom.splint_cut`` on the original input arrays and adopts the
    candidate only when it reloads strict-watertight.  ``stats['splint']``
    records the core report fields (``out_parts``/``out_manifold``/``seconds``
    and the per-phase split) plus the adopt verdict and the EN/TR reason the
    CLI/GUI surface.  The tier is bounded by ``spec.splint_max_faces`` (a hard
    input cap: a single ``splint_cut`` call is not interruptible) and
    ``spec.splint_timeout`` (an OUTER wall-clock check that abandons the
    candidate all-or-nothing, keeping the ladder's current result).  Inputs
    above the cap and a wall-clock overrun are reported honestly and keep the
    current result.  Never raises.
    """
    if not splint:
        return ms, after
    cap = getattr(spec, 'splint_max_faces', SPLINT_MAX_FACES) if spec is not None \
        else SPLINT_MAX_FACES
    timeout = getattr(spec, 'splint_timeout', SPLINT_TIMEOUT) if spec is not None \
        else SPLINT_TIMEOUT
    res = int(getattr(spec, 'splint_res', SPLINT_RES) if spec is not None
              else SPLINT_RES)
    mode = adaptive or 'retry'
    rec = {'tier': 'splint', 'ran': False, 'adopted': False,
           'watertight': False, 'res': res, 'adaptive': mode,
           'input_faces': None, 'output_faces': None, 'output_vertices': None,
           'output_parts': None, 'out_manifold': None,
           'holes': None, 'non_manifold': None,
           'seconds': None, 'skipped_budget': False, 'aborted': False,
           'reject_reason': None, 'reason': None, 'warnings': [],
           'max_faces': int(cap) if cap is not None else None,
           'time_budget': (float(timeout) if timeout else None)}
    with method_health.tier_guard('splint', 'tier') as _g:
        try:
            import sutura_geom as _sg
            cut = getattr(_sg, 'splint_cut', None)
        except Exception:  # noqa: BLE001
            cut = None
        if cut is None:
            rec['reject_reason'] = 'unavailable'
            rec['reason'] = ('splint_cut is unavailable in this sutura_geom '
                             'build; the ladder result was kept')
            stats['splint'] = rec
            return ms, after
        in_v = np.asarray(v, dtype=np.float64)
        in_t = np.asarray(t, dtype=np.int64)
        rec['input_faces'] = int(len(in_t))
        if cap is not None and len(in_t) > int(cap):
            # Hard input cap: a splint_cut call is not interruptible, so do not
            # start the rebuild on a mesh too large to finish in budget.
            rec['skipped_budget'] = True
            rec['reject_reason'] = 'too_large'
            rec['holes'] = (stats.get('stage1') or {}).get('holes_remaining')
            rec['non_manifold'] = (stats.get('stage1') or {}).get(
                'non_manifold_edges_remaining')
            rec['reason'] = ('input has %d faces, above the Splint cap of %d; '
                             'the ladder result was kept'
                             % (len(in_t), int(cap)))
            stats['splint'] = rec
            return ms, after
        t0 = time.perf_counter()
        cand_v, cand_t, srec = cut(in_v, in_t, res, adaptive=mode)
        elapsed = time.perf_counter() - t0
        rec['ran'] = True
        rec['seconds'] = float(srec.get('seconds', elapsed))
        rec['output_faces'] = srec.get('output_faces')
        rec['output_vertices'] = srec.get('output_vertices')
        rec['output_parts'] = srec.get('output_parts')
        rec['out_manifold'] = srec.get('out_manifold')
        rec['core'] = dict(srec)
        if timeout and elapsed > float(timeout):
            # Outer wall-clock abort (consistent with the fTetWild timeout): the
            # candidate is abandoned all-or-nothing and the ladder's current
            # result is kept (never a partially-verified mesh).
            rec['aborted'] = True
            rec['reject_reason'] = 'aborted_time'
            rec['reason'] = ('Splint was stopped for time (%.1fs > %.1fs); '
                             'the ladder result was kept'
                             % (elapsed, float(timeout)))
            rec['holes'] = (stats.get('stage1') or {}).get('holes_remaining')
            rec['non_manifold'] = (stats.get('stage1') or {}).get(
                'non_manifold_edges_remaining')
            stats['splint'] = rec
            return ms, after
        cand_v = np.asarray(cand_v)
        cand_t = np.asarray(cand_t)
        if len(cand_t):
            cand_v, cand_t = weld_reload_equivalent(cand_v, cand_t)
            cand_v, cand_t = _repair_welded_topology(ml, cand_v, cand_t)
            cand_v = np.asarray(cand_v, dtype=np.float32)
            cand_t = np.asarray(cand_t, dtype=np.int32)
            det = detect_defects(cand_v, cand_t)
            cand_holes = len(det['holes'])
            cand_nm = len(det['non_manifold'])
            rec['holes'] = cand_holes
            rec['non_manifold'] = cand_nm
            rec['faces'] = int(len(cand_t))
            if len(cand_t) and cand_holes == 0 and cand_nm == 0:
                ms = ml.MeshSet()
                ms.add_mesh(ml.Mesh(vertex_matrix=cand_v, face_matrix=cand_t))
                after = topology.mesh_topology(cand_v, cand_t)
                rec['adopted'] = True
                rec['watertight'] = False  # stage 2 confirms at the top level
                rec['candidate_watertight'] = True
                rec['reason'] = 'strict-watertight candidate (stage 2 pending)'
            else:
                rec['reason'] = ('candidate not strict-watertight '
                                 '(holes=%d non-manifold=%d)'
                                 % (cand_holes, cand_nm))
        else:
            rec['reason'] = 'splint returned no geometry'
    if _g.error is not None:
        # A Rust panic surfaces as pyo3_runtime.PanicException (a BaseException
        # subclass); degrade to the reason field instead of aborting the repair.
        rec['reason'] = 'error: %s: %s' % (type(_g.error).__name__, _g.error)
    stats['splint'] = rec
    return ms, after
