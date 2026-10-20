# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""P-WELD reload-safe seam healing and P-HONEST verdict.

The single implementation of the float32-collision split, the welded-topology
repair, the reload-equivalent watertight metric and enforce_reload_verdict used
by the pipeline and the graft/dressing tiers. The former duplicate copies in
``sutura_engine.stitch`` / ``sutura_engine.xray`` had no live caller and were
removed.
"""
import numpy as np

import topology
from defects import detect as detect_defects
from sutura_engine.pipeline.stage1 import (
    _remove_unref_vertices_ms,
    _repair_nme_ms,
    _repair_nmv_ms,
)


def weld_reload_equivalent(verts, tris):
    """Weld a mesh into the form an STL save/reload reproduces.

    STL stores no vertex sharing: on reload a loader re-welds vertices whose
    stored float32 positions are exactly equal. Two positions that are distinct
    in the in-memory float64 arrays can therefore collapse after a float32
    write/read, creating edges used by more than two triangles that the
    in-memory index topology does not show (observed on the proxy/closing
    seam vertices: 3052 vertices, 3046 unique in float64 but 3040 in float32,
    i.e. 5 non-manifold edges after reload).

    Returns ``(verts, tris)`` in that reload-equivalent form: positions cast
    to float32, exactly-coincident positions merged, degenerate and duplicate
    faces dropped, unreferenced vertices removed. Pure numpy (no pymeshlab) so
    the method registry's guard can apply the same rule without importing it.
    """
    return topology.weld_reload_equivalent(verts, tris)


def _repair_welded_topology(ml, verts, tris):
    """Topology repair of a reload-equivalent mesh (P-FIX).

    Merging coincident positions can leave non-manifold edges / duplicate
    faces; this runs the same repair subset the Stage-1 chain uses so a
    legitimately repairable candidate is not falsely rejected. Never raises.
    """
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float32),
                        face_matrix=np.asarray(tris, np.int32)))
    for name, params in (('meshing_repair_non_manifold_edges', {}),
                         ('meshing_remove_duplicate_faces', {}),
                         ('meshing_repair_non_manifold_vertices', {}),
                         ('meshing_remove_unreferenced_vertices', {})):
        try:
            if name == 'meshing_remove_duplicate_faces':
                m = ms.current_mesh()
                t = m.face_matrix()
                t_new = topology.remove_duplicate_faces(t)
                if len(t_new) < len(t):
                    v = m.vertex_matrix()
                    ms.clear()
                    if len(t_new) > 0:
                        ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t_new.astype(np.int32)))
            elif name == 'meshing_remove_unreferenced_vertices':
                _remove_unref_vertices_ms(ms, ml)
            elif name == 'meshing_repair_non_manifold_edges':
                _repair_nme_ms(ms, ml)
            elif name == 'meshing_repair_non_manifold_vertices':
                _repair_nmv_ms(ms, ml)
            else:
                ms.apply_filter(name, **params)
        except Exception:  # noqa: BLE001 - a repair pass never crashes a tier
            pass
    return (np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float32),
            np.asarray(ms.current_mesh().face_matrix(), dtype=np.int32))


def reload_strict_holes_nm(verts, tris):
    """Strict (holes, non-manifold) on the STL save/reload-equivalent mesh.

    Applies ``weld_reload_equivalent`` (positions cast to float32 with
    exactly-coincident positions merged) and then ``defects.detect`` -- the
    same strict metric the repair guards use, measured on the mesh the user
    gets back after a save/reload rather than the in-memory index topology.
    Pure numpy (no pymeshlab)."""
    wv, wt = weld_reload_equivalent(verts, tris)
    d = detect_defects(wv, wt)
    return len(d['holes']), len(d['non_manifold'])


P_WELD_MAX_NUDGE_ATTEMPTS = 12


P_WELD_MAX_HOLE = 200


P_WELD_MIN_FACE_FRACTION = 0.98


def _separate_weld_collisions(verts, tris):
    """Split vertices that are distinct by index but coincide after the float32
    STL write.

    STL stores no vertex sharing, so two in-memory vertices whose float32
    positions are exactly equal are welded by the loader; when the in-memory
    index topology is a clean 2-manifold this weld is the ONLY reason the
    reloaded mesh is non-manifold. Keeping the vertices distinct by nudging
    all but one member of each collision group by a few float32 ULPs preserves
    the in-memory (manifold) topology on reload, with a geometry change below
    one part in 1e6. Pure numpy.
    """
    v64 = np.asarray(verts, dtype=np.float64).copy()
    t = np.asarray(tris, dtype=np.int64)
    if len(v64) == 0 or len(t) == 0:
        return v64.astype(np.float32), t
    v32 = v64.astype(np.float32)
    inv = topology.weld_vertices(v32)[1]
    counts = np.bincount(inv, minlength=int(inv.max()) + 1)
    if (counts <= 1).all():
        return v32, t
    order = np.argsort(inv, kind='stable')
    pos = np.empty(len(inv), dtype=np.int64)
    pos[order] = np.arange(len(inv))
    group_start = (np.cumsum(counts) - counts)[inv]
    rank = (pos - group_start).astype(np.float64)
    nonrep = rank > 0
    mag = np.maximum(np.abs(v32), np.float32(1.0))
    ulp = np.maximum(np.spacing(mag).max(axis=1).astype(np.float64), 1e-7)
    direction = np.array([1.0, 1.0, 1.0]) / np.sqrt(3.0)
    for attempt in range(P_WELD_MAX_NUDGE_ATTEMPTS):
        d = v64.copy()
        step = ulp[nonrep] * rank[nonrep] * (2.0 ** (attempt + 1))
        d[nonrep] += step[:, None] * direction[None, :]
        d32 = d.astype(np.float32)
        if len(topology.weld_vertices(d32)[0]) == len(d32):
            return d32, t
    return v32, t


def _close_small_holes(ml, verts, tris):
    """Repair non-manifold vertices then cap small boundary loops.

    Only used by the P-WELD fallback for a welded mesh that is not a clean
    in-memory topology; a failure leaves the input unchanged."""
    try:
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float64),
                            face_matrix=np.asarray(tris, np.int32)))
        _repair_nmv_ms(ms, ml)
        ms.apply_filter('meshing_close_holes', maxholesize=P_WELD_MAX_HOLE)
        _remove_unref_vertices_ms(ms, ml)
        m = ms.current_mesh()
        return (np.asarray(m.vertex_matrix(), dtype=np.float32),
                np.asarray(m.face_matrix(), dtype=np.int32))
    except Exception:  # noqa: BLE001 - a repair pass never crashes a tier
        return np.asarray(verts, np.float32), np.asarray(tris, np.int32)


def p_weld_final(ml, verts, tris):
    """Reload-safe final pass (P-WELD).

    The last mesh of every STL repair is checked in the form the user reloads
    (float32 positions with exactly-coincident vertices welded). When that form
    is not strict-watertight the pass tries, in order:

      1. splitting float32-coincidence collisions when the in-memory index
         topology is already a clean 2-manifold (the common stage-2 seam case);
      2. topologically repairing the welded mesh (remove/split the extra faces)
         and capping the small holes that creates.

    A candidate is adopted only when it is strict-watertight after the weld
    (0 holes, 0 non-manifold) and, for the fallback, keeps at least
    ``P_WELD_MIN_FACE_FRACTION`` of the welded faces. The untouched mesh is
    returned otherwise, so the pass can never make the output worse. Returns
    ``(verts, tris, report)`` with ``report`` None when the mesh was already
    reload-watertight.
    """
    v0 = np.asarray(verts)
    t0 = np.asarray(tris)
    base_h, base_nm = reload_strict_holes_nm(v0, t0)
    if base_h == 0 and base_nm == 0:
        return v0, t0, None
    rec = {'applied': False, 'method': None,
           'holes_before': int(base_h),
           'non_manifold_before': int(base_nm),
           'holes_after': int(base_h),
           'non_manifold_after': int(base_nm),
           'vertices_nudged': 0, 'faces_before': int(len(t0)),
           'faces_after': int(len(t0))}
    try:
        d = detect_defects(v0, t0)
        if len(d['holes']) == 0 and len(d['non_manifold']) == 0:
            cv, ct = _separate_weld_collisions(v0, t0)
            ch, cnm = reload_strict_holes_nm(cv, ct)
            if ch == 0 and cnm == 0:
                nudged = int((np.asarray(cv, np.float32)
                              != np.asarray(v0, np.float32)).any(axis=1).sum())
                rec.update(applied=True, method='split-collisions',
                           holes_after=0, non_manifold_after=0,
                           vertices_nudged=nudged,
                           faces_after=int(len(ct)))
                return cv, ct, rec
        wv, wt = weld_reload_equivalent(v0, t0)
        welded_faces = max(len(wt), 1)
        cands = [('weld-repair', _repair_welded_topology(ml, wv, wt))]
        rep_v, rep_t = cands[0][1]
        cands.append(('weld-repair+close', _close_small_holes(ml, rep_v, rep_t)))
        for name, (cv, ct) in cands:
            cv = np.asarray(cv, np.float32)
            ct = np.asarray(ct, np.int32)
            if len(ct) < P_WELD_MIN_FACE_FRACTION * welded_faces:
                continue
            ch, cnm = reload_strict_holes_nm(cv, ct)
            if ch == 0 and cnm == 0:
                rec.update(applied=True, method=name, holes_after=0,
                           non_manifold_after=0, faces_after=int(len(ct)))
                return cv, ct, rec
    except Exception:  # noqa: BLE001 - the final pass never crashes a repair
        pass
    return v0, t0, rec


def enforce_reload_verdict(report, verts, tris):
    """Honest top-level verdict: never claim watertight for a mesh that is
    not strict-watertight after the reload-equivalent weld (P-HONEST).

    Only a report that currently claims watertight is considered: stage 1
    two-manifold with no hole AND a successful stage-2 rebuild. If the saved
    mesh fails the reload check, the fields ``classification.classify()``
    reads are rewritten so the category can no longer be watertight, plus
    explicit reload markers. A genuinely watertight mesh returns False with
    NO report change, so its report and output stay byte-identical.
    Returns True when the report was downgraded."""
    if not isinstance(report, dict):
        return False
    s1 = report.get('stage1')
    if not isinstance(s1, dict):
        return False
    s2 = report.get('stage2')
    claims = (bool(s1.get('two_manifold'))
              and s1.get('holes_remaining', 0) == 0
              and bool(s2 and s2.get('ok')))
    if not claims:
        return False
    holes, nm = reload_strict_holes_nm(verts, tris)
    if holes == 0 and nm == 0:
        return False
    s1['reload_holes'] = int(holes)
    s1['reload_non_manifold'] = int(nm)
    s1['two_manifold'] = bool(nm == 0)
    s1['holes_remaining'] = int(holes)
    report['reload_watertight'] = False
    if isinstance(s2, dict):
        s2['watertight_after_reload'] = False
    return True
