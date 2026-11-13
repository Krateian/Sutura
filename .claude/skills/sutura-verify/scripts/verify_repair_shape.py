#!/usr/bin/env python3
"""verify_repair_shape.py - independent shape verifier for a repaired mesh.

Given an input mesh and a repaired output this reports the shape facts that
decide whether the repair is acceptable, WITHOUT trusting the repair tool's
own JSON claims.  It is the measurement half of the sutura-verify skill; the
repair itself is run by the caller (or by verify_default_vs_optout.py).

Reported, all recomputed from the files on disk:

  * reload topology   - boundary holes, non-manifold edges, vertex-connected
                        components and fully closed shells of the OUTPUT,
                        after an exact float32 weld (the reload-equivalent
                        weld the pipeline's P-HONEST verdict uses;
                        ``defects.detect``).
  * Orca verdict      - ``number_of_parts`` and ``manifold`` from a headless
                        ``--info`` run, when an OrcaSlicer binary is found.
  * two-sided distance- max / p95 of the OUTPUT vertices to the INPUT surface
                        and the INPUT vertices to the OUTPUT surface
                        (``sutura_geom.closest_points``).
  * outside leak      - added output vertices that lie outside the input solid
                        (generalized winding < 0.5) farther than the tolerance.
  * healthy faces     - fraction of INPUT triangles carried through verbatim.
  * tolerance         - ``clamp(bbox_diag / 10000, 0.0125, 0.05)`` mm, the same
                        gate the Tamponade plug acceptance uses.

Runtime is the caller's responsibility; this tool times nothing.

Dependencies: numpy always; ``sutura_geom`` for distance / leak / exact SI;
``defects`` (numpy-only) from the ``sutura/`` source tree for reload topology.
PySide6 is needed only for ``--render``.

Usage:
  verify_repair_shape.py INPUT OUTPUT [--out-dir DIR] [--render]
                        [--crop X0 Y0 Z0 X1 Y1 Z1] [--repo DIR] [--tag TAG]
"""
import argparse
import json
import os
import shutil
import struct
import subprocess
import sys

import numpy as np

ORANGE = (255, 140, 60)


# --------------------------------------------------------------------------- #
# repo wiring (repo-relative, no hard-coded paths)
# --------------------------------------------------------------------------- #
def default_repo():
    """Walk up from this file until a directory holding ``sutura/repair.py``."""
    here = os.path.dirname(os.path.abspath(__file__))
    for _ in range(8):
        if os.path.exists(os.path.join(here, 'sutura', 'repair.py')):
            return here
        parent = os.path.dirname(here)
        if parent == here:
            break
        here = parent
    return os.getcwd()


def wire_repo(repo):
    """Put the repo's ``sutura/`` source tree and root on ``sys.path``."""
    for p in (os.path.join(repo, 'sutura'), repo):
        if p not in sys.path:
            sys.path.insert(0, p)
    return repo


# --------------------------------------------------------------------------- #
# STL loading (raw triangles, float32) + exact weld
# --------------------------------------------------------------------------- #
def read_stl_raw(path):
    """Return raw triangles as an (M, 3, 3) float32 array (no vertex weld)."""
    size = os.path.getsize(path)
    with open(path, 'rb') as fh:
        head = fh.read(84)
    if size >= 84:
        n = struct.unpack('<I', head[80:84])[0]
        if 84 + 50 * n == size:
            dt = np.dtype([('n', '<f4', 3), ('v', '<f4', (3, 3)), ('a', '<u2')])
            arr = np.fromfile(path, dtype=dt, count=n, offset=84)
            return np.ascontiguousarray(arr['v'].astype(np.float32))
    # ASCII fallback
    verts = []
    tris = []
    with open(path, 'r', errors='replace') as fh:
        for line in fh:
            line = line.strip()
            if line.startswith('vertex'):
                verts.append([float(x) for x in line.split()[1:4]])
            elif line.startswith('endfacet'):
                if len(verts) >= 3:
                    tris.append(verts[-3:])
    return np.asarray(tris, dtype=np.float32)


def tri_keys(tris):
    """Canonical cyclic-rotation key (tuple of 3 vertex byte strings) per face."""
    raw = np.ascontiguousarray(tris, dtype='<f4').view(np.uint8).reshape(len(tris), 3, 12)
    keys = []
    for t in raw:
        v0, v1, v2 = t[0].tobytes(), t[1].tobytes(), t[2].tobytes()
        keys.append(min((v0, v1, v2), (v1, v2, v0), (v2, v0, v1)))
    return keys


