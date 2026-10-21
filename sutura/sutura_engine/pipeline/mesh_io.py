# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Mesh and file I/O for the Sutura repair pipeline (P1a split).

STL/OBJ/3MF reading and writing, unit handling, 3MF object parsing and the
bridge-module path resolution. Moved verbatim from sutura/repair.py; no logic
change.
"""
import copy
import math
import os
import re
import shutil
import struct
import sys
import tempfile
import zipfile
from collections import defaultdict, namedtuple

import numpy as np

from sutura_engine.pipeline.presets import SUTURA_DIR


def _resolve_ftetwild_bridge():
    """Locate ftetwild_bridge.py (bundled-copy aware)."""
    if getattr(sys, 'frozen', False):
        base = os.path.dirname(os.path.abspath(sys.executable))
        for cand in (os.path.join(base, 'ftetwild_bridge.py'),
                     os.path.join(base, '..', 'ftetwild_bridge.py')):
            cand = os.path.abspath(cand)
            if os.path.isfile(cand):
                return cand
    return os.path.join(SUTURA_DIR, 'ftetwild_bridge.py')


def _resolve_indirect_bridge():
    """Locate indirect_bridge.py (bundled-copy aware)."""
    if getattr(sys, 'frozen', False):
        base = os.path.dirname(os.path.abspath(sys.executable))
        for cand in (os.path.join(base, 'indirect_bridge.py'),
                     os.path.join(base, '..', 'indirect_bridge.py')):
            cand = os.path.abspath(cand)
            if os.path.isfile(cand):
                return cand
    return os.path.join(SUTURA_DIR, 'indirect_bridge.py')


FTETWILD_BRIDGE = _resolve_ftetwild_bridge()


INDIRECT_BRIDGE = _resolve_indirect_bridge()


def write_obj(path, verts, tris):
    with open(path, 'w') as f:
        f.write('# sutura intermediate\n')
        for v in verts:
            f.write('v %.9g %.9g %.9g\n' % (v[0], v[1], v[2]))
        for t in tris:
            f.write('f %d %d %d\n' % (t[0] + 1, t[1] + 1, t[2] + 1))


def check_units(verts, declared_unit=None):
    """Return a unit warning dict or None.

    A mesh whose bounding-box dimensions only become plausible when scaled
    from inches or centimetres to millimetres was probably not authored in
    millimetres. This matters because the size-based repair logic
    (maxholesize, hole diameter, volume-change thresholds) assumes mm, so a
    wrong-scale mesh is worth a visible, NON-BLOCKING warning -- it never
    changes the repair itself.

    ``declared_unit``: the <model unit="..."> attribute for 3MF inputs (None
    for STL/OBJ, which carry no unit metadata). A declared non-millimetre
    unit is definitive and reported as-is; ``'millimeter'`` (the 3MF spec
    default) is trusted and suppresses the heuristic. With no declared unit
    the heuristic below is used.

    Heuristic: a plausible printable part spans ~25-400 mm on its longest
    axis. If the mesh's longest axis only lands in that range after scaling
    by 25.4 (inches) or 10 (centimetres), flag it. Inches is checked first
    (the most common alternate CAD/export default); in the overlapping
    ~2.5-16-unit band the hint names both.

    Returns ``{'unit_warning': True, 'unit_hint': '...'}`` or None.
    """
    if declared_unit is not None and declared_unit != 'millimeter':
        return {'unit_warning': True,
                'unit_hint': ('The 3MF model declares its unit as "%s"; '
                              'size-based repair thresholds assume millimetres.'
                              % declared_unit)}
    if declared_unit == 'millimeter':
        return None
    if verts is None or len(verts) == 0:
        return None
    v = np.asarray(verts, dtype=np.float32)
    if len(v) == 0 or not np.isfinite(v).all():
        return None
    L = float(np.max(v.max(axis=0) - v.min(axis=0)))
    if not (L > 0.0):
        return None
    if not (25.0 <= 25.4 * L <= 400.0) and not (25.0 <= 10.0 * L <= 400.0):
        return None
    return {'unit_warning': True,
            'unit_hint': ('Model may not be in millimetres: the longest '
                          'bounding-box axis is %.3g units (as inches '
                          '≈ %.1f mm, as centimetres ≈ %.1f mm). Size-based '
                          'repair thresholds assume millimetres.'
                          % (L, 25.4 * L, 10.0 * L))}


_ASCII_LOWER = {c: c.lower() for c in map(chr, range(65, 91))}


def _ascii_lower(s):
    """Lowercase ASCII A-Z only; never folds non-ASCII (OPC part-name rule)."""
    return ''.join(_ASCII_LOWER.get(c, c) for c in s)


def _is_model_part(name):
    """True when a 3MF part name ends in ``.model``.

    OPC part names are compared case-insensitively over ASCII, so ``.MODEL``
    (and any ASCII-case variant) is a valid model part.
    """
    return _ascii_lower(name[-6:]) == '.model'


def read_3mf_units(path):
    """Return {model_name: declared_unit} for every .model file in a 3MF.

    The 3MF spec defaults the <model> unit attribute to ``millimeter`` when
    absent; allowed values are micron/millimeter/centimeter/inch. STL/OBJ
    carry no unit metadata, so this only applies to .3mf inputs. Never
    raises: a broken archive yields an empty dict (the caller's own 3MF
    parsing will report the real error)."""
    out = {}
    try:
        with zipfile.ZipFile(path) as z:
            for name in z.namelist():
                if not _is_model_part(name):
                    continue
                xml = _read_zip_entry(z, name).decode('utf-8', errors='replace')
                # Optional namespace prefix and either quoting style; [^>]*? is
                # bounded by the start tag's '>' so it cannot cross elements.
                m = re.search(
                    r'<(?:[\w.-]+:)?model\b[^>]*?\bunit\s*=\s*"([^"]+)"', xml)
                if m is None:
                    m = re.search(
                        r"<(?:[\w.-]+:)?model\b[^>]*?\bunit\s*=\s*'([^']+)'", xml)
                out[name] = m.group(1) if m else 'millimeter'
    except Exception:
        return {}
    return out


class MalformedSTLError(ValueError):
    """Raised when a file is neither a valid binary nor a valid ASCII STL."""


def _obj_vertex_index(token, vertex_count):
    """Resolve one OBJ face token to a 0-based vertex index.

    ``token`` may carry texture/normal suffixes (``v/vt/vn``, ``v//vn``); only
    the vertex part before the first slash is used. Negative indices are
    relative to the vertices defined so far (OBJ is 1-based).
    """
    raw = token.split('/', 1)[0]
    if raw == '':
        raise ValueError('OBJ face token has no vertex index: %r' % token)
    try:
        idx = int(raw)
    except ValueError:
        raise ValueError('OBJ face token is not an integer: %r' % token)
    if idx == 0:
        raise ValueError('OBJ vertex index 0 is invalid (indices are 1-based)')
    if idx < 0:
        resolved = vertex_count + idx
        if resolved < 0:
            raise ValueError('OBJ negative vertex index %d is out of range '
                             '(%d vertices defined)' % (idx, vertex_count))
        return resolved
    if idx > vertex_count:
        raise ValueError('OBJ vertex index %d is out of range '
                         '(%d vertices defined)' % (idx, vertex_count))
    return idx - 1


def _iter_obj_lines(f):
    """Yield ``(first_line_no, logical_line)`` for OBJ statements.

    Inline ``#`` comments are stripped and a trailing backslash joins the
    next physical line (Wavefront continuation syntax). The reported line
    number is the first physical line of the logical statement.
    """
    buf = None
    start = 0
    for line_no, raw in enumerate(f, 1):
        text = raw.split('#', 1)[0].rstrip('\r\n')
        stripped = text.rstrip()
        continued = stripped.endswith('\\')
        if continued:
            stripped = stripped[:-1]
        if buf is None:
            buf = stripped
            start = line_no
        else:
            buf += ' ' + stripped
        if continued:
            continue
        yield start, buf
        buf = None
    if buf is not None and buf.strip():
        yield start, buf


def read_obj(path):
    verts = []
    tris = []
    with open(path, encoding='utf-8', errors='replace') as f:
        for line_no, line in _iter_obj_lines(f):
            parts = line.split()
            if not parts:
                continue
            if parts[0] == 'v':
                if len(parts) < 4:
                    raise ValueError('%s:%d: OBJ vertex needs at least 3 '
                                     'coordinates' % (path, line_no))
                try:
                    verts.append((float(parts[1]), float(parts[2]),
                                  float(parts[3])))
                except ValueError as exc:
                    raise ValueError('%s:%d: %s' % (path, line_no, exc))
            elif parts[0] == 'f':
                try:
                    idxs = [_obj_vertex_index(tok, len(verts))
                            for tok in parts[1:]]
                except ValueError as exc:
                    raise ValueError('%s:%d: %s' % (path, line_no, exc))
                if len(idxs) < 3:
                    raise ValueError('%s:%d: OBJ face needs at least 3 '
                                     'vertices' % (path, line_no))
                # Fan-triangulate quads/ngons from the first vertex.
                for i in range(1, len(idxs) - 1):
                    tris.append((idxs[0], idxs[i], idxs[i + 1]))
    if not verts:
        if tris:
            raise ValueError('%s: OBJ has face records but no vertex '
                             'definitions' % path)
        return (np.zeros((0, 3), dtype=np.float32),
                np.zeros((0, 3), dtype=np.int32))
    out_verts = np.asarray(verts, dtype=np.float32)
    out_tris = np.asarray(tris, dtype=np.int32).reshape(-1, 3)
    if out_tris.size and int(out_tris.max()) >= len(out_verts):
        raise ValueError('%s: OBJ face index %d is out of range '
                         '(%d vertices)' % (path, int(out_tris.max()) + 1,
                                            len(out_verts)))
    return out_verts, out_tris


def _read_stl_ascii(path):
    verts = []
    saw_solid = False
    saw_endsolid = False
    saw_facet = False
    with open(path, 'r', errors='ignore') as f:
        for line in f:
            p = line.split()
            if not p:
                continue
            # ASCII STL keywords are case-insensitive and may be separated by
            # any whitespace/tabs.
            keyword = p[0].lower()
            if keyword == 'solid':
                saw_solid = True
                continue
            if keyword == 'endsolid':
                saw_endsolid = True
                continue
            if keyword == 'facet':
                saw_facet = True
                continue
            if keyword != 'vertex':
                continue
            if len(p) < 4:
                raise MalformedSTLError(
                    'Malformed STL: incomplete vertex line %r' % line.strip())
            try:
                verts.append((float(p[1]), float(p[2]), float(p[3])))
            except ValueError:
                raise MalformedSTLError(
                    'Malformed STL: non-numeric vertex line %r' % line.strip())
    if not verts:
        if saw_solid and saw_endsolid and not saw_facet:
            return (np.zeros((0, 3), dtype=np.float32),
                    np.zeros((0, 3), dtype=np.int32))
        raise MalformedSTLError(
            'Malformed STL: no vertex records found in binary or ASCII form')
    if len(verts) % 3 != 0:
        raise MalformedSTLError(
            'Malformed STL: ASCII vertex count %d is not a multiple of 3'
            % len(verts))
    v = np.asarray(verts, dtype=np.float32)
    n = len(v) // 3
    return v, np.arange(n * 3, dtype=np.int32).reshape(n, 3)


def _looks_like_ascii_stl(path, limit=64 * 1024):
    """True when the file prefix contains ASCII facet structure."""
    try:
        with open(path, 'rb') as f:
            chunk = f.read(limit)
    except OSError:
        return False
    text = chunk.decode('latin-1').lower()
    return 'facet' in text or 'outer loop' in text


def _read_stl_binary_records(f, n):
    f.seek(84)
    data = f.read(50 * n)
    if len(data) != 50 * n:
        raise MalformedSTLError('Malformed STL: truncated binary records')
    arr = np.frombuffer(data, dtype=np.uint8).reshape(n, 50)
    tri = arr[:, 12:48].copy().view(np.float32).reshape(n, 3, 3)
    return (tri.reshape(-1, 3).astype(np.float32),
            np.arange(3 * n, dtype=np.int32).reshape(n, 3))


def read_stl(path):
    """Read a binary or ASCII STL into (verts float32, tris int32).

    Binary is detected by the exact ``84 + 50 * n`` byte layout first: an
    ASCII file may also start with the word ``solid`` and a binary header may
    contain it, so the header word alone is not a reliable discriminator.
    A binary file longer than its records (trailing metadata/padding) is
    accepted when it carries no ASCII facet structure. Files matching neither
    layout raise :class:`MalformedSTLError` instead of silently returning an
    empty mesh.
    """
    with open(path, 'rb') as f:
        head = f.read(84)
        if len(head) == 84:
            n = struct.unpack('<I', head[80:84])[0]
            f.seek(0, os.SEEK_END)
            size = f.tell()
            if size == 84 + 50 * n:
                return _read_stl_binary_records(f, n)
            if (size > 84 + 50 * n and n > 0
                    and not _looks_like_ascii_stl(path)):
                return _read_stl_binary_records(f, n)
    return _read_stl_ascii(path)


_PLY_TYPES = {
    'char': 'i1', 'uchar': 'u1', 'int8': 'i1', 'uint8': 'u1',
    'short': 'i2', 'ushort': 'u2', 'int16': 'i2', 'uint16': 'u2',
    'int': 'i4', 'uint': 'u4', 'int32': 'i4', 'uint32': 'u4',
    'float': 'f4', 'float32': 'f4', 'double': 'f8', 'float64': 'f8',
}
_PLY_STRUCT = {'i1': 'b', 'u1': 'B', 'i2': 'h', 'u2': 'H',
               'i4': 'i', 'u4': 'I', 'f4': 'f', 'f8': 'd'}


def _ply_key(prop):
    return prop[3] if prop[0] == 'list' else prop[2]


def read_ply(path):
    """Read an ASCII or binary_little_endian PLY (vertex xyz + face list)."""
    with open(path, 'rb') as f:
        raw = f.read()
    cut = raw.find(b'end_header')
    if cut < 0:
        raise ValueError('malformed PLY: missing end_header')
    nl = raw.find(b'\n', cut)
    header = raw[:nl + 1].decode('ascii', 'replace').splitlines()
    body = raw[nl + 1:]
    fmt = None
    elements = []
    current = None
    for line in header:
        parts = line.strip().split()
        if not parts:
            continue
        if parts[0] == 'format':
            fmt = parts[1]
        elif parts[0] == 'element':
            current = {'name': parts[1], 'count': int(parts[2]), 'props': []}
            elements.append(current)
        elif parts[0] == 'property' and current is not None:
            if parts[1] == 'list':
                current['props'].append(('list', parts[2], parts[3], parts[4]))
            else:
                current['props'].append(('scalar', parts[1], parts[2]))
    v_el = next((e for e in elements if e['name'] == 'vertex'), None)
    f_el = next((e for e in elements if e['name'] == 'face'), None)
    if v_el is None:
        raise ValueError('PLY has no vertex element')
    vprops = v_el['props']
    axes = []
    for axis in ('x', 'y', 'z'):
        idx = next((i for i, p in enumerate(vprops)
                    if p[0] == 'scalar' and p[2] == axis), None)
        if idx is None:
            raise ValueError('PLY vertex has no %s property' % axis)
        axes.append(idx)
    vertex_dtype = np.dtype([(_ply_key(p), _PLY_TYPES[p[1]]) for p in vprops])
    if fmt == 'ascii':
        lines = body.decode('ascii', 'replace').splitlines()
        vals = [[float(x) for x in ln.split()] for ln in lines if ln.strip()]
        vrows = vals[:v_el['count']]
        rest = vals[v_el['count']:]
        verts = np.asarray([[r[i] for i in axes] for r in vrows], dtype=np.float32)
        faces = []
        if f_el is not None:
            for row in rest[:f_el['count']]:
                n = int(row[0])
                faces.append([int(x) for x in row[1:1 + n]])
    elif fmt == 'binary_little_endian':
        rec = vertex_dtype.itemsize
        arr = np.frombuffer(body, dtype=vertex_dtype, count=v_el['count'])
        verts = np.stack([arr[vprops[a][2]] for a in axes], axis=1)
        verts = np.asarray(verts, dtype=np.float32)
        off = v_el['count'] * rec
        faces = []
        if f_el is not None:
            list_prop = next((p for p in f_el['props'] if p[0] == 'list'), None)
            if list_prop is None:
                raise ValueError('PLY face element has no list property')
            cchar = _PLY_STRUCT[_PLY_TYPES[list_prop[1]]]
            ichar = _PLY_STRUCT[_PLY_TYPES[list_prop[2]]]
            ctype = '<' + cchar
            csz = struct.calcsize(ctype)
            isz = struct.calcsize(ichar)
            for _ in range(f_el['count']):
                n = struct.unpack_from(ctype, body, off)[0]
                off += csz
                idx = list(struct.unpack_from('<%d%s' % (n, ichar),
                                              body, off))
                off += isz * n
                faces.append(idx)
    else:
        raise ValueError('unsupported PLY format: %s' % fmt)
    tris = (np.asarray(faces, dtype=np.int32) if faces
            else np.zeros((0, 3), dtype=np.int32))
    return verts, tris.reshape(-1, 3)


def write_ply(path, verts, tris):
    """Write a binary_little_endian PLY with only vertex xyz and face lists."""
    v = np.asarray(verts, dtype=np.float32).reshape(-1, 3)
    t = np.asarray(tris, dtype=np.int32).reshape(-1, 3)
    head = ('ply\nformat binary_little_endian 1.0\n'
            'element vertex %d\nproperty float x\nproperty float y\n'
            'property float z\nelement face %d\n'
            'property list uchar int vertex_indices\nend_header\n'
            % (len(v), len(t)))
    with open(path, 'wb') as f:
        f.write(head.encode('ascii'))
        f.write(v.astype('<f4').tobytes())
        for tri in t:
            f.write(struct.pack('<B', 3))
            f.write(struct.pack('<3i', *[int(x) for x in tri]))


def read_mesh(path):
    """Read an STL, OBJ or PLY file into (verts float32, tris int32)."""
    ext = os.path.splitext(str(path))[1].lower()
    if ext == '.obj':
        return read_obj(path)
    if ext == '.stl':
        return read_stl(path)
    if ext == '.ply':
        return read_ply(path)
    raise ValueError('unsupported mesh extension for reading: %s' % ext)


def stl_write_binary(path, verts, tris):
    with open(path, 'wb') as f:
        f.write(b'Sutura intermediate'.ljust(80, b'\0'))
        f.write(struct.pack('<I', len(tris)))
        for t in tris:
            f.write(struct.pack('<3f', 0, 0, 0))
            for i in t:
                f.write(struct.pack('<3f', verts[i][0], verts[i][1], verts[i][2]))
            f.write(struct.pack('<H', 0))


def should_extract_outer_shell(verts, tris, mesh_type=None, si_count=0):
    """True ONLY when mesh fits mechanical or dense_scan_heavy_si templates,
    components > 1, and the closed components overlap or touch.

    Pure-numpy replacement for the former ``trimesh.Trimesh(...).split()`` path:
    the unconditional ``import trimesh`` (and its optional stack) cost ~0.5 s on
    every mechanical repair even when this pre-check returned False.  Faces are
    grouped into connected components through edges shared by exactly two faces
    (trimesh's ``face_adjacency`` rule); a component counts as closed only when
    every edge of its own faces appears exactly twice.  A component that is open
    (>=4 faces and a genuine boundary loop) makes the whole check decline, which
    preserves the previous trimesh ``fill_holes`` outcome.
    """
    is_mech = (mesh_type == 'mechanical')
    is_heavy_si = (si_count >= 200)
    if not (is_mech or is_heavy_si):
        return False
    try:
        verts = np.asarray(verts)
        tris = np.asarray(tris, dtype=np.int64)
        n_faces = len(tris)
        if n_faces == 0 or len(verts) == 0:
            return False
        # Per-face edges, sorted so that the two faces sharing an edge agree on
        # its vertex pair.  Each edge is tagged with the face it came from.
        face_of_edge = np.concatenate(
            [np.arange(n_faces), np.arange(n_faces), np.arange(n_faces)])
        edges = np.concatenate(
            [tris[:, [0, 1]], tris[:, [1, 2]], tris[:, [2, 0]]], axis=0)
        edges = np.sort(edges, axis=1)
        order = np.lexsort((edges[:, 1], edges[:, 0]))
        edges = edges[order]
        face_of_edge = face_of_edge[order]
        starts = np.concatenate(
            [[0], np.nonzero(np.any(edges[1:] != edges[:-1], axis=1))[0] + 1])
        sizes = np.diff(np.append(starts, len(edges)))
        # Connected components over faces, joined only by edges used by exactly
        # two faces.  Vectorised union-find: hook each edge's endpoint roots to
        # their minimum, shortcut, repeat until stable (converges in a handful
        # of passes on real meshes).
        pair_starts = starts[sizes == 2]
        edge_a = face_of_edge[pair_starts].astype(np.int64)
        edge_b = face_of_edge[pair_starts + 1].astype(np.int64)
        parent = np.arange(n_faces, dtype=np.int64)

        def _roots(x):
            while True:
                px = parent[x]
                if np.array_equal(px, x):
                    return x
                x = px

        while True:
            root_a = _roots(edge_a)
            root_b = _roots(edge_b)
            low = np.minimum(root_a, root_b)
            new_parent = parent.copy()
            np.minimum.at(new_parent, root_a, low)
            np.minimum.at(new_parent, root_b, low)
            new_parent = new_parent[new_parent]
            if np.array_equal(new_parent, parent):
                break
            parent = new_parent
        roots = _roots(np.arange(n_faces, dtype=np.int64))
        _, roots = np.unique(roots, return_inverse=True)
        n_comp = int(roots.max()) + 1 if n_faces else 0
        if n_comp <= 1:
            return False
        # Per-component edge multiset (a component's own faces only, matching
        # trimesh's reindexed submesh): group edge occurrences by
        # (component, vertex pair).
        edge_comp = roots[face_of_edge]
        order = np.lexsort((edges[:, 1], edges[:, 0], edge_comp))
        grouped_comp = edge_comp[order]
        grouped_lo = edges[order, 0]
        grouped_hi = edges[order, 1]
        changed = ((grouped_comp[1:] != grouped_comp[:-1]) |
                   (grouped_lo[1:] != grouped_lo[:-1]) |
                   (grouped_hi[1:] != grouped_hi[:-1]))
        grp_start = np.concatenate([[0], np.nonzero(changed)[0] + 1])
        grp_size = np.diff(np.append(grp_start, len(grouped_comp)))
        grp_comp = grouped_comp[grp_start]
        comp_faces = np.bincount(roots, minlength=n_comp)
        boundary_edges = np.bincount(grp_comp[grp_size == 1], minlength=n_comp)
        not_closed = np.zeros(n_comp, dtype=bool)
        not_closed[grp_comp[grp_size != 2]] = True
        # The former trimesh fill_holes path declined whenever a component with
        # >=4 faces had a genuine boundary loop (>=3 boundary edges).
        if np.any((comp_faces >= 4) & (boundary_edges >= 3)):
            return False
        closed_count = int((~not_closed).sum())
        if closed_count < n_comp * 0.7:
            return False
        # Per-component bounding boxes from the referenced vertices.
        vert_comp = np.repeat(roots, 3)
        vert_idx = tris.reshape(-1)
        vorder = np.argsort(vert_comp, kind='stable')
        sorted_comp = vert_comp[vorder]
        sorted_verts = verts[vert_idx][vorder]
        comp_start = np.searchsorted(sorted_comp, np.arange(n_comp), side='left')
        bounds = list(zip(np.minimum.reduceat(sorted_verts, comp_start, axis=0),
                          np.maximum.reduceat(sorted_verts, comp_start, axis=0)))
        # Pairwise bounding-box overlap check
        diag = float(np.linalg.norm(np.ptp(verts, axis=0)))
        eps = 1e-4 * (diag if diag > 1e-6 else 1.0)
        n = len(bounds)
        for i in range(n):
            min_i, max_i = bounds[i][0], bounds[i][1]
            for j in range(i + 1, n):
                min_j, max_j = bounds[j][0], bounds[j][1]
                if (min(max_i[0], max_j[0]) >= max(min_i[0], min_j[0]) - eps and
                    min(max_i[1], max_j[1]) >= max(min_i[1], min_j[1]) - eps and
                    min(max_i[2], max_j[2]) >= max(min_i[2], min_j[2]) - eps):
                    return True
        return False
    except Exception:
        return False


def _write_3mf_single(out_path, verts, tris):
    """Write one object as a minimal, standards-compliant 3MF container."""
    out_dir = os.path.dirname(os.path.abspath(out_path)) or '.'
    os.makedirs(out_dir, exist_ok=True)
    model = (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<model unit="millimeter" xml:lang="en-US" '
        'xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">\n'
        '<resources>\n<object id="1" type="model">\n'
        + build_mesh_block(verts, tris)
        + '\n</object>\n</resources>\n'
        '<build><item objectid="1"/></build>\n</model>\n'
    )
    content_types = (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">\n'
        '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>\n'
        '<Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>\n'
        '</Types>\n'
    )
    rels = (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">\n'
        '<Relationship Target="/3D/3dmodel.model" Id="rel0" '
        'Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>\n'
        '</Relationships>\n'
    )
    with zipfile.ZipFile(out_path, 'w', compression=zipfile.ZIP_DEFLATED) as z:
        z.writestr('[Content_Types].xml', content_types)
        z.writestr('_rels/.rels', rels)
        z.writestr('3D/3dmodel.model', model)


def save_mesh(out_path, verts, tris):
    """Write one mesh natively, dispatched by extension.

    STL is binary (the existing reload-equivalent writer), OBJ uses the
    standard ``v``/``f`` records, and 3MF is a single-object container.
    Multi-object 3MF rewriting remains the existing ``rewrite_3mf_meshes``
    path.
    """
    ext = os.path.splitext(str(out_path))[1].lower()
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32).reshape(-1, 3)
    if ext == '.stl':
        stl_write_binary(out_path, v, t)
    elif ext == '.obj':
        write_obj(out_path, v, t)
    elif ext == '.3mf':
        _write_3mf_single(out_path, v, t)
    elif ext == '.ply':
        write_ply(out_path, v, t)
    else:
        raise ValueError('unsupported mesh extension for writing: %s' % ext)


def scan_bad_coordinates(path):
    """Return a description of NaN/Inf coordinates in STL/OBJ files, or None."""
    ext = os.path.splitext(path)[1].lower()
    if ext == '.3mf':
        # 3MF meshes are parsed as XML (parse_3mf_meshes); NaN/Inf vertex
        # attributes simply don't match the numeric regex and are skipped, so
        # there is nothing to scan here.
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
        # A valid binary STL is exactly 84 + n*50 bytes; a mismatch means a
        # bad header count or a cut-off file, which pymeshlab hangs on instead
        # of failing cleanly.
        size = os.path.getsize(path)
        if size != 84 + n * 50:
            return 'malformed STL (declared %d triangles, file size mismatch)' % n
        return _scan_bin_stl(f, n)
    return None


def _scan_bin_stl(f, n):
    """Bulk-read the n 50-byte binary STL records after the 84-byte header and
    report the FIRST non-finite vertex coordinate (only the 3 vertex vectors,
    offsets 12..48 — normals and the 2-byte attribute are not checked, exactly
    as before). Returns an error message string or None."""
    dt = np.dtype([('normal', '<f4', (3,)),
                   ('verts', '<f4', (3, 3)),
                   ('attr', '<u2')])
    data = np.fromfile(f, dtype=dt, count=n)
    if data.shape[0] != n:
        return 'malformed STL (truncated data)'
    if not np.isfinite(data['verts']).all():
        return 'NaN or infinite coordinates'
    return None


def obj_has_material_refs(path):
    """True when an OBJ file references materials/textures (mtllib/usemtl).

    The repair pipeline rebuilds the mesh (verts + tris only), so any
    material/texture references in the input are not preserved in the
    repaired output. This lets the report surface that loss explicitly
    instead of dropping it silently. Only meaningful for .obj inputs."""
    try:
        with open(path, errors='replace') as f:
            for line in f:
                s = line.lstrip().lower()
                if s.startswith(('mtllib ', 'usemtl ')):
                    return True
    except OSError:
        return False
    return False


MeshBlock = namedtuple(
    'MeshBlock',
    'model_name index span open_tag close_tag prefix verts tris triangle_attrs')

_MESH_TAG_RE = re.compile(
    r'<(?P<prefix>(?:[A-Za-z_][\w.-]*:)?)mesh\b'
    r'(?P<attrs>[^>]*)>(?P<body>.*?)</(?P=prefix)mesh\s*>', re.S)
_COMMENT_RE = re.compile(r'<!--.*?-->', re.S)
# XML NameStartChar (word char, not a digit) + NameChar ([:\w.-]); Unicode-aware
# \w accepts non-ASCII attribute names such as "özel_renk" or "颜色".
_ATTR_RE = re.compile(
    r"""([^\W\d][\w:.-]*)\s*=\s*(?:"([^"]*)"|'([^']*)')""")
