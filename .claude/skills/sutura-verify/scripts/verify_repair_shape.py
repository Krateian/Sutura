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


def submesh(verts, tris, face_idx):
    """Build a standalone welded submesh from a set of face indices."""
    if len(face_idx) == 0:
        return np.zeros((0, 3)), np.zeros((0, 3), np.int64)
    sub = np.asarray(tris)[np.asarray(face_idx, np.int64)]
    return weld_exact(verts[sub])


# --------------------------------------------------------------------------- #
# distance / leak
# --------------------------------------------------------------------------- #
def two_sided_distance(vi, ti, vo, to):
    """Symmetric vertex-to-surface distance (mm) between input and output."""
    empty = {'o2i_max': 0.0, 'o2i_p95': 0.0, 'i2o_max': 0.0,
             'i2o_p95': 0.0, 'two_sided_max': 0.0, 'two_sided_p95': 0.0}
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
    all_d = np.concatenate([d_o2i, d_i2o]) if (len(d_o2i) and len(d_i2o)) \
        else (d_o2i if len(d_o2i) else d_i2o)
    return {
        'o2i_max': round(float(np.max(d_o2i)), 4) if len(d_o2i) else 0.0,
        'o2i_p95': round(float(np.percentile(d_o2i, 95)), 4) if len(d_o2i) else 0.0,
        'i2o_max': round(float(np.max(d_i2o)), 4) if len(d_i2o) else 0.0,
        'i2o_p95': round(float(np.percentile(d_i2o, 95)), 4) if len(d_i2o) else 0.0,
        'two_sided_max': round(float(max(np.max(d_o2i), np.max(d_i2o))), 4)
        if len(d_o2i) and len(d_i2o) else 0.0,
        'two_sided_p95': round(float(np.percentile(all_d, 95)), 4) if len(all_d) else 0.0,
    }


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
           tag=None, repo=None):
    wire_repo(repo or default_repo())
    in_raw = read_stl_raw(in_path)
    out_raw = read_stl_raw(out_path)
    if tag is None:
        tag = os.path.splitext(os.path.basename(in_path))[0]

    vi, ti = weld_exact(in_raw)
    vo, to = weld_exact(out_raw)
    bbox_diag = float(np.linalg.norm(vi.max(0) - vi.min(0))) if len(vi) else 1.0
    tol = min(max(bbox_diag / 10000.0, 0.0125), 0.05)

    in_keys = tri_keys(in_raw)
    in_set = set(in_keys)
    out_set = set(tri_keys(out_raw))
    healthy = sum(1 for k in in_keys if k in out_set)
    added = [j for j, k in enumerate(tri_keys(out_raw)) if k not in in_set]

    from defects import detect
    rep = detect(vo, to)
    comp, closed = topology_counts(vo, to)
    si_out, si_note = exact_si(vo, to)
    dist = two_sided_distance(vi, ti, vo, to)
    leak = leak_metric(in_raw, out_raw, vi, ti, tol)
    orca = orca_info(out_path)

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
        'o2i_max_mm': dist['o2i_max'],
        'i2o_max_mm': dist['i2o_max'],
        'leak_max_mm': leak['leak_max'],
        'leak_count': leak['leak_count'],
        'si_output': si_out,
        'si_output_note': si_note,
    }

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
    args = ap.parse_args()
    if os.environ.get('QT_QPA_PLATFORM') is None:
        os.environ['QT_QPA_PLATFORM'] = 'offscreen'
    r = verify(args.input, args.output, crop=args.crop, out_dir=args.out_dir,
               do_render=args.render, tag=args.tag, repo=args.repo)
    print(json.dumps(r, indent=2))


if __name__ == '__main__':
    main()