def weld_exact(tris_raw):
    """Reload-equivalent weld: cast to float32, merge exact coincident positions.

    Keeps every input face (no degeneracy/duplicate dropping) so output face
    indices stay aligned with the raw triangles.
    """
    flat = np.asarray(tris_raw, dtype=np.float32).reshape(-1, 3)
    if len(flat) == 0:
        return np.zeros((0, 3), np.float64), np.zeros((0, 3), np.int64)
    verts, inv = np.unique(flat, axis=0, return_inverse=True)
    tris = inv.reshape(-1, 3).astype(np.int64)
    return verts.astype(np.float64), tris


def _vertex_ids(tris):
    """Map each float32 vertex to an integer id; returns (ids_per_face, lut)."""
    v = np.ascontiguousarray(tris, dtype='<f4').view(np.uint8).reshape(-1, 12)
    uniq, inv = np.unique(v, axis=0, return_inverse=True)
    lut = {uniq[i].tobytes(): i for i in range(len(uniq))}
    return inv.reshape(-1, 3).astype(np.int64), lut


def boundary_edges(tris):
    """Input edges used by exactly one face, as (min_id, max_id) tuples."""
    ids, _ = _vertex_ids(tris)
    e = np.concatenate([ids[:, [0, 1]], ids[:, [1, 2]], ids[:, [2, 0]]], axis=0)
    e = np.sort(e, axis=1)
    uniq, counts = np.unique(e, axis=0, return_counts=True)
    return {(int(a), int(b)) for a, b in uniq[counts == 1]}


def shell_volumes(verts, tris):
    """Absolute signed volume of each connected component, largest first."""
    if len(tris) == 0:
        return []
    n = len(verts)
    parent = np.arange(n)

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    for f in tris:
        for u, w in ((f[0], f[1]), (f[1], f[2]), (f[2], f[0])):
            ru, rw = find(int(u)), find(int(w))
            if ru != rw:
                parent[ru] = rw
    from collections import defaultdict
    comp = defaultdict(list)
    for f in tris:
        comp[find(int(f[0]))].append(f)
    out = []
    for faces in comp.values():
        ft = np.asarray(faces, dtype=np.int64)
        a = verts[ft[:, 0]]
        b = verts[ft[:, 1]]
        c = verts[ft[:, 2]]
        out.append(abs(float(np.einsum('ij,ij->i', a, np.cross(b, c)).sum() / 6.0)))
    out.sort(reverse=True)
    return [round(v, 6) for v in out]

def topology_counts(verts, tris):
    """Vectorised (components, closed_shells) of a welded mesh."""
    if len(tris) == 0:
        return 0, 0
    n = len(verts)
    parent = np.arange(n)

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    for a, b, c in tris:
        for u, w in ((a, b), (b, c), (c, a)):
            ru, rw = find(int(u)), find(int(w))
            if ru != rw:
                parent[ru] = rw
    from collections import defaultdict
    comp = defaultdict(list)
    for f in tris:
        comp[find(int(f[0]))].append(f)
    closed = 0
    for faces in comp.values():
        ec = defaultdict(int)
        for f in faces:
            for x, y in ((f[0], f[1]), (f[1], f[2]), (f[2], f[0])):
                ec[(min(int(x), int(y)), max(int(x), int(y)))] += 1
        if all(c == 2 for c in ec.values()):
            closed += 1
    return len(comp), closed


# --------------------------------------------------------------------------- #
# exact self-intersection (sutura_geom)
# --------------------------------------------------------------------------- #
def exact_si(verts, tris):
    """Return (count_or_None, note). count None = classifier skipped."""
    try:
        import sutura_geom
    except Exception as e:  # noqa: BLE001
        return None, 'sutura_geom unavailable: %s' % e
    if len(tris) == 0:
        return 0, 'empty'
    try:
        _mask, count = sutura_geom.self_intersecting_faces(
            np.asarray(verts, np.float64), np.asarray(tris, np.int32))
        if count < 0:
            return None, 'skipped (face cap)'
        return int(count), None
    except Exception as e:  # noqa: BLE001
        return None, 'si failed: %s' % e