_VERTEX_TAG_RE = re.compile(
    r'<(?P<prefix>(?:[A-Za-z_][\w.-]*:)?)vertex\b(?P<attrs>[^>]*?)/?>',
    re.S)
_TRIANGLE_TAG_RE = re.compile(
    r'<(?P<prefix>(?:[A-Za-z_][\w.-]*:)?)triangle\b(?P<attrs>[^>]*?)/?>',
    re.S)


def _mask_comments(xml):
    """Blank out XML comments on a length-preserving copy.

    A comment such as ``<!-- <mesh></mesh> -->`` must not be mistaken for a real
    tag, yet every kept span/offset must still index the ORIGINAL text. Each
    comment is replaced by spaces of the same length, so offsets are unchanged.
    Unterminated comments (malformed XML) are left as-is.
    """
    if '<!--' not in xml:
        return xml
    return _COMMENT_RE.sub(lambda m: ' ' * (m.end() - m.start()), xml)


def _attrs_dict(text):
    out = {}
    for m in _ATTR_RE.finditer(text):
        value = m.group(2) if m.group(2) is not None else m.group(3)
        out[m.group(1)] = value
    return out


def _extra_triangle_attrs(text):
    parts = []
    for m in _ATTR_RE.finditer(text):
        if m.group(1) not in ('v1', 'v2', 'v3'):
            parts.append(m.group(0))
    return ' ' + ' '.join(parts) if parts else None


