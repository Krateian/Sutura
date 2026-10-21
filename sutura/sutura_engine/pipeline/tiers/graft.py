# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""graft tier (P1a split). Moved verbatim from sutura/repair.py; no logic change."""
import os

import numpy as np

import topology
from defects import detect as detect_defects
from sutura_engine import health as method_health
from sutura_engine.pipeline.p_weld import (
    _repair_welded_topology, weld_reload_equivalent,
)
from sutura_engine.pipeline.presets import GRAFT_MAX_FACES, GRAFT_TIMEOUT


def graft_tier(v, t, input_v, input_t, after, stats, tmpdir, graft=False,
               intensity=None, spec=None):
    """Morphology shell-wrap tier for registry method 13 (Graft).

    ``v``/``t`` is the result the ladder currently holds and
    ``input_v``/``input_t`` the original user surface this tier wraps (like the
    closing and fTetWild tiers).  The candidate is adopted only when it is
    strict-watertight after the save/reload-equivalent weld.  ``stats['graft']``
    records the fidelity verdict (``fidelity_ok``, ``hausdorff_healthy``,
    ``detail_max_mm``/``detail_area_moved``) and the EN/TR detail-loss
    warnings the CLI/GUI surface; the auto policy reads ``fidelity_ok`` to
    decide whether to also try fTetWild.

    The tier is bounded two ways: ``spec`` (the Triage ``IntensitySpec``)
    supplies ``graft_max_faces`` (a hard input cap; a single ``morph_close``
    attempt is not interruptible) and ``graft_timeout`` (an OUTER wall-clock
    abort checked between ladder attempts).  The r-ladder itself stops on a
    DETERMINISTIC work budget inside ``shell_wrap`` (a pure function of the
    input grid + preset), so the produced mesh does not depend on CPU speed or
    load.  Inputs above the cap, a work-budget stop, and a wall-clock abort are
    reported honestly (``skipped_budget`` / ``budget_limited_by`` / ``aborted``
    / ``reject_reason``) and keep the stage-1 result.  An abort is
    all-or-nothing: the partial ladder is discarded, never returned.  Never
    raises.
    """
    v = np.asarray(v, dtype=np.float64)
    t = np.asarray(t, dtype=np.int64)
    input_v = np.asarray(input_v if input_v is not None else v, dtype=np.float64)
    input_t = np.asarray(input_t if input_t is not None else t, dtype=np.int64)
    if not graft:
        return v, t, after
    cap = getattr(spec, 'graft_max_faces', GRAFT_MAX_FACES) if spec is not None \
        else GRAFT_MAX_FACES
    timeout = getattr(spec, 'graft_timeout', GRAFT_TIMEOUT) if spec is not None \
        else GRAFT_TIMEOUT
    rec = {'tier': 'graft', 'ran': False, 'adopted': False,
           'watertight': False, 'fidelity_ok': None,
           'input_coverage': None, 'coverage_ok': None,
           'hausdorff_healthy': None, 'detail_max_mm': None,
           'detail_area_moved': None, 'r_used': None, 'mode': None,
           'faces': None, 'holes': None, 'non_manifold': None,
           'warnings': [], 'reason': None,
           'skipped_budget': False, 'timed_out': False,
           'low_memory': False, 'aborted': False,
           'grid_fallback': None,
           'budget_limited_by': None, 'work_budget': None,
           'reject_reason': None,
           'input_faces': None,
           'max_faces': int(cap) if cap is not None else None,
           'time_budget': (float(timeout) if timeout else None)}
    with method_health.tier_guard('graft', 'tier') as _g:
        try:
            from sutura_engine import graft as _graft
        except Exception:  # noqa: BLE001
            import shell_wrap as _graft
        in_v = input_v
        in_t = input_t
        rec['input_faces'] = int(len(in_t))
        if cap is not None and len(in_t) > int(cap):
            # Hard input cap: a morph_close call is not interruptible, so do
            # not start the ladder on a mesh too large to finish in budget.
            rec['skipped_budget'] = True
            rec['reject_reason'] = 'too_large'
            rec['holes'] = (stats.get('stage1') or {}).get('holes_remaining')
            rec['non_manifold'] = (stats.get('stage1') or {}).get(
                'non_manifold_edges_remaining')
            rec['reason'] = ('input has %d faces, above the Graft cap of %d; '
                             'stage-1 result kept' % (len(in_t), int(cap)))
            stats['graft'] = rec
            return v, t, after
        cand_v, cand_t, grec = _graft.shell_wrap(in_v, in_t,
                                                 intensity=intensity,
                                                 time_budget=timeout)
        rec['ran'] = True
        rec['budget_exceeded'] = bool(grec.get('budget_exceeded'))
        rec['budget_limited_by'] = grec.get('budget_limited_by')
        rec['work_budget'] = grec.get('work_budget')
        rec['grid_fallback'] = grec.get('grid_fallback')
        if grec.get('aborted'):
            # Outer wall-clock abort: Graft was stopped for time and the
            # stage-1 result is kept (never a partial-ladder mesh).
            rec['aborted'] = True
            rec['timed_out'] = True
            rec['reject_reason'] = 'aborted_time'
            rec['reason'] = grec.get('reason')
            rec['warnings'] = list(grec.get('warnings') or [])
            rec['holes'] = (stats.get('stage1') or {}).get('holes_remaining')
            rec['non_manifold'] = (stats.get('stage1') or {}).get(
                'non_manifold_edges_remaining')
            stats['graft'] = rec
            return v, t, after
        if rec['budget_exceeded']:
            # Deterministic inner work budget: keep stage-1 if not adopted.
            rec['reject_reason'] = 'budget'
        if grec.get('low_memory'):
            # RAM guard: the deterministic grid budget did not fit, so Graft
            # refused rather than coarsening.  Keep the stage-1 result and
            # surface the clear message (never a silently different shape).
            rec['low_memory'] = True
            rec['reject_reason'] = 'low_memory'
            rec['reason'] = grec.get('reason')
            rec['warnings'] = list(grec.get('warnings') or [])
            rec['holes'] = (stats.get('stage1') or {}).get('holes_remaining')
            rec['non_manifold'] = (stats.get('stage1') or {}).get(
                'non_manifold_edges_remaining')
            stats['graft'] = rec
            return v, t, after
        rec['fidelity_ok'] = grec.get('fidelity_ok')
        rec['input_coverage'] = grec.get('input_coverage')
        rec['coverage_ok'] = grec.get('coverage_ok')
        rec['hausdorff_healthy'] = grec.get('hausdorff_healthy')
        rec['detail_max_mm'] = grec.get('detail_max_mm')
        rec['detail_area_moved'] = grec.get('detail_area_moved')
        rec['r_used'] = grec.get('r_used')
        rec['mode'] = grec.get('mode')
        rec['seconds'] = grec.get('seconds')
        rec['warnings'] = list(grec.get('warnings') or [])
        if len(cand_t):
            cand_v, cand_t = weld_reload_equivalent(cand_v, cand_t)
            cand_v, cand_t = _repair_welded_topology(cand_v, cand_t)
            cand_v = np.asarray(cand_v, dtype=np.float32)
            cand_t = np.asarray(cand_t, dtype=np.int32)
            det = detect_defects(cand_v, cand_t)
            cand_holes = len(det['holes'])
            cand_nm = len(det['non_manifold'])
            rec['holes'] = cand_holes
            rec['non_manifold'] = cand_nm
            rec['faces'] = int(len(cand_t))
            if len(cand_t) and cand_holes == 0 and cand_nm == 0:
                after = topology.mesh_topology(cand_v, cand_t)
                rec['adopted'] = True
                rec['watertight'] = False  # stage 2 confirms at the top level
                rec['candidate_watertight'] = True
                rec['reason'] = 'strict-watertight candidate (stage 2 pending)'
            elif rec['reason'] is None:
                rec['reason'] = ('candidate not strict-watertight '
                                 '(holes=%d non-manifold=%d)'
                                 % (cand_holes, cand_nm))
        else:
            rec['reason'] = 'graft returned no geometry'
        if rec['adopted']:
            # A watertight candidate was found before the work budget ran out.
            rec['timed_out'] = False
            rec['reject_reason'] = None
        elif rec.get('budget_exceeded'):
            # Deterministic inner work budget spent without a watertight
            # candidate: keep stage 1.  Machine-independent (a pure function of
            # the input grid + preset), unlike the old wall-clock truncation.
            rec['skipped_budget'] = True
            if rec['holes'] is None:
                rec['holes'] = (stats.get('stage1') or {}).get(
                    'holes_remaining')
                rec['non_manifold'] = (stats.get('stage1') or {}).get(
                    'non_manifold_edges_remaining')
            rec['reason'] = ('Graft hit its deterministic work budget (%s '
                             'cell-attempts) before a watertight candidate; '
                             'stage-1 result kept'
                             % (rec.get('work_budget') or '?'))
    if _g.error is not None:
        # PyO3 surfaces a Rust panic as pyo3_runtime.PanicException, which
        # subclasses BaseException (not Exception); a tier must degrade to its
        # reason field rather than abort the whole repair.
        rec['reason'] = 'error: %s: %s' % (type(_g.error).__name__, _g.error)
    stats['graft'] = rec
    if rec['adopted']:
        return (np.asarray(cand_v, dtype=np.float64),
                np.asarray(cand_t, dtype=np.int64), after)
    return v, t, after