def gwn_union_volume(verts, tris, samples=12000, seed=0, chunk=16):
    """Monte-Carlo GWN union volume (inside fraction x bbox volume).

    Summed signed volume double-counts overlapping bodies; the GWN membership
    fraction is the union volume. Fixed seed and small chunks keep it
    deterministic and memory-bounded. Uses the native PyMeshBvh when present.
    """
    v = np.asarray(verts, np.float64)
    t = np.asarray(tris, np.int64)
    if len(v) == 0 or len(t) == 0:
        return 0.0
    lo, hi = v.min(0), v.max(0)
    bbox_vol = float(np.prod(np.maximum(hi - lo, 1e-12)))
    rng = np.random.default_rng(int(seed))
    pts = lo + (hi - lo) * rng.random((int(samples), 3))
    try:
        import sutura_geom
        bvh = sutura_geom.PyMeshBvh(v, t.astype(np.int32))
        w = np.concatenate([
            np.asarray(bvh.winding_points(pts[s:s + chunk]))
            for s in range(0, len(pts), chunk)
        ])
    except Exception:  # noqa: BLE001 - numpy oracle fallback
        A, B, C = v[t[:, 0]], v[t[:, 1]], v[t[:, 2]]
        w = np.zeros(len(pts), np.float64)
        for s in range(0, len(pts), chunk):
            p = pts[s:s + chunk, None, :]
            a, b, c = A[None] - p, B[None] - p, C[None] - p
            la = np.linalg.norm(a, axis=2)
            lb = np.linalg.norm(b, axis=2)
            lc = np.linalg.norm(c, axis=2)
            det = np.einsum('ijk,ijk->ij', a, np.cross(b, c))
            den = (la * lb * lc + np.einsum('ijk,ijk->ij', a, b) * lc
                   + np.einsum('ijk,ijk->ij', b, c) * la
                   + np.einsum('ijk,ijk->ij', c, a) * lb)
            w[s:s + chunk] = np.arctan2(det, den).sum(1) / (2.0 * np.pi)
    inside = np.asarray(w) > 0.5
    return float(inside.mean() * bbox_vol)


def submesh(verts, tris, face_idx):
    """Build a standalone welded submesh from a set of face indices."""
    if len(face_idx) == 0:
        return np.zeros((0, 3)), np.zeros((0, 3), np.int64)
    sub = np.asarray(tris)[np.asarray(face_idx, np.int64)]
    return weld_exact(verts[sub])


# --------------------------------------------------------------------------- #
# distance / leak
# --------------------------------------------------------------------------- #
def _face_areas(vi, ti):
    tri = np.asarray(vi, dtype=np.float64)[np.asarray(ti, dtype=np.int64)]
    return 0.5 * np.linalg.norm(
        np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0]), axis=1)


def _buried_input_mask(vi, ti, defect_face_mask=None, delta=0.1):
    """Input-alone interior-wall mask from ``sutura_geom.buried_face_mask``.

    Returns ``(mask, info, areas)``.  The mask is already zero-area-free (the
    kernel skips those faces); the defect mask is applied here because it lives
    in the Python pipeline, not the kernel.  Debris and cavity walls are never
    in the mask.
    """
    ti = np.asarray(ti, dtype=np.int64)
    areas = _face_areas(vi, ti)
    info = {'buried_mask_available': False,
            'buried_faces_input': 0, 'buried_area_input_mm2': 0.0,
            'buried_faces_removed': 0, 'buried_area_mm2': 0.0,
            'buried_debris_faces': 0, 'buried_degenerate_faces': 0,
            'buried_epsilon_mm': 0.0}
    mask = np.zeros(len(ti), dtype=bool)
    try:
        import sutura_geom
        if not hasattr(sutura_geom, 'buried_face_mask'):
            info['buried_note'] = 'sutura_geom lacks buried_face_mask'
            return mask, info, areas
        m, d = sutura_geom.buried_face_mask(
            np.asarray(vi, dtype=np.float64), ti.astype(np.int32), float(delta))
        mask = np.asarray(m, dtype=bool)
        info.update({
            'buried_mask_available': True,
            'buried_debris_faces': int(d.get('debris_count', 0)),
            'buried_degenerate_faces': int(d.get('degenerate_count', 0)),
            'buried_epsilon_mm': round(float(d.get('epsilon_mm', 0.0)), 6),
        })
    except Exception as e:  # noqa: BLE001
        info['buried_note'] = 'buried_face_mask failed: %s' % e
        return mask, info, areas
    if defect_face_mask is not None:
        mask &= ~np.asarray(defect_face_mask, dtype=bool)
    info['buried_faces_input'] = int(np.count_nonzero(mask))
    info['buried_area_input_mm2'] = round(float(areas[mask].sum()), 6)
    return mask, info, areas


