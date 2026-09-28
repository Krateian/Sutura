# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Sutura engine core geometry, I/O, validation, and reload-honest checks.

Contains:
  - Mesh arrays I/O helpers: load_meshes, stl_write_binary, parse_3mf_meshes.
  - Validation: boundary_loop_stats, scan_bad_coordinates, signed_volume, check_units.
  - Reload-equivalent welding (P-WELD): weld_reload_equivalent, p_weld_final.
  - Reload-honest strict watertight checks (P-HONEST): reload_strict_holes_nm, enforce_reload_verdict.
  - Hausdorff scan preservation helpers: one_sided_hausdorff.

Rule: This module must NOT import pymeshlab at module load time.
"""
from collections import defaultdict
import math
import os
import re
import struct
from typing import Any, Dict, List, Optional, Tuple
import zipfile

import numpy as np

# 3MF zip-bomb guard: 1 GiB decompressed entry cap
_3MF_MAX_ENTRY_BYTES = 1 << 30

# P-WELD tuning constants
P_WELD_MAX_NUDGE_ATTEMPTS = 12
P_WELD_MAX_HOLE = 200
P_WELD_MIN_FACE_FRACTION = 0.98


# --- I/O Helpers -------------------------------------------------------------

def _read_zip_entry(z: zipfile.ZipFile, name: str) -> bytes:
    """Read one 3MF zip entry, bounded by the decompressed-size cap."""
    info = z.getinfo(name)
    if info.file_size > _3MF_MAX_ENTRY_BYTES:
        raise ValueError(
            '3MF entry "%s" declares %d bytes uncompressed (> %d): '
            'suspicious compression ratio / oversized input.'
            % (name, info.file_size, _3MF_MAX_ENTRY_BYTES))
    return z.read(name)


def parse_3mf_meshes(path: str) -> Dict[str, List[Tuple[np.ndarray, np.ndarray, Tuple[int, int]]]]:
    """Return {model_name: [(verts, tris, (start, end))]} for every <mesh> block."""
    out = {}
    with zipfile.ZipFile(path) as z:
        for name in z.namelist():
            if not name.endswith('.model'):
                continue
            xml = _read_zip_entry(z, name).decode('utf-8', errors='replace')
            blocks = []
            for m in re.finditer(r'<mesh>.*?</mesh>', xml, re.S):
                block = m.group(0)
                vs = np.array([
                    list(map(float, v)) for v in re.findall(
                        r'<vertex\s+x="(-?[\d.eE+-]+)"\s+y="(-?[\d.eE+-]+)"\s+z="(-?[\d.eE+-]+)"\s*/>', block)
                ], dtype=np.float32)
                ts = np.array([
                    list(map(int, t)) for t in re.findall(
                        r'<triangle\s+v1="(\d+)"\s+v2="(\d+)"\s+v3="(\d+)"[^>]*/>', block)
                ], dtype=np.int32)
                if len(vs) and len(ts):
                    blocks.append((vs, ts, m.span()))
            if blocks:
                out[name] = blocks
    return out


def build_mesh_block(verts: np.ndarray, tris: np.ndarray) -> str:
    """Build a 3MF XML <mesh> block string from verts and tris."""
    lines = ['<mesh>', '    <vertices>']
    for v in verts:
        lines.append('     <vertex x="%.7g" y="%.7g" z="%.7g"/>' % (v[0], v[1], v[2]))
    lines.append('    </vertices>')
    lines.append('    <triangles>')
    for t in tris:
        lines.append('     <triangle v1="%d" v2="%d" v3="%d"/>' % (t[0], t[1], t[2]))
    lines.append('    </triangles>')
    lines.append('</mesh>')
    return '\n'.join(lines)


def load_meshes(src: str) -> List[Tuple[Optional[str], np.ndarray, np.ndarray]]:
    """Load a mesh file as a list of (model_name|None, verts, tris).

    STL/OBJ give one entry with model_name None; 3MF yields one entry per object.
    Lazy imports pymeshlab for STL/OBJ.
    """
    ext = os.path.splitext(src)[1].lower()
    if ext == '.3mf':
        out = []
        for name, blocks in parse_3mf_meshes(src).items():
            for vs, ts, _span in blocks:
                out.append((name, np.asarray(vs, dtype=np.float32),
                            np.asarray(ts, dtype=np.int32)))
        return out
    import pymeshlab as ml
    load_ms = ml.MeshSet()
    load_ms.load_new_mesh(src)
    m = load_ms.current_mesh()
    return [(None, np.asarray(m.vertex_matrix(), dtype=np.float32),
             np.asarray(m.face_matrix(), dtype=np.int32))]


def stl_write_binary(path: str, verts: np.ndarray, tris: np.ndarray) -> None:
    """Write binary STL using float32 coordinates."""
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    with open(path, 'wb') as f:
        header = b'sutura repaired mesh' + b' ' * (80 - 20)
        f.write(header)
        f.write(struct.pack('<I', len(t)))
        if len(t) == 0:
            return
        v0 = v[t[:, 0]]
        v1 = v[t[:, 1]]
        v2 = v[t[:, 2]]
        cross = np.cross(v1 - v0, v2 - v0)
        norm = np.linalg.norm(cross, axis=1, keepdims=True)
        norm = np.where(norm < 1e-12, 1.0, norm)
        normals = (cross / norm).astype(np.float32)
        record_dtype = np.dtype([
            ('normal', '<f4', (3,)),
            ('v0', '<f4', (3,)),
            ('v1', '<f4', (3,)),
            ('v2', '<f4', (3,)),
            ('attr', '<u2'),
        ])
        rec = np.empty(len(t), dtype=record_dtype)
        rec['normal'] = normals
        rec['v0'] = v0
        rec['v1'] = v1
        rec['v2'] = v2
        rec['attr'] = 0
        rec.tofile(f)


# --- Geometry & Validation Helpers ------------------------------------------

def boundary_loop_stats(verts: np.ndarray, tris: np.ndarray) -> Tuple[int, int]:
    """(n_loops, max_loop_len) of the mesh boundary. Pure numpy + dict."""
    t = np.asarray(tris, dtype=np.int64)
    if len(t) == 0:
        return 0, 0
    f0, f1, f2 = t[:, 0], t[:, 1], t[:, 2]
    max_v = int(t.max()) + 1
    keys = np.concatenate([
        np.minimum(f0, f1) * max_v + np.maximum(f0, f1),
        np.minimum(f1, f2) * max_v + np.maximum(f1, f2),
        np.minimum(f0, f2) * max_v + np.maximum(f0, f2),
    ]).astype(np.int64)
    uniq, counts = np.unique(keys, return_counts=True)
    bkeys = uniq[counts == 1]
    if len(bkeys) == 0:
        return 0, 0
    ba, bb = bkeys // max_v, bkeys % max_v
    adj = defaultdict(list)
    for a, c in zip(ba.tolist(), bb.tolist()):
        adj[a].append(c)
        adj[c].append(a)
    seen, loops = set(), []
    for s in adj:
        if s in seen:
            continue
        n, cur, prev = 0, s, None
        while cur not in seen:
            seen.add(cur)
            n += 1
            nxt = [x for x in adj[cur] if x != prev]
            if not nxt:
                break
            prev, cur = cur, nxt[0]
        loops.append(n)
    return len(loops), (max(loops) if loops else 0)


def scan_bad_coordinates(path: str) -> Optional[str]:
    """Return a description of NaN/Inf coordinates in STL/OBJ files, or None."""
    ext = os.path.splitext(path)[1].lower()
    if ext == '.3mf':
        return None
    if ext == '.obj':
        with open(path, errors='replace') as f:
            for line in f:
                if line.startswith('v '):
                    for tok in line.split()[1:4]:
                        try:
                            if not math.isfinite(float(tok)):
                                return 'NaN or infinite coordinates'
                        except ValueError:
                            pass
        return None
    with open(path, 'rb') as f:
        head = f.read(80)
        if head[:5] == b'solid':
            f.seek(0)
            for line in f:
                if line.strip().startswith(b'vertex'):
                    for tok in line.split()[1:4]:
                        try:
                            if not math.isfinite(float(tok)):
                                return 'NaN or infinite coordinates'
                        except ValueError:
                            pass
            return None
        buf = f.read(4)
        if len(buf) < 4:
            return 'malformed STL (no triangle count)'
        n = struct.unpack('<I', buf)[0]
        record_size = 50
        file_size = os.path.getsize(path)
        expected = 84 + n * record_size
        if file_size < expected:
            return 'truncated STL file'
        fmt = '<12fH'
        for _ in range(min(n, 10000)):
            b = f.read(record_size)
            if len(b) < record_size:
                break
            floats = struct.unpack(fmt, b)[:12]
            for val in floats:
                if not math.isfinite(val):
                    return 'NaN or infinite coordinates'
    return None


def signed_volume(verts: np.ndarray, tris: np.ndarray) -> float:
    """Signed volume of a triangle mesh (sum of origin-tetrahedra volumes)."""
    if len(tris) == 0 or len(verts) == 0:
        return 0.0
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    a = v[t[:, 0]]
    b = v[t[:, 1]]
    c = v[t[:, 2]]
    return float(np.sum(np.einsum('ij,ij->i', a, np.cross(b, c))) / 6.0)


def check_units(verts: np.ndarray, declared_unit: Optional[str] = None) -> Optional[dict]:
    """Return a unit warning dict or None based on bounding-box heuristics."""
    v = np.asarray(verts, dtype=np.float32)
    if len(v) == 0:
        return None
    bbox_min = v.min(axis=0)
    bbox_max = v.max(axis=0)
    dims = bbox_max - bbox_min
    max_dim = float(dims.max())
    if declared_unit is not None:
        unit = declared_unit.lower().strip()
        if unit in ('inch', 'inches', 'in'):
            return {'declared_unit': declared_unit, 'suspected_unit': 'inch',
                    'scale_factor': 25.4, 'max_dim': max_dim,
                    'scaled_max_dim': max_dim * 25.4}
        if unit in ('centimeter', 'centimeters', 'cm'):
            return {'declared_unit': declared_unit, 'suspected_unit': 'cm',
                    'scale_factor': 10.0, 'max_dim': max_dim,
                    'scaled_max_dim': max_dim * 10.0}
        return None
    if max_dim < 5.0 and max_dim > 0.01:
        scaled_in = max_dim * 25.4
        if 25.0 <= scaled_in <= 400.0:
            return {'suspected_unit': 'inch', 'scale_factor': 25.4,
                    'max_dim': max_dim, 'scaled_max_dim': scaled_in}
        scaled_cm = max_dim * 10.0
        if 25.0 <= scaled_cm <= 400.0:
            return {'suspected_unit': 'cm', 'scale_factor': 10.0,
                    'max_dim': max_dim, 'scaled_max_dim': scaled_cm}
    return None


# --- Reload-Equivalent Welding (P-WELD) & Honest Verdict (P-HONEST) ---------

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


# --- Hausdorff Scan-Preservation Helpers ------------------------------------

def one_sided_hausdorff(in_v: np.ndarray, in_t: np.ndarray,
                        out_v: np.ndarray, out_t: np.ndarray,
                        samples: int = 100000) -> Tuple[Optional[float], Optional[float]]:
    """Distance from input surface samples to output relative to input bbox diagonal: (max, mean)."""
    in_v = np.asarray(in_v, dtype=np.float64)
    in_t = np.asarray(in_t, dtype=np.int32)
    out_v = np.asarray(out_v, dtype=np.float64)
    out_t = np.asarray(out_t, dtype=np.int32)
    if len(in_t) == 0 or len(out_t) == 0:
        return None, None
    diag = float(np.linalg.norm(in_v.max(axis=0) - in_v.min(axis=0)))
    if diag <= 1e-12:
        return None, None
    try:
        import pymeshlab as ml
        hd = ml.MeshSet()
        hd.add_mesh(ml.Mesh(vertex_matrix=in_v, face_matrix=in_t))
        hd.add_mesh(ml.Mesh(vertex_matrix=out_v, face_matrix=out_t))
        r = hd.apply_filter(
            'get_hausdorff_distance',
            sampledmesh=0,
            targetmesh=1,
            samplevert=True,
            sampleface=True,
            samplenum=samples,
            maxdist=ml.PercentageValue(100),
        )
        max_d = float(r.get('max') or 0.0) / diag
        mean_d = float(r.get('mean') or 0.0) / diag
        return round(max_d, 6), round(mean_d, 6)
    except Exception:
        return None, None