def _parse_mesh_body(body):
    vs = []
    ts = []
    tri_attrs = []
    for m in _VERTEX_TAG_RE.finditer(body):
        a = _attrs_dict(m.group('attrs'))
        if not all(k in a for k in ('x', 'y', 'z')):
            continue
        try:
            vs.append((float(a['x']), float(a['y']), float(a['z'])))
        except ValueError:
            continue
    for m in _TRIANGLE_TAG_RE.finditer(body):
        a = _attrs_dict(m.group('attrs'))
        if not all(k in a for k in ('v1', 'v2', 'v3')):
            continue
        try:
            ts.append((int(a['v1']), int(a['v2']), int(a['v3'])))
        except ValueError:
            continue
        tri_attrs.append(_extra_triangle_attrs(m.group('attrs')))
    return (np.asarray(vs, dtype=np.float32),
            np.asarray(ts, dtype=np.int32),
            tri_attrs)


def parse_3mf_mesh_blocks(path):
    """Return ordered MeshBlock records for every <mesh> in every .model file.

    Each record keeps the original mesh span and open/close tags so a repair can
    replace only the vertices/triangles and leave the rest of the XML untouched.
    ``triangle_attrs`` holds every non-index triangle attribute (paint_color,
    paint_seam, slic3rpe:mmu_segmentation, pid, ...) verbatim, or None.
    """
    blocks = []
    with zipfile.ZipFile(path) as z:
        for info in z.infolist():
            name = info.filename
            if not _is_model_part(name):
                continue
            xml = _read_zip_info(z, info).decode('utf-8', errors='replace')
            # Locate tags/spans on a comment-masked copy (offsets preserved),
            # but splice open/close tags out of the ORIGINAL xml.
            masked = _mask_comments(xml)
            for index, m in enumerate(_MESH_TAG_RE.finditer(masked)):
                vs, ts, tri_attrs = _parse_mesh_body(m.group('body'))
                if not len(vs) or not len(ts):
                    continue
                blocks.append(MeshBlock(
                    model_name=name,
                    index=index,
                    span=m.span(),
                    open_tag=xml[m.start():m.start('body')],
                    close_tag=xml[m.end('body'):m.end()],
                    prefix=m.group('prefix') or '',
                    verts=vs,
                    tris=ts,
                    triangle_attrs=tri_attrs,
                ))
    return blocks


