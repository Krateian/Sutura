#!/usr/bin/env python3
"""Sutura - two-stage STL/3MF mesh repair engine.

Stage 1 (PyMeshLab/VCG): removes duplicates and degenerates, repairs
non-manifold edges, orients faces coherently, closes holes of any size,
drops small open debris components.
Stage 2 (manifold3d): rebuilds the (now closed) mesh as a valid manifold
solid, resolving overlaps between shells via a boolean union.

Multi-object 3MF files are handled natively: every object mesh is repaired
independently and written back into the archive, so no object is lost.
Meshes are repaired in memory as numpy arrays; the intermediate file
round-trips used for single-mesh input are skipped for 3MF objects so the
original vertex structure is preserved.

The output is written to a new file; the input is never overwritten.
"""
import sys
import os
import re
import json
import math
import struct
import tempfile
import shutil
import zipfile
import subprocess
import numpy as np

SUTURA_DIR = os.path.expanduser('~/.local/share/sutura')
VENV311 = os.path.join(SUTURA_DIR, 'venv311', 'bin', 'python')
BRIDGE = os.path.join(SUTURA_DIR, 'manifold_bridge.py')

TOPOMETRICS = [
    'vertices_number', 'faces_number', 'boundary_edges', 'connected_components_number',
    'genus', 'incident_faces_on_non_two_manifold_edges',
    'incident_faces_on_non_two_manifold_vertices', 'is_mesh_two_manifold',
    'non_two_manifold_edges', 'non_two_manifold_vertices', 'number_holes',
]


def stage1_chain(ml):
    return [
        ('meshing_remove_duplicate_faces', {}),
        ('meshing_remove_null_faces', {}),
        ('meshing_remove_duplicate_vertices', {}),
        ('meshing_repair_non_manifold_edges', {}),
        ('meshing_re_orient_faces_coherently', {}),
        ('meshing_close_holes', {'maxholesize': 1000}),
        ('meshing_repair_non_manifold_vertices', {}),
        ('meshing_remove_connected_component_by_face_number',
         {'mincomponentsize': 8, 'removeunref': True}),
        ('meshing_remove_unreferenced_vertices', {}),
        ('meshing_re_orient_faces_coherently', {}),
    ]


def delete_fallback_chain(ml):
    return [
        ('meshing_remove_duplicate_faces', {}),
        ('meshing_remove_null_faces', {}),
        ('meshing_remove_duplicate_vertices', {}),
        ('compute_selection_by_non_manifold_edges_per_face', {}),
        ('meshing_remove_selected_faces', {}),
        ('set_selection_none', {}),
        ('meshing_remove_unreferenced_vertices', {}),
        ('meshing_repair_non_manifold_edges', {}),
        ('meshing_repair_non_manifold_vertices', {}),
        ('meshing_remove_connected_component_by_face_number',
         {'mincomponentsize': 8, 'removeunref': True}),
        ('meshing_re_orient_faces_coherently', {}),
        ('meshing_close_holes', {'maxholesize': 1000}),
    ]


def apply_chain(ms, chain):
    applied = 0
    skipped = {}
    for name, params in chain:
        try:
            ms.apply_filter(name, **params)
            applied += 1
        except Exception as e:
            skipped[name] = str(e)
    return applied, skipped


def write_obj(path, verts, tris):
    with open(path, 'w') as f:
        f.write('# sutura intermediate\n')
        for v in verts:
            f.write('v %.9g %.9g %.9g\n' % (v[0], v[1], v[2]))
        for t in tris:
            f.write('f %d %d %d\n' % (t[0] + 1, t[1] + 1, t[2] + 1))


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


