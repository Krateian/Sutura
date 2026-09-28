# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""sutura_engine.mirror - Mirror-plane completion for single-sided scans.

Legacy import path: ``sutura/mirror_repair.py`` re-exports this module.

Completes the missing back of a surface that was scanned from one side when the
object is (approximately) bilaterally symmetric about the plane that contains
the dominant open boundary loop:

  1. detect_mirror_plane: fit the dominant boundary loop's plane and score how
     planar and dominant that loop is (the cut of a symmetric half).
  2. mirror_close: reflect the visible surface across that plane (flipping the
     winding so the shared boundary edges cancel), weld the seam, and stitch
     the two halves with ``hull.extract_outer_shell`` when a plain merge is not
     yet watertight.
  3. Fall back to screened-Poisson reconstruction (``closing.poisson_close``,
     registry method #8) when the symmetry confidence is low or the mirrored
     stitch does not produce a valid solid.

Operates on numpy arrays (verts float64 Nx3, tris int Mx3). numpy + scipy only
at import time; pymeshlab / trimesh / manifold3d are imported lazily inside the
functions that need them, so the module stays importable anywhere.
"""
import numpy as np
from scipy.spatial import cKDTree

# Plane-fit RMS (relative to bbox diagonal) below which the dominant boundary
# loop counts as planar (a symmetric cut), and above which it does not.
PLANE_RMS_GOOD_REL = 0.01
PLANE_RMS_BAD_REL = 0.06

# Minimum symmetry confidence [0..1] required to attempt the mirror fill; below
# it the tier goes straight to the Poisson fallback.
MIRROR_MIN_SCORE = 0.35

# Vertex-weld tolerance for the mirror seam, relative to the bbox diagonal.
MIRROR_WELD_REL = 2.0e-4


def _referenced_only(verts, tris):
    """Drop unreferenced vertices and remap triangle indices."""
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    if len(tris) == 0:
        return np.zeros((0, 3), dtype=np.float64), np.zeros((0, 3), dtype=np.int64)
    used = np.unique(tris)
    remap = np.full(len(verts), -1, dtype=np.int64)
    remap[used] = np.arange(len(used))
    return verts[used], remap[tris]


def _closing_mod():
    """The shared closing helpers, imported lazily (shim-aware)."""
    try:
        from sutura_engine.methods import closing as _closing
        return _closing
    except Exception:
        import closing as _closing  # noqa: F401 - installed shim layout
        return _closing


def detect_mirror_plane(verts, tris):
    """Fit the dominant boundary loop's mirror plane.

    Returns ``(normal, center, score, info)`` or ``(None, None, 0.0, info)``.
    The score is the product of a loop-planarity factor and a loop-dominance
    factor: a single dominant, roughly planar boundary loop is the cut of a
    bilaterally symmetric half.
    """
    closing = _closing_mod()
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    info = {'n_loops': 0}
    if len(tris) == 0:
        info['reason'] = 'empty'
        return None, None, 0.0, info

    loops = closing.boundary_loops(verts, tris)
    info['n_loops'] = len(loops)
    if not loops:
        info['reason'] = 'closed'
        return None, None, 0.0, info

    perims = [closing._loop_stats(verts, l)[0] for l in loops]
    total_perim = float(sum(perims))
    if total_perim <= 1e-12:
        info['reason'] = 'zero_perimeter'
        return None, None, 0.0, info
    dom_idx = int(np.argmax(perims))
    loop = loops[dom_idx]
    dom_ratio = perims[dom_idx] / total_perim

    diag = float(np.linalg.norm(verts.max(axis=0) - verts.min(axis=0)))
    if diag <= 1e-12:
        info['reason'] = 'degenerate_bbox'
        return None, None, 0.0, info

    pts = verts[loop]
    c = np.mean(pts, axis=0)
    centered = pts - c
    _, _, vh = np.linalg.svd(centered)
    n = vh[2]
    rms = float(np.sqrt(np.mean(np.dot(centered, n) ** 2)))
    rms_rel = rms / diag

    # Planarity factor: 1 at PLANE_RMS_GOOD_REL, 0 at PLANE_RMS_BAD_REL.
    s_planar = float(np.clip(
        (PLANE_RMS_BAD_REL - rms_rel) / (PLANE_RMS_BAD_REL - PLANE_RMS_GOOD_REL),
        0.0, 1.0))
    # Dominance factor (same ramp as closing.single_side_score).
    s_dom = float(np.clip((dom_ratio - 0.40) / 0.45, 0.0, 1.0))
    score = round(s_planar * s_dom, 4)
    info.update({
        'reason': 'ok',
        'dominant_ratio': round(dom_ratio, 4),
        'rms_rel': round(rms_rel, 5),
        'loop_len': len(loop),
        'normal': n.tolist(),
        'center': c.tolist(),
        'score': score,
    })
    if score <= 0.0:
        return None, None, 0.0, info
    return n, c, score, info


def _weld(verts, tris, tol):
    """Weld vertices whose rounded coordinates coincide (seam stitching)."""
    if tol <= 0:
        return verts, tris
    keys = np.round(verts / tol).astype(np.int64)
    # unique(axis=0) sorts the keys; return_index gives the first occurrence of
    # each unique key and return_inverse maps every vertex into that array.
    _uniq, first_pos, inverse = np.unique(
        keys, axis=0, return_index=True, return_inverse=True)
    new_verts = verts[first_pos]
    new_tris = inverse[tris]
    return new_verts, new_tris


def _repair_topology(ml, verts, tris):
    """Best-effort pymeshlab repair of a stitched surface (never raises)."""
    try:
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float64),
                            face_matrix=np.asarray(tris, np.int32)))
        ms.apply_filter('meshing_repair_non_manifold_edges')
        ms.apply_filter('meshing_repair_non_manifold_vertices')
        ms.apply_filter('meshing_close_holes', maxholesize=10000)
        cur = ms.current_mesh()
        return (np.asarray(cur.vertex_matrix(), dtype=np.float64),
                np.asarray(cur.face_matrix(), dtype=np.int32))
    except Exception:
        return verts, tris


def _try_shell_wrap(verts, tris):
    """Union overlapping mirror halves into one outer shell (best effort)."""
    try:
        from sutura_engine.hull import extract_outer_shell
        sv, st = extract_outer_shell(verts, tris)
        if st is not None and len(st) > 0:
            return np.asarray(sv, dtype=np.float64), np.asarray(st, dtype=np.int64)
    except Exception:
        pass
    return verts, tris


def mirror_close(verts, tris, tmpdir=None, min_score=MIRROR_MIN_SCORE):
    """Complete a single-sided surface by mirroring it across its symmetry plane.

    Returns ``(out_verts, out_tris, report)``. Falls back to Poisson
    reconstruction when the plane is not confident or the mirrored result is
    not a valid solid; the report's ``mode`` says which path produced it.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    base = {'watertight': False, 'faces': len(tris), 'mode': 'none',
            'mirror_score': 0.0, 'notes': [], 'error': None}

    n, c, score, info = detect_mirror_plane(verts, tris)
    base['mirror_score'] = float(score)
    base['plane'] = info
    if n is None or score < min_score:
        return _poisson_fallback(verts, tris, base, tmpdir,
                                 'low mirror confidence (%.2f < %.2f)'
                                 % (score, min_score))

    diag = float(np.linalg.norm(verts.max(axis=0) - verts.min(axis=0)))
    in_v, in_t = _referenced_only(verts, tris)

    # Reflect the surface across the plane and flip the winding so the shared
    # boundary edges (which lie on the plane) cancel and the seam closes.
    proj = np.dot(in_v - c, n)
    refl_v = in_v - 2.0 * proj[:, None] * n
    refl_t = in_t[:, ::-1]

    merged_v = np.vstack([in_v, refl_v])
    merged_t = np.vstack([in_t, refl_t + len(in_v)])
    merged_v, merged_t = _weld(merged_v, merged_t, MIRROR_WELD_REL * diag)

    closing = _closing_mod()
    ok, reason = closing._check_validity(merged_v, merged_t)
    if not ok:
        # Overlapping / non-coincident halves: fuse them into an outer shell.
        wrapped_v, wrapped_t = _try_shell_wrap(merged_v, merged_t)
        if not (np.array_equal(wrapped_v, merged_v)
                and np.array_equal(wrapped_t, merged_t)):
            merged_v, merged_t = wrapped_v, wrapped_t
        try:
            import pymeshlab as ml
            merged_v, merged_t = _repair_topology(ml, merged_v, merged_t)
        except Exception:
            pass
        ok, reason = closing._check_validity(merged_v, merged_t)

    if not ok:
        return _poisson_fallback(verts, tris, base, tmpdir,
                                 'mirrored stitch not watertight: %s' % reason)

    return (merged_v.astype(np.float64), merged_t.astype(np.int32),
            {'watertight': True, 'faces': int(len(merged_t)), 'mode': 'mirror',
             'mirror_score': float(score), 'plane': info,
             'notes': ['back surface completed by mirrored geometry'],
             'error': None})


def _poisson_fallback(verts, tris, base, tmpdir, reason):
    """Screened-Poisson fallback (method #8) for mirror completion."""
    try:
        closing = _closing_mod()
        out_v, out_t, rep = closing.poisson_close(
            np.asarray(verts, dtype=np.float64),
            np.asarray(tris, dtype=np.int64), tmpdir=tmpdir)
    except Exception as e:  # noqa: BLE001 - a tier never crashes a repair
        base['mode'] = 'poisson_fallback'
        base['error'] = str(e)
        base['notes'] = [reason]
        return verts, tris, base
    base.update({
        'watertight': bool(rep.get('watertight')),
        'faces': int(len(out_t)),
        'mode': 'poisson_fallback',
        'error': rep.get('error'),
        'notes': [reason] + list(rep.get('notes') or []),
    })
    return out_v, out_t, base
