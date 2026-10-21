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
    """Update ms with (v, t), preserving an empty current mesh if empty.

    PyMeshLab cannot hold a zero-vertex mesh. The empty case keeps one
    unreferenced vertex instead of running the old
    ``meshing_remove_unreferenced_vertices`` filter: every caller here looks
    at face counts, and W4a removes the last MeshSet edge case without a
    PyMeshLab call.
    """
    ms.clear()
    if len(v) > 0 and len(t) > 0:
        ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t.astype(np.int32)))
    elif len(v) > 0:
        ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=np.zeros((0, 3), dtype=np.int32)))
    else:
        ms.add_mesh(ml.Mesh(vertex_matrix=np.zeros((1, 3), dtype=np.float64),
                            face_matrix=np.zeros((0, 3), dtype=np.int32)))


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


def apply_chain(verts, tris, chain):
    """Run a Stage-1 chain on plain arrays and return ``(v, t, applied, skipped)``.

    Every filter used by the Stage-1 chains is native; no MeshSet is created.
    ``set_selection_none`` is a no-op and a preceding
    ``compute_selection_by_non_manifold_edges_per_face`` writes a mask consumed
    by ``meshing_remove_selected_faces``.
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
                t = topology.remove_null_faces(v, t)
            elif name == 'meshing_remove_duplicate_vertices':
                v, t = topology.remove_duplicate_vertices(v, t)
            elif name == 'meshing_remove_unreferenced_vertices':
                v, t = topology.remove_unreferenced_vertices(v, t)
            elif name == 'meshing_remove_connected_component_by_face_number':
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
    """Coherently re-orient faces on a MeshSet with the native kernel."""
    m = ms.current_mesh()
    v = m.vertex_matrix()
    t = m.face_matrix()
    t_new = topology.re_orient_faces_coherently(t)
    if not np.array_equal(t_new, t):
        _set_mesh_on_ms(ms, ml, v, t_new)


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
        v, t, stage1_chain(None, **params))
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
    """Exact self-intersection count and face mask; ``-1`` means skipped."""
    m = ms.current_mesh()
    mask, count = topology.self_intersecting_faces(
        m.vertex_matrix(), m.face_matrix())
    return int(count), mask


def _si_count(ms):
    """Exact self-intersection face count (``-1`` when the classifier skips)."""
    cnt, _ = _si_count_and_mask(ms)
    return cnt


def si_excise_recap(verts, tris, *, maxholesize=1000,
                    max_rounds=SI_EXCISE_MAX_ROUNDS,
                    time_budget=SI_EXCISE_DEFAULT_BUDGET,
                    face_fraction=SI_EXCISE_MAX_FACE_FRACTION):
    """Guarded SI excise + re-cap, array-only (no MeshSet).

    Starts with a one-ring dilation. On the native exact path a rejected
    one-ring round is retried once with two rings only when it reduced (but
    did not close) the pre-existing boundary: the exact classifier does not
    flag adjacent shared-vertex / touching faces, so the extra ring clears
    that contact pocket before re-capping. The round is committed only when
    self-intersections strictly decrease, non-manifold edges and the
    connected-component count do not increase, and the mesh closes completely
    (``holes_after == 0``); otherwise the pre-round arrays are restored.
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

    report['ran'] = True
    t0 = time.perf_counter()
    current_si = report['initial_si']
    total_faces = len(t)
    for round_idx in range(max_rounds):
        if time_budget is not None and (time.perf_counter() - t0) > time_budget:
            report['reason'] = 'budget'
            break
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
            if (si_ok and holes_ok and nm_after <= nm_before
                    and components_ok):
                accepted = True
                v, t = v_work, t_work
                report['history'].append({
                    'round': round_idx + 1,
                    'si_before': int(current_si),
                    'si_after': int(si_after),
                    'faces_after': int(topo_after.get('faces_number', 0) or 0),
                    'dilation_rings': rings,
                })
                current_si = int(si_after)
                report['rounds'] += 1
                if current_si == 0:
                    report['reason'] = 'clean'
                elif round_idx + 1 >= max_rounds:
                    report['reason'] = 'max_rounds'
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
            if report['reason'] not in ('no_selection', 'scope'):
                report['reason'] = 'rollback'
            break
        if report['reason'] in ('clean', 'max_rounds'):
            break
    report['si_after'] = int(current_si)
    report['removed_si'] = int(report['initial_si'] - current_si)
    report['applied'] = report['rounds'] > 0
    report['seconds'] = round(time.perf_counter() - t0, 3)
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