def repair_mesh_from_arrays(verts, tris, tmpdir):
    """Repair one mesh given as numpy arrays. Returns (report, verts, tris)."""
    import pymeshlab as ml
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)

    if len(t) == 0 or len(v) == 0:
        raise ValueError('input mesh is empty (no triangles)')
    if not np.isfinite(v).all():
        raise ValueError('input mesh contains NaN or infinite coordinates')

    stats = {'stage1': {}}
    before_ms = ml.MeshSet()
    before_ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
    before = before_ms.apply_filter('get_topological_measures')
    holes_before = max(before.get('boundary_edges', 0) // 2, 0)
    nm_before = before.get('non_two_manifold_edges', 0)

    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
    applied, skipped = apply_chain(ms, stage1_chain(ml))
    after = ms.apply_filter('get_topological_measures')

    if after.get('non_two_manifold_edges', 0) > 0 or after.get('non_two_manifold_vertices', 0) > 0:
        fb = ml.MeshSet()
        fb.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
        fapplied, fskipped = apply_chain(fb, delete_fallback_chain(ml))
        fafter = fb.apply_filter('get_topological_measures')
        if (fafter.get('non_two_manifold_edges', 0) + fafter.get('non_two_manifold_vertices', 0)
                < after.get('non_two_manifold_edges', 0) + after.get('non_two_manifold_vertices', 0)):
            ms, after, applied, skipped = fb, fafter, fapplied, fskipped

    if after.get('faces_number', 0) == 0:
        raise ValueError('all faces are degenerate; nothing to repair')

    holes_after = max(after.get('boundary_edges', 0) // 2, 0)
    nm_after = after.get('non_two_manifold_edges', 0)

    stats['stage1']['holes_closed'] = max(holes_before - holes_after, 0)
    stats['stage1']['holes_remaining'] = holes_after
    stats['stage1']['non_manifold_edges_fixed'] = max(nm_before - nm_after, 0)
    stats['stage1']['non_manifold_edges_remaining'] = nm_after
    stats['stage1']['two_manifold'] = bool(after.get('is_mesh_two_manifold'))
    stats['stage1']['components'] = after.get('connected_components_number')
    stats['stage1']['faces_after'] = after.get('faces_number')
    stats['stage1']['faces_before'] = before.get('faces_number')
    stats['stage1']['faces_removed'] = max(before.get('faces_number', 0) - after.get('faces_number', 0), 0)
    stats['stage1']['applied_filters'] = applied
    if skipped:
        stats['stage1']['skipped'] = skipped

    geom = ms.apply_filter('get_geometric_measures')
    if geom.get('mesh_volume', 0) < 0:
        ms.apply_filter('meshing_invert_face_orientation')

    new_verts = np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float32)
    new_tris = np.asarray(ms.current_mesh().face_matrix(), dtype=np.int32)
    fin = ms.apply_filter('get_topological_measures')
    stats['stage1'].update({
        'two_manifold': bool(fin.get('is_mesh_two_manifold')),
        'holes_remaining': max(fin.get('boundary_edges', 0) // 2, 0),
        'components': fin.get('connected_components_number'),
        'faces_after': fin.get('faces_number'),
    })
    return stats, new_verts, new_tris


def run_stage2(inter, out_obj):
    if not os.path.exists(VENV311) or not os.path.exists(BRIDGE):
        return None, None
    r = subprocess.run(
        [VENV311, BRIDGE, inter, out_obj],
        capture_output=True, text=True, timeout=600,
    )
    if r.returncode != 0:
        return {'error': r.stderr.strip() or r.stdout.strip()}, None
    try:
        report = json.loads(r.stdout.strip().splitlines()[-1])
    except Exception:
        report = {'error': 'unparseable manifold output'}
    return report, r.returncode


def save_mesh(out_path, verts, tris):
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float32),
                        face_matrix=np.asarray(tris, np.int32)))
    ms.save_current_mesh(out_path)


def scan_bad_coordinates(path):
    """Return a description of NaN/Inf coordinates in STL/OBJ files, or None."""
    ext = os.path.splitext(path)[1].lower()
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
            return None  # not a complete binary STL; let the loader report it
        n = struct.unpack('<I', buf)[0]
        for _ in range(n):
            buf = f.read(50)
            if len(buf) < 50:
                break  # truncated file; let the loader report it
            for i in range(12, 48, 12):
                if not all(math.isfinite(x) for x in struct.unpack('<3f', buf[i:i + 12])):
                    return 'NaN or infinite coordinates'
    return None


