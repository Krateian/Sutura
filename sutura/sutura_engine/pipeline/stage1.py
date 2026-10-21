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


def _refine_hole_enabled():
    """Experimental (env-gated): refine the Stage-1 hole fill.

    ``SUTURA_REFINE_HOLE=1`` makes ``meshing_close_holes`` use Liepa-style
    refinement (``refinehole=True``, default 3 % edge length) so large /
    non-convex boundary loops get interior vertices instead of long ear-cut
    chords. OFF by default; not yet proven on the real-world corpus, so it
    stays behind the env flag and out of the CLI/GUI (see
    ``docs/cli-gui-parity-notes.md``).
    """
    return os.environ.get('SUTURA_REFINE_HOLE', '').strip().lower() in (
        '1', 'true', 'yes', 'on')


def _close_holes_step(maxholesize):
    """The ``meshing_close_holes`` chain entry, refined only when opted in."""
    params = {'maxholesize': maxholesize}
    if _refine_hole_enabled():
        params['refinehole'] = True
    return ('meshing_close_holes', params)


def _close_holes_ms(ms, ml, maxholesize=1000, self_intersection=True,
                    newfaceselected=False, refinehole=False,
                    refineholeedgelen=None):
    """Apply ``meshing_close_holes`` to a MeshSet in place (native).

    Thin delegate to :func:`topology.close_holes_ms`: the Rust ear-clipping
    closer handles the plain path, while the Liepa-style ``refinehole=True``
    refinement is not ported yet (Faz 4D) and is routed to PyMeshLab unchanged.
    Returns ``(closed_holes, new_faces)`` for the native path and ``None`` for
    the PyMeshLab fallback.
    """
    return topology.close_holes_ms(
        ms, ml, maxholesize=maxholesize, self_intersection=self_intersection,
        newfaceselected=newfaceselected, refinehole=refinehole,
        refineholeedgelen=refineholeedgelen)


def stage1_cleanup_chain(ml):
    """The Stage-1 mesh-hygiene passes: duplicate/null removal, non-manifold
    edge/vertex repair and a coherent re-orientation, with NO hole closing and
    NO component removal.

    ``stage1_chain`` builds on this list; the Tamponade prepass (T4) also uses
    it verbatim so the reference-based hole plug runs on the same cleaned
    surface the chain would otherwise hand to its generic ``close_holes``.
    """
    return [
        ('meshing_remove_duplicate_faces', {}),
        ('meshing_remove_null_faces', {}),
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


def stage1_chain(ml, maxholesize=1000, mincomponentsize=8, join_components=False):
    chain = stage1_cleanup_chain(ml) + [
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


def delete_fallback_chain(ml, maxholesize=1000, mincomponentsize=8, join_components=False):
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


def _set_mesh_on_ms(ms, ml, v, t):
    """Update ms with (v, t), preserving an empty current mesh if empty."""
    ms.clear()
    if len(v) > 0 and len(t) > 0:
        ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t.astype(np.int32)))
    elif len(v) > 0:
        ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=np.zeros((0, 3), dtype=np.int32)))
    else:
        ms.add_mesh(ml.Mesh(vertex_matrix=np.zeros((1, 3), dtype=np.float64),
                            face_matrix=np.zeros((0, 3), dtype=np.int32)))
        ms.apply_filter('meshing_remove_unreferenced_vertices')


