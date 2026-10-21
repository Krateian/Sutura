# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""wall tier (P1a split). Moved verbatim from sutura/repair.py; no logic change."""
import os

import numpy as np

import topology
from defects import detect as detect_defects
from sutura_engine import health as method_health
from sutura_engine.pipeline.tiers.repeat import _clean_and_orient
from sutura_engine.pipeline.utils import boundary_loop_stats


def wall_thicken_tier(cur_v, cur_t, after, stats, v, t, min_thickness=None):
    """Thin-wall thicken-to-min tier for registry method 15 (P-WALL).

    Array-native: ``cur_v``/``cur_t`` is the current mesh and the adopted
    ``(cur_v, cur_t, after)`` is returned.

    ``wall_thickness.thicken_to_min`` measures the per-vertex wall thickness
    from an SDF grid and thickens walls below ``min_thickness`` (default: 1% of
    the bbox diagonal) with a morphological closing of the solid. It runs on
    the ORIGINAL input arrays; the candidate is cleaned with PyMeshLab, then
    adopted only when it is strict-watertight and no worse than the stage-1
    baseline on holes + non-manifold edges. Records ``stats['wall_thicken']``
    (before/after minimum thickness, counts, notes); never raises.
    """
    cur_v = np.asarray(cur_v, dtype=np.float64)
    cur_t = np.asarray(cur_t, dtype=np.int64)
    rec = {'tier': 'wall_thicken', 'ran': True, 'adopted': False,
           'watertight': False, 'min_before': None, 'min_after': None,
           'min_thickness': None, 'delta': None, 'thin_before': None,
           'faces': None, 'notes': [], 'reason': None}
    with method_health.tier_guard('wall_thicken', 'tier') as _g:
        import wall_thickness as _wt
        base_v = cur_v
        base_t = cur_t
        base_holes = boundary_loop_stats(base_v, base_t)[0]
        base_nm = int(after.get('non_two_manifold_edges', 0))
        in_v = np.asarray(v, dtype=np.float64)
        in_t = np.asarray(t, dtype=np.int64)
        cand_v, cand_t, rep = _wt.thicken_to_min(
            in_v, in_t, min_thickness=min_thickness)
        rec['min_thickness'] = rep.get('min_thickness')
        rec['min_before'] = rep.get('min_before')
        rec['thin_before'] = rep.get('thin_before')
        rec['delta'] = rep.get('delta')
        rec['notes'] = list(rep.get('notes') or [])
        if 'error' in rep:
            rec['reason'] = 'module error: %s' % rep['error']
        if len(cand_t):
            cand_v, cand_t = _clean_and_orient(cand_v, cand_t)
            cand_v = np.asarray(cand_v, dtype=np.float32)
            cand_t = np.asarray(cand_t, dtype=np.int32)
            det = detect_defects(cand_v, cand_t)
            cand_holes = len(det['holes'])
            cand_nm = len(det['non_manifold'])
            rec['holes'] = cand_holes
            rec['non_manifold'] = cand_nm
            rec['faces'] = int(len(cand_t))
            rec['min_after'] = rep.get('min_after')
            rec['thin_after'] = rep.get('thin_after')
            if (cand_holes == 0 and cand_nm == 0
                    and cand_holes + cand_nm <= base_holes + base_nm):
                cur_v = np.asarray(cand_v, dtype=np.float64)
                cur_t = np.asarray(cand_t, dtype=np.int64)
                after = topology.mesh_topology(cur_v, cur_t)
                rec['adopted'] = True
                rec['candidate_watertight'] = True
                rec['reason'] = 'walls thickened (stage 2 pending)'
            elif rec['reason'] is None:
                rec['reason'] = ('candidate not strict-watertight '
                                 '(holes=%d non-manifold=%d)'
                                 % (cand_holes, cand_nm))
        elif rec['reason'] is None:
            rec['reason'] = 'module returned no geometry'
    if _g.error is not None:
        rec['ran'] = False
        rec['reason'] = 'error: %s' % _g.error
    stats['wall_thicken'] = rec
    return cur_v, cur_t, after