def parse_3mf_meshes(path):
    """Return {model_name: [(verts, tris, (start,end))]} for every <mesh> block."""
    out = {}
    for block in parse_3mf_mesh_blocks(path):
        out.setdefault(block.model_name, []).append(
            (block.verts, block.tris, block.span))
    return out


def _mesh_inner(prefix, verts, tris, tri_attrs=None):
    p = prefix or ''
    lines = ['    <%svertices>' % p]
    for v in verts:
        lines.append('     <%svertex x="%.7g" y="%.7g" z="%.7g"/>'
                     % (p, v[0], v[1], v[2]))
    lines.append('    </%svertices>' % p)
    lines.append('    <%striangles>' % p)
    for i, t in enumerate(tris):
        extra = ''
        if tri_attrs is not None and i < len(tri_attrs) and tri_attrs[i]:
            extra = tri_attrs[i]
        lines.append('     <%striangle v1="%d" v2="%d" v3="%d"%s/>'
                     % (p, t[0], t[1], t[2], extra))
    lines.append('    </%striangles>' % p)
    return '\n'.join(lines)


def build_mesh_block(verts, tris, tri_attrs=None):
    return '\n'.join([
        '<mesh>',
        _mesh_inner('', verts, tris, tri_attrs),
        '</mesh>',
    ])