INTERIOR_WALLS_SKIPPED_NOTE = 'interior walls not computed (large mesh)'


def _interior_wall_gate():
    """The product's own interior-wall face threshold, or None if unavailable.

    Imported from ``sutura/fidelity.py``'s budget config so the verifier and the
    product cannot drift apart: raising or lowering the product's gate moves the
    verifier with it, with no second number to keep in sync.
    """
    try:
        import fidelity
        budgets = ((fidelity.load_config().get('fidelity') or {})
                   .get('budgets') or {})
        v = budgets.get('max_faces')
        return int(v) if v is not None else None
    except Exception:  # noqa: BLE001
        return None


def two_sided_distance(vi, ti, vo, to, tol=None, defect_face_mask=None,
                       skip_buried_mask=False):
    """Symmetric vertex-to-surface distance (mm) between input and output.

    ``i2o_*`` and ``two_sided_*`` are the **visible** values: internal walls of
    the input's own solid union (Case B, input alone) are excluded and reported
    separately.  ``i2o_all_*`` / ``two_sided_all_*`` keep the old all-face
    values.  A visible input face is scored at its worst vertex, so partially
    lost visible skin is never hidden.
    """
    empty = {'o2i_max': 0.0, 'o2i_p95': 0.0, 'i2o_max': 0.0,
             'i2o_p95': 0.0, 'i2o_all_max': 0.0, 'i2o_all_p95': 0.0,
             'two_sided_max': 0.0, 'two_sided_p95': 0.0,
             'two_sided_all_max': 0.0, 'two_sided_all_p95': 0.0,
             'buried_mask_available': False, 'buried_faces_input': 0,
             'buried_area_input_mm2': 0.0, 'buried_faces_removed': 0,
             'buried_area_mm2': 0.0, 'buried_debris_faces': 0,
             'buried_degenerate_faces': 0, 'buried_epsilon_mm': 0.0,
             'buried_mask': np.zeros(0, dtype=bool)}
    if len(ti) == 0 or len(to) == 0 or len(vi) == 0 or len(vo) == 0:
        return empty
    try:
        import sutura_geom
        _x, d_o2i, _f = sutura_geom.closest_points(vi, ti.astype(np.int32), vo)
        _x, d_i2o, _f = sutura_geom.closest_points(vo, to.astype(np.int32), vi)
    except Exception:  # noqa: BLE001
        return empty
    d_o2i = np.asarray(d_o2i, dtype=np.float64)
    d_i2o = np.asarray(d_i2o, dtype=np.float64)
    ti = np.asarray(ti, dtype=np.int64)

    if tol is None:
        diag = float(np.linalg.norm(np.asarray(vi).max(0)
                                    - np.asarray(vi).min(0)))
        tol = min(max(diag / 10000.0, 0.0125), 0.05)
    if skip_buried_mask:
        # Large mesh: the product does not compute interior walls either, so the
        # visible values fall back to the all-face ones.  The mask pass is NOT
        # run at all -- that is the point -- and the reason is reported in the
        # product's own words.
        areas = _face_areas(vi, ti)
        buried = np.zeros(len(ti), dtype=bool)
        info = {'buried_mask_available': False,
                'buried_faces_input': 0, 'buried_area_input_mm2': 0.0,
                'buried_faces_removed': 0, 'buried_area_mm2': 0.0,
                'buried_debris_faces': 0, 'buried_degenerate_faces': 0,
                'buried_epsilon_mm': 0.0,
                'buried_note': INTERIOR_WALLS_SKIPPED_NOTE}
    else:
        buried, info, areas = _buried_input_mask(
            vi, ti, defect_face_mask=defect_face_mask)
    nonzero = areas > 1e-12
    if info['buried_faces_input']:
        face_d = d_i2o[ti[buried]].max(axis=1)
        removed = face_d > tol
        info['buried_faces_removed'] = int(np.count_nonzero(removed))
        info['buried_area_mm2'] = round(float(areas[buried][removed].sum()), 6)
    all_i2o_max = float(np.max(d_i2o)) if len(d_i2o) else 0.0
    all_i2o_p95 = (float(np.percentile(d_i2o, 95)) if len(d_i2o) else 0.0)
    # With no interior walls (or no kernel mask) the visible values are
    # byte-for-byte the old all-face values; the split only changes meshes
    # that actually have buried faces.
    if not info['buried_mask_available'] or not info['buried_faces_input']:
        i2o_vis_max = all_i2o_max
        i2o_vis_p95 = all_i2o_p95
        vis_pool = d_i2o
    else:
        visible = nonzero & ~buried
        if visible.any():
            vis_d = d_i2o[ti[visible]].max(axis=1)
            i2o_vis_max = float(np.max(vis_d))
            i2o_vis_p95 = float(np.percentile(vis_d, 95))
            vis_pool = d_i2o[ti[visible]].ravel()
        else:
            i2o_vis_max = 0.0
            i2o_vis_p95 = 0.0
            vis_pool = np.zeros(0, dtype=np.float64)
    all_d = np.concatenate([d_o2i, d_i2o]) if (len(d_o2i) and len(d_i2o)) \
        else (d_o2i if len(d_o2i) else d_i2o)
    two_pool = np.concatenate([d_o2i, vis_pool]) if (len(d_o2i) and len(vis_pool)) \
        else (d_o2i if len(d_o2i) else vis_pool)
    o2i_max = float(np.max(d_o2i)) if len(d_o2i) else 0.0
    o2i_p95 = float(np.percentile(d_o2i, 95)) if len(d_o2i) else 0.0
    result = {
        'o2i_max': round(o2i_max, 4),
        'o2i_p95': round(o2i_p95, 4),
        'i2o_max': round(i2o_vis_max, 4),
        'i2o_p95': round(i2o_vis_p95, 4),
        'i2o_all_max': round(all_i2o_max, 4),
        'i2o_all_p95': round(all_i2o_p95, 4),
        'two_sided_max': round(max(o2i_max, i2o_vis_max), 4),
        'two_sided_p95': (round(float(np.percentile(two_pool, 95)), 4)
                          if len(two_pool) else 0.0),
        'two_sided_all_max': round(max(o2i_max, all_i2o_max), 4),
        'two_sided_all_p95': (round(float(np.percentile(all_d, 95)), 4)
                              if len(all_d) else 0.0),
        'buried_mask': buried,
    }
    result.update(info)
    return result


