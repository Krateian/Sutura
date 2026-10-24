# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Coaptation tier (method #19): tolerance seam-weld + per-body union.

A folded / overlapping multi-body soup (e.g. a thin sheet repaired as two
overlapping halves) reloads as several edge-connected bodies whose seams are
separated by roughly one float32 ULP.  The plain stage-1 chain leaves the
result closed but riddled with self-intersections where the bodies cross;
Graft / Dressing / Splint / fTetWild never run because the result already has
zero holes and zero non-manifold edges.

Coaptation runs on the ORIGINAL input arrays (like the Graft / fTetWild /
Splint tiers) and closes the seam exactly:

  1. exact float32 weld (`weld_reload_equivalent`) -> the reload-equivalent
     soup, split into edge-connected bodies;
  2. seam-only tolerance weld: vertices incident to an open boundary edge are
     merged when they lie within one float32 ULP (`tol = k * ulp_f32(max|coord|)`)
     -- interior vertices are NEVER merged, so a genuine thin wall cannot be
     collapsed;
  3. per-body `close_holes`, orientation by signed volume;
  4. fold the bodies with the exact `sutura_geom.union_pair`.

The candidate is adopted only when it is STRICTLY better than the result the
ladder currently holds: reload-watertight, exact self-intersections 0 and
strictly below the baseline's measurable count, and no worse on holes,
non-manifold edges or component count.  Otherwise the current result is
returned byte-identical.  Never raises.
"""
import time

import numpy as np

import topology
from sutura_engine import health as method_health
from sutura_engine.pipeline.p_weld import reload_strict_holes_nm
from sutura_engine.pipeline.presets import (
    COAPTATION_DEFAULT_WORK, COAPTATION_MAX_FACES, COAPTATION_SEAM_ULP_K,
    COAPTATION_WORK_BY_INTENSITY,
)


def _face_labels(tris):
    """Edge-connected face labels (int64, ``0..k-1``), pure numpy.

    Two faces are in the same body when they share an undirected edge."""
    t = np.ascontiguousarray(tris, dtype=np.int64)
    nf = len(t)
    if nf == 0:
        return np.zeros(0, dtype=np.int64)
    nv = int(t.max()) + 1 if t.size else 0
    edges, counts, inverse = topology.edge_table(t)
    face_of_he = np.arange(3 * nf, dtype=np.int64) % nf
    edge_of_he = inverse
    parent = np.arange(nf, dtype=np.int64)

    def find(x):
        root = x
        while parent[root] != root:
            root = parent[root]
        while parent[x] != root:
            parent[x], x = root, parent[x]
        return root

    order = np.argsort(edge_of_he, kind='stable')
    eh = edge_of_he[order]
    fo = face_of_he[order]
    is_start = np.ones(len(eh), dtype=bool)
    if len(eh) > 1:
        is_start[1:] = eh[1:] != eh[:-1]
    starts = np.where(is_start)[0]
    ends = np.append(starts[1:], len(eh))
    for s, e in zip(starts, ends):
        if e - s < 2:
            continue
        r = find(int(fo[s]))
        for m in range(s + 1, e):
            r2 = find(int(fo[m]))
            if r2 != r:
                parent[r2] = r
    roots = np.array([find(x) for x in range(nf)], dtype=np.int64)
    _, labels = np.unique(roots, return_inverse=True)
    return labels.astype(np.int64)


def _boundary_vertex_ids(tris):
    """Vertices incident to an open (used-once) edge."""
    t = np.ascontiguousarray(tris, dtype=np.int64)
    if len(t) == 0:
        return np.zeros(0, dtype=np.int64)
    edges, counts, _ = topology.edge_table(t)
    open_edges = edges[counts == 1]
    if len(open_edges) == 0:
        return np.zeros(0, dtype=np.int64)
    return np.unique(open_edges.reshape(-1))


def _seam_weld(v32, tris, tol):
    """Merge only seam (boundary-edge) vertices within ``tol`` per axis.

    Returns ``(verts, tris, merged)``.  Interior vertices are never merged and
    degenerate faces created by the merge are dropped."""
    t = np.ascontiguousarray(tris, dtype=np.int64)
    nv = len(v32)
    seam = _boundary_vertex_ids(t)
    if len(seam) == 0 or tol <= 0:
        return v32, t, 0
    pts = np.asarray(v32[seam], dtype=np.float64)
    cell = np.floor(pts / tol).astype(np.int64)
    buckets = {}
    parent = np.arange(nv, dtype=np.int64)

    def find(x):
        root = x
        while parent[root] != root:
            root = parent[root]
        while parent[x] != root:
            parent[x], x = root, parent[x]
        return root

    order = np.lexsort((cell[:, 2], cell[:, 1], cell[:, 0]))
    for i in order:
        ci = cell[i]
        for dx in (-1, 0, 1):
            for dy in (-1, 0, 1):
                for dz in (-1, 0, 1):
                    key = (int(ci[0]) + dx, int(ci[1]) + dy, int(ci[2]) + dz)
                    for j in buckets.get(key, ()):
                        if np.all(np.abs(pts[i] - pts[j]) <= tol):
                            ra, rb = find(int(seam[i])), find(int(seam[j]))
                            if ra != rb:
                                parent[max(ra, rb)] = min(ra, rb)
        buckets.setdefault((int(ci[0]), int(ci[1]), int(ci[2])), []).append(i)
    root = np.array([find(x) for x in range(nv)], dtype=np.int64)
    roots = np.unique(root)
    _, remap = np.unique(root, return_inverse=True)
    remap = remap.astype(np.int64)
    nt = remap[t]
    good = (nt[:, 0] != nt[:, 1]) & (nt[:, 1] != nt[:, 2]) & (nt[:, 2] != nt[:, 0])
    merged = int(len(seam) - len(np.unique(remap[seam])))
    return np.ascontiguousarray(np.asarray(v32)[roots]), np.ascontiguousarray(nt[good]), merged


def _signed_volume(v, t):
    a, b, c = v[t[:, 0]], v[t[:, 1]], v[t[:, 2]]
    return float(np.einsum('ij,ij->i', a, np.cross(b, c)).sum() / 6.0)


def _measure(v, t, sg):
    """Reload-honest ``(holes, nm, parts, si, watertight)``."""
    v32 = np.asarray(v, dtype=np.float32)
    t32 = np.asarray(t, dtype=np.int32)
    holes, nm = reload_strict_holes_nm(v, t)
    labels = _face_labels(t32)
    parts = int(labels.max() + 1) if len(labels) else 0
    try:
        si = topology.si_face_count(v, t)
    except Exception:  # noqa: BLE001
        si = None
    try:
        wt = bool(sg.watertight_check(v, t)[0])
    except Exception:  # noqa: BLE001
        wt = False
    return int(holes), int(nm), parts, si, wt


from sutura_engine.pipeline.adopt_guard import check_candidate, policy_for


def coaptation_tier(v, t, input_v, input_t, after, stats, tmpdir,
                    coaptation=False, intensity=None, spec=None):
    """Tolerance seam-weld + per-body union tier (registry method #19).

    ``v``/``t`` is the result the ladder currently holds and
    ``input_v``/``input_t`` the original user surface this tier rebuilds.
    ``stats['coaptation']`` records the gate, the baseline/candidate measures
    and the adopt verdict.  Never raises.
    """
    v = np.asarray(v, dtype=np.float64)
    t = np.asarray(t, dtype=np.int64)
    input_v = np.asarray(input_v if input_v is not None else v, dtype=np.float64)
    input_t = np.asarray(input_t if input_t is not None else t, dtype=np.int64)
    if not coaptation:
        return v, t, after
    cap = getattr(spec, 'coaptation_max_faces', COAPTATION_MAX_FACES) \
        if spec is not None else COAPTATION_MAX_FACES
    key = (intensity or getattr(spec, 'base', None)
           or getattr(spec, 'name', None))
    work_limit = int(getattr(
        spec, 'coaptation_work',
        COAPTATION_WORK_BY_INTENSITY.get(key, COAPTATION_DEFAULT_WORK))
        if spec is not None else
        COAPTATION_WORK_BY_INTENSITY.get(key, COAPTATION_DEFAULT_WORK))
    ulp_k = int(COAPTATION_SEAM_ULP_K)
    rec = {'tier': 'coaptation', 'ran': False, 'adopted': False,
           'gate': None, 'ulp_k': ulp_k, 'tol': None, 'bodies_in': None,
           'bodies_after_weld': None, 'merged': None,
           'baseline': None, 'candidate': None, 'output_faces': None,
           'guard_rejected': None, 'guard_reason': None,
           'si_policy': None, 'si_checked': False, 'si_flagged': False,
           'seconds': None, 'skipped_budget': False, 'aborted': False,
           'work_units': None, 'work_budget': int(work_limit),
           'budget_limited_by': None,
           'reject_reason': None, 'reason': None, 'warnings': []}
    with method_health.tier_guard('coaptation', 'tier') as _g:
        try:
            import sutura_geom as _sg
        except Exception:  # noqa: BLE001
            _sg = None
        if _sg is None or not all(hasattr(_sg, n)
                                  for n in ('close_holes', 'union_pair')):
            rec['reject_reason'] = 'unavailable'
            rec['reason'] = ('sutura_geom close_holes/union_pair are unavailable '
                             'in this build; the ladder result was kept')
            stats['coaptation'] = rec
            return v, t, after
        rec['input_faces'] = int(len(input_t))
        if cap is not None and len(input_t) > int(cap):
            rec['skipped_budget'] = True
            rec['reject_reason'] = 'too_large'
            rec['reason'] = ('input has %d faces, above the Coaptation cap of '
                             '%d; the ladder result was kept'
                             % (len(input_t), int(cap)))
            stats['coaptation'] = rec
            return v, t, after
        # --- cheap gate: the reload-equivalent soup must be multi-body with a
        # seam that actually coapts (>=1 merged boundary vertex).
        wv, wt = topology.weld_reload_equivalent(input_v, input_t)
        lab_in = _face_labels(wt)
        bodies_in = int(lab_in.max() + 1) if len(lab_in) else 0
        rec['bodies_in'] = bodies_in
        if bodies_in < 2:
            rec['gate'] = 'single_body'
            rec['reject_reason'] = 'single_body'
            rec['reason'] = 'a single connected body; nothing to coapt'
            stats['coaptation'] = rec
            return v, t, after
        max_abs = float(np.abs(np.asarray(wv, dtype=np.float64)).max()) \
            if len(wv) else 0.0
        tol = ulp_k * float(np.spacing(np.float32(max_abs))) \
            if max_abs > 0 else 0.0
        rec['tol'] = tol
        sw_v, sw_t, merged = _seam_weld(np.asarray(wv, dtype=np.float32), wt, tol)
        rec['merged'] = int(merged)
        if merged <= 0:
            rec['gate'] = 'no_seam'
            rec['reject_reason'] = 'no_seam'
            rec['reason'] = ('no boundary vertices coapt within %d ULP; the '
                             'ladder result was kept' % ulp_k)
            stats['coaptation'] = rec
            return v, t, after
        src = 'input'
        t0 = time.perf_counter()
        lab = _face_labels(sw_t)
        bodies = []
        for b in range(int(lab.max() + 1) if len(lab) else 0):
            fb = np.where(lab == b)[0]
            if len(fb) < 1:
                continue
            u, inv = np.unique(sw_t[fb], return_inverse=True)
            bv = np.asarray(sw_v[u], dtype=np.float64)
            bt = inv.reshape(-1, 3).astype(np.int64)
            bv2, bt2, _closed, _nf = topology.close_holes(
                bv, bt, maxholesize=max(1000, len(bt)),
                self_intersection=True, newfaceselected=False)
            bv2 = np.asarray(bv2, dtype=np.float64)
            bt2 = np.asarray(bt2, dtype=np.int64)
            if len(bt2) and _signed_volume(bv2, bt2) < 0:
                bt2 = bt2[:, ::-1]
            bodies.append((bv2, bt2))
        rec['bodies_after_weld'] = len(bodies)
        work_units = int(sum(len(bt) for _bv, bt in bodies))
        rec['work_units'] = work_units
        if work_limit and work_units > int(work_limit):
            # DETERMINISTIC work budget (total body faces): stop before the
            # exact union, independent of CPU speed/load.
            rec['aborted'] = True
            rec['budget_limited_by'] = 'work'
            rec['reject_reason'] = 'work'
            rec['reason'] = ('Coaptation used %d work units > %d at intensity '
                             '%r; the ladder result was kept'
                             % (work_units, int(work_limit), key))
            stats['coaptation'] = rec
            return v, t, after
        if len(bodies) < 2:
            rec['gate'] = 'single_body_after_weld'
            rec['reject_reason'] = 'single_body_after_weld'
            rec['reason'] = ('the seam weld leaves a single body; the ladder '
                             'result was kept')
            stats['coaptation'] = rec
            return v, t, after
        acc_v = np.asarray(bodies[0][0], dtype=np.float64)
        acc_t = np.asarray(bodies[0][1], dtype=np.int32)
        for i in range(1, len(bodies)):
            bv, bt = bodies[i]
            acc_v, acc_t = _sg.union_pair(
                acc_v, acc_t, np.asarray(bv, dtype=np.float64),
                np.asarray(bt, dtype=np.int32))
            acc_v = np.asarray(acc_v, dtype=np.float64)
            acc_t = np.asarray(acc_t, dtype=np.int64)
        elapsed = time.perf_counter() - t0
        rec['seconds'] = float(elapsed)   # measurement only; never a decision
        rec['ran'] = True
        rec['source'] = src
        # --- strict adopt guard against the result the ladder currently holds.
        base_holes, base_nm, base_parts, base_si, _base_wt = _measure(v, t, _sg)
        cand_holes, cand_nm, cand_parts, cand_si, cand_wt = _measure(
            acc_v, acc_t, _sg)
        rec['baseline'] = {'holes': base_holes, 'non_manifold': base_nm,
                           'parts': base_parts, 'self_intersections': base_si}
        rec['candidate'] = {'holes': cand_holes, 'non_manifold': cand_nm,
                            'parts': cand_parts, 'self_intersections': cand_si,
                            'watertight': cand_wt, 'faces': int(len(acc_t))}
        rec['output_faces'] = int(len(acc_t))
        guard = check_candidate(
            len(input_t), len(input_v), len(acc_t), len(acc_v),
            slack_units=0, measure_si=False,
            policy=policy_for('coaptation'))
        for key, value in guard.fields.items():
            if key in rec:
                rec[key] = value
        strictly_better = (guard.ok
            and cand_wt and cand_holes == 0 and cand_nm == 0
            and cand_si is not None and cand_si == 0
            and base_si is not None and cand_si < base_si
            and cand_holes <= base_holes and cand_nm <= base_nm
            and cand_parts <= base_parts)
        if strictly_better:
            after = topology.mesh_topology(
                np.asarray(acc_v, dtype=np.float32),
                np.asarray(acc_t, dtype=np.int32))
            rec['adopted'] = True
            rec['watertight'] = False  # stage 2 confirms at the top level
            rec['candidate_watertight'] = True
            rec['reason'] = ('strict-watertight union candidate '
                             '(self-intersections %s -> 0, parts %d)'
                             % (base_si, cand_parts))
        else:
            rec['reject_reason'] = 'not_strictly_better'
            rec['reason'] = (
                'union result is not strictly better (candidate SI=%s wt=%s '
                'holes=%d nm=%d parts=%d vs baseline SI=%s holes=%d nm=%d '
                'parts=%d); the ladder result was kept'
                % (cand_si, cand_wt, cand_holes, cand_nm, cand_parts,
                   base_si, base_holes, base_nm, base_parts))
    if _g.error is not None:
        rec['reason'] = 'error: %s: %s' % (type(_g.error).__name__, _g.error)
    stats['coaptation'] = rec
    if rec['adopted'] and rec.get('ran'):
        return (np.asarray(acc_v, dtype=np.float64),
                np.asarray(acc_t, dtype=np.int64), after)
    return v, t, after