def _tri_key(a, b, c):
    return min((a, b, c), (b, c, a), (c, a, b))


def match_triangle_attributes(orig_verts, orig_tris, orig_attrs,
                              new_verts, new_tris):
    """Phase 1: keep triangle attributes on exact-coordinate survivors.

    Returns ``(attrs_for_new_tris, stats)``. Matching is cyclic-rotation
    invariant and uses float32 coordinates. Duplicate coordinate triangles are
    paired one-to-one, preferring an original that carries attributes.
    """
    ov = [(float(x), float(y), float(z))
          for x, y, z in np.asarray(orig_verts, dtype=np.float32)]
    nv = [(float(x), float(y), float(z))
          for x, y, z in np.asarray(new_verts, dtype=np.float32)]
    lookup = defaultdict(list)
    attrs = list(orig_attrs or [])
    for i, t in enumerate(np.asarray(orig_tris, dtype=np.int64)):
        try:
            key = _tri_key(ov[int(t[0])], ov[int(t[1])], ov[int(t[2])])
        except (IndexError, ValueError):
            continue
        lookup[key].append(attrs[i] if i < len(attrs) else None)
    out = [None] * len(new_tris)
    kept = 0
    for j, t in enumerate(np.asarray(new_tris, dtype=np.int64)):
        try:
            key = _tri_key(nv[int(t[0])], nv[int(t[1])], nv[int(t[2])])
        except (IndexError, ValueError):
            continue
        bucket = lookup.get(key)
        if not bucket:
            continue
        pick = next((n for n, a in enumerate(bucket) if a), 0)
        attr = bucket.pop(pick)
        if attr:
            out[j] = attr
            kept += 1
    original_attributed = sum(1 for a in attrs if a)
    return out, {
        'original_attributed': original_attributed,
        'kept': kept,
        'dropped': original_attributed - kept,
        'output_triangles': int(len(new_tris)),
        'method': 'exact_vertex_coordinates',
    }