def leak_metric(in_raw, out_raw, vi, ti, tol):
    """Added output vertices outside the input solid, farther than ``tol`` (mm)."""
    try:
        import sutura_geom
    except Exception:  # noqa: BLE001
        return {'leak_max': 0.0, 'leak_count': 0}
    in_keys = set(tri_keys(in_raw))
    added = [j for j, k in enumerate(tri_keys(out_raw)) if k not in in_keys]
    if len(added) == 0 or len(ti) == 0:
        return {'leak_max': 0.0, 'leak_count': 0}
    av, _at = weld_exact(out_raw[np.asarray(added, np.int64)])
    if len(av) == 0:
        return {'leak_max': 0.0, 'leak_count': 0}
    bvh = sutura_geom.PyMeshBvh(vi, ti.astype(np.int32))
    w = np.asarray(bvh.winding_points(av))
    _x, dist, _f = sutura_geom.closest_points(vi, ti.astype(np.int32), av)
    dist = np.asarray(dist, dtype=np.float64)
    leak = (w < 0.5) & (dist > tol)
    return {'leak_max': round(float(np.max(dist[leak])), 4) if leak.any() else 0.0,
            'leak_count': int(np.count_nonzero(leak))}


# --------------------------------------------------------------------------- #
# OrcaSlicer headless check
# --------------------------------------------------------------------------- #
def _find_orca():
    env = os.environ.get('SUTURA_ORCA')
    if env and os.path.exists(env):
        return env
    mac = ('/Applications/OrcaSlicer.app/Contents/MacOS/OrcaSlicer',
           os.path.expanduser('~/Applications/OrcaSlicer.app/Contents/MacOS/OrcaSlicer'))
    for p in mac:
        if os.path.exists(p):
            return p
    for name in ('orca-slicer', 'OrcaSlicer', 'orcaslicer'):
        w = shutil.which(name)
        if w:
            return w
    return None


