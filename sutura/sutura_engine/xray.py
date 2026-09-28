# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""sutura_engine.xray - Reload-honest strict watertight checks (P-HONEST).

Validates meshes according to what an STL save/reload cycle produces:
STL drops topological vertex sharing, converting a mesh into disjoint float32
triangles. An index-watertight mesh can reload non-manifold if coincident float32
positions weld into non-manifold edges.

Provides:
  - weld_reload_equivalent: reconstruct the reload topology in pure numpy
  - reload_strict_holes_nm: defect count on the reload topology
  - _is_strict_watertight: zero holes and zero non-manifold edges after reload
  - enforce_reload_verdict: downgrade false watertight claims (P-HONEST)
"""
from typing import Tuple
import numpy as np


def weld_reload_equivalent(verts: np.ndarray, tris: np.ndarray) -> Tuple[np.ndarray, np.ndarray]:
    """Weld a mesh into the form an STL save/reload reproduces. Pure numpy."""
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int64)
    if len(v) == 0 or len(t) == 0:
        return v, t
    unique, inverse = np.unique(v, axis=0, return_inverse=True)
    inverse = np.asarray(inverse).reshape(-1)
    t = inverse[t.reshape(-1)].reshape(t.shape).astype(np.int64)
    nondeg = ((t[:, 0] != t[:, 1]) & (t[:, 1] != t[:, 2]) & (t[:, 0] != t[:, 2]))
    t = t[nondeg]
    if len(t) == 0:
        return np.zeros((0, 3), dtype=np.float32), t
    keys = np.sort(t, axis=1)
    _uniq, first = np.unique(keys, axis=0, return_index=True)
    t = t[np.sort(first)]
    used = np.unique(t)
    remap = np.full(len(unique), -1, dtype=np.int64)
    remap[used] = np.arange(len(used))
    return unique[used], remap[t]


def reload_strict_holes_nm(verts: np.ndarray, tris: np.ndarray) -> Tuple[int, int]:
    """Strict (holes, non-manifold) on the STL save/reload-equivalent mesh. Pure numpy."""
    wv, wt = weld_reload_equivalent(verts, tris)
    try:
        from defects import detect as detect_defects
    except ImportError:
        from sutura.defects import detect as detect_defects
    d = detect_defects(wv, wt)
    return len(d['holes']), len(d['non_manifold'])


def _is_strict_watertight(verts: np.ndarray, tris: np.ndarray) -> bool:
    """True if mesh has 0 holes and 0 non-manifold edges after reload weld."""
    h, nm = reload_strict_holes_nm(verts, tris)
    return (h == 0 and nm == 0)


def enforce_reload_verdict(report: dict, verts: np.ndarray, tris: np.ndarray) -> bool:
    """Never claim watertight for a mesh that is not strict-watertight on reload (P-HONEST)."""
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