def aggregate_triangle_attributes(reports):
    """Sum per-object ``triangle_attributes`` stats into one aggregate dict.

    Single source of truth shared by ``service.repair_3mf`` (baseline) and the
    triage escalation path, so the top-level aggregate can never drift from the
    per-object stats it summarises. Returns ``None`` when no report carries a
    stats dict.
    """
    attrs = [r['triangle_attributes'] for r in reports
             if isinstance(r.get('triangle_attributes'), dict)]
    if not attrs:
        return None
    return {
        'original_attributed': sum(
            a.get('original_attributed', 0) for a in attrs),
        'kept': sum(a.get('kept', 0) for a in attrs),
        'dropped': sum(a.get('dropped', 0) for a in attrs),
        'output_triangles': sum(a.get('output_triangles', 0) for a in attrs),
        'method': 'exact_vertex_coordinates',
    }


def _format_mesh_block(block, verts, tris, tri_attrs):
    return '\n'.join([
        block.open_tag,
        _mesh_inner(block.prefix, verts, tris, tri_attrs),
        block.close_tag,
    ])


def rewrite_3mf_meshes(container_src, out, updates, attr_source=None):
    """Rewrite only repaired mesh blocks in a 3MF ZIP.

    ``updates`` is an ordered list of ``(verts, tris)`` aligned with the mesh
    blocks of ``container_src``. Every other entry is copied byte-for-byte on
    decompression, preserving order, names, compression type and ZIP metadata.
    ``attr_source`` optionally supplies the original triangles for phase-1
    attribute matching (the triage escalation path passes the original input
    while the container is the intermediate output). Returns one stats dict per
    updated block.
    """
    updates = list(updates)
    container_blocks = parse_3mf_mesh_blocks(container_src)
    if len(updates) != len(container_blocks):
        raise ValueError(
            '3MF update count (%d) does not match mesh block count (%d)'
            % (len(updates), len(container_blocks)))
    if attr_source is None or os.path.abspath(attr_source) == \
            os.path.abspath(container_src):
        attr_blocks = container_blocks
    else:
        attr_blocks = parse_3mf_mesh_blocks(attr_source)
    attr_by_key = {(b.model_name, b.index): b for b in attr_blocks}

    replacements = defaultdict(list)
    stats = []
    for block, (new_v, new_t) in zip(container_blocks, updates):
        orig = attr_by_key.get((block.model_name, block.index), block)
        tri_attrs, st = match_triangle_attributes(
            orig.verts, orig.tris, orig.triangle_attrs, new_v, new_t)
        st['model'] = block.model_name
        st['block'] = block.index
        stats.append(st)
        replacements[block.model_name].append(
            (block.span[0], block.span[1],
             _format_mesh_block(block, new_v, new_t, tri_attrs)))

    out_dir = os.path.dirname(os.path.abspath(out)) or '.'
    os.makedirs(out_dir, exist_ok=True)
    fd, tmp_path = tempfile.mkstemp(
        prefix='.sutura-3mf-', suffix='.tmp', dir=out_dir)
    os.close(fd)
    try:
        import xml.etree.ElementTree as ET
        with zipfile.ZipFile(container_src) as zin, \
                zipfile.ZipFile(tmp_path, 'w',
                                compression=zipfile.ZIP_DEFLATED) as zout:
            zout.comment = zin.comment
            for info in zin.infolist():
                zinfo = copy.copy(info)
                reps = replacements.get(info.filename)
                if not reps:
                    with zin.open(info) as src, \
                            zout.open(zinfo, 'w',
                                      force_zip64=info.file_size > 0x7FFFFFFF) as dst:
                        # ZipFile.open() replaces a falsy external_attr with
                        # 0o600<<16; the central directory is written later,
                        # so restore the source value before close.
                        zinfo.external_attr = info.external_attr
                        shutil.copyfileobj(src, dst, length=1024 * 1024)
                    continue
                data = _read_zip_info(zin, info)
                xml = data.decode('utf-8', errors='replace')
                parts = []
                pos = 0
                for start, end, text in sorted(reps):
                    if not 0 <= start <= end <= len(xml):
                        raise ValueError(
                            'invalid mesh span for 3MF entry "%s"' % info.filename)
                    parts.append(xml[pos:start])
                    parts.append(text)
                    pos = end
                parts.append(xml[pos:])
                new_xml = ''.join(parts)
                try:
                    ET.fromstring(xml)
                except ET.ParseError:
                    original_ok = False
                else:
                    original_ok = True
                if original_ok:
                    try:
                        ET.fromstring(new_xml)
                    except ET.ParseError as e:
                        raise ValueError(
                            'rebuilt 3MF XML is not well-formed in "%s": %s'
                            % (info.filename, e))
                with zout.open(
                        zinfo, 'w',
                        force_zip64=len(new_xml) > 0x7FFFFFFF) as dst:
                    zinfo.external_attr = info.external_attr
                    dst.write(new_xml.encode('utf-8'))
        bad = zipfile.ZipFile(tmp_path).testzip()
        if bad is not None:
            raise ValueError('rebuilt 3MF entry failed CRC: %s' % bad)
        os.replace(tmp_path, out)
    except BaseException:
        try:
            os.unlink(tmp_path)
        except OSError:
            pass
        raise
    return stats