def orca_info(path):
    """Headless OrcaSlicer ``--info`` on the output, when available."""
    orca = _find_orca()
    if not orca:
        return {'available': False, 'reason': 'OrcaSlicer not found',
                'number_of_parts': None, 'manifold': 'unknown'}
    try:
        p = subprocess.run([orca, '--info', path], capture_output=True,
                           text=True, timeout=600)
    except Exception as e:  # noqa: BLE001
        return {'available': True, 'ok': False, 'reason': str(e),
                'number_of_parts': None, 'manifold': 'unknown'}
    out = (p.stdout or '') + (p.stderr or '')
    tail = '\n'.join(out.strip().splitlines()[-25:])
    import re
    m = re.search(r'number_of_parts\s*=\s*(\d+)', tail)
    parts = int(m.group(1)) if m else None
    manifold = ('yes' if 'manifold = yes' in tail
                else 'no' if 'manifold = no' in tail else 'unknown')
    return {'available': True, 'ok': p.returncode == 0, 'returncode': p.returncode,
            'number_of_parts': parts, 'manifold': manifold, 'raw_tail': tail}


# --------------------------------------------------------------------------- #
# optional rendering (PySide6 + heatmap; skipped silently when absent)
# --------------------------------------------------------------------------- #
def _render_pair(in_raw, out_raw, added_keys, crop, out_dir, tag, w=720, h=540):
    from PySide6.QtGui import QGuiApplication
    if QGuiApplication.instance() is None:
        os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')
        QGuiApplication(sys.argv[:1])
    from heatmap import shared_frame, render

    def crop_faces(v, t):
        if crop is None:
            return np.arange(len(t))
        lo, hi = np.asarray(crop[:3]), np.asarray(crop[3:])
        keep = np.where(np.all((v >= lo) & (v <= hi), axis=1)[t].any(axis=1))[0]
        return keep

    iv, it = weld_exact(in_raw)
    ov, ot = weld_exact(out_raw)
    ik, ok = crop_faces(iv, it), crop_faces(ov, ot)
    if len(ik) == 0 and len(ok) == 0:
        return None, None
    in_sv, in_st = submesh(iv, it, ik)
    out_sv, out_st = submesh(ov, ot, ok)
    frame = shared_frame([in_sv, out_sv], w, h, 24)
    img_in = render(in_sv, in_st, w=w, h=h, frame=frame)
    sub_added = np.array([k in added_keys for k in
                          tri_keys(out_sv[out_st].astype(np.float32))], dtype=bool)
    fc = np.tile(np.array([178, 186, 194], float), (len(out_st), 1))
    fc[sub_added] = ORANGE
    img_out = render(out_sv, out_st, w=w, h=h, frame=frame, face_colors=fc)
    os.makedirs(out_dir, exist_ok=True)
    p_in = os.path.join(out_dir, '%s_input.png' % tag)
    p_out = os.path.join(out_dir, '%s_output.png' % tag)
    return (p_in if img_in.save(p_in, 'PNG') else None,
            p_out if img_out.save(p_out, 'PNG') else None)