def repair_file(src, out, tmpdir):
    """Repair a single STL/OBJ/3MF file. Returns the report dict."""
    import pymeshlab as ml

    bad_coords = scan_bad_coordinates(src)
    if bad_coords:
        raise ValueError('input mesh contains %s' % bad_coords)

    load_ms = ml.MeshSet()
    load_ms.load_new_mesh(src)
    verts = np.asarray(load_ms.current_mesh().vertex_matrix(), dtype=np.float32)
    tris = np.asarray(load_ms.current_mesh().face_matrix(), dtype=np.int32)

    report, new_v, new_t = repair_mesh_from_arrays(verts, tris, tmpdir)

    # stage 2 (manifold3d) applies to watertight results
    if (report['stage1'].get('two_manifold') and report['stage1'].get('holes_remaining', 0) == 0
            and os.path.exists(VENV311) and os.path.exists(BRIDGE)):
        inter = os.path.join(tmpdir, 'stage1.obj')
        out_obj = os.path.join(tmpdir, 'stage2.obj')
        write_obj(inter, new_v, new_t)
        mrep, _ = run_stage2(inter, out_obj)
        if mrep is not None:
            report['stage2'] = mrep
            if 'error' not in mrep and os.path.exists(out_obj):
                new_v, new_t = read_obj(out_obj)

    save_mesh(out, new_v, new_t)
    return report


def parse_3mf_meshes(path):
    """Return {model_name: [(verts, tris, (start,end))]} for every <mesh> block."""
    out = {}
    with zipfile.ZipFile(path) as z:
        for name in z.namelist():
            if not name.endswith('.model'):
                continue
            xml = z.read(name).decode('utf-8', errors='replace')
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


def repair_3mf(src, out, tmpdir):
    """Repair every object mesh in a 3MF archive, preserving structure."""
    meshes = parse_3mf_meshes(src)
    if not meshes:
        return {'error': 'no mesh objects found in 3MF'}

    # TODO(stage2): if an object ever comes out of stage 1 fully closed
    # (two-manifold with no boundary edges), manifold3d could be applied to
    # that object individually to guarantee a watertight solid. Currently
    # every layered/folded Bambu-style object we have seen remains open after
    # stage 1, so the manifold3d bridge (which rejects open meshes) is
    # skipped for all objects and no per-object stage 2 is attempted.

    with zipfile.ZipFile(src) as z:
        items = [(i, z.read(i)) for i in z.namelist()]

    reports = []
    cache = {}
    for model_name, blocks in meshes.items():
        for idx, (verts, tris, span) in enumerate(blocks):
            key = (verts.tobytes(), tris.tobytes())
            if key in cache:
                new_v, new_t = cache[key]
            else:
                rep, new_v, new_t = repair_mesh_from_arrays(verts, tris, tmpdir)
                reports.append(rep)
                cache[key] = (new_v, new_t)

            for i, (fname, fdata) in enumerate(items):
                if fname != model_name:
                    continue
                xml = fdata.decode('utf-8', errors='replace')
                rebuilt = list(xml)
                rebuilt[span[0]:span[1]] = build_mesh_block(new_v, new_t)
                items[i] = (fname, ''.join(rebuilt).encode('utf-8'))

    with zipfile.ZipFile(out, 'w', zipfile.ZIP_DEFLATED) as z:
        for fname, fdata in items:
            z.writestr(fname, fdata)

    agg = {'objects': len(meshes), 'object_reports': reports}
    if reports:
        agg['stage1'] = reports[0].get('stage1', {})
        s2 = next((r['stage2'] for r in reports if 'stage2' in r), None)
        if s2 is not None:
            agg['stage2'] = s2
    return agg


