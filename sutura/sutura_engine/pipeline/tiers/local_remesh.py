# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Local-remesh deep-repair tier (P1a split).

Moved verbatim from sutura/repair.py; no logic change.
"""
import time

import numpy as np

import topology
from sutura_engine import health as method_health
from sutura_engine.pipeline.presets import (
    LOCAL_MAX_LOOP_LEN, LOCAL_MAX_NM_EDGES, LOCAL_MAX_REGIONS,
    LOCAL_MAX_REMOVED_FACES, LOCAL_MAX_SECONDS, LOCAL_REMESH_FAIR_STEPS,
    LOCAL_REMESH_MAX_HOLE, LOCAL_REMESH_REFINE,
)
from sutura_engine.pipeline.stage1 import _repair_nmv_ms
from sutura_engine.pipeline.utils import (
    _canonical_face_keys, _count_si, _edge_uses, _faces_preserved,
    _hausdorff_rel, boundary_loop_stats,
)


def _damaged_region(tris, max_faces=None):
    """Faces of the damaged region: every face with a boundary edge (used
    once) or a non-manifold edge (used more than twice), grown by one vertex
    ring. Returns (face mask, number of connected regions); the region count
    is None when the region has more than ``max_faces`` faces (not
    computed)."""
    tris = np.asarray(tris, dtype=np.int64)
    F = len(tris)
    _keys, _uniq, inv, counts = _edge_uses(tris)
    bad_edge = (counts[inv] != 2)
    seed = bad_edge[:F] | bad_edge[F:2 * F] | bad_edge[2 * F:]
    if not seed.any():
        return seed, 0
    vmark = np.zeros(int(tris.max()) + 1, dtype=bool)
    vmark[tris[seed].ravel()] = True
    region = vmark[tris].any(axis=1)
    if max_faces is not None and int(region.sum()) > max_faces:
        return region, None
    # connected regions (faces sharing a vertex), union-find over vertices
    parent = np.arange(len(vmark))

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x
    for f in tris[region]:
        r0 = find(f[0])
        for x in f[1:]:
            rx = find(x)
            if rx != r0:
                parent[rx] = r0
    roots = {find(x) for x in np.unique(tris[region])}
    return region, len(roots)


def _local_si_rise(base_v, base_t, cand_v, cand_t):
    """Exact self-intersection rise restricted to the faces the candidate changed.

    The candidate keeps the base's healthy faces verbatim, so any SI difference
    is attributable to the faces that differ between the two meshes.  This
    counts the proper-crossing pairs that involve a changed candidate face and
    the same count for the changed base faces, and reports a rise when the
    candidate's count is higher.  Far cheaper than rescanning both meshes on a
    large input.  Returns ``(base_local, cand_local, si_rise, used)`` where
    ``used`` is False when the extension or its subset API is unavailable (the
    caller then falls back to the full scans).
    """
    try:
        import sutura_geom
    except Exception:  # noqa: BLE001 - no extension is a normal case
        return None, None, None, False
    fn = getattr(sutura_geom, 'self_intersections_subset', None)
    if fn is None:
        return None, None, None, False
    try:
        bk = _canonical_face_keys(base_v, base_t)
        ck = _canonical_face_keys(cand_v, cand_t)
        cand_changed = (~np.isin(ck, bk)) if len(ck) else np.zeros(0, bool)
        base_changed = (~np.isin(bk, ck)) if len(bk) else np.zeros(0, bool)
        base_local = 0
        if base_changed.any():
            base_local = int(fn(np.asarray(base_v, dtype=np.float64),
                                np.asarray(base_t, dtype=np.int32),
                                base_changed))
        cand_local = 0
        if cand_changed.any():
            cand_local = int(fn(np.asarray(cand_v, dtype=np.float64),
                                np.asarray(cand_t, dtype=np.int32),
                                cand_changed))
        return base_local, cand_local, bool(cand_local > base_local), True
    except Exception:  # noqa: BLE001 - a classifier failure is not fatal
        return None, None, None, False


def _umbrella_fair(verts, tris, first_free, steps):
    """Umbrella-operator smoothing (each vertex moves to the mean of its
    1-ring neighbours) of the vertices with index >= ``first_free``; all
    other vertices are fixed."""
    n = len(verts)
    if first_free >= n or steps <= 0:
        return verts
    a = np.concatenate([tris[:, 0], tris[:, 1], tris[:, 2]])
    b = np.concatenate([tris[:, 1], tris[:, 2], tris[:, 0]])
    src = np.concatenate([a, b])
    dst = np.concatenate([b, a])
    deg = np.bincount(src, minlength=n).astype(np.float64)
    free = np.zeros(n, dtype=bool)
    free[first_free:] = True
    free &= deg > 0
    out = verts.copy()
    for _ in range(steps):
        acc = np.zeros_like(out)
        np.add.at(acc, src, out[dst])
        out[free] = acc[free] / deg[free, None]
    return out


def _local_remesh_tier_ms(ml, ms, after):
    """Local re-mesh basamak (pymeshlab-only prototype).

    The damaged region (faces on a boundary or non-manifold edge, grown by
    one vertex ring) is deleted, pinched vertices of the new boundary are
    split, the openings are closed with ``meshing_close_holes`` (refined to
    the mean edge length of the deleted region) and only the new interior
    vertices are smoothed (Laplacian, the hole boundary stays fixed). The
    result is adopted only when holes and non-manifold edges are no worse
    and not both unchanged, self-intersections do not increase, and every
    face outside the deleted region is still present with identical
    coordinates. Returns ``(ms, after, report)``.
    """
    t0 = time.perf_counter()
    m = ms.current_mesh()
    v = np.asarray(m.vertex_matrix(), dtype=np.float64)
    t = np.asarray(m.face_matrix(), dtype=np.int64)
    rep = {'ran': True, 'adopted': False, 'outside_unchanged': False}
    base_holes, base_loop = boundary_loop_stats(v, t)
    base_nm = after.get('non_two_manifold_edges', 0)
    rep.update(holes_before=int(base_holes), nm_before=int(base_nm),
               max_loop_len=int(base_loop))

    def skip(reason):
        rep['reject_reason'] = reason
        rep['time'] = round(time.perf_counter() - t0, 3)
        return ms, after, rep
    if base_nm > LOCAL_MAX_NM_EDGES:
        return skip('scope: non-manifold edges')
    if base_loop > LOCAL_MAX_LOOP_LEN:
        return skip('scope: boundary loop too long')
    region, n_regions = _damaged_region(t, max_faces=LOCAL_MAX_REMOVED_FACES)
    rep['faces_removed'] = int(region.sum())
    if n_regions is None:
        return skip('scope: region too large')
    rep['regions'] = int(n_regions)
    if n_regions == 0:
        return skip('no damaged region')
    if n_regions > LOCAL_MAX_REGIONS:
        return skip('scope: too many regions')
    kept = t[~region]
    if len(kept) == 0:
        return skip('region covers the whole mesh')
    rt = t[region]
    el = np.linalg.norm(v[rt] - v[np.roll(rt, 1, axis=1)], axis=2)
    edge_len = float(el.mean()) if el.size else 0.0
    used = np.unique(kept)
    remap = -np.ones(len(v), dtype=np.int64)
    remap[used] = np.arange(len(used))
    kv, kt = v[used], remap[kept]
    with method_health.tier_guard('local_remesh', 'tier') as _g:
        trial = ml.MeshSet()
        trial.add_mesh(ml.Mesh(vertex_matrix=kv, face_matrix=kt.astype(np.int32)))
        _repair_nmv_ms(trial, ml)
        n0 = trial.current_mesh().face_number()
        nv0 = trial.current_mesh().vertex_number()
        topology.close_holes_ms(
            trial, ml, maxholesize=LOCAL_REMESH_MAX_HOLE,
            newfaceselected=True, self_intersection=True,
            refinehole=LOCAL_REMESH_REFINE and edge_len > 0,
            refineholeedgelen=ml.PureValue(edge_len))
        rep['faces_added'] = int(trial.current_mesh().face_number() - n0)
        if time.perf_counter() - t0 > LOCAL_MAX_SECONDS:
            return skip('time')
        tm = trial.current_mesh()
        nv = np.asarray(tm.vertex_matrix(), dtype=np.float64)
        nt = np.asarray(tm.face_matrix(), dtype=np.int64)
        # Fairing on the new interior vertices only (the refinement appends
        # them after the existing ones), so the hole boundary stays fixed.
        # pymeshlab's selected-only Laplacian also moved vertices of the
        # surrounding surface, hence the explicit umbrella operator here.
        nv = _umbrella_fair(nv, nt, nv0, LOCAL_REMESH_FAIR_STEPS)
        rep['fairing_iters'] = LOCAL_REMESH_FAIR_STEPS
        trial = ml.MeshSet()
        trial.add_mesh(ml.Mesh(vertex_matrix=nv, face_matrix=nt.astype(np.int32)))
        t_after = topology.mesh_topology(nv, nt)
        holes = boundary_loop_stats(nv, nt)[0]
        nm = t_after.get('non_two_manifold_edges', 0)
        rep.update(holes_after=int(holes), nm_after=int(nm))
        # guard 1: outside faces unchanged (exact coordinates, any order)
        outside_ok = _faces_preserved(kv, kt, nv, nt)
        rep['outside_unchanged'] = bool(outside_ok)
        # guard 2: holes / nm no worse, and some progress
        better = (holes <= base_holes and nm <= base_nm
                  and (holes, nm) != (base_holes, base_nm))
        si_ok = True
        if outside_ok and better and time.perf_counter() - t0 > LOCAL_MAX_SECONDS:
            return skip('time')
        if outside_ok and better:
            base_si = _count_si(ml, ms)
            new_si = _count_si(ml, trial)
            rep.update(si_before=base_si, si_after=new_si)
            si_ok = new_si <= base_si
        if not outside_ok:
            rep['reject_reason'] = 'faces outside the region changed'
        elif not better:
            rep['reject_reason'] = 'no improvement on holes/non-manifold edges'
        elif not si_ok:
            rep['reject_reason'] = 'more self-intersections'
        else:
            rep['hausdorff_rel'] = _hausdorff_rel(ml, v, t, nv, nt)[0]
            rep['adopted'] = True
            rep['time'] = round(time.perf_counter() - t0, 3)
            return trial, t_after, rep
    if _g.error is not None:
        rep['error'] = str(_g.error)
    rep['time'] = round(time.perf_counter() - t0, 3)
    return ms, after, rep


def _local_remesh_tier(*args, **kwargs):
    """Array API; legacy (ml, ms, after) is forwarded."""
    if not (len(args) >= 2 and isinstance(args[0], np.ndarray)
            and isinstance(args[1], np.ndarray)):
        return _local_remesh_tier_ms(*args, **kwargs)
    v, t, input_v, input_t, after = args[:5]
    import pymeshlab as _ml
    ms = _ml.MeshSet()
    ms.add_mesh(_ml.Mesh(vertex_matrix=np.asarray(v, np.float64),
                         face_matrix=np.asarray(t, np.int32)))
    ms, after, rep = _local_remesh_tier_ms(_ml, ms, after)
    m = ms.current_mesh()
    return (np.asarray(m.vertex_matrix(), dtype=np.float64),
            np.asarray(m.face_matrix(), dtype=np.int32), after, rep)