# --------------------------------------------------------------------------- #
# main
# --------------------------------------------------------------------------- #
def verify(in_path, out_path, crop=None, out_dir=None, do_render=False,
           tag=None, repo=None, surface=True, surface_budget='sweep'):
    wire_repo(repo or default_repo())
    in_raw = read_stl_raw(in_path)
    out_raw = read_stl_raw(out_path)
    if tag is None:
        tag = os.path.splitext(os.path.basename(in_path))[0]

    vi, ti = weld_exact(in_raw)
    vo, to = weld_exact(out_raw)
    if len(ti) == 0 or len(to) == 0:
        # An unreadable or empty mesh would otherwise flow through as zero
        # holes / zero distance, i.e. metrics that look perfect. Fail loudly
        # instead of producing a flattering row.
        raise ValueError(
            'mesh has no triangles (input=%d, output=%d); unreadable or empty '
            'STL: %s' % (len(ti), len(to), in_path))
    bbox_diag = float(np.linalg.norm(vi.max(0) - vi.min(0))) if len(vi) else 1.0
    tol = min(max(bbox_diag / 10000.0, 0.0125), 0.05)

    in_keys = tri_keys(in_raw)
    in_set = set(in_keys)
    out_set = set(tri_keys(out_raw))
    healthy = sum(1 for k in in_keys if k in out_set)
    added = [j for j, k in enumerate(tri_keys(out_raw)) if k not in in_set]

    from defects import detect
    in_rep = detect(vi, ti, with_indices=True)
    rep = detect(vo, to)
    comp, closed = topology_counts(vo, to)
    si_out, si_note = exact_si(vo, to)
    si_in, _si_in_note = exact_si(vi, ti)
    shell_vols = shell_volumes(vo, to)
    vol_in = gwn_union_volume(vi, ti)
    vol_out = gwn_union_volume(vo, to)
    vol_err = abs(vol_out - vol_in) / max(abs(vol_in), 1e-12)
    try:
        import surface_metrics as _sm
        defect_mask = _sm.defect_face_mask(ti, in_rep)
    except Exception:  # noqa: BLE001
        defect_mask = None
    # Mirror the product: sutura/fidelity.py skips the interior-wall mask above
    # its own `budgets.max_faces` (150k today) and reports the note
    # 'healthy mask skipped (large mesh)'. The verifier follows the same gate so
    # it never pays a per-face winding pass the product itself refuses to run.
    # The threshold is READ from the product's config, not copied.
    mask_gate = _interior_wall_gate()
    walls_skipped = mask_gate is not None and max(len(ti), len(to)) > mask_gate
    if walls_skipped:
        dist = two_sided_distance(vi, ti, vo, to, tol=tol,
                                  defect_face_mask=defect_mask,
                                  skip_buried_mask=True)
    else:
        dist = two_sided_distance(vi, ti, vo, to, tol=tol,
                                  defect_face_mask=defect_mask)
    leak = leak_metric(in_raw, out_raw, vi, ti, tol)
    orca = orca_info(out_path)

    buried_mask = dist.pop('buried_mask', None)
    if buried_mask is None or len(buried_mask) != len(in_keys):
        buried_mask = np.zeros(len(in_keys), dtype=bool)
    in_areas = _face_areas(vi, ti)
    visible = (in_areas > 1e-12) & ~buried_mask
    present = np.array([k in out_set for k in in_keys], dtype=bool)
    healthy_visible = int(np.count_nonzero(present & visible))
    healthy_visible_pct = round(
        100.0 * healthy_visible / max(int(np.count_nonzero(visible)), 1), 3)

    result = {
        'tag': tag,
        'input': in_path,
        'output': out_path,
        'bbox_diag_mm': round(bbox_diag, 4),
        'tolerance_mm': round(tol, 4),
        'input_faces': len(in_raw),
        'output_faces': len(out_raw),
        'healthy_faces': healthy,
        'healthy_faces_pct': round(100.0 * healthy / max(len(in_raw), 1), 3),
        'healthy_faces_visible': healthy_visible,
        'healthy_faces_visible_pct': healthy_visible_pct,
        'added_faces': len(added),
        'reload_holes': len(rep['holes']),
        'reload_non_manifold_edges': len(rep['non_manifold']),
        'components': comp,
        'closed_shells': closed,
        'orca_number_of_parts': orca['number_of_parts'],
        'orca_manifold': orca['manifold'],
        'orca_available': orca['available'],
        'two_sided_max_mm': dist['two_sided_max'],
        'two_sided_p95_mm': dist['two_sided_p95'],
        'two_sided_max_all_mm': dist['two_sided_all_max'],
        'two_sided_p95_all_mm': dist['two_sided_all_p95'],
        'o2i_max_mm': dist['o2i_max'],
        'o2i_p95_mm': dist['o2i_p95'],
        'i2o_max_mm': dist['i2o_max'],
        'i2o_p95_mm': dist['i2o_p95'],
        'i2o_max_all_mm': dist['i2o_all_max'],
        'i2o_p95_all_mm': dist['i2o_all_p95'],
        'buried_mask_available': dist['buried_mask_available'],
        'buried_note': dist.get('buried_note'),
        'buried_faces_input': dist['buried_faces_input'],
        'buried_area_input_mm2': dist['buried_area_input_mm2'],
        'buried_faces_removed': dist['buried_faces_removed'],
        'buried_area_mm2': dist['buried_area_mm2'],
        'buried_debris_faces': dist['buried_debris_faces'],
        'buried_degenerate_faces': dist['buried_degenerate_faces'],
        'buried_epsilon_mm': dist['buried_epsilon_mm'],
        'leak_max_mm': leak['leak_max'],
        'leak_count': leak['leak_count'],
        'si_output': si_out,
        'si_output_note': si_note,
        'input_si': si_in,
        'shell_volumes': shell_vols,
        'input_gwn_volume_mm3': round(vol_in, 4),
        'output_gwn_volume_mm3': round(vol_out, 4),
        'volume_error_rel': round(vol_err, 9),
    }

    if surface:
        try:
            import surface_metrics as sm
            # Reuse the interior-wall mask already computed above instead of
            # paying for a second per-face winding pass over the whole surface.
            # Keys are translated to surface_metrics' naming; the mask itself is
            # byte-identical (same kernel call, same delta, same defect ring).
            _buried = None
            if not walls_skipped and buried_mask is not None \
                    and len(buried_mask) == len(ti):
                _buried = (buried_mask, {
                    'buried_input_available': bool(dist.get('buried_mask_available')),
                    'buried_input_faces': int(np.count_nonzero(buried_mask)),
                    'buried_input_area_mm2': round(float(
                        (in_areas[buried_mask]).sum()), 6) if len(in_areas) == len(ti) else 0.0,
                    'buried_input_debris_faces': int(dist.get('buried_debris_faces') or 0),
                    'buried_input_degenerate_faces': int(
                        dist.get('buried_degenerate_faces') or 0),
                    'buried_input_epsilon_mm': float(dist.get('buried_epsilon_mm') or 0.0),
                    'buried_input_note': None,
                })
            result.update(sm.measure(vi, ti, vo, to, in_rep,
                                     budget=surface_budget, buried=_buried,
                                     skip_buried_mask=walls_skipped))
        except Exception as e:  # noqa: BLE001
            result['surface_metrics_note'] = 'surface metrics failed: %s' % e

    if do_render:
        try:
            added_keys = set(tri_keys(out_raw)[j] for j in added)
            box = crop
            if box is None and len(added):
                pts = out_raw[np.asarray(added, np.int64)].reshape(-1, 3)
                lo, hi = pts.min(0), pts.max(0)
                pad = 0.15 * (float(np.linalg.norm(hi - lo)) or 1.0)
                box = [lo[0] - pad, lo[1] - pad, lo[2] - pad,
                       hi[0] + pad, hi[1] + pad, hi[2] + pad]
            rd = out_dir or os.path.join(os.getcwd(), 'verify-renders')
            pi, po = _render_pair(in_raw, out_raw, added_keys, box, rd, tag)
            result['render_input_png'] = pi
            result['render_output_png'] = po
        except Exception as e:  # noqa: BLE001
            result['render_error'] = str(e)
    return result