def apply_chain(ms, chain):
    import pymeshlab as ml
    applied = 0
    skipped = {}
    for name, params in chain:
        try:
            handled = False
            if name == 'meshing_remove_duplicate_faces':
                m = ms.current_mesh()
                t = m.face_matrix()
                t_new = topology.remove_duplicate_faces(t)
                if len(t_new) < len(t):
                    v = m.vertex_matrix()
                    _set_mesh_on_ms(ms, ml, v, t_new)
                handled = True
            elif name == 'meshing_remove_null_faces':
                m = ms.current_mesh()
                v = m.vertex_matrix()
                t = m.face_matrix()
                t_new = topology.remove_null_faces(v, t)
                if len(t_new) < len(t):
                    _set_mesh_on_ms(ms, ml, v, t_new)
                handled = True
            elif name == 'meshing_remove_duplicate_vertices':
                m = ms.current_mesh()
                v = m.vertex_matrix()
                t = m.face_matrix()
                v_new, t_new = topology.remove_duplicate_vertices(v, t)
                if len(v_new) < len(v):
                    _set_mesh_on_ms(ms, ml, v_new, t_new)
                handled = True
            elif name == 'meshing_remove_unreferenced_vertices':
                _remove_unref_vertices_ms(ms, ml)
                handled = True
            elif name == 'meshing_remove_connected_component_by_face_number':
                min_sz = params.get('mincomponentsize', 8)
                remove_unref = params.get('removeunref', True)
                m = ms.current_mesh()
                v = m.vertex_matrix()
                t = m.face_matrix()
                v_new, t_new = topology.remove_connected_components(v, t, min_faces=min_sz, remove_unref=remove_unref)
                if len(t_new) < len(t):
                    _set_mesh_on_ms(ms, ml, v_new, t_new)
                handled = True
            elif name == 'meshing_invert_face_orientation':
                _invert_faces_ms(ms, ml)
                handled = True
            elif name == 'meshing_repair_non_manifold_edges':
                _repair_nme_ms(ms, ml)
                handled = True
            elif name == 'meshing_re_orient_faces_coherently':
                _re_orient_faces_ms(ms, ml)
                handled = True
            elif name == 'meshing_repair_non_manifold_vertices':
                _repair_nmv_ms(ms, ml)
                handled = True
            elif name == 'meshing_remove_selected_faces':
                _remove_selected_faces_ms(ms, ml)
                handled = True
            elif name == 'meshing_close_holes':
                _close_holes_ms(
                    ms, ml, maxholesize=params.get('maxholesize', 1000),
                    self_intersection=params.get('selfintersection', True),
                    newfaceselected=params.get('newfaceselected', False),
                    refinehole=params.get('refinehole', False),
                    refineholeedgelen=params.get('refineholeedgelen'))
                handled = True

            if not handled:
                ms.apply_filter(name, **params)
            applied += 1
        except Exception as exc:
            method_health.record_error('stage1:' + name, 'filter', exc)
            skipped[name] = str(exc)
    return applied, skipped


def _remove_unref_vertices_ms(ms, ml):
    """Remove unreferenced vertices from a MeshSet in-place with native topology."""
    m = ms.current_mesh()
    v = m.vertex_matrix()
    t = m.face_matrix()
    v_new, t_new = topology.remove_unreferenced_vertices(v, t)
    if len(v_new) < len(v):
        _set_mesh_on_ms(ms, ml, v_new, t_new)


def _invert_faces_ms(ms, ml):
    """Invert face orientation on a MeshSet in-place with native topology."""
    m = ms.current_mesh()
    v = m.vertex_matrix()
    t = m.face_matrix()
    t_new = topology.invert_face_orientation(t)
    _set_mesh_on_ms(ms, ml, v, t_new)


def _re_orient_faces_ms(ms, ml):
    ms.apply_filter('meshing_re_orient_faces_coherently')


def _repair_nme_ms(ms, ml):
    """Repair non-manifold edges on a MeshSet in-place with native topology."""
    m = ms.current_mesh()
    v = m.vertex_matrix()
    t = m.face_matrix()
    t_new = topology.repair_non_manifold_edges(v, t)
    if len(t_new) < len(t):
        _set_mesh_on_ms(ms, ml, v, t_new)


def _repair_nmv_ms(ms, ml):
    """Repair non-manifold vertices on a MeshSet in-place with native topology."""
    m = ms.current_mesh()
    v = m.vertex_matrix()
    t = m.face_matrix()
    v_new, t_new = topology.repair_non_manifold_vertices(v, t)
    if len(v_new) > len(v):
        _set_mesh_on_ms(ms, ml, v_new, t_new)


def _remove_selected_faces_ms(ms, ml, mask=None):
    """Remove selected faces on a MeshSet in-place with native topology."""
    m = ms.current_mesh()
    v = m.vertex_matrix()
    t = m.face_matrix()
    sel = (np.asarray(mask, dtype=bool) if mask is not None
           else np.asarray(m.face_selection_array(), dtype=bool))
    if sel.any():
        t_new = topology.remove_selected_faces(t, sel)
        _set_mesh_on_ms(ms, ml, v, t_new)


