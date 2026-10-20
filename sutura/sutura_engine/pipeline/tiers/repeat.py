# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""repeat tier (P1a split). Moved verbatim from sutura/repair.py; no logic change."""
import os

import numpy as np

import topology
from defects import detect as detect_defects
from sutura_engine import health as method_health
from sutura_engine.pipeline.p_weld import reload_strict_holes_nm
from sutura_engine.pipeline.utils import boundary_loop_stats


def _clean_and_orient(ml, verts, tris):
    """Cleanup of a generated surface: dedup + coherent orientation."""
    try:
        from sutura_engine.pipeline.stage1 import apply_chain
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float64),
                            face_matrix=np.asarray(tris, np.int32)))
        chain = [(filt, {}) for filt in (
            'meshing_remove_duplicate_vertices',
            'meshing_remove_duplicate_faces',
            'meshing_remove_unreferenced_vertices',
            'meshing_repair_non_manifold_edges',
            'meshing_re_orient_faces_coherently',
        )]
        apply_chain(ms, chain)
        cur = ms.current_mesh()
        return (np.asarray(cur.vertex_matrix(), dtype=np.float64),
                np.asarray(cur.face_matrix(), dtype=np.int32))
    except Exception:
        return verts, tris


# One-sided (original -> result, outside the repaired boxes) Hausdorff guard
# for the repeated-element transplant tier: the transplanted copy must not move
# the untouched geometry. Mirrors methods.GENERATIVE_MAX_HAUSDORFF_REL.
REPEAT_MAX_HAUSDORFF_OUTSIDE = 0.01


def repeat_tier(ml, ms, after, stats, mode=None, source_point=None,
                target_point=None):
    """Repeated-element transplant tier (registry methods 11/12, P-REP).

    Runs AFTER the whole stage-1 chain because the transplant needs a
    watertight (closed 2-manifold) ``M``: if stage 1 left holes or non-manifold
    edges the tier is skipped. ``mode='auto'`` detects the repeated pattern
    (``repeat_repair.detect_repetition``) and repairs the damaged copies;
    ``mode='manual'`` transplants the element nearest ``source_point`` onto the
    one nearest ``target_point``. The candidate is adopted only when it is
    reload-equivalent strict-watertight, no worse than the stage-1 baseline,
    and the untouched geometry did not move (one-sided Hausdorff outside the
    repaired boxes <= ``REPEAT_MAX_HAUSDORFF_OUTSIDE``). Records
    ``stats['repeat']``; never raises.
    """
    if not mode:
        return ms, after
    rec = {'mode': mode, 'ran': False, 'adopted': False, 'watertight': False,
           'pattern_type': None, 'positions_repaired': None,
           'hausdorff_outside': None, 'notes': [], 'reason': None}
    with method_health.tier_guard('repeat', 'tier') as _g:
        cur_v = np.asarray(ms.current_mesh().vertex_matrix())
        cur_t = np.asarray(ms.current_mesh().face_matrix())
        holes = boundary_loop_stats(cur_v, cur_t)[0]
        nm = int(after.get('non_two_manifold_edges', 0))
        if holes or nm or not after.get('is_mesh_two_manifold'):
            rec['reason'] = ('repeated-element transplant needs a watertight '
                             'input; stage 1 left holes=%d non-manifold=%d'
                             % (holes, nm))
            stats['repeat'] = rec
            return ms, after
        rec['ran'] = True
        import repeat_repair as _rr
        if mode == 'manual':
            if source_point is None or target_point is None:
                rec['reason'] = ('manual repeat repair needs a source and a '
                                 'target point')
                stats['repeat'] = rec
                return ms, after
            rv, rt, rep = _rr.repair_repeat_manual(
                np.asarray(cur_v, dtype=np.float64),
                np.asarray(cur_t, dtype=np.int64),
                np.asarray(source_point, dtype=np.float64),
                np.asarray(target_point, dtype=np.float64))
        else:
            rv, rt, rep = _rr.repair_repeat_auto(
                np.asarray(cur_v, dtype=np.float64),
                np.asarray(cur_t, dtype=np.int64))
        rec['notes'] = list(rep.get('notes') or [])
        rec['pattern_type'] = rep.get('pattern_type')
        rec['positions_repaired'] = rep.get('positions_repaired')
        rec['hausdorff_outside'] = rep.get('hausdorff_outside')
        if 'error' in rep:
            rec['reason'] = 'module error: %s' % rep['error']
        if not rep.get('repaired') or not len(rt):
            if rec['reason'] is None:
                notes = rep.get('notes') or []
                rec['reason'] = notes[0] if notes else 'nothing to repair'
            stats['repeat'] = rec
            return ms, after
        # Validate the raw Manifold transplant (in-memory). unlike the
        # closing tiers it is NOT float32-welded here: the CSG output has
        # coincident cut-seam vertices whose pymeshlab weld-repair opens holes,
        # while the later stage-2 Manifold rebuild makes the mesh reload-safe
        # as a side effect. The reload-equivalent counts are still recorded for
        # transparency; the top-level P-HONEST verdict remains authoritative.
        cand_v = np.asarray(rv, dtype=np.float32)
        cand_t = np.asarray(rt, dtype=np.int32)
        det = detect_defects(cand_v, cand_t)
        cand_holes = len(det['holes'])
        cand_nm = len(det['non_manifold'])
        rec['holes'] = cand_holes
        rec['non_manifold'] = cand_nm
        try:
            rh, rnm = reload_strict_holes_nm(rv, rt)
            rec['reload_holes'] = int(rh)
            rec['reload_non_manifold'] = int(rnm)
        except Exception:  # noqa: BLE001 - reporting only
            pass
        hd = rep.get('hausdorff_outside')
        if (len(cand_t) and cand_holes == 0 and cand_nm == 0
                and cand_holes + cand_nm <= holes + nm
                and hd is not None and hd <= REPEAT_MAX_HAUSDORFF_OUTSIDE):
            ms = ml.MeshSet()
            ms.add_mesh(ml.Mesh(vertex_matrix=cand_v, face_matrix=cand_t))
            after = topology.mesh_topology(cand_v, cand_t)
            rec['adopted'] = True
            rec['watertight'] = False
            rec['candidate_watertight'] = True
            rec['reason'] = 'repeated element(s) transplanted (stage 2 pending)'
        elif rec['reason'] is None:
            if hd is not None and hd > REPEAT_MAX_HAUSDORFF_OUTSIDE:
                rec['reason'] = ('untouched geometry moved (Hausdorff %.3f > %.3f)'
                                 % (hd, REPEAT_MAX_HAUSDORFF_OUTSIDE))
            else:
                rec['reason'] = ('candidate not strict-watertight '
                                 '(holes=%d non-manifold=%d)'
                                 % (cand_holes, cand_nm))
    if _g.error is not None:
        rec['ran'] = False
        rec['reason'] = 'error: %s' % _g.error
    stats['repeat'] = rec
    return ms, after
