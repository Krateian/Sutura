# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Flap surface-based hole-filler tier (P1a split).

Moved verbatim from sutura/repair.py; no logic change.
"""
import hashlib
import os
import time
from collections import OrderedDict

import numpy as np

import topology
from sutura_engine import health as method_health
from sutura_engine.pipeline.p_weld import (
    reload_strict_holes_nm, weld_reload_equivalent,
)


FLAP_SKIP_KEYS = ('loops_skipped_nm', 'loops_skipped_flat',
                  'loops_skipped_sheet', 'loops_skipped_coplanar',
                  'loops_skipped_sliver', 'loops_skipped_safety',
                  'loops_fan_fallback')


def _flap_adopt(base_holes, base_nm, cand_holes, cand_nm):
    """Whether a flap candidate should replace the pre-flap Stage-1 mesh.

    Holes and non-manifold edges are both boundary defects, so a candidate is
    accepted when the combined reload-honest damage ``holes + non-manifold``
    does not increase. This lets a flap that closes many loops while welding
    near-coincident rims (which introduces a few new non-manifold edges) be
    adopted -- the deep-repair ladder rebuilds the residual non-manifold edges
    -- while still rejecting a candidate that opens holes or explodes the
    non-manifold count. Measured on ornate-frame (67/16 -> 16/22) and
    artec_metal-nut (2/1 -> 0/2), which the old ``holes<= and nm<=`` gate
    rejected despite the standalone flap->Graft pipeline being a clear win.
    """
    return (int(cand_holes) + int(cand_nm)) <= (int(base_holes) + int(base_nm))


def _flap_max_orig_move(frep):
    """Largest movement of an ORIGINAL vertex from the flap's crack weld (mm).

    ``flap_fill`` welds nearly-coincident open-chain ends before filling; each
    merge records ``(va, vb, dist, is_orig)`` in ``boundary_normalize``'s
    ``weld_sites``. Only merges that touch an original vertex move one, so the
    max ``dist`` over those is the geometry change the flap imposes on the input
    surface (the patch itself is new geometry built from hole vertices).
    """
    sites = (frep.get('boundary_normalize') or {}).get('weld_sites') or []
    moves = [float(s[2]) for s in sites if len(s) >= 4 and s[3]]
    return round(max(moves), 6) if moves else 0.0


def _reload_component_count(verts, tris):
    """Vertex-connected components in the reload-equivalent mesh (the "parts"
    a slicer sees, since STL drops vertex sharing)."""
    wv, wt = weld_reload_equivalent(verts, tris)
    if len(wt) == 0:
        return 0
    n = len(wv)
    edges = np.vstack([wt[:, [0, 1]], wt[:, [1, 2]], wt[:, [2, 0]]])
    return int(topology.connected_components((n, edges), return_labels=False))


def _flap_final_verdict(in_verts, in_tris, out_verts, out_tris):
    """(holes, non-manifold, input_parts, output_parts) for the safety net."""
    holes, nm = reload_strict_holes_nm(out_verts, out_tris)
    parts_in = _reload_component_count(in_verts, in_tris)
    parts_out = _reload_component_count(out_verts, out_tris)
    return int(holes), int(nm), int(parts_in), int(parts_out)


def _flap_final_needs_off(in_verts, in_tris, out_verts, out_tris):
    """Whether a flap-adopted FINAL mesh warrants a flap-OFF re-run (HIGH-1).

    The flap adoption gate is topological-only and decided on the pre-chain
    mesh, so it cannot guarantee the final result is no worse than flap-OFF.
    This cheap check flags the two observable regressions: the final mesh is
    not strict-watertight, or it has more connected parts than the input had.
    Returns ``(reason, metrics)`` with ``reason`` None when no re-run is needed.
    """
    holes, nm, parts_in, parts_out = _flap_final_verdict(
        in_verts, in_tris, out_verts, out_tris)
    metrics = {'holes': holes, 'non_manifold': nm, 'input_parts': parts_in,
               'output_parts': parts_out,
               'watertight': holes == 0 and nm == 0}
    if holes or nm:
        return 'not_watertight', metrics
    if parts_out > parts_in:
        return 'more_parts', metrics
    return None, metrics


def _flap_off_preferred(on_verdict, off_verdict):
    """Whether the flap-OFF result should replace the flap-ON one.

    OFF wins only when it is lexicographically better on (damage, part-count);
    a tie keeps ON, so Flap is discarded only when it is actually worse.
    """
    def _score(verdict):
        m = verdict[1]
        return m['holes'] + m['non_manifold'], m['output_parts']
    return _score(off_verdict) < _score(on_verdict)


# Flap time budget by intensity preset (seconds). ``quick`` disables Flap
# entirely; a run that exceeds its preset budget is measured (and cached for a
# repeat attempt) but never adopted, so a slow Flap can stall neither the
# repair nor the triage escalation. Mirrors ``SI_EXCISE_BUDGET_BY_INTENSITY``.
FLAP_TIMEOUT_BY_INTENSITY = {'quick': 0.0, 'balanced': 15.0,
                             'thorough': 30.0, 'extreme': 60.0}


FLAP_DEFAULT_BUDGET = 15.0


# Per-process cache so the Flap pre-pass runs once per unique input mesh, even
# when triage re-runs the repair for several escalation attempts (each attempt
# reloads the same bytes from ``src``). Keyed by the exact input arrays; a
# bounded LRU. Storing at most two entries costs at most two candidate meshes.
_FLAP_CACHE = OrderedDict()


_FLAP_CACHE_MAX = 2


def _flap_cache_key(base_v, base_t):
    h = hashlib.sha256()
    h.update(np.ascontiguousarray(base_v, dtype=np.float64).tobytes())
    h.update(np.ascontiguousarray(base_t, dtype=np.int64).tobytes())
    return h.digest()


def _flap_step(ml, ms, after, stats, _p=None):
    """Stage-1 hole fill via the Flap surface filler (on by default in the CLI).

    Runs ``sutura_engine.flap.flap_fill`` on the mesh in ``ms`` (the input as
    it stands before the VCG chain -- normally the original surface, though
    outer-shell extraction may already have replaced it; see
    ``repair_mesh_from_arrays``), covering the input's open boundary loops with
    a patch that continues the surrounding curvature. The candidate is adopted
    when the combined reload-honest damage ``holes + non-manifold edges`` does
    not increase; the deep-repair ladder then rebuilds the residual
    non-manifold edges. This gate is intentionally topological-only and is
    decided on the pre-chain mesh, so ``repair_file`` adds a final safety net
    that re-runs with Flap OFF when the *final* result is not strict-watertight
    or has more parts than the input had (see ``_flap_final_needs_off``). The
    Flap pre-pass runs at most once per unique input mesh (bounded
    ``_FLAP_CACHE``) and is skipped when the intensity preset sets a zero
    budget; a run slower than its budget is not adopted. Records
    ``stats['flap']``; never raises.

    ``_p`` (the resolved Stage-1 params) is accepted for call-site symmetry and
    ignored: Flap has no threshold that maps onto them.
    """
    rec = {'ran': False, 'adopted': False, 'reason': None,
           'baseline_holes': None, 'baseline_non_manifold': None,
           'baseline_damage': None, 'candidate_holes': None,
           'candidate_non_manifold': None, 'candidate_damage': None,
           'loops_found': None, 'loops_filled': None, 'loops_skipped': None,
           'loops_skipped_by_reason': None, 'patch_faces': None,
           'new_vertices': None, 'time_s': None, 'cache_hit': False,
           'budget_s': None, 'max_orig_vertex_move_mm': None}
    stats['flap'] = rec
    with method_health.tier_guard('flap', 'tier') as _g:
        return _flap_step_impl(ml, ms, after, stats, rec)
    if _g.error is not None:
        rec['reason'] = 'flap error: %s' % _g.error
        return ms, after


def _flap_step_impl(ml, ms, after, stats, rec):
    base_v = np.asarray(ms.current_mesh().vertex_matrix())
    base_t = np.asarray(ms.current_mesh().face_matrix())
    with method_health.tier_guard('flap', 'measure') as _gm:
        base_holes, base_nm = reload_strict_holes_nm(base_v, base_t)
    if _gm.error is not None:
        rec['reason'] = 'baseline measure failed: %s' % _gm.error
        return ms, after
    rec['baseline_holes'] = int(base_holes)
    rec['baseline_non_manifold'] = int(base_nm)
    if base_holes == 0 and base_nm == 0:
        rec['reason'] = 'no residual boundary loops or non-manifold edges'
        return ms, after

    intensity = stats.get('triage_intensity') or 'balanced'
    budget = FLAP_TIMEOUT_BY_INTENSITY.get(intensity, FLAP_DEFAULT_BUDGET)
    rec['budget_s'] = budget
    if budget <= 0.0:
        rec['reason'] = ('budget: disabled by intensity preset %r' % intensity)
        return ms, after

    cache_key = _flap_cache_key(base_v, base_t)
    cached = _FLAP_CACHE.get(cache_key)
    if cached is not None:
        cand_v, cand_t, frep, prev_time = cached
        _FLAP_CACHE.move_to_end(cache_key)
        rec['ran'] = True
        rec['cache_hit'] = True
        rec['time_s'] = prev_time
    else:
        t0 = time.perf_counter()
        with method_health.tier_guard('flap', 'tier') as _gf:
            from sutura_engine import flap as _flap
            cand_v, cand_t, frep = _flap.flap_fill(base_v, base_t,
                                                   separate_stl=False,
                                                   time_budget=budget)
        if _gf.error is not None:
            rec['reason'] = 'flap error: %s' % _gf.error
            return ms, after
        rec['ran'] = True
        rec['time_s'] = round(time.perf_counter() - t0, 3)
        _FLAP_CACHE[cache_key] = (cand_v, cand_t, frep, rec['time_s'])
        while len(_FLAP_CACHE) > _FLAP_CACHE_MAX:
            _FLAP_CACHE.popitem(last=False)

    rec['loops_found'] = frep.get('loops_found')
    rec['loops_filled'] = frep.get('loops_filled')
    rec['loops_skipped'] = frep.get('loops_skipped')
    rec['loops_skipped_by_reason'] = {
        k: int(frep.get(k) or 0) for k in FLAP_SKIP_KEYS}
    rec['patch_faces'] = frep.get('patch_faces')
    rec['new_vertices'] = frep.get('new_vertices')
    rec['max_orig_vertex_move_mm'] = _flap_max_orig_move(frep)

    if not len(cand_t):
        rec['reason'] = 'flap returned no geometry'
        return ms, after
    with method_health.tier_guard('flap', 'measure') as _gc:
        cand_holes, cand_nm = reload_strict_holes_nm(cand_v, cand_t)
    if _gc.error is not None:
        rec['reason'] = 'candidate measure failed: %s' % _gc.error
        return ms, after
    rec['candidate_holes'] = int(cand_holes)
    rec['candidate_non_manifold'] = int(cand_nm)
    base_damage = int(base_holes) + int(base_nm)
    cand_damage = int(cand_holes) + int(cand_nm)
    rec['baseline_damage'] = base_damage
    rec['candidate_damage'] = cand_damage

    # Budget: a run slower than the preset budget is never adopted, so a heavy
    # mesh cannot slow a repair through Flap. The candidate is cached above so
    # a repeat attempt pays the measure, not the run again.
    if rec['time_s'] is not None and rec['time_s'] > budget:
        rec['reason'] = ('budget: flap took %ss > %ss at intensity %r; '
                         'candidate not adopted'
                         % (rec['time_s'], budget, intensity))
        return ms, after

    if _flap_adopt(base_holes, base_nm, cand_holes, cand_nm):
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(cand_v, np.float32),
                            face_matrix=np.asarray(cand_t, np.int32)))
        after = ms.apply_filter('get_topological_measures')
        rec['adopted'] = True
        rec['reason'] = ('adopted (holes %d->%d, non-manifold %d->%d; '
                         'damage %d->%d)' % (
                             base_holes, cand_holes, base_nm, cand_nm,
                             base_damage, cand_damage))
    else:
        rec['reason'] = ('rejected: reload damage %d->%d would worsen '
                         '(holes %d->%d, non-manifold %d->%d)' % (
                             base_damage, cand_damage, base_holes, cand_holes,
                             base_nm, cand_nm))
    return ms, after


# Flap debris cleanup: after Flap changes the surface the deep-repair tiers
# start from, the shell wrap / Stage-2 rebuild can leave a few extra closed
# shells (thingi10k_1038441: 2 output parts -> 11, nine of them 4-8 faces with
# ~zero volume). They are BOTH collapsed sheets (effective thickness 2*V/A far
# below the part's own bounding-box diagonal) AND effectively zero-volume, and
# so carry no printable material alone -- exactly the debris
# the Stage-2 debris filter already prunes before a union. A genuine
# solid part has 2*V/A ~= its wall thickness (measured 2.0-2.6 on every corpus
# part) and is never dropped. See ``drop_flap_debris``.
FLAP_DEBRIS_VOLUME_EPS = 1e-3


FLAP_DEBRIS_VOLUME_REL = 1e-9


FLAP_DEBRIS_FLATNESS = 1e-4


def drop_flap_debris(in_verts, in_tris, out_verts, out_tris):
    """Drop flat, effectively zero-volume components the repair left behind.

    Flap changes the surface the deep-repair tiers start from, and on a small
    number of meshes the shell-wrap / Stage-2 rebuild then leaves a handful of
    extra closed shells (thingi10k_1038441: 2 output parts -> 11). They are
    flat sheets with no printable material, not parts: a component is dropped
    only when it is NOT the largest AND BOTH

    - carries effectively no volume (at or below the input-scaled floor), and
    - is a collapsed sheet: its effective thickness ``2*V/A`` is at or below
      ``FLAP_DEBRIS_FLATNESS`` of its own bounding-box diagonal.

    Requiring BOTH conditions is the symmetric rule: a component with real
    volume is never dropped no matter how flat it is, and a non-flat chunk is
    never dropped no matter how small its volume. The volume floor scales with
    the original input bbox (``max(FLAP_DEBRIS_VOLUME_EPS, in_diag**3 *
    FLAP_DEBRIS_VOLUME_REL)``); a genuine solid part has thickness ~= its wall
    size and orders of magnitude more volume, so it is never dropped (measured:
    every legit corpus part has ``2*V/A`` ~2.0-2.6 vs debris 4e-9..4e-4). The
    cleanup is gated on the flap-adopted path only, so identical debris from a
    non-flap repair is left untouched. The largest component is always kept and
    the mesh is never emptied. Runs on the STL save/reload-equivalent form
    (``weld_reload_equivalent``) so the verdict sees exactly what is dropped.
    Pure numpy/stdlib (topology.connected_components); never raises; a byte-identical
    no-op (returns the input arrays and ``None``) when nothing qualifies or on any
    error. Returns ``(verts, tris, info)``; ``info['dropped']`` logs every removed
    component (faces, volume, thickness, diag, centroid).
    """
    with method_health.tier_guard('flap_debris', 'cleanup') as _g:
        iv, _it = weld_reload_equivalent(in_verts, in_tris)
        wv, wt = weld_reload_equivalent(out_verts, out_tris)
        if len(iv) == 0 or len(wt) == 0:
            return out_verts, out_tris, None
        n = len(wv)
        edges = np.vstack([wt[:, [0, 1]], wt[:, [1, 2]], wt[:, [2, 0]]])
        ncomp, labels = topology.connected_components((n, edges))
        face_label = labels[wt[:, 0]]
        face_counts = np.bincount(face_label, minlength=ncomp)
        in_diag = float(np.linalg.norm(iv.max(0) - iv.min(0)))
        vol_eps = max(FLAP_DEBRIS_VOLUME_EPS,
                      (in_diag ** 3) * FLAP_DEBRIS_VOLUME_REL)
        main = int(np.argmax(face_counts))
        keep = np.ones(ncomp, dtype=bool)
        dropped = []
        for c in range(ncomp):
            if c == main or face_counts[c] == 0:
                continue
            tri = wt[face_label == c]
            p0 = wv[tri[:, 0]].astype(np.float64)
            p1 = wv[tri[:, 1]].astype(np.float64)
            p2 = wv[tri[:, 2]].astype(np.float64)
            vol = abs(float(np.einsum('ij,ij->i', p0,
                                      np.cross(p1, p2)).sum() / 6.0))
            area = float(0.5 * np.linalg.norm(
                np.cross(p1 - p0, p2 - p0), axis=1).sum())
            cxyz = wv[np.unique(tri)].astype(np.float64)
            diag = float(np.linalg.norm(cxyz.max(0) - cxyz.min(0)))
            thickness = (2.0 * vol / area) if area > 0.0 else 0.0
            flat = (area <= 0.0 or diag <= 0.0 or vol <= 0.0
                    or thickness <= FLAP_DEBRIS_FLATNESS * diag)
            # Symmetric (AND) rule: drop only a component that is BOTH a
            # collapsed sheet AND effectively zero-volume. Requiring both
            # means a component with real volume is never dropped, regardless
            # of how flat it is (the review's MEDIUM/LOW-5); known repair
            # debris is both flat and ~zero-volume, so it is still removed.
            if flat and vol <= vol_eps:
                keep[c] = False
                dropped.append({
                    'faces': int(face_counts[c]),
                    'volume': vol,
                    'thickness': thickness,
                    'diag': diag,
                    'centroid': [round(float(x), 4) for x in cxyz.mean(0)]})
        if not dropped:
            return out_verts, out_tris, None
        fkeep = keep[face_label]
        if not fkeep.any():
            return out_verts, out_tris, None
        cv, ct = weld_reload_equivalent(wv, wt[fkeep])
        info = {
            'components_before': int(np.count_nonzero(face_counts)),
            'components_after': int(np.unique(face_label[fkeep]).size),
            'dropped_faces': int(np.count_nonzero(~fkeep)),
            'dropped_volume': float(sum(d['volume'] for d in dropped)),
            'volume_eps': vol_eps,
            'dropped': dropped,
        }
        return cv, ct, info
    return out_verts, out_tris, None