def main():
    ap = argparse.ArgumentParser(description='independent repaired-mesh shape verifier')
    ap.add_argument('input')
    ap.add_argument('output')
    ap.add_argument('--crop', type=float, nargs=6,
                    metavar=('X0', 'Y0', 'Z0', 'X1', 'Y1', 'Z1'))
    ap.add_argument('--out-dir', default=None)
    ap.add_argument('--render', action='store_true',
                    help='also write input/output PNGs (needs PySide6)')
    ap.add_argument('--tag', default=None)
    ap.add_argument('--repo', default=None,
                    help='repository root (default: parent of scripts/)')
    ap.add_argument('--no-surface', dest='surface', action='store_false',
                    help='skip the feature-recall / healthy-fill metrics')
    ap.add_argument('--surface-budget', choices=('sweep', 'gate'),
                    default='sweep',
                    help='surface sampling budget (default: sweep 4k x 10)')
    args = ap.parse_args()
    if os.environ.get('QT_QPA_PLATFORM') is None:
        os.environ['QT_QPA_PLATFORM'] = 'offscreen'
    r = verify(args.input, args.output, crop=args.crop, out_dir=args.out_dir,
               do_render=args.render, tag=args.tag, repo=args.repo,
               surface=args.surface, surface_budget=args.surface_budget)
    print(json.dumps(r, indent=2))


if __name__ == '__main__':
    main()