# Decompressed-size cap for any single 3MF archive entry (FAZ14 zip-bomb
# guard). A crafted 3MF can declare a tiny compressed size with a huge
# uncompressed size; we check zipfile's header `file_size` BEFORE reading the
# entry, so a bomb fails with a controlled error instead of exhausting memory.
_3MF_MAX_ENTRY_BYTES = 1 << 30  # 1 GiB


def _read_zip_info(z, info):
    if info.file_size > _3MF_MAX_ENTRY_BYTES:
        raise ValueError(
            '3MF entry "%s" declares %d bytes uncompressed '
            '(> %d): suspicious compression ratio / oversized input.'
            % (info.filename, info.file_size, _3MF_MAX_ENTRY_BYTES))
    return z.read(info)


def _read_zip_entry(z, name):
    """Read one 3MF zip entry, bounded by the decompressed-size cap.

    Checks the zip header's declared ``file_size`` BEFORE reading so a crafted
    entry (tiny compressed, huge declared uncompressed) fails with a controlled
    ValueError instead of exhausting memory (FAZ14 zip-bomb guard)."""
    return _read_zip_info(z, z.getinfo(name))


def _read_native_single(src):
    """Read one STL/OBJ and match the former loader's vertex semantics.

    The former PyMeshLab STL loader welded exactly coincident positions; OBJ
    is already index-based and keeps duplicate vertex records. The native STL
    reader returns three unshared vertices per triangle, so apply the same
    exact-position weld here (the topology kernel is bit-for-bit with the
    reload weld used elsewhere).
    """
    ext = os.path.splitext(str(src))[1].lower()
    if ext == '.stl':
        v, t = read_stl(src)
        import topology

        return topology.remove_duplicate_vertices(v, t)
    if ext == '.obj':
        return read_obj(src)
    if ext == '.ply':
        return read_ply(src)
    raise ValueError('unsupported mesh extension for reading: %s' % ext)


