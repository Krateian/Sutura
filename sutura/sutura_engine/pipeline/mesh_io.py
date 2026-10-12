# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Mesh and file I/O for the Sutura repair pipeline (P1a split).

STL/OBJ/3MF reading and writing, unit handling, 3MF object parsing and the
bridge-module path resolution. Moved verbatim from sutura/repair.py; no logic
change.
"""
import math
import os
import re
import struct
import sys
import zipfile

import numpy as np

from sutura_engine.pipeline.presets import SUTURA_DIR


def _resolve_bridge():
    """Locate manifold_bridge.py.

    Prefers a copy bundled next to the executable in a PyInstaller bundle
    (onefile: ``<dir>/manifold_bridge.py``; onedir: ``<dir>/<tool>/``), so a
    standalone .app can run stage 2 in-process with no system install. Falls
    back to the standard ``SUTURA_DIR`` layout (unchanged) outside bundles.
    """
    if getattr(sys, 'frozen', False):
        base = os.path.dirname(os.path.abspath(sys.executable))
        for cand in (os.path.join(base, 'manifold_bridge.py'),
                     os.path.join(base, '..', 'manifold_bridge.py')):
            cand = os.path.abspath(cand)
            if os.path.isfile(cand):
                return cand
    return os.path.join(SUTURA_DIR, 'manifold_bridge.py')


def _resolve_ftetwild_bridge():
    """Locate ftetwild_bridge.py (bundled-copy aware, same as _resolve_bridge)."""
    if getattr(sys, 'frozen', False):
        base = os.path.dirname(os.path.abspath(sys.executable))
        for cand in (os.path.join(base, 'ftetwild_bridge.py'),
                     os.path.join(base, '..', 'ftetwild_bridge.py')):
            cand = os.path.abspath(cand)
            if os.path.isfile(cand):
                return cand
    return os.path.join(SUTURA_DIR, 'ftetwild_bridge.py')


def _resolve_indirect_bridge():
    """Locate indirect_bridge.py (bundled-copy aware, same as _resolve_bridge)."""
    if getattr(sys, 'frozen', False):
        base = os.path.dirname(os.path.abspath(sys.executable))
        for cand in (os.path.join(base, 'indirect_bridge.py'),
                     os.path.join(base, '..', 'indirect_bridge.py')):
            cand = os.path.abspath(cand)
            if os.path.isfile(cand):
                return cand
    return os.path.join(SUTURA_DIR, 'indirect_bridge.py')


BRIDGE = _resolve_bridge()


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
                if not name.endswith('.model'):
                    continue
                xml = _read_zip_entry(z, name).decode('utf-8', errors='replace')
                m = re.search(r'<model\b[^>]*\bunit="([^"]+)"', xml)
                out[name] = m.group(1) if m else 'millimeter'
    except Exception:
        return {}
    return out


def read_obj(path):
    verts = []
    tris = []
    with open(path) as f:
        for line in f:
            parts = line.split()
            if not parts:
                continue
            if parts[0] == 'v':
                verts.append((float(parts[1]), float(parts[2]), float(parts[3])))
            elif parts[0] == 'f':
                tris.append((int(parts[1]) - 1, int(parts[2]) - 1, int(parts[3]) - 1))
    return np.array(verts, dtype=np.float32), np.array(tris, dtype=np.int32)


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
    the unconditional ``import trimesh`` (and its scipy stack) cost ~0.5 s on
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


def save_mesh(out_path, verts, tris):
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float32),
                        face_matrix=np.asarray(tris, np.int32)))
    ms.save_current_mesh(out_path)


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


def parse_3mf_meshes(path):
    """Return {model_name: [(verts, tris, (start,end))]} for every <mesh> block."""
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


def build_mesh_block(verts, tris):
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


# Decompressed-size cap for any single 3MF archive entry (FAZ14 zip-bomb
# guard). A crafted 3MF can declare a tiny compressed size with a huge
# uncompressed size; we check zipfile's header `file_size` BEFORE reading the
# entry, so a bomb fails with a controlled error instead of exhausting memory.
_3MF_MAX_ENTRY_BYTES = 1 << 30  # 1 GiB


def _read_zip_entry(z, name):
    """Read one 3MF zip entry, bounded by the decompressed-size cap.

    Checks the zip header's declared ``file_size`` BEFORE reading so a crafted
    entry (tiny compressed, huge declared uncompressed) fails with a controlled
    ValueError instead of exhausting memory (FAZ14 zip-bomb guard)."""
    info = z.getinfo(name)
    if info.file_size > _3MF_MAX_ENTRY_BYTES:
        raise ValueError(
            '3MF entry "%s" declares %d bytes uncompressed '
            '(> %d): suspicious compression ratio / oversized input.'
            % (name, info.file_size, _3MF_MAX_ENTRY_BYTES))
    return z.read(name)


def load_meshes(src):
    """Load a mesh file as a list of (model_name|None, verts, tris).

    STL/OBJ give one entry with ``model_name`` None; a 3MF yields one entry
    per object (or an empty list when it has no mesh objects). Shared by
    validate and dry-run so both see exactly what repair would see."""
    import pymeshlab as ml
    ext = os.path.splitext(src)[1].lower()
    if ext == '.3mf':
        out = []
        for name, blocks in parse_3mf_meshes(src).items():
            for vs, ts, _span in blocks:
                out.append((name, np.asarray(vs, dtype=np.float32),
                            np.asarray(ts, dtype=np.int32)))
        return out
    load_ms = ml.MeshSet()
    load_ms.load_new_mesh(src)
    m = load_ms.current_mesh()
    return [(None, np.asarray(m.vertex_matrix(), dtype=np.float32),
             np.asarray(m.face_matrix(), dtype=np.int32))]