def extreme_extra_passes(ms, ml, params):
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
     second_chain_applied, second_chain_skipped).
    When the mesh already has no self-intersecting faces the extra passes are
    skipped harmlessly (no error, nothing removed) and passes_applied is False.
    """
    if topology.native_si_enabled():
        m = ms.current_mesh()
        mask, found = topology.self_intersecting_faces(m.vertex_matrix(), m.face_matrix())
        if int(found) >= 0:
            if int(found) == 0:
                return False, 0, 0, 0, {}
            before_removal = m.face_number()
            _remove_selected_faces_ms(ms, ml, mask=mask)
            removed = max(before_removal - ms.current_mesh().face_number(), 0)
            _remove_unref_vertices_ms(ms, ml)
            applied2, skipped2 = apply_chain(ms, stage1_chain(ml, **params))
            return True, int(found), removed, applied2, skipped2
        # Classifier skip sentinel (-1, mesh above the cap): fall through to
        # the PyMeshLab path rather than treating the mesh as SI-free.
    ms.apply_filter('compute_selection_by_self_intersections_per_face')
    found = int(ms.current_mesh().face_selection_array().sum())
    if found == 0:
        return False, 0, 0, 0, {}
    before_removal = ms.current_mesh().face_number()
    _remove_selected_faces_ms(ms, ml)
    removed = max(before_removal - ms.current_mesh().face_number(), 0)
    _remove_unref_vertices_ms(ms, ml)
    applied2, skipped2 = apply_chain(ms, stage1_chain(ml, **params))
    return True, found, removed, applied2, skipped2


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


SI_EXCISE_REFINE_EDGE_PCT = 2.0     # refineholeedgelen, % of bbox diagonal


# Liepa refinement (refinehole=True) costs ~3 s per round on a 90k-face mesh
# but, measured on the 40-mesh corpus, changed NO excise outcome vs the plain
# cap; it is therefore used only where it is cheap (small meshes).
SI_EXCISE_REFINE_MAX_FACES = 20000


SI_EXCISE_DEFAULT_BUDGET = 5.0


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


def _si_count_and_mask(ms):
    """Number of faces flagged and boolean mask (or None)."""
    if topology.native_si_enabled():
        m = ms.current_mesh()
        mask, count = topology.self_intersecting_faces(m.vertex_matrix(), m.face_matrix())
        if int(count) >= 0:
            return int(count), mask
        # Classifier skip sentinel (-1): fall through to the PyMeshLab path.
    ms.apply_filter('compute_selection_by_self_intersections_per_face')
    mask = np.asarray(ms.current_mesh().face_selection_array(), dtype=bool)
    n = int(mask.sum())
    ms.apply_filter('set_selection_none')
    return n, mask


def _si_count(ms):
    """Number of faces flagged by the self-intersection filter (selection is
    cleared afterwards; the filter never changes geometry)."""
    cnt, _ = _si_count_and_mask(ms)
    return cnt


def si_excise_recap(ms, ml, *, maxholesize=1000,
                    max_rounds=SI_EXCISE_MAX_ROUNDS,
                    time_budget=SI_EXCISE_DEFAULT_BUDGET,
                    face_fraction=SI_EXCISE_MAX_FACE_FRACTION):
    """Guarded self-intersection excise + refined re-cap for the Full Mend path.

    Selects the self-intersecting faces, dilates the selection by one vertex
    ring, deletes it and re-caps the opened loops with ``meshing_close_holes``
    (``selfintersection=True`` -- which PREVENTS new self-intersecting caps;
    ``refinehole=True`` only up to ``SI_EXCISE_REFINE_MAX_FACES``, since the
    Liepa refinement costs ~3 s/round on a 90k mesh without changing the
    corpus outcome). On the native exact path a rejected one-ring round is
    retried once on the same pre-round mesh with two rings only when it reduced
    (but did not close) the pre-existing boundary: the exact classifier does
    not flag adjacent shared-vertex / touching faces, so the extra ring clears
    that contact pocket before re-capping. A round that opens more boundary
    edges than the pre-round mesh is not retried. A round is
    committed only when the self-intersection count strictly decreases,
    non-manifold edges do not increase, the connected-component count does not
    increase, and the native path closes completely
    (``holes_after == 0``); the fallback keeps the historical
    ``holes_after <= holes_before``. Otherwise the pre-round mesh is restored
    byte-for-byte. The connected-component removal filter is never called (no
    component is dropped). ``report['dilation_rings']`` is the ring count
    actually used by the last attempt (and each accepted round records it in
    ``history``).
    Returns ``(ms, report)``; ``report`` is JSON-serialisable.
    """
    native_si = topology.native_si_enabled()
    report = {
        'ran': False, 'applied': False, 'rounds': 0, 'initial_si': 0,
        'si_after': 0, 'removed_si': 0, 'reason': None,
        'dilation_rings': 1,
        'budget_s': (None if time_budget is None else float(time_budget)),
        'seconds': 0.0, 'history': [],
    }
    init_mask = None
    with method_health.tier_guard('si_excise', 'tier') as _g0:
        report['initial_si'], init_mask = _si_count_and_mask(ms)
    if _g0.error is not None:
        report['reason'] = 'error: %s' % _g0.error
        return ms, report
    if report['initial_si'] == 0:
        report['reason'] = 'no_si'
        return ms, report
    if time_budget is not None and time_budget <= 0.0:
        report['reason'] = 'budget'
        return ms, report

    report['ran'] = True
    t0 = time.perf_counter()
    current_si = report['initial_si']
    total_faces = int(ms.current_mesh().face_number())
    for round_idx in range(max_rounds):
        if time_budget is not None and (time.perf_counter() - t0) > time_budget:
            report['reason'] = 'budget'
            break
        topo_before = topology.mesh_topology(ms)
        # Use boundary EDGES, not ``number_holes``: the latter is unreliable on
        # meshes with non-manifold vertices (reported -1 on thingi10k_145065,
        # which let a round that opened 42 boundary loops be accepted).
        holes_before = int(topo_before.get('boundary_edges', 0) or 0)
        nm_before = (int(topo_before.get('non_two_manifold_edges', 0) or 0)
                     + int(topo_before.get('non_two_manifold_vertices', 0) or 0))
        comp_before = int(topo_before.get('connected_components_number', 0) or 0)
        saved_v = ms.current_mesh().vertex_matrix().copy()
        saved_t = ms.current_mesh().face_matrix().copy()
        if native_si:
            if round_idx == 0 and init_mask is not None:
                sel_cur = init_mask
            else:
                m_cur = ms.current_mesh()
                sel_cur, _ = topology.self_intersecting_faces(
                    m_cur.vertex_matrix(), m_cur.face_matrix())
        else:
            ms.apply_filter('compute_selection_by_self_intersections_per_face')
            sel_cur = np.asarray(ms.current_mesh().face_selection_array(), dtype=bool)
        m_cur = ms.current_mesh()
        t_cur = np.asarray(m_cur.face_matrix(), dtype=np.int64)
        accepted = False
        for attempt, rings in enumerate((1, 2) if native_si else (1,)):
            if attempt:
                ms.clear()
                ms.add_mesh(ml.Mesh(vertex_matrix=saved_v, face_matrix=saved_t))
            report['dilation_rings'] = rings
            with method_health.tier_guard('si_excise', 'round') as _gr:
                sel_dil = topology.dilate_face_selection(
                    t_cur, sel_cur, rings=rings)
                selected = int(sel_dil.sum())
                if selected == 0:
                    report['reason'] = 'no_selection'
                    break
                if selected > face_fraction * max(total_faces, 1):
                    report['reason'] = 'scope'
                    break
                _remove_selected_faces_ms(ms, ml, mask=sel_dil)
                _remove_unref_vertices_ms(ms, ml)
                _repair_nme_ms(ms, ml)
                _repair_nmv_ms(ms, ml)
                close_kwargs = {'maxholesize': maxholesize,
                                'newfaceselected': False,
                                'self_intersection': True}
                if total_faces <= SI_EXCISE_REFINE_MAX_FACES:
                    close_kwargs['refinehole'] = True
                    close_kwargs['refineholeedgelen'] = ml.PercentageValue(
                        SI_EXCISE_REFINE_EDGE_PCT)
                _close_holes_ms(ms, ml, **close_kwargs)
                _remove_unref_vertices_ms(ms, ml)
                topo_after = topology.mesh_topology(ms)
                holes_after = int(topo_after.get('boundary_edges', 0) or 0)
                nm_after = (int(topo_after.get('non_two_manifold_edges', 0) or 0)
                            + int(topo_after.get('non_two_manifold_vertices', 0) or 0))
                comp_after = int(topo_after.get('connected_components_number', 0) or 0)
                si_after = _si_count(ms)
            if _gr.error is not None:
                ms.clear()
                ms.add_mesh(ml.Mesh(vertex_matrix=saved_v, face_matrix=saved_t))
                report['reason'] = 'rollback'
                break
            holes_ok = holes_after == 0 if native_si else holes_after <= holes_before
            components_ok = comp_after <= comp_before
            if (si_after < current_si and holes_ok
                    and nm_after <= nm_before and components_ok):
                accepted = True
                report['history'].append({
                    'round': round_idx + 1,
                    'si_before': int(current_si),
                    'si_after': int(si_after),
                    'faces_after': int(topo_after.get('faces_number', 0) or 0),
                    'dilation_rings': rings,
                })
                current_si = si_after
                report['rounds'] += 1
                if current_si == 0:
                    report['reason'] = 'clean'
                elif round_idx + 1 >= max_rounds:
                    report['reason'] = 'max_rounds'
                break
            # Retry only the 40886-type rejection: the one-ring round reduced
            # the pre-existing boundary but could not close it (the contact
            # pocket needs the second ring). A round that opens *more* boundary
            # edges than the pre-round mesh must not trigger the larger cut.
            if (rings == 1 and native_si and holes_after > 0
                    and holes_after < holes_before
                    and si_after < current_si and nm_after <= nm_before):
                continue
            ms.clear()
            ms.add_mesh(ml.Mesh(vertex_matrix=saved_v, face_matrix=saved_t))
            break
        if not accepted:
            if report['reason'] not in ('no_selection', 'scope'):
                report['reason'] = 'rollback'
            break
        if report['reason'] in ('clean', 'max_rounds'):
            break
    report['si_after'] = int(current_si)
    report['removed_si'] = int(report['initial_si'] - current_si)
    report['applied'] = report['rounds'] > 0
    report['seconds'] = round(time.perf_counter() - t0, 3)
    return ms, report


def join_small_components(ms, ml, mincomponentsize, maxholesize):
    """Prototype (flag-gated): join small components onto the nearest larger
    one instead of deleting them.

    Every connected component with fewer than ``mincomponentsize`` faces is
    translated so its closest vertex coincides with the nearest vertex of the
    nearest larger component, then duplicate vertices are merged and holes
    re-closed. This is a deliberate geometry change (small debris is MOVED,
    not dropped) so it must stay behind ``--experimental-join-components`` and
    is NOT part of the default chain.

    Returns ``(moved, remaining_small)`` -- moved = number of small components
    joined, remaining_small = small components that could not be joined (no
    larger component to move onto, or the mesh has only small components).
    """
    v = np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float64)
    t = np.asarray(ms.current_mesh().face_matrix(), dtype=np.int64)
    if len(t) == 0:
        return 0, 0

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
        return 0, len(small)

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
    # (add_mesh makes the rebuilt mesh current)
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(v_new, dtype=np.float32),
                        face_matrix=np.asarray(t, dtype=np.int32)))
    for name, params in (('meshing_remove_duplicate_vertices', {}),
                         ('meshing_repair_non_manifold_vertices', {}),
                         ('meshing_remove_duplicate_faces', {}),
                         ('meshing_close_holes', {'maxholesize': maxholesize}),
                         ('meshing_remove_unreferenced_vertices', {})):
        try:
            if name == 'meshing_close_holes':
                _close_holes_ms(ms, ml, maxholesize=params['maxholesize'])
            else:
                ms.apply_filter(name, **params)
        except Exception:
            pass
    return moved, max(len(small) - moved, 0)