def load_meshes(src):
    """Load a mesh file as a list of (model_name|None, verts float32, tris int32).

    STL (binary/ascii) and OBJ give one entry with ``model_name`` None; a 3MF
    yields one entry per mesh block (or an empty list when it has no mesh
    objects). Shared by validate and dry-run so both see exactly what repair
    would see. Fully native: no MeshSet is created.
    """
    ext = os.path.splitext(str(src))[1].lower()
    if ext == '.3mf':
        out = []
        for name, blocks in parse_3mf_meshes(src).items():
            for vs, ts, _span in blocks:
                out.append((name, np.asarray(vs, dtype=np.float32),
                            np.asarray(ts, dtype=np.int32)))
        return out
    v, t = _read_native_single(src)
    return [(None, np.asarray(v, dtype=np.float32),
             np.asarray(t, dtype=np.int32))]


def load_mesh(src):
    """Load one mesh as ``(verts float32, tris int32)``.

    STL/OBJ give their single mesh; for a multi-object 3MF the first mesh
    block is returned (use ``load_meshes`` for every object). Raises
    ``ValueError`` when the file has no mesh geometry.
    """
    meshes = load_meshes(src)
    if not meshes:
        raise ValueError('no mesh geometry found in %s' % src)
    _name, verts, tris = meshes[0]
    return verts, tris
