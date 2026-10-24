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
resolution is capped at ``SPLINT_RES`` (128).  The raw emitted boundary of a
cut can carry non-manifold edges (a boundary shared by more than two faces);
because a destructive non-manifold repair would manufacture holes instead, such
a cut is rebuilt as a closed Manifold solid through ``run_stage2_arrays``
(mirroring the fTetWild post-process) and the rebuilt boundary is used only when
it is no worse on holes and non-manifold edges.  The candidate is adopted only
when it is strict-watertight after the save/reload-equivalent weld (holes and
non-manifold edges equal to the best possible zero), which also guarantees it is
no worse than the result the ladder already holds.  Never raises.
"""
import time

import numpy as np

import topology
from defects import detect as detect_defects
from sutura_engine import health as method_health
from sutura_engine.pipeline.p_weld import weld_reload_equivalent
from sutura_engine.pipeline.presets import (
    SPLINT_DEFAULT_WORK, SPLINT_MAX_FACES, SPLINT_RES,
    SPLINT_WORK_BY_INTENSITY,
)
from sutura_engine.pipeline.stage2 import run_stage2_arrays


def _count_defects(verts, tris):
    """Return ``(holes, non_manifold_edges)`` on the saved/reloaded form."""
    v32 = np.asarray(verts, dtype=np.float32)
    t32 = np.asarray(tris, dtype=np.int32)
    det = detect_defects(v32, t32)
    return len(det['holes']), len(det['non_manifold'])


from sutura_engine.pipeline.adopt_guard import check_candidate, policy_for


def splint_tier(v, t, input_v, input_t, after, stats, tmpdir, splint=False,
                intensity=None, spec=None, adaptive=None):
    """Exact solid-rebuild tier for registry method 18 (Splint).

    ``v``/``t`` is the result the ladder currently holds and
    ``input_v``/``input_t`` the original user surface this tier rebuilds. Runs
    ``sutura_geom.splint_cut`` on the original input arrays and adopts the
    candidate only when it reloads strict-watertight.  ``stats['splint']``
    records the core report fields (``out_parts``/``out_manifold``/``seconds``
    and the per-phase split) plus the adopt verdict and the EN/TR reason the
    CLI/GUI surface.  The tier is bounded by ``spec.splint_max_faces`` (a hard
    input cap: a single ``splint_cut`` call is not interruptible) and a
    DETERMINISTIC work budget in Delaunay tetrahedra
    (``SPLINT_WORK_BY_INTENSITY``); the former wall-clock rejection was a
    post-hoc, load-dependent decision.  Inputs above the cap and a work-budget
    overrun are reported honestly and keep the current result.  Never raises.
    """
    v = np.asarray(v, dtype=np.float64)
    t = np.asarray(t, dtype=np.int64)
    input_v = np.asarray(input_v if input_v is not None else v, dtype=np.float64)
    input_t = np.asarray(input_t if input_t is not None else t, dtype=np.int64)
    if not splint:
        return v, t, after
    cap = getattr(spec, 'splint_max_faces', SPLINT_MAX_FACES) if spec is not None \
        else SPLINT_MAX_FACES
    key = (intensity or getattr(spec, 'base', None)
           or getattr(spec, 'name', None))
    work_limit = int(getattr(
        spec, 'splint_work',
        SPLINT_WORK_BY_INTENSITY.get(key, SPLINT_DEFAULT_WORK))
        if spec is not None else
        SPLINT_WORK_BY_INTENSITY.get(key, SPLINT_DEFAULT_WORK))
    res = int(getattr(spec, 'splint_res', SPLINT_RES) if spec is not None
              else SPLINT_RES)
    mode = adaptive or 'retry'
    rec = {'tier': 'splint', 'ran': False, 'adopted': False,
           'watertight': False, 'res': res, 'adaptive': mode,
           'input_faces': None, 'output_faces': None, 'output_vertices': None,
           'output_parts': None, 'out_manifold': None,
           'holes': None, 'non_manifold': None,
           'construct_status': None, 'manifold_postprocessed': False,
           'seconds': None, 'skipped_budget': False, 'aborted': False,
           'reject_reason': None, 'reason': None, 'warnings': [],
           'guard_rejected': None, 'guard_reason': None,
           'baseline_self_intersections': None,
           'candidate_self_intersections': None,
           'si_policy': None, 'si_checked': False, 'si_flagged': False,
           'max_faces': int(cap) if cap is not None else None,
           'work_units': None, 'work_budget': int(work_limit),
           'budget_limited_by': None}
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
            return v, t, after
        in_v = input_v
        in_t = input_t
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
            return v, t, after
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
        work_units = int(srec.get('tets') or 0)
        rec['work_units'] = work_units
        if work_limit and work_units > int(work_limit):
            # DETERMINISTIC work budget (Delaunay tetrahedra): the candidate is
            # abandoned all-or-nothing and the ladder result is kept.  This is
            # independent of CPU speed/load, unlike the former elapsed check.
            rec['aborted'] = True
            rec['budget_limited_by'] = 'work'
            rec['reject_reason'] = 'work'
            rec['reason'] = ('Splint used %d work units > %d at intensity %r; '
                             'the ladder result was kept'
                             % (work_units, int(work_limit), key))
            rec['holes'] = (stats.get('stage1') or {}).get('holes_remaining')
            rec['non_manifold'] = (stats.get('stage1') or {}).get(
                'non_manifold_edges_remaining')
            stats['splint'] = rec
            return v, t, after
        cand_v = np.asarray(cand_v)
        cand_t = np.asarray(cand_t)
        if len(cand_t):
            cand_v, cand_t = weld_reload_equivalent(cand_v, cand_t)
            cand_holes, cand_nm = _count_defects(cand_v, cand_t)
            after0 = topology.mesh_topology(
                np.asarray(cand_v, dtype=np.float32),
                np.asarray(cand_t, dtype=np.int32))
            two_manifold = bool(after0.get('is_mesh_two_manifold'))
            if not (cand_holes == 0 and cand_nm == 0 and two_manifold):
                # The raw emitted boundary carries non-manifold edges; a
                # destructive nm-edge repair would turn them into holes, so
                # rebuild it as a closed Manifold solid (the same post-process
                # fTetWild applies to its boundary) and keep the rebuild only
                # when it is no worse.
                m3_v, m3_t, m3rep = run_stage2_arrays(cand_v, cand_t)
                rec['construct_status'] = m3rep.get('construct_status')
                if 'error' not in m3rep and len(m3_t):
                    m3_v, m3_t = weld_reload_equivalent(m3_v, m3_t)
                    m3_holes, m3_nm = _count_defects(m3_v, m3_t)
                    if m3_holes <= cand_holes and m3_nm <= cand_nm:
                        cand_v, cand_t = m3_v, m3_t
                        cand_holes, cand_nm = m3_holes, m3_nm
                        rec['manifold_postprocessed'] = True
            rec['holes'] = cand_holes
            rec['non_manifold'] = cand_nm
            rec['faces'] = int(len(cand_t))
            if len(cand_t) and cand_holes == 0 and cand_nm == 0:
                guard = check_candidate(
                    len(input_t), len(input_v), len(cand_t), len(cand_v),
                    slack_units=0, base_arrays=(v, t),
                    candidate_arrays=(cand_v, cand_t), measure_si=True,
                    policy=policy_for('splint'))
                for key, value in guard.fields.items():
                    if key in rec:
                        rec[key] = value
                if not guard.ok:
                    rec['guard_rejected'] = guard.reason
                    rec['reason'] = 'rejected: %s' % guard.reason
                else:
                    after = topology.mesh_topology(
                        np.asarray(cand_v, dtype=np.float32),
                        np.asarray(cand_t, dtype=np.int32))
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
    if rec['adopted']:
        return (np.asarray(cand_v, dtype=np.float64),
                np.asarray(cand_t, dtype=np.int64), after)
    return v, t, after
