# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Sutura engine core geometry, I/O, validation, and generic mesh helpers.

Contains:
  - Mesh arrays I/O helpers: load_meshes, stl_write_binary, parse_3mf_meshes.
  - Validation: boundary_loop_stats, scan_bad_coordinates, signed_volume, check_units.
  - Hausdorff scan preservation helpers: one_sided_hausdorff.

Specialised passes live in dedicated modules and are re-exported here for
backward compatibility:
  - ``sutura_engine.xray``  - reload-honest strict watertight checks (P-HONEST).
  - ``sutura_engine.stitch`` - reload-safe seam healing pass (P-WELD).
  - ``sutura_engine.hull``  - outer-shell extraction.

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
from sutura_engine.xray import (
    weld_reload_equivalent,
    reload_strict_holes_nm,
    _is_strict_watertight,
    enforce_reload_verdict,
)
from sutura_engine.stitch import (
    P_WELD_MAX_NUDGE_ATTEMPTS,
    P_WELD_MAX_HOLE,
    P_WELD_MIN_FACE_FRACTION,
    _separate_weld_collisions,
    _repair_welded_topology,
    _close_small_holes,
    p_weld_final,
)
from sutura_engine.hull import extract_outer_shell


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