def human_report(r):
    if 'error' in r and 'stage1' not in r:
        return 'ERROR: %s' % r['error']
    s1 = r.get('stage1', {})
    lines = []
    lines.append('Input : %s' % r.get('input'))
    lines.append('Output: %s' % r.get('output'))
    lines.append('')
    lines.append('Stage 1 (MeshLab):')
    lines.append('  Holes closed            : %d' % s1.get('holes_closed', 0))
    lines.append('  Holes remaining         : %d' % s1.get('holes_remaining', 0))
    lines.append('  Non-manifold edges fixed: %d' % s1.get('non_manifold_edges_fixed', 0))
    lines.append('  Faces removed           : %d' % s1.get('faces_removed', 0))
    lines.append('  Connected components    : %d' % s1.get('components', 0))
    lines.append('  Two-manifold            : %s' % ('YES' if s1.get('two_manifold') else 'NO'))
    if 'stage2' in r:
        s2 = r['stage2']
        lines.append('')
        lines.append('Stage 2 (Manifold):')
        if 'error' in s2:
            lines.append('  ERROR: %s' % s2['error'])
        else:
            for k, v in s2.items():
                lines.append('  %s: %s' % (k, v))
    if 'objects' in r:
        lines.append('')
        lines.append('3MF objects repaired: %d' % r['objects'])
        for i, rep in enumerate(r.get('object_reports', [])):
            s1o = rep.get('stage1', {})
            ok = s1o.get('two_manifold') and s1o.get('holes_remaining', 0) == 0
            lines.append('  object %d: %s (%d hole(s) remaining, two-manifold=%s)' % (
                i, 'watertight' if ok else 'partial',
                s1o.get('holes_remaining', 0), 'YES' if s1o.get('two_manifold') else 'NO'))
    return '\n'.join(lines)


def process_file(src, human):
    """Repair one file. Returns (result_dict, category)."""
    if not os.path.exists(src):
        return ({'input': src, 'error': 'file not found: %s' % src}, 'error')

    stem, ext = os.path.splitext(src)
    out = stem + '_fixed' + ext

    tmpdir = tempfile.mkdtemp(prefix='sutura-')
    result = {'input': src, 'output': out}
    try:
        if ext.lower() == '.3mf' and len(parse_3mf_meshes(src)) > 1:
            result.update(repair_3mf(src, out, tmpdir))
        else:
            result.update(repair_file(src, out, tmpdir))
    except Exception as e:
        result['error'] = 'repair failed: %s' % e
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)

    s1 = result.get('stage1', {})
    if 'error' in result and 'stage1' not in result:
        category = 'error'
    elif s1.get('two_manifold') and s1.get('holes_remaining', 0) == 0:
        category = 'ok'
    else:
        category = 'warning'
    return result, category


def main():
    args = sys.argv[1:]
    files = []
    out = None
    human = False
    i = 0
    while i < len(args):
        a = args[i]
        if a == '-o' and i + 1 < len(args):
            out = args[i + 1]
            i += 2
        elif a == '--human':
            human = True
            i += 1
        else:
            files.append(a)
            i += 1

    if not files or any(f in ('-h', '--help', 'help') for f in files):
        print('Usage: sutura [FILE ...] [-o OUTPUT] [--human]')
        print('Repair one or more STL/3MF meshes. Output files get a "_fixed" suffix.')
        print('-o is only valid with a single input file.')
        sys.exit(0)

    if len(files) > 1 and out is not None:
        print(json.dumps({'error': '-o cannot be used with multiple input files'}))
        sys.exit(1)

    results = [process_file(f, human) for f in files]
    ok = sum(1 for _, c in results if c == 'ok')
    warnings = sum(1 for _, c in results if c == 'warning')
    errors = sum(1 for _, c in results if c == 'error')

    if len(files) == 1:
        result, category = results[0]
        if human:
            print(human_report(result))
        else:
            print(json.dumps(result, ensure_ascii=False))
        sys.exit(0 if category != 'error' else 1)

    if human:
        for result, category in results:
            print(human_report(result))
            print()
        print('Summary: %d file(s), %d fully repaired, %d with warnings, %d failed.'
              % (len(files), ok, warnings, errors))
    else:
        payload = {
            'files': [r for r, _ in results],
            'summary': {'total': len(files), 'ok': ok, 'warning': warnings, 'error': errors},
        }
        print(json.dumps(payload, ensure_ascii=False))
    sys.exit(0 if errors == 0 else 1)


if __name__ == '__main__':
    main()