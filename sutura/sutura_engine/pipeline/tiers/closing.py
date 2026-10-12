# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Scan-closing ladder tier (P1a split).

Moved verbatim from sutura/repair.py; no logic change.
"""
import numpy as np

from defects import detect as detect_defects
from sutura_engine import health as method_health
from sutura_engine.pipeline.p_weld import (
    _repair_welded_topology, weld_reload_equivalent,
)
from sutura_engine.pipeline.utils import boundary_loop_stats


def closing_ladder(ml, ms, after, stats, v, t, tmpdir,
                   closing=None, proxy_template=False):
    """Scan-closing / proxy-template tier for registry methods 8/9/10 (P-INT).

    ``closing='poisson'`` runs ``closing.poisson_close``; ``'flat_back'`` runs
    ``closing.flat_back_close``; ``proxy_template=True`` runs
    ``proxy_repair.proxy_template_repair``. All three operate on the ORIGINAL
    input arrays ``(v, t)`` rather than the stage-1 result: stage 1's own
    ``meshing_close_holes`` already flat-caps a single boundary loop
    (``maxholesize`` is raised to cover the largest loop), so a closing method
    run on the cleaned arrays would only ever see an already-closed mesh. This
    mirrors the fTetWild tier, which also works from the original input.

    The candidate is adopted only when it is strict-watertight (the same
    ``defects.detect`` metric ``methods._evaluate`` uses: zero holes and zero
    non-manifold edges) and no worse than the stage-1 baseline on holes +
    non-manifold edges. The per-method one-sided (input -> output) Hausdorff
    shape guard is applied later by ``methods._evaluate`` for
    ``invents_geometry`` methods. Records ``stats['closing']`` (tier, ran,
    adopted, counts, the module's ``notes``); never raises.
    """
    if not closing and not proxy_template:
        return ms, after
    tier = 'proxy_template' if proxy_template else closing
    base_v = np.asarray(ms.current_mesh().vertex_matrix())
    base_t = np.asarray(ms.current_mesh().face_matrix())
    base_holes = boundary_loop_stats(base_v, base_t)[0]
    base_nm = int(after.get('non_two_manifold_edges', 0))
    rec = {'tier': tier, 'ran': True, 'adopted': False, 'watertight': False,
           'holes': None, 'non_manifold': None, 'faces': None,
           'notes': [], 'reason': None}
    with method_health.tier_guard('closing', 'tier') as _g:
        in_v = np.asarray(v, dtype=np.float64)
        in_t = np.asarray(t, dtype=np.int64)
        if proxy_template:
            import proxy_repair as _proxy
            cand_v, cand_t, rep = _proxy.proxy_template_repair(in_v, in_t)
        else:
            import closing as _closing
            if closing == 'poisson':
                cand_v, cand_t, rep = _closing.poisson_close(
                    in_v, in_t, tmpdir=tmpdir)
            elif closing == 'flat_back':
                cand_v, cand_t, rep = _closing.flat_back_close(in_v, in_t)
            elif closing == 'mirror':
                import mirror_repair as _mirror
                cand_v, cand_t, rep = _mirror.mirror_close(
                    in_v, in_t, tmpdir=tmpdir)
            else:
                rec.update(ran=False, reason='unknown closing mode %r' % closing)
                stats['closing'] = rec
                return ms, after
        rec['notes'] = list(rep.get('notes') or [])
        if 'error' in rep:
            rec['reason'] = 'module error: %s' % rep['error']
        if len(cand_t):
            # P-FIX: validate the mesh the user will actually reload. STL drops
            # vertex sharing and the loader re-welds coincident float32
            # positions, which can expose non-manifold edges the in-memory
            # index arrays do not show. Weld into that reload-equivalent form,
            # repair what the weld exposes, then check -- never adopt a
            # candidate that is not strict-watertight after a save/reload.
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
            if (len(cand_t) and cand_holes == 0 and cand_nm == 0
                    and cand_holes + cand_nm <= base_holes + base_nm):
                ms = ml.MeshSet()
                ms.add_mesh(ml.Mesh(vertex_matrix=cand_v, face_matrix=cand_t))
                after = ms.apply_filter('get_topological_measures')
                rec['adopted'] = True
                # 5.2: this is a stage-1 strict-watertight CANDIDATE only --
                # 'watertight' is claimed by the top-level verdict after stage 2
                # actually runs and confirms it, not here.
                rec['watertight'] = False
                rec['candidate_watertight'] = True
                rec['reason'] = 'strict-watertight candidate (stage 2 pending)'
            elif rec['reason'] is None:
                rec['reason'] = ('candidate not strict-watertight '
                                 '(holes=%d non-manifold=%d)'
                                 % (cand_holes, cand_nm))
        else:
            rec['reason'] = 'module returned no geometry'
    if _g.error is not None:
        rec['ran'] = False
        rec['reason'] = 'error: %s' % _g.error
    stats['closing'] = rec
    return ms, after
