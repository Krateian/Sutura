# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Stage 1 (PyMeshLab/VCG) cleanup chain and SI-excision helpers (P1a split).

The VCG filter chains, extreme extra passes, the guarded self-intersection
excise + re-cap and the opt-in join-small-components prototype. Moved verbatim
from sutura/repair.py; no logic change.
"""
import os
import time

import numpy as np

import topology
import triage
from sutura_engine import health as method_health


def _close_holes_step(maxholesize):
    """The ``meshing_close_holes`` chain entry (native ear-clipping closer).

    The experimental Liepa-style refinement (``SUTURA_REFINE_HOLE``) was
    removed in Phase 4 W3d: it was OFF by default and measured to worsen an
    open mesh (thingi10k_100827: 9 -> 23 holes, SI 29 -> 77), so the plain
    native closer is always used (see ``docs/cli-gui-parity-notes.md``).
    """
    return ('meshing_close_holes', {'maxholesize': maxholesize})


def stage1_cleanup_chain(safe_null=False):
    """The Stage-1 mesh-hygiene passes: duplicate/null removal, non-manifold
    edge/vertex repair and a coherent re-orientation, with NO hole closing and
    NO component removal.

    ``stage1_chain`` builds on this list; the Tamponade prepass (T4) also uses
    it verbatim so the reference-based hole plug runs on the same cleaned
    surface the chain would otherwise hand to its generic ``close_holes``.

    ``safe_null`` switches ``meshing_remove_null_faces`` to the safe mode: a
    zero-area but topologically valid face is kept when removing it would open
    a boundary edge. ``stage1_chain`` and the Tamponade prepass pass True (a
    closed surface's null faces are load-bearing and the prepass cleans the
    same surface the chain hands over), while the delete fallback keeps the
    historical drop-everything behaviour.
    """
    return [
        ('meshing_remove_duplicate_faces', {}),
        ('meshing_remove_null_faces', {'safe': True} if safe_null else {}),
        ('meshing_remove_duplicate_vertices', {}),
        # layered/duplicated-vertex meshes (e.g. Bambu/Orca exports) collapse
        # onto few unique vertices here; the faces become duplicates only
        # AFTER vertex dedup, so a second duplicate-faces pass is required or
        # meshing_repair_non_manifold_edges sees a per-edge face soup and
        # deletes the mesh (the layered-3MF macOS failure, see
        # docs/stage2-3mf-per-object.md Investigation A).
        ('meshing_remove_duplicate_faces', {}),
        ('meshing_repair_non_manifold_edges', {}),
        ('meshing_re_orient_faces_coherently', {}),
        # non-manifold vertices are repaired BEFORE hole closing: closing a
        # hole on a mesh with non-manifold vertices fan-fills it and can
        # CREATE new non-manifold edges (verified in the corpus analysis),
        # so the mesh must be vertex-manifold first. A final close_holes pass
        # re-closes anything the debris removal / re-orient opened.
        ('meshing_repair_non_manifold_vertices', {}),
    ]


def stage1_chain(maxholesize=1000, mincomponentsize=8, join_components=False):
    chain = stage1_cleanup_chain(safe_null=True) + [
        _close_holes_step(maxholesize),
        # --experimental-join-components replaces this removal step with a
        # join (small components are moved onto the nearest larger component
        # instead of being deleted) -- prototype, flag-gated, see
        # join_small_components.
        ('meshing_remove_connected_component_by_face_number',
         {'mincomponentsize': mincomponentsize, 'removeunref': True}),
        ('meshing_remove_unreferenced_vertices', {}),
        ('meshing_re_orient_faces_coherently', {}),
        _close_holes_step(maxholesize),
    ]
    if join_components:
        chain = [s for s in chain if s[0] != 'meshing_remove_connected_component_by_face_number']
    return chain


def delete_fallback_chain(maxholesize=1000, mincomponentsize=8, join_components=False):
    chain = [
        ('meshing_remove_duplicate_faces', {}),
        ('meshing_remove_null_faces', {}),
        ('meshing_remove_duplicate_vertices', {}),
        # same layered-mesh fix as stage1_chain: faces become duplicates only
        # after vertex dedup, so dedup them again before the non-manifold pass.
        ('meshing_remove_duplicate_faces', {}),
        ('compute_selection_by_non_manifold_edges_per_face', {}),
        ('meshing_remove_selected_faces', {}),
        ('set_selection_none', {}),
        ('meshing_remove_unreferenced_vertices', {}),
        ('meshing_repair_non_manifold_edges', {}),
        ('meshing_repair_non_manifold_vertices', {}),
        ('meshing_remove_connected_component_by_face_number',
         {'mincomponentsize': mincomponentsize, 'removeunref': True}),
        ('meshing_re_orient_faces_coherently', {}),
        _close_holes_step(maxholesize),
    ]
    if join_components:
        chain = [s for s in chain if s[0] != 'meshing_remove_connected_component_by_face_number']
    return chain


def _non_manifold_edge_faces(tris):
    """Faces incident to an edge used by more than two triangles."""
    t = np.asarray(tris, dtype=np.int64)
    if len(t) == 0:
        return np.zeros(0, dtype=bool)
    edges, counts, inverse = topology.edge_table(t)
    bad = np.where(counts > 2)[0]
    if len(bad) == 0:
        return np.zeros(len(t), dtype=bool)
    face_ids = np.concatenate([np.arange(len(t)), np.arange(len(t)),
                               np.arange(len(t))])
    selected = np.zeros(len(t), dtype=bool)
    bad_set = set(int(x) for x in bad)
    for h, edge_id in enumerate(np.asarray(inverse).tolist()):
        if edge_id in bad_set:
            selected[int(face_ids[h])] = True
    return selected


def apply_chain(verts, tris, chain, sink=None):
    """Run a Stage-1 chain on plain arrays and return ``(v, t, applied, skipped)``.

    Every filter used by the Stage-1 chains is native; no MeshSet is created.
    ``set_selection_none`` is a no-op and a preceding
    ``compute_selection_by_non_manifold_edges_per_face`` writes a mask consumed
    by ``meshing_remove_selected_faces``.

    ``sink`` (optional dict): when given, the
    ``meshing_remove_connected_component_by_face_number`` step records what it
    dropped under ``sink['removed_small_parts']`` (read-only measurement, never
    changes the removal itself).
    """
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    if t.ndim != 2:
        t = t.reshape(-1, 3)
    applied = 0
    skipped = {}
    selected = np.zeros(len(t), dtype=bool)
    for name, params in chain:
        try:
            if name == 'meshing_remove_duplicate_faces':
                t = topology.remove_duplicate_faces(t)
            elif name == 'meshing_remove_null_faces':
                t = topology.remove_null_faces(v, t, safe=bool(params.get('safe', False)))
            elif name == 'meshing_remove_duplicate_vertices':
                v, t = topology.remove_duplicate_vertices(v, t)
            elif name == 'meshing_remove_unreferenced_vertices':
                v, t = topology.remove_unreferenced_vertices(v, t)
            elif name == 'meshing_remove_connected_component_by_face_number':
                if sink is not None:
                    try:
                        from sutura_engine import debris as _debris
                        sink['removed_small_parts'] = _debris.removed_small_parts(
                            v, t, params.get('mincomponentsize', 8))
                    except Exception:
                        sink['removed_small_parts'] = None
                v, t = topology.remove_connected_components(
                    v, t, min_faces=params.get('mincomponentsize', 8),
                    remove_unref=params.get('removeunref', True))
            elif name == 'meshing_invert_face_orientation':
                t = topology.invert_face_orientation(t)
            elif name == 'meshing_repair_non_manifold_edges':
                t = topology.repair_non_manifold_edges(v, t)
            elif name == 'meshing_re_orient_faces_coherently':
                t = topology.re_orient_faces_coherently(t)
            elif name == 'meshing_repair_non_manifold_vertices':
                v, t = topology.repair_non_manifold_vertices(v, t)
            elif name == 'compute_selection_by_non_manifold_edges_per_face':
                selected = _non_manifold_edge_faces(t)
            elif name == 'set_selection_none':
                selected = np.zeros(len(t), dtype=bool)
            elif name == 'meshing_remove_selected_faces':
                t = topology.remove_selected_faces(
                    t, selected[:len(t)] if len(selected) == len(t)
                    else np.zeros(len(t), dtype=bool))
            elif name == 'meshing_close_holes':
                v, t, _closed, _new = topology.close_holes(
                    v, t, maxholesize=params.get('maxholesize', 1000),
                    self_intersection=params.get('selfintersection', True),
                    newfaceselected=params.get('newfaceselected', False))
            else:
                raise ValueError('unhandled native chain filter: %s' % name)
            applied += 1
        except Exception as exc:  # noqa: BLE001 - a chain step never crashes a repair
            method_health.record_error('stage1:' + name, 'filter', exc)
            skipped[name] = str(exc)
    return v, t, applied, skipped


def extreme_extra_passes(verts, tris, params):
    """Extreme-mode extra Stage 1 passes (Stage C).

    After the main chain has run, select and remove self-intersecting faces
    (``compute_selection_by_self_intersections_per_face`` ->
    ``meshing_remove_selected_faces`` -> ``meshing_remove_unreferenced_vertices``)
    and then run the main chain ONE more time with the same thresholds to close
    the new holes / drop the new debris the removal exposed.

    Deliberately NOT meshing_isotropic_explicit_remeshing: a full remesh can
    unpredictably change topology, so it stays out of scope.

    Returns
    (passes_applied, self_intersections_found, self_intersections_removed,
     second_chain_applied, second_chain_skipped, verts, tris).
    When the mesh already has no self-intersecting faces the extra passes are
    skipped harmlessly (no error, nothing removed) and passes_applied is False.
    """
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    mask, found = topology.self_intersecting_faces(v, t)
    if int(found) <= 0:
        return False, max(int(found), 0), 0, 0, {}, v, t
    before_removal = len(t)
    t = topology.remove_selected_faces(t, mask)
    v, t = topology.remove_unreferenced_vertices(v, t)
    removed = max(before_removal - len(t), 0)
    v, t, applied2, skipped2 = apply_chain(
        v, t, stage1_chain(**params))
    return True, int(found), removed, applied2, skipped2, v, t


# --- Guarded self-intersection excise + re-cap (Full Mend) ------------------
# After the Stage-1 chain, Full Mend (deep_repair='full') may still carry
# residual self-intersecting faces (overlapping / folded caps). The legacy
# extreme-only pass deletes them blindly and re-runs the whole chain (which
# can drop newly-severed small components); this guarded loop instead
# localises the excision (SI faces + 1 ring), re-caps with Liepa refinement
# and accepts a round ONLY when self-intersections strictly decrease and
# holes / non-manifold edges do not increase -- otherwise it atomically
# rolls back to the pre-round mesh. The connected-component removal filter is
# never called, so no component is ever dropped. OFF-by-env switch
# (SUTURA_SI_EXCISE=0); no CLI/GUI flag (see docs/cli-gui-parity-notes.md).
SI_EXCISE_MAX_ROUNDS = 3


SI_EXCISE_MAX_FACE_FRACTION = 0.30  # scope brake: never excise > 30 % at once


SI_EXCISE_DEFAULT_BUDGET = 5.0


# Visible-surface movement budget of one SI-excise pass (millimetres). A round
# may not move the visible input surface further than this from the frozen
# original load, measured with the verifier's input-alone interior-wall rule;
# without it a slot cut through the outer wall is committed as long as the
# topology checks pass. Single source for code, tests and docs.
SI_EXCISE_SHAPE_BUDGET_MM = 0.1


SI_EXCISE_BUDGET_BY_INTENSITY = {
    'quick': 0.0,
    'balanced': 5.0,
    'thorough': 20.0,
    'extreme': 60.0,
}


def si_excise_enabled(enabled=None):
    """Resolve the SI-excise switch (default on) from an explicit value then
    ``SUTURA_SI_EXCISE``."""
    if enabled is None:
        env = os.environ.get('SUTURA_SI_EXCISE', '').strip().lower()
        if env in ('0', 'false', 'no', 'off'):
            enabled = False
        elif env in ('1', 'true', 'yes', 'on'):
            enabled = True
        else:
            enabled = True
    return bool(enabled)


def _si_excise_budget(spec):
    """SI-excise wall-clock budget (seconds) from the intensity preset."""
    key = getattr(spec, 'base', None) or getattr(spec, 'name', None)
    return SI_EXCISE_BUDGET_BY_INTENSITY.get(key, SI_EXCISE_DEFAULT_BUDGET)


def si_excise_shape_guard_enabled(enabled=None):
    """Resolve the SI-excise visible-shape budget (default on) from an explicit
    value then ``SUTURA_SI_EXCISE_GUARD``."""
    if enabled is None:
        env = os.environ.get('SUTURA_SI_EXCISE_GUARD', '').strip().lower()
        if env in ('0', 'false', 'no', 'off'):
            enabled = False
        elif env in ('1', 'true', 'yes', 'on'):
            enabled = True
        else:
            enabled = True
    return bool(enabled)


def _si_excise_visible_metrics(ref_v, ref_t, cand_v, cand_t, *, buried=None,
                               centroids=None):
    """Verifier-rule visible movement of ``cand`` against the frozen ``ref``.

    The rule matches the shape verifier on main: the visible input faces are
    the ones ``sutura_geom.buried_face_mask`` does not mark (input alone), and
    the distance is ``closest_points`` from their centroids to the candidate.
    ``buried``/``centroids`` let the caller reuse the once-per-pass mask. Returns
    ``(visible_mm, all_mm)`` or ``None`` when the geometry kernel cannot answer.
    """
    import sutura_geom
    if not hasattr(sutura_geom, 'buried_face_mask'):
        return None
    try:
        if buried is None:
            buried, _detail = sutura_geom.buried_face_mask(
                np.ascontiguousarray(ref_v, dtype=np.float64),
                np.ascontiguousarray(ref_t, dtype=np.int32), 0.1)
        buried = np.asarray(buried, dtype=bool)
        if centroids is None:
            centroids = np.asarray(ref_v, dtype=np.float64)[
                np.asarray(ref_t, dtype=np.int64)].mean(axis=1)
        d = np.asarray(sutura_geom.closest_points(
            np.ascontiguousarray(cand_v, dtype=np.float64),
            np.ascontiguousarray(cand_t, dtype=np.int32),
            np.ascontiguousarray(centroids, dtype=np.float64))[1], dtype=np.float64)
    except Exception:  # noqa: BLE001 - the guard must never crash the repair
        return None
    if d.size == 0:
        return None
    visible = ~buried
    vis_mm = float(d[visible].max()) if visible.any() else 0.0
    return vis_mm, float(d.max())


def _si_excise_new_face_max_mm(ref_v, ref_t, cand_v, cand_t, new_faces):
    """Max distance from the round's NEW re-cap faces back to the frozen input.

    The input-to-output metric cannot see material a re-cap adds across a
    concavity (39635: 261 mm3 of output-only volume), so the new faces are
    checked in the opposite direction. Returns ``None`` when unavailable.
    """
    import sutura_geom
    nf = np.asarray(new_faces, dtype=bool)
    if nf.size == 0:
        return None
    try:
        pts = np.asarray(cand_v, dtype=np.float64)[
            np.asarray(cand_t, dtype=np.int64)[nf]].mean(axis=1)
        d = np.asarray(sutura_geom.closest_points(
            np.ascontiguousarray(ref_v, dtype=np.float64),
            np.ascontiguousarray(ref_t, dtype=np.int32),
            np.ascontiguousarray(pts, dtype=np.float64))[1], dtype=np.float64)
    except Exception:  # noqa: BLE001
        return None
    return float(d.max()) if len(d) else None


# Localized exact self-union of residual self-intersection clusters (after
# Stage 2 + P-WELD). EXPERIMENTAL: measured on the 40-mesh corpus it removes SI
# on only an isolated real mesh (see docs/cli-gui-parity-notes.md); the
# ornate-frame fold is global, not local, so patch boundaries never match
# (97 % `boundary_split`). OFF by default; `SUTURA_LOCAL_EXACT=1` forces it.
LOCAL_EXACT_DEFAULT_ENABLED = False


LOCAL_EXACT_BUDGET_BY_INTENSITY = {
    'quick': 0.0,
    'balanced': 10.0,
    'thorough': 45.0,
    'extreme': 120.0,
}


LOCAL_EXACT_DEFAULT_BUDGET = 10.0


def local_exact_enabled(enabled=None):
    """Resolve the local-exact switch (default OFF) from an explicit value then
    ``SUTURA_LOCAL_EXACT``."""
    if enabled is None:
        env = os.environ.get('SUTURA_LOCAL_EXACT', '').strip().lower()
        if env in ('1', 'true', 'yes', 'on'):
            enabled = True
        elif env in ('0', 'false', 'no', 'off'):
            enabled = False
        else:
            enabled = LOCAL_EXACT_DEFAULT_ENABLED
    return bool(enabled)


def _local_exact_budget(spec):
    """Local-exact wall-clock budget (seconds) from the intensity preset."""
    key = getattr(spec, 'base', None) or getattr(spec, 'name', None)
    return LOCAL_EXACT_BUDGET_BY_INTENSITY.get(key, LOCAL_EXACT_DEFAULT_BUDGET)


def _should_run_local_exact(report, spec):
    """Gate: enabled, Stage 2 succeeded, and a positive time budget."""
    if not local_exact_enabled():
        return False
    if not (report.get('stage2') or {}).get('ok'):
        return False
    return _local_exact_budget(spec or triage.resolve_intensity(None)) > 0.0


def si_excise_recap(verts, tris, *, maxholesize=1000,
                    max_rounds=SI_EXCISE_MAX_ROUNDS,
                    time_budget=SI_EXCISE_DEFAULT_BUDGET,
                    face_fraction=SI_EXCISE_MAX_FACE_FRACTION,
                    shape_guard=None, ref_verts=None, ref_tris=None):
    """Guarded SI excise + re-cap, array-only (no MeshSet).

    Starts with a one-ring dilation. On the native exact path a rejected
    one-ring round is retried once with two rings only when it reduced (but
    did not close) the pre-existing boundary: the exact classifier does not
    flag adjacent shared-vertex / touching faces, so the extra ring clears
    that contact pocket before re-capping. The round is committed only when
    self-intersections strictly decrease, non-manifold edges and the
    connected-component count do not increase, and the mesh closes completely
    (``holes_after == 0``); otherwise the pre-round arrays are restored.

    With the visible-shape budget (default on, ``SUTURA_SI_EXCISE_GUARD=0``
    disables; ``shape_guard`` overrides) a round must also keep the visible
    surface within ``SI_EXCISE_SHAPE_BUDGET_MM`` of the frozen reference
    ``(ref_verts, ref_tris)`` -- the arrays as Stage 1 received them -- both
    against the previous round's value and against the pass's initial value,
    and the new re-cap faces must lie within the same budget of the reference.
    A round that breaks the budget is rolled back and ends the pass with
    ``reason='shape_guard'``; residual self-intersections stay reported.
    Without a reference the function behaves exactly as before the budget.

    Returns ``(verts, tris, report)``.
    """
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    report = {
        'ran': False, 'applied': False, 'rounds': 0, 'initial_si': 0,
        'si_after': 0, 'removed_si': 0, 'reason': None,
        'dilation_rings': 1,
        'budget_s': (None if time_budget is None else float(time_budget)),
        'seconds': 0.0, 'history': [],
    }
    guard_on = si_excise_shape_guard_enabled(shape_guard)
    guard_ref_v = guard_ref_t = None
    guard_buried = guard_centroids = None
    guard_available = False
    initial_vis = None
    last_vis = None
    init_mask = None
    with method_health.tier_guard('si_excise', 'tier') as _g0:
        init_mask, _si = topology.self_intersecting_faces(v, t)
        report['initial_si'] = max(int(_si), 0)
        if int(_si) < 0:
            init_mask = None
    if _g0.error is not None:
        report['reason'] = 'error: %s' % _g0.error
        return v, t, report
    if report['initial_si'] == 0:
        report['reason'] = 'no_si'
        return v, t, report
    if time_budget is not None and time_budget <= 0.0:
        report['reason'] = 'budget'
        return v, t, report

    if guard_on:
        guard_ref_v = np.ascontiguousarray(
            np.asarray(verts if ref_verts is None else ref_verts,
                       dtype=np.float64))
        guard_ref_t = np.ascontiguousarray(
            np.asarray(tris if ref_tris is None else ref_tris, dtype=np.int64))
        # REF is fixed for the whole pass, so the mask and the centroids are
        # built once; only the candidate BVH is rebuilt per evaluation.
        try:
            import sutura_geom
            guard_available = hasattr(sutura_geom, 'buried_face_mask')
        except ImportError:
            guard_available = False
        if guard_available:
            guard_buried, _gd = sutura_geom.buried_face_mask(
                guard_ref_v, guard_ref_t.astype(np.int32), 0.1)
            guard_buried = np.asarray(guard_buried, dtype=bool)
            guard_centroids = guard_ref_v[guard_ref_t].mean(axis=1)
            _init = _si_excise_visible_metrics(
                guard_ref_v, guard_ref_t, v, t, buried=guard_buried,
                centroids=guard_centroids)
            if _init is not None:
                initial_vis = last_vis = float(_init[0])
        report['shape_guard'] = {
            'enabled': True,
            'available': bool(guard_available),
            'budget_mm': float(SI_EXCISE_SHAPE_BUDGET_MM),
            'initial_vis_mm': (round(initial_vis, 4)
                               if initial_vis is not None else None),
            'final_vis_mm': None,
            'rejected_round': None,
            'rounds_rejected': 0,
            'si_remaining_after': None,
            'reason': None,
        }
        if not guard_available:
            report['shape_guard']['reason'] = 'unavailable'

    report['ran'] = True
    t0 = time.perf_counter()
    current_si = report['initial_si']
    total_faces = len(t)
    for round_idx in range(max_rounds):
        # Deterministic bound: the round count (max_rounds) and the per-round
        # face-fraction brake limit the work; elapsed time is only reported, so
        # the result does not depend on machine load. A non-positive budget
        # (quick preset) still disables the pass above.
        topo_before = topology.mesh_topology(v, t)
        holes_before = int(topo_before.get('boundary_edges', 0) or 0)
        nm_before = (int(topo_before.get('non_two_manifold_edges', 0) or 0)
                     + int(topo_before.get('non_two_manifold_vertices', 0) or 0))
        comp_before = int(topo_before.get('connected_components_number', 0) or 0)
        saved_v = v.copy()
        saved_t = t.copy()
        if round_idx == 0 and init_mask is not None:
            sel_cur = np.asarray(init_mask, dtype=bool)
        else:
            sel_cur, _ = topology.self_intersecting_faces(v, t)
        sel_cur = np.asarray(sel_cur, dtype=bool)
        accepted = False
        for attempt, rings in enumerate((1, 2)):
            if attempt:
                v, t = saved_v.copy(), saved_t.copy()
            report['dilation_rings'] = rings
            with method_health.tier_guard('si_excise', 'round') as _gr:
                sel_dil = topology.dilate_face_selection(
                    saved_t, sel_cur, rings=rings)
                selected = int(sel_dil.sum())
                if selected == 0:
                    report['reason'] = 'no_selection'
                    break
                if selected > face_fraction * max(total_faces, 1):
                    report['reason'] = 'scope'
                    break
                t_work = topology.remove_selected_faces(saved_t, sel_dil)
                v_work, t_work = topology.remove_unreferenced_vertices(
                    saved_v, t_work)
                t_work = topology.repair_non_manifold_edges(v_work, t_work)
                v_work, t_work = topology.repair_non_manifold_vertices(
                    v_work, t_work)
                v_work, t_work, _closed, _new = topology.close_holes(
                    v_work, t_work, maxholesize=maxholesize,
                    self_intersection=True, newfaceselected=False)
                # close_holes appends the re-cap triangles after the kept ones
                # (original faces are preserved bit-for-bit), so the new-face
                # mask is the tail; the guard checks it against REF directly.
                new_face_mask = np.zeros(len(t_work), dtype=bool)
                if _new:
                    new_face_mask[max(len(t_work) - int(_new), 0):] = True
                v_work, t_work = topology.remove_unreferenced_vertices(
                    v_work, t_work)
                topo_after = topology.mesh_topology(v_work, t_work)
                holes_after = int(topo_after.get('boundary_edges', 0) or 0)
                nm_after = (int(topo_after.get('non_two_manifold_edges', 0) or 0)
                            + int(topo_after.get('non_two_manifold_vertices', 0) or 0))
                comp_after = int(topo_after.get('connected_components_number', 0) or 0)
                _, _si_after = topology.self_intersecting_faces(v_work, t_work)
                si_after = int(_si_after) if int(_si_after) >= 0 else None
            if _gr.error is not None:
                v, t = saved_v.copy(), saved_t.copy()
                report['reason'] = 'rollback'
                break
            holes_ok = holes_after == 0
            components_ok = comp_after <= comp_before
            si_ok = si_after is not None and si_after < current_si
            topo_ok = bool(si_ok and holes_ok and nm_after <= nm_before
                           and components_ok)
            guard_ok = True
            post_vis = new_face_mm = None
            if topo_ok and guard_on and guard_available:
                _post = _si_excise_visible_metrics(
                    guard_ref_v, guard_ref_t, v_work, t_work,
                    buried=guard_buried, centroids=guard_centroids)
                if _post is not None:
                    post_vis = float(_post[0])
                    new_face_mm = _si_excise_new_face_max_mm(
                        guard_ref_v, guard_ref_t, v_work, t_work,
                        new_face_mask)
                    budget = float(SI_EXCISE_SHAPE_BUDGET_MM)
                    per_round_ok = (last_vis is None
                                    or post_vis <= last_vis + budget)
                    cumulative_ok = (initial_vis is None
                                     or post_vis <= initial_vis + budget)
                    new_face_ok = (new_face_mm is None
                                   or new_face_mm <= budget)
                    guard_ok = per_round_ok and cumulative_ok and new_face_ok
            if topo_ok and guard_ok:
                accepted = True
                v, t = v_work, t_work
                entry = {
                    'round': round_idx + 1,
                    'si_before': int(current_si),
                    'si_after': int(si_after),
                    'faces_after': int(topo_after.get('faces_number', 0) or 0),
                    'dilation_rings': rings,
                }
                if post_vis is not None:
                    entry['pre_vis_mm'] = (round(last_vis, 4)
                                           if last_vis is not None else None)
                    entry['post_vis_mm'] = round(post_vis, 4)
                    entry['new_face_mm'] = (round(new_face_mm, 4)
                                            if new_face_mm is not None else None)
                    last_vis = post_vis
                report['history'].append(entry)
                current_si = int(si_after)
                report['rounds'] += 1
                if current_si == 0:
                    report['reason'] = 'clean'
                elif round_idx + 1 >= max_rounds:
                    report['reason'] = 'max_rounds'
                break
            if not guard_ok:
                # The visible shape budget, not the topology check, rejected
                # this round: restore and stop the whole pass so no later
                # round can compound the movement. Residual SI stays reported.
                v, t = saved_v.copy(), saved_t.copy()
                sg = report['shape_guard']
                sg['rounds_rejected'] = int(sg['rounds_rejected']) + 1
                sg['rejected_round'] = round_idx + 1
                sg['pre_vis_mm'] = (round(last_vis, 4)
                                    if last_vis is not None else None)
                sg['post_vis_mm'] = (round(post_vis, 4)
                                     if post_vis is not None else None)
                sg['new_face_mm'] = (round(new_face_mm, 4)
                                     if new_face_mm is not None else None)
                sg['reason'] = 'shape_guard'
                report['history'].append({
                    'round': round_idx + 1,
                    'si_before': int(current_si),
                    'si_after': int(si_after) if si_after is not None else None,
                    'faces_after': int(topo_after.get('faces_number', 0) or 0),
                    'dilation_rings': rings,
                    'reason': 'shape_guard',
                    'pre_vis_mm': sg.get('pre_vis_mm'),
                    'post_vis_mm': sg.get('post_vis_mm'),
                    'new_face_mm': sg.get('new_face_mm'),
                })
                report['reason'] = 'shape_guard'
                break
            # Retry only the 40886-type rejection: the one-ring round reduced
            # the pre-existing boundary but could not close it.
            if (rings == 1 and holes_after > 0
                    and holes_after < holes_before
                    and si_ok and nm_after <= nm_before):
                continue
            v, t = saved_v.copy(), saved_t.copy()
            break
        if not accepted:
            if report['reason'] not in ('no_selection', 'scope', 'shape_guard'):
                report['reason'] = 'rollback'
            break
        if report['reason'] in ('clean', 'max_rounds'):
            break
    report['si_after'] = int(current_si)
    report['removed_si'] = int(report['initial_si'] - current_si)
    report['applied'] = report['rounds'] > 0
    report['seconds'] = round(time.perf_counter() - t0, 3)
    if guard_on:
        sg = report.get('shape_guard')
        if sg is not None:
            sg['final_vis_mm'] = (round(last_vis, 4)
                                  if last_vis is not None else None)
            sg['si_remaining_after'] = int(current_si)
    return v, t, report


def join_small_components(verts, tris, mincomponentsize, maxholesize):
    """Prototype (flag-gated): join small components onto the nearest larger
    one instead of deleting them.

    Every connected component with fewer than ``mincomponentsize`` faces is
    translated so its closest vertex coincides with the nearest vertex of the
    nearest larger component, then duplicate vertices are merged and holes
    re-closed. This is a deliberate geometry change (small debris is MOVED,
    not dropped) so it must stay behind ``--experimental-join-components`` and
    is NOT part of the default chain.

    Returns ``(verts, tris, moved, remaining_small)`` -- moved = number of
    small components joined, remaining_small = small components that could not
    be joined (no larger component to move onto, or only small components).
    """
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    if len(t) == 0:
        return v, t, 0, 0

    # connected components via union-find over faces sharing a vertex
    parent = list(range(len(t)))

    def find(i):
        while parent[i] != i:
            parent[i] = parent[parent[i]]
            i = parent[i]
        return i

    def union(i, j):
        ri, rj = find(i), find(j)
        if ri != rj:
            parent[ri] = rj

    vertex_faces = {}
    for fi, tri in enumerate(t):
        for vi in tri:
            vertex_faces.setdefault(int(vi), []).append(fi)
    for faces in vertex_faces.values():
        first = faces[0]
        for fi in faces[1:]:
            union(first, fi)

    comps = np.array([find(i) for i in range(len(t))], dtype=np.int64)
    comp_ids, counts = np.unique(comps, return_counts=True)
    large = [c for c, n in zip(comp_ids, counts) if n >= mincomponentsize]
    small = [c for c, n in zip(comp_ids, counts) if n < mincomponentsize]
    if not large or not small:
        return v, t, 0, len(small)

    # per-component vertex index sets (component id -> sorted vertex ids)
    comp_verts = {}
    for fi, ci in enumerate(comps):
        comp_verts.setdefault(int(ci), set()).update(int(x) for x in t[fi])
    comp_verts = {c: np.array(sorted(s), dtype=np.int64) for c, s in comp_verts.items()}

    v_new = v.copy()
    moved = 0
    for sc in small:
        sv = comp_verts[sc]
        sv3 = v[sv]
        # nearest larger component + closest vertex pair (small comps are tiny,
        # so the O(|small| * |large|) scan is cheap in practice)
        best_dist = np.inf
        best_translate = None
        for lc in large:
            lv3 = v[comp_verts[lc]]
            d = np.linalg.norm(sv3[:, None, :] - lv3[None, :, :], axis=2)
            i, j = np.unravel_index(np.argmin(d), d.shape)
            if d[i, j] < best_dist:
                best_dist = float(d[i, j])
                best_translate = lv3[j] - sv3[i]
        if best_translate is None:
            continue
        # move the whole small component onto the nearest larger component
        v_new[sv] = v[sv] + best_translate
        moved += 1

    # fuse the coincident vertices and re-close what the move opened
    chain = [('meshing_remove_duplicate_vertices', {}),
             ('meshing_repair_non_manifold_vertices', {}),
             ('meshing_remove_duplicate_faces', {}),
             ('meshing_close_holes', {'maxholesize': maxholesize}),
             ('meshing_remove_unreferenced_vertices', {})]
    v_new, t_new, _applied, _skipped = apply_chain(v_new, t, chain)
    return v_new, t_new, moved, max(len(small) - moved, 0)
