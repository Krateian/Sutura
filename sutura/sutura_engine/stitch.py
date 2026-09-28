# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""sutura_engine.stitch - Reload-safe seam healing pass (P-WELD).

Separates coincident float32 vertex collisions using tiny float32 ULPs and repairs
welded topology before output so meshes reload watertight without modifying faces.

Provides:
  - p_weld_final: main reload-safe final pass
  - _separate_weld_collisions: nudges coincident vertices by float32 ULPs
  - _repair_welded_topology: topological repair on welded submesh
  - _close_small_holes: closes small boundary loops on welded submesh
"""
from typing import Any, Optional, Tuple
import numpy as np

from sutura_engine.xray import reload_strict_holes_nm, weld_reload_equivalent

P_WELD_MAX_NUDGE_ATTEMPTS = 12
P_WELD_MAX_HOLE = 200
P_WELD_MIN_FACE_FRACTION = 0.98


def _separate_weld_collisions(verts: np.ndarray, tris: np.ndarray) -> Tuple[np.ndarray, np.ndarray]:
    """Split vertices that coincide after float32 STL write via tiny float32 ULPs."""
    v64 = np.asarray(verts, dtype=np.float64).copy()
    t = np.asarray(tris, dtype=np.int64)
    if len(v64) == 0 or len(t) == 0:
        return v64.astype(np.float32), t
    v32 = v64.astype(np.float32)
    inv = np.asarray(np.unique(v32, axis=0, return_inverse=True)[1]).reshape(-1)
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
        if len(np.unique(d32, axis=0)) == len(d32):
            return d32, t
    return v32, t


def _repair_welded_topology(ml: Any, verts: np.ndarray, tris: np.ndarray) -> Tuple[np.ndarray, np.ndarray]:
    """Topology repair of a reload-equivalent mesh subset (P-FIX)."""
    if ml is None:
        return np.asarray(verts, np.float32), np.asarray(tris, np.int32)
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float32),
                        face_matrix=np.asarray(tris, np.int32)))
    for name, params in (('meshing_repair_non_manifold_edges', {}),
                         ('meshing_remove_duplicate_faces', {}),
                         ('meshing_repair_non_manifold_vertices', {}),
                         ('meshing_remove_unreferenced_vertices', {})):
        try:
            ms.apply_filter(name, **params)
        except Exception:
            pass
    return (np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float32),
            np.asarray(ms.current_mesh().face_matrix(), dtype=np.int32))


def _close_small_holes(ml: Any, verts: np.ndarray, tris: np.ndarray) -> Tuple[np.ndarray, np.ndarray]:
    """Close small boundary loops in P-WELD fallback."""
    if ml is None:
        return np.asarray(verts, np.float32), np.asarray(tris, np.int32)
    try:
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float64),
                            face_matrix=np.asarray(tris, np.int32)))
        ms.apply_filter('meshing_repair_non_manifold_vertices')
        ms.apply_filter('meshing_close_holes', maxholesize=P_WELD_MAX_HOLE)
        ms.apply_filter('meshing_remove_unreferenced_vertices')
        m = ms.current_mesh()
        return (np.asarray(m.vertex_matrix(), dtype=np.float32),
                np.asarray(m.face_matrix(), dtype=np.int32))
    except Exception:
        return np.asarray(verts, np.float32), np.asarray(tris, np.int32)


def p_weld_final(ml: Any, verts: np.ndarray, tris: np.ndarray) -> Tuple[np.ndarray, np.ndarray, Optional[dict]]:
    """Reload-safe final pass (P-WELD). Pure numpy split-collisions + optional VCG fallback."""
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
        try:
            from defects import detect as detect_defects
        except ImportError:
            from sutura.defects import detect as detect_defects
        d = detect_defects(v0, t0)
        if len(d['holes']) == 0 and len(d['non_manifold']) == 0:
            cv, ct = _separate_weld_collisions(v0, t0)
            ch, cnm = reload_strict_holes_nm(cv, ct)
            if ch == 0 and cnm == 0:
                nudged = int((np.asarray(cv, np.float32) != np.asarray(v0, np.float32)).any(axis=1).sum())
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
    except Exception:
        pass
    return v0, t0, rec
