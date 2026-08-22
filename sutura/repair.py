#!/usr/bin/env python3
"""Sutura - two-stage STL/3MF mesh repair engine.

Stage 1 (PyMeshLab/VCG): clean up, orient, close holes, drop debris.
Stage 2 (manifold3d): rebuild the closed mesh as a watertight solid and
merge overlapping shells.

Multi-object 3MF files are repaired per object and written back, so no
object is lost. Meshes are handled in memory as numpy arrays to preserve
the original vertex structure. Output is a new file; the input is never
overwritten.
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

from classification import classify, issue_label, is_stage2_skipped
from defects import detect as detect_defects
from mesh_classifier import classify_mesh

SUTURA_DIR = os.environ.get('SUTURA_DIR', os.path.expanduser('~/.local/share/sutura'))
VENV311 = os.path.join(SUTURA_DIR, 'venv311', 'bin', 'python')
BRIDGE = os.path.join(SUTURA_DIR, 'manifold_bridge.py')

VERSION = "0.1.8-beta.1"

# Confidence gate for mesh-type-aware Stage 1 tuning: a classified mesh only
# gets its per-type thresholds (see _type_params in repair_mesh_from_arrays)
# when the classifier is reasonably sure; below the gate we use the historical
# default thresholds while still REPORTING the detected type. Class-specific
# because the organic confidence is structurally capped at ~0.62 (sigmoid over
# the coplanar metric), so the organic gate cannot be as high as mechanical.
# Values from scripts/calibrate_classifier.py: 0.75 drops the 3 borderline
# mechanical (lattice/damaged box at ~0.72), 0.55 drops the 2 noisiest
# organic blobs; everything clearly mechanical/organic stays tuned.
MECH_TUNE_GATE = 0.75
ORG_TUNE_GATE = 0.55


def tuning_applied_for(mesh_type, confidence):
    """Whether the tuned Stage 1 thresholds should be used for a classified
    mesh, given the confidence gate. Unknown always returns False (defaults
    are used); a classified mesh must clear its class-specific gate. Returns
    a plain Python bool so the value is JSON-serializable."""
    if mesh_type == 'mechanical':
        return bool(confidence >= MECH_TUNE_GATE)
    if mesh_type == 'organic':
        return bool(confidence >= ORG_TUNE_GATE)
    return False


# Repair modes: a fixed five-step ladder from conservative to aggressive.
# 'auto' is NOT a fixed parameter set - it resolves at repair time through
# classify_mesh + the confidence gate (the shipped default behaviour); the
# fixed modes (low/medium/aggressive/extreme) bypass the classifier entirely
# and use these exact thresholds. 'medium' is the historical default
# (mincomponentsize=8, maxholesize=1000). mincomponentsize stays >= 8 so
# small/degenerate meshes never survive the debris cutoff (CI regression
# risk if lowered, see the _type_params comment).
MODE_PARAMS = {
    'low': {'mincomponentsize': 8, 'maxholesize': 200},
    'medium': {'mincomponentsize': 8, 'maxholesize': 1000},
    'aggressive': {'mincomponentsize': 12, 'maxholesize': 3000},
    'extreme': {'mincomponentsize': 20, 'maxholesize': 10000},
}
REPAIR_MODES = ('low', 'medium', 'auto', 'aggressive', 'extreme')

# Mesh-type-aware Stage 1 thresholds (organic vs mechanical).
#
# These per-type values are ESTIMATED starting points, not calibrated on
# real repair data - a deliberate, conservative, reversible choice. They
# only shift debris/hole-closing thresholds; the classifier reports
# 'unknown' in ambiguous cases and we keep the default parameters, so a
# wrong guess cannot badly distort a mesh.
#
#   mechanical: avoid oversized hole fill on precise geometry (300).
#               mincomponentsize is kept at the default 8 (not lowered):
#               lowering it to 4 let small/degenerate meshes (e.g. the
#               2-triangle case in tests/test_adversarial.py) survive the
#               debris cutoff and be "repaired" instead of rejected - a
#               CI regression (test_adversarial 'degenerate').
#   organic   : aggressively drop scan debris (higher cutoff, 12) and
#               close large open regions (1000, same as default).
#   unknown   : fall back to the historical defaults (8, 1000).
_TYPE_PARAMS = {
    'mechanical': {'mincomponentsize': 8, 'maxholesize': 300},
    'organic': {'mincomponentsize': 12, 'maxholesize': 1000},
    'unknown': {'mincomponentsize': 8, 'maxholesize': 1000},
}


def resolve_mode_params(mode, mesh_type, confidence):
    """Resolve the Stage 1 thresholds for a repair/dry-run run.

    Single source of truth shared by the real repair chain and --dry-run so
    they can never diverge (same principle as classification.py). Returns
    ({mincomponentsize, maxholesize}, tuning_applied).

    ``mode`` is one of REPAIR_MODES: the fixed modes (low/medium/aggressive/
    extreme) use MODE_PARAMS directly (the classifier still runs for the
    informative detected_type/confidence, but does not drive parameters);
    ``auto`` uses the mesh classifier + the class-specific confidence gate.
    """
    if mode != 'auto':
        return dict(MODE_PARAMS[mode]), False
    applied = tuning_applied_for(mesh_type, confidence)
    if not applied:
        return dict(_TYPE_PARAMS['unknown']), False
    return dict(_TYPE_PARAMS.get(mesh_type, _TYPE_PARAMS['unknown'])), True

TOPOMETRICS = [
    'vertices_number', 'faces_number', 'boundary_edges', 'connected_components_number',
    'genus', 'incident_faces_on_non_two_manifold_edges',
    'incident_faces_on_non_two_manifold_vertices', 'is_mesh_two_manifold',
    'non_two_manifold_edges', 'non_two_manifold_vertices', 'number_holes',
]


def stage1_chain(ml, maxholesize=1000, mincomponentsize=8):
    return [
        ('meshing_remove_duplicate_faces', {}),
        ('meshing_remove_null_faces', {}),
        ('meshing_remove_duplicate_vertices', {}),
        ('meshing_repair_non_manifold_edges', {}),
        ('meshing_re_orient_faces_coherently', {}),
        ('meshing_close_holes', {'maxholesize': maxholesize}),
        ('meshing_repair_non_manifold_vertices', {}),
        ('meshing_remove_connected_component_by_face_number',
         {'mincomponentsize': mincomponentsize, 'removeunref': True}),
        ('meshing_remove_unreferenced_vertices', {}),
        ('meshing_re_orient_faces_coherently', {}),
    ]


def delete_fallback_chain(ml, maxholesize=1000, mincomponentsize=8):
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
         {'mincomponentsize': mincomponentsize, 'removeunref': True}),
        ('meshing_re_orient_faces_coherently', {}),
        ('meshing_close_holes', {'maxholesize': maxholesize}),
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


def extreme_extra_passes(ms, ml, params):
    """Extreme-mode extra Stage 1 passes (Stage C).

    After the main chain has run, select and remove self-intersecting faces
    (``compute_selection_by_self_intersections_per_face`` ->
    ``meshing_remove_selected_faces`` -> ``meshing_remove_unreferenced_vertices``)
    and then run the main chain ONE more time with the same thresholds to close
    the new holes / drop the new debris the removal exposed.

    Deliberately NOT meshing_isotropic_explicit_remeshing: a full remesh can
    unpredictably change topology, so it stays out of scope.

    Returns
    (passes_applied, self_intersections_found, self_intersections_removed,
     second_chain_applied, second_chain_skipped).
    When the mesh already has no self-intersecting faces the extra passes are
    skipped harmlessly (no error, nothing removed) and passes_applied is False.
    """
    ms.apply_filter('compute_selection_by_self_intersections_per_face')
    found = int(ms.current_mesh().face_selection_array().sum())
    if found == 0:
        return False, 0, 0, 0, {}
    before_removal = ms.current_mesh().face_number()
    ms.apply_filter('meshing_remove_selected_faces')
    removed = max(before_removal - ms.current_mesh().face_number(), 0)
    ms.apply_filter('meshing_remove_unreferenced_vertices')
    applied2, skipped2 = apply_chain(ms, stage1_chain(ml, **params))
    return True, found, removed, applied2, skipped2


def write_obj(path, verts, tris):
    with open(path, 'w') as f:
        f.write('# sutura intermediate\n')
        for v in verts:
            f.write('v %.9g %.9g %.9g\n' % (v[0], v[1], v[2]))
        for t in tris:
            f.write('f %d %d %d\n' % (t[0] + 1, t[1] + 1, t[2] + 1))


def surface_area(verts, tris):
    """Total triangle surface area of a mesh, computed directly from geometry.

    Works for both open and closed meshes (VCG's geometric-measures volume
    and area are only meaningful on closed meshes). Pure numpy, platform
    independent."""
    if len(tris) == 0 or len(verts) == 0:
        return 0.0
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    a = v[t[:, 0]]
    b = v[t[:, 1]]
    c = v[t[:, 2]]
    cross = np.cross(b - a, c - a)
    return float(np.sum(0.5 * np.linalg.norm(cross, axis=1)))


def signed_volume(verts, tris):
    """Signed volume of a triangle mesh (sum of origin-tetrahedra volumes).

    The sign reflects the winding orientation: a negative value means the
    faces are globally inverted (normals pointing inward). Pure numpy, works
    for open meshes too (the value is then not a real volume, only a signed
    sum - interpret with care)."""
    if len(tris) == 0 or len(verts) == 0:
        return 0.0
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    a = v[t[:, 0]]
    b = v[t[:, 1]]
    c = v[t[:, 2]]
    return float(np.sum(np.einsum('ij,ij->i', a, np.cross(b, c))) / 6.0)


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


def repair_mesh_from_arrays(verts, tris, tmpdir, mode='auto'):
    """Repair one mesh given as numpy arrays. Returns (report, verts, tris).

    ``mode`` is one of REPAIR_MODES: 'auto' (the default) uses the mesh
    classifier + confidence gate exactly as before; the fixed modes
    (low/medium/aggressive/extreme) use the MODE_PARAMS thresholds directly.
    """
    import pymeshlab as ml
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)

    if len(t) == 0 or len(v) == 0:
        raise ValueError('input mesh is empty (no triangles)')
    if not np.isfinite(v).all():
        raise ValueError('input mesh contains NaN or infinite coordinates')

    stats = {'stage1': {}}

    # Mesh-type-aware Stage 1 tuning (organic vs mechanical): resolved through
    # the shared resolve_mode_params so repair and --dry-run stay in sync.
    _cls = classify_mesh(verts, tris)
    stats['detected_type'] = _cls['type']
    stats['detected_confidence'] = _cls['confidence']
    stats['repair_mode'] = mode
    _p, stats['tuning_applied'] = resolve_mode_params(
        mode, _cls['type'], _cls['confidence'])

    before_ms = ml.MeshSet()
    before_ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
    before = before_ms.apply_filter('get_topological_measures')
    before_geom = before_ms.apply_filter('get_geometric_measures')
    before_volume = before_geom.get('mesh_volume', 0)
    before_area = surface_area(v, t)
    before_verts = before.get('vertices_number', 0)
    before_faces = before.get('faces_number', 0)
    holes_before = max(before.get('boundary_edges', 0) // 2, 0)
    nm_before = before.get('non_two_manifold_edges', 0)

    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
    applied, skipped = apply_chain(
        ms, stage1_chain(ml, **_p))
    after = ms.apply_filter('get_topological_measures')

    if after.get('non_two_manifold_edges', 0) > 0 or after.get('non_two_manifold_vertices', 0) > 0:
        fb = ml.MeshSet()
        fb.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
        fapplied, fskipped = apply_chain(
            fb, delete_fallback_chain(ml, **_p))
        fafter = fb.apply_filter('get_topological_measures')
        if (fafter.get('non_two_manifold_edges', 0) + fafter.get('non_two_manifold_vertices', 0)
                < after.get('non_two_manifold_edges', 0) + after.get('non_two_manifold_vertices', 0)):
            ms, after, applied, skipped = fb, fafter, fapplied, fskipped

    if after.get('faces_number', 0) == 0:
        raise ValueError('all faces are degenerate; nothing to repair')

    # Extreme-only extra passes (Stage C): self-intersection cleanup + one more
    # run of the main chain to close the holes / drop the debris the removal
    # exposed. ONLY for mode == 'extreme'; every other mode is untouched.
    # meshing_isotropic_explicit_remeshing (full remesh) is deliberately out of
    # scope: it can unpredictably change topology.
    stats['extreme_passes_applied'] = False
    if mode == 'extreme':
        applied_extra, si_found, si_removed, applied2, skipped2 = \
            extreme_extra_passes(ms, ml, _p)
        stats['extreme_passes_applied'] = applied_extra
        applied += applied2
        if skipped2:
            skipped.update(skipped2)
        if si_found:
            stats['self_intersections_found'] = si_found
            stats['self_intersections_removed'] = si_removed
        after = ms.apply_filter('get_topological_measures')
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
        geom = ms.apply_filter('get_geometric_measures')
    after_volume = geom.get('mesh_volume', 0)

    new_verts = np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float32)
    new_tris = np.asarray(ms.current_mesh().face_matrix(), dtype=np.int32)
    fin = ms.apply_filter('get_topological_measures')
    after_area = surface_area(new_verts, new_tris)
    after_verts = fin.get('vertices_number', 0)
    after_faces = fin.get('faces_number', 0)

    volume_change_pct = 0.0
    if before_volume and after_volume:
        volume_change_pct = (after_volume - before_volume) / abs(before_volume) * 100
    surface_change_pct = 0.0
    if before_area and after_area:
        surface_change_pct = (after_area - before_area) / abs(before_area) * 100
    stats['stage1']['volume_before'] = float(before_volume)
    stats['stage1']['volume_after'] = float(after_volume)
    stats['stage1']['volume_change_percent'] = round(volume_change_pct, 2)
    stats['stage1']['surface_area_before'] = round(before_area, 3)
    stats['stage1']['surface_area_after'] = round(after_area, 3)
    stats['stage1']['surface_area_change_percent'] = round(surface_change_pct, 2)
    if abs(volume_change_pct) > 15:
        stats['stage1']['volume_warning'] = (
            'Volume changed by %.1f%% - verify in slicer before printing.'
            % abs(volume_change_pct))

    stats['stage1'].update({
        'two_manifold': bool(fin.get('is_mesh_two_manifold')),
        'holes_remaining': max(fin.get('boundary_edges', 0) // 2, 0),
        'components': fin.get('connected_components_number'),
        'faces_after': fin.get('faces_number'),
        'vertices_before': int(before_verts),
        'vertices_after': int(after_verts),
        'faces_before': int(before_faces),
        'faces_after': int(after_faces),
    })
    return stats, new_verts, new_tris


def run_stage2(inter, out_obj):
    """Run stage 2. Returns (report, ok); reports a skip, never silence."""
    # 1) primary: the fixed Linux two-venv layout (python3.11 + manifold3d)
    if os.path.exists(VENV311) and os.path.exists(BRIDGE):
        r = subprocess.run(
            [VENV311, BRIDGE, inter, out_obj],
            capture_output=True, text=True, timeout=600,
        )
        if r.returncode == 0:
            try:
                report = json.loads(r.stdout.strip().splitlines()[-1])
            except Exception:
                report = {'error': 'unparseable manifold output'}
            if 'error' not in report:
                return report, True
        # fall through to the in-process attempt if the venv failed

    # 2) single-environment installs (macOS/conda): call the bridge in-process
    try:
        import importlib.util
        spec = importlib.util.spec_from_file_location('sutura_manifold_bridge', BRIDGE)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        report = module.run_bridge(inter, out_obj)
        if 'error' not in report:
            return report, True
    except Exception:
        pass

    # 3) unavailable: report it explicitly, never silently
    return {'error': 'Stage 2 skipped: manifold3d not available in this environment.'}, False


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
        for _ in range(n):
            buf = f.read(50)
            if len(buf) < 50:
                return 'malformed STL (truncated data)'
            for i in range(12, 48, 12):
                if not all(math.isfinite(x) for x in struct.unpack('<3f', buf[i:i + 12])):
                    return 'NaN or infinite coordinates'
    return None


def repair_file(src, out, tmpdir, mode='auto'):
    """Repair a single STL/OBJ/3MF file. Returns the report dict."""
    import pymeshlab as ml

    bad_coords = scan_bad_coordinates(src)
    if bad_coords:
        raise ValueError('input mesh contains %s' % bad_coords)

    load_ms = ml.MeshSet()
    load_ms.load_new_mesh(src)
    verts = np.asarray(load_ms.current_mesh().vertex_matrix(), dtype=np.float32)
    tris = np.asarray(load_ms.current_mesh().face_matrix(), dtype=np.int32)

    report, new_v, new_t = repair_mesh_from_arrays(verts, tris, tmpdir, mode=mode)
    report['defects'] = detect_defects(verts, tris)

    # stage 2 applies to watertight results; run_stage2 handles the fixed
    # venv, in-process, or an explicit "skipped" report.
    if (report['stage1'].get('two_manifold') and report['stage1'].get('holes_remaining', 0) == 0
            and os.path.exists(BRIDGE)):
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


def repair_3mf(src, out, tmpdir, mode='auto'):
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
                rep, new_v, new_t = repair_mesh_from_arrays(verts, tris, tmpdir, mode=mode)
                rep['defects'] = detect_defects(verts, tris)
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


def validate_mesh_from_arrays(verts, tris):
    """Analyze one mesh WITHOUT repairing it.

    Combines defects.detect(), the mesh classifier and cheap pymeshlab
    measures (self-intersecting faces, connected components) into a
    read-only report. Never writes anything."""
    import pymeshlab as ml
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    if len(t) == 0 or len(v) == 0:
        raise ValueError('input mesh is empty (no triangles)')
    if not np.isfinite(v).all():
        raise ValueError('input mesh contains NaN or infinite coordinates')

    cls = classify_mesh(verts, tris)
    d = detect_defects(verts, tris)
    holes = d['holes']
    nm = d['non_manifold']

    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
    topo = ms.apply_filter('get_topological_measures')
    ms.apply_filter('compute_selection_by_self_intersections_per_face')
    self_intersections = int(ms.current_mesh().face_selection_array().sum())

    vol = signed_volume(v, t)
    validation = {
        'vertices': int(v.shape[0]),
        'faces': int(t.shape[0]),
        'holes': holes,
        'non_manifold': nm,
        'self_intersections': self_intersections,
        'connected_components': int(topo.get('connected_components_number', 1)),
        'watertight': bool(len(holes) == 0 and len(nm) == 0 and self_intersections == 0),
        'signed_volume': round(float(vol), 6),
        'surface_area': round(surface_area(v, t), 3),
        'orientation': 'inverted' if vol < 0 else 'consistent',
    }
    return {'validation': validation,
            'detected_type': cls['type'],
            'detected_confidence': cls['confidence']}


def validate_file(src):
    """Validate a mesh file without repairing it. Returns the report dict;
    a hard error (missing/malformed input) is a dict with an 'error' key."""
    result = {'input': src}
    try:
        if not os.path.exists(src):
            raise ValueError('file not found: %s' % src)
        bad_coords = scan_bad_coordinates(src)
        if bad_coords:
            raise ValueError('input mesh contains %s' % bad_coords)
        meshes = load_meshes(src)
        if not meshes:
            raise ValueError('no mesh objects found in 3MF')
        reports = []
        for name, vs, ts in meshes:
            rep = validate_mesh_from_arrays(vs, ts)
            if name is not None:
                rep['model'] = name
            reports.append(rep)
        if len(reports) == 1:
            result.update(reports[0])
        else:
            result['objects'] = len(reports)
            result['object_reports'] = reports
            result['detected_type'] = reports[0].get('detected_type')
            result['detected_confidence'] = reports[0].get('detected_confidence')
    except Exception as e:
        result['error'] = 'validation failed: %s' % e
    return result


def dry_run_mesh_from_arrays(verts, tris, mode='auto'):
    """Report what a repair WOULD do for one mesh, without doing it.

    Detects the type, resolves the mode/thresholds and counts the holes /
    debris / self-intersections. The debris count reuses the same
    meshing_remove_connected_component_by_face_number filter the repair
    chain runs, applied to an in-memory scratch mesh - nothing is written."""
    import pymeshlab as ml
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    if len(t) == 0 or len(v) == 0:
        raise ValueError('input mesh is empty (no triangles)')
    if not np.isfinite(v).all():
        raise ValueError('input mesh contains NaN or infinite coordinates')

    cls = classify_mesh(verts, tris)
    d = detect_defects(verts, tris)
    holes = d['holes']
    nm = d['non_manifold']
    params, tuning = resolve_mode_params(mode, cls['type'], cls['confidence'])

    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
    topo = ms.apply_filter('get_topological_measures')
    # "found" counts describe the INPUT mesh (like defects.detect does), so
    # self-intersections are measured before the debris-removal step below.
    ms.apply_filter('compute_selection_by_self_intersections_per_face')
    self_intersections = int(ms.current_mesh().face_selection_array().sum())
    ms.apply_filter('set_selection_none')
    before_faces = ms.current_mesh().face_number()
    ms.apply_filter('meshing_remove_connected_component_by_face_number',
                    mincomponentsize=params['mincomponentsize'], removeunref=True)
    debris_faces = max(before_faces - ms.current_mesh().face_number(), 0)

    return {
        'repair_mode': mode,
        'detected_type': cls['type'],
        'detected_confidence': cls['confidence'],
        'tuning_applied': tuning,
        'would_apply': params,
        'holes_found': len(holes),
        'largest_hole_diameter': round(max((h['diameter'] for h in holes), default=0.0), 4),
        'non_manifold_regions': len(nm),
        'debris_faces_removable': debris_faces,
        'self_intersections': self_intersections,
        'connected_components': int(topo.get('connected_components_number', 1)),
        'stage2_bridge_available': bool(os.path.exists(BRIDGE)),
    }


def dry_run_file(src, mode='auto'):
    """Dry-run one mesh file. Returns the report dict; a hard error is a
    dict with an 'error' key. Never writes any output file."""
    result = {'input': src}
    try:
        if not os.path.exists(src):
            raise ValueError('file not found: %s' % src)
        bad_coords = scan_bad_coordinates(src)
        if bad_coords:
            raise ValueError('input mesh contains %s' % bad_coords)
        meshes = load_meshes(src)
        if not meshes:
            raise ValueError('no mesh objects found in 3MF')
        reports = []
        for name, vs, ts in meshes:
            rep = dry_run_mesh_from_arrays(vs, ts, mode)
            if name is not None:
                rep['model'] = name
            reports.append(rep)
        if len(reports) == 1:
            result.update(reports[0])
        else:
            result['objects'] = len(reports)
            result['object_reports'] = reports
    except Exception as e:
        result['error'] = 'dry run failed: %s' % e
    return result


def human_defects(r):
    """Render the defect list for --human mode (only with --defects)."""
    d = r.get('defects')
    if not d:
        return None
    lines = ['', 'Defects (input):']
    holes = d.get('holes', [])
    nm = d.get('non_manifold', [])
    if not holes and not nm:
        lines.append('  none')
    for h in holes:
        c = h['centroid']
        lines.append('  hole: centroid=(%.3f, %.3f, %.3f), diameter=%.3f mm, %d verts'
                     % (c[0], c[1], c[2], h['diameter'], h['vertices']))
    for r_ in nm:
        c = r_['centroid']
        lines.append('  non-manifold: centroid=(%.3f, %.3f, %.3f), %d faces'
                     % (c[0], c[1], c[2], r_['faces']))
    return '\n'.join(lines)


def human_report(r, show_defects=False, show_diff=False):
    if 'error' in r and 'stage1' not in r:
        return 'ERROR: %s' % r['error']
    s1 = r.get('stage1', {})
    lines = []
    lines.append('Input : %s' % r.get('input'))
    lines.append('Output: %s' % r.get('output'))
    lines.append('Mode  : %s' % r.get('repair_mode', 'auto'))
    dt = r.get('detected_type')
    if dt:
        conf = r.get('detected_confidence', 0.0)
        tuning = r.get('tuning_applied')
        line = 'Type  : %s (confidence %.2f)' % (dt, conf)
        if tuning is not None:
            line += ' - tuned thresholds' if tuning else ' - default thresholds (below confidence gate)'
        lines.append(line)
    lines.append('')
    lines.append('Stage 1 (MeshLab):')
    lines.append('  Holes closed            : %d' % s1.get('holes_closed', 0))
    lines.append('  Holes remaining         : %d' % s1.get('holes_remaining', 0))
    lines.append('  Non-manifold edges fixed: %d' % s1.get('non_manifold_edges_fixed', 0))
    lines.append('  Faces removed           : %d' % s1.get('faces_removed', 0))
    lines.append('  Connected components    : %d' % s1.get('components', 0))
    lines.append('  Two-manifold            : %s' % ('YES' if s1.get('two_manifold') else 'NO'))
    if r.get('extreme_passes_applied'):
        lines.append('  Extreme passes          : applied (%d self-intersecting face(s) removed)'
                     % r.get('self_intersections_removed', 0))
    elif r.get('repair_mode') == 'extreme':
        lines.append('  Extreme passes          : none needed (no self-intersections)')
    if show_diff:
        lines.append('  Vertices                : %s -> %s' % (
            s1.get('vertices_before', 0), s1.get('vertices_after', 0)))
        lines.append('  Faces                   : %s -> %s' % (
            s1.get('faces_before', 0), s1.get('faces_after', 0)))
        if s1.get('surface_area_change_percent') is not None:
            lines.append('  Surface area change     : %s%%' % s1.get('surface_area_change_percent'))
    if s1.get('volume_change_percent') is not None:
        lines.append('  Volume change          : %s%%' % s1.get('volume_change_percent'))
    if s1.get('volume_warning'):
        lines.append('')
        lines.append('  WARNING: %s' % s1['volume_warning'])
    if 'stage2' in r:
        s2 = r['stage2']
        lines.append('')
        lines.append('Stage 2 (Manifold):')
        if 'error' in s2:
            if s2['error'].startswith('Stage 2 skipped'):
                lines.append('  SKIPPED: %s' % s2['error'])
            else:
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
            if show_diff:
                lines.append('    vertices %s -> %s, faces %s -> %s, surface %s%%' % (
                    s1o.get('vertices_before', 0), s1o.get('vertices_after', 0),
                    s1o.get('faces_before', 0), s1o.get('faces_after', 0),
                    s1o.get('surface_area_change_percent', 0)))
            if show_defects:
                ds = human_defects(rep)
                if ds:
                    lines.append(ds)
    elif show_defects:
        ds = human_defects(r)
        if ds:
            lines.append(ds)
    return '\n'.join(lines)


def human_validate(r, show_defects=False):
    """Render a validate report for --human mode."""
    if 'error' in r and 'validation' not in r:
        return 'ERROR: %s' % r['error']
    lines = ['Input : %s' % r.get('input')]
    if 'object_reports' in r:
        lines.append('3MF objects validated: %d' % r.get('objects', 0))
        for i, rep in enumerate(r.get('object_reports', [])):
            v = rep.get('validation', {})
            lines.append('  object %d (%s): %d verts, %d faces, %d hole(s), '
                         '%d non-manifold region(s), %d self-intersection(s), watertight=%s' % (
                i, rep.get('model', '?'), v.get('vertices', 0), v.get('faces', 0),
                len(v.get('holes', [])), len(v.get('non_manifold', [])),
                v.get('self_intersections', 0), 'YES' if v.get('watertight') else 'NO'))
        return '\n'.join(lines)
    v = r.get('validation', {})
    lines.append('Type  : %s (confidence %.2f)' % (
        r.get('detected_type', '?'), r.get('detected_confidence', 0.0)))
    lines.append('')
    lines.append('Validation:')
    lines.append('  Vertices            : %d' % v.get('vertices', 0))
    lines.append('  Faces               : %d' % v.get('faces', 0))
    lines.append('  Holes               : %d' % len(v.get('holes', [])))
    lines.append('  Non-manifold regions: %d' % len(v.get('non_manifold', [])))
    lines.append('  Self-intersections  : %d' % v.get('self_intersections', 0))
    lines.append('  Connected components: %d' % v.get('connected_components', 0))
    lines.append('  Watertight          : %s' % ('YES' if v.get('watertight') else 'NO'))
    lines.append('  Signed volume       : %.6f' % v.get('signed_volume', 0.0))
    lines.append('  Surface area        : %.3f' % v.get('surface_area', 0.0))
    lines.append('  Orientation         : %s' % v.get('orientation', '?'))
    if show_defects:
        for h in v.get('holes', []):
            c = h['centroid']
            lines.append('    hole: centroid=(%.3f, %.3f, %.3f), diameter=%.3f, %d verts'
                         % (c[0], c[1], c[2], h['diameter'], h['vertices']))
        for nm in v.get('non_manifold', []):
            c = nm['centroid']
            lines.append('    non-manifold: centroid=(%.3f, %.3f, %.3f), %d faces'
                         % (c[0], c[1], c[2], nm['faces']))
    return '\n'.join(lines)


def human_dry_run(r):
    """Render a --dry-run report for --human mode."""
    if 'error' in r and 'repair_mode' not in r:
        return 'ERROR: %s' % r['error']
    lines = ['Input : %s' % r.get('input'),
             'Dry-run: no output file will be written']
    if 'object_reports' in r:
        lines.append('3MF objects: %d' % r.get('objects', 0))
        for i, rep in enumerate(r.get('object_reports', [])):
            lines.append('  object %d (%s): mode=%s, type=%s, holes=%d, '
                         'non-manifold=%d, debris=%d face(s), self-intersections=%d' % (
                i, rep.get('model', '?'), rep.get('repair_mode', '?'),
                rep.get('detected_type', '?'), rep.get('holes_found', 0),
                rep.get('non_manifold_regions', 0), rep.get('debris_faces_removable', 0),
                rep.get('self_intersections', 0)))
        return '\n'.join(lines)
    lines.append('Mode  : %s' % r.get('repair_mode', 'auto'))
    lines.append('Type  : %s (confidence %.2f)' % (
        r.get('detected_type', '?'), r.get('detected_confidence', 0.0)))
    pa = r.get('would_apply', {})
    tuning = 'tuned thresholds' if r.get('tuning_applied') else 'default thresholds'
    lines.append('Tuning: %s' % tuning)
    lines.append('Would apply: mincomponentsize=%d, maxholesize=%d' % (
        pa.get('mincomponentsize', 0), pa.get('maxholesize', 0)))
    lines.append('Found  : %d hole(s) (largest %.3f), %d non-manifold region(s), '
                 '%d self-intersection(s), %d debris face(s) (< mincomponentsize)' % (
        r.get('holes_found', 0), r.get('largest_hole_diameter', 0.0),
        r.get('non_manifold_regions', 0), r.get('self_intersections', 0),
        r.get('debris_faces_removable', 0)))
    s2 = r.get('stage2_bridge_available')
    lines.append('Stage 2: %s' % (
        'would run if stage 1 closes the mesh (bridge available)' if s2
        else 'not available on this system'))
    return '\n'.join(lines)


def process_file(src, human, mode='auto'):
    """Repair one file. Returns (result_dict, category)."""
    if not os.path.exists(src):
        return ({'input': src, 'error': 'file not found: %s' % src}, 'error')

    stem, ext = os.path.splitext(src)
    out = stem + '_fixed' + ext

    tmpdir = tempfile.mkdtemp(prefix='sutura-')
    result = {'input': src, 'output': out}
    try:
        if ext.lower() == '.3mf' and len(parse_3mf_meshes(src)) > 1:
            result.update(repair_3mf(src, out, tmpdir, mode=mode))
        else:
            result.update(repair_file(src, out, tmpdir, mode=mode))
    except Exception as e:
        result['error'] = 'repair failed: %s' % e
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)

    category, issues, _summary = classify(result)
    result['category'] = category
    result['issues'] = issues
    return result, category


def main():
    import argparse
    parser = argparse.ArgumentParser(
        prog='sutura',
        description='Repair one or more STL/3MF meshes. Output files get a "_fixed" suffix.')
    parser.add_argument('files', nargs='*', metavar='FILE',
                        help='input mesh file(s)')
    parser.add_argument('-o', '--output', metavar='OUTPUT',
                        help='output file (only valid with a single input)')
    parser.add_argument('--human', action='store_true',
                        help='print a human-readable report')
    parser.add_argument('--defects', action='store_true',
                        help='with --human, also list input defects (holes / '
                             'non-manifold regions). JSON always includes defects.')
    parser.add_argument('--diff', action='store_true',
                        help='with --human, also show before/after geometry '
                             'diff (vertices, faces, surface area change). '
                             'JSON always includes these fields.')
    parser.add_argument('--mode', choices=REPAIR_MODES, default='auto',
                        help='repair mode: low/medium/aggressive/extreme use '
                             'fixed Stage 1 thresholds; auto (default) uses '
                             'the mesh classifier + confidence gate.')
    parser.add_argument('--dry-run', action='store_true',
                        help='do not repair: report what would be done and '
                             'write no output file')
    parser.add_argument('--version', action='version', version='%(prog)s ' + VERSION)
    args = parser.parse_args()
    files = args.files
    out = args.output
    human = args.human
    show_defects = args.defects
    show_diff = args.diff
    mode = args.mode
    dry_run = args.dry_run

    if len(files) > 1 and out is not None:
        print(json.dumps({'error': '-o cannot be used with multiple input files'}))
        sys.exit(1)

    # 'validate' as the first positional argument switches to validate-only
    # mode: analyze the mesh(es) without repairing or writing anything.
    if files and files[0] == 'validate':
        targets = files[1:]
        if not targets:
            print(json.dumps({'error': 'validate requires at least one input file'}))
            sys.exit(1)
        if out is not None:
            print(json.dumps({'error': '-o is not valid with validate'}))
            sys.exit(1)
        if dry_run:
            print(json.dumps({'error': '--dry-run is not valid with validate'}))
            sys.exit(1)
        results = [validate_file(f) for f in targets]
        nerr = sum(1 for r in results if 'error' in r)
        if len(targets) == 1:
            result = results[0]
            if human:
                print(human_validate(result, show_defects=show_defects))
            else:
                print(json.dumps(result, ensure_ascii=False))
            sys.exit(0 if nerr == 0 else 1)
        if human:
            for result in results:
                print(human_validate(result, show_defects=show_defects))
                print()
        else:
            print(json.dumps({'files': results}, ensure_ascii=False))
        sys.exit(0 if nerr == 0 else 1)

    # --dry-run: analyze and report the plan, write no output file at all.
    if dry_run:
        if out is not None:
            print(json.dumps({'error': '-o is not valid with --dry-run'}))
            sys.exit(1)
        results = [dry_run_file(f, mode=mode) for f in files]
        nerr = sum(1 for r in results if 'error' in r)
        if len(files) == 1:
            result = results[0]
            if human:
                print(human_dry_run(result))
            else:
                print(json.dumps(result, ensure_ascii=False))
            sys.exit(0 if nerr == 0 else 1)
        if human:
            for result in results:
                print(human_dry_run(result))
                print()
        else:
            print(json.dumps({'files': results}, ensure_ascii=False))
        sys.exit(0 if nerr == 0 else 1)

    results = [process_file(f, human, mode=mode) for f in files]
    ok = sum(1 for _, c in results if c == 'watertight')
    warnings = sum(1 for _, c in results if c == 'warning')
    errors = sum(1 for _, c in results if c == 'error')

    # issue counts across the whole batch (e.g. volume_warning: 3)
    issue_counts = {}
    for r, _ in results:
        for code in r.get('issues', []):
            issue_counts[code] = issue_counts.get(code, 0) + 1

    if len(files) == 1:
        result, category = results[0]
        if human:
            print(human_report(result, show_defects=show_defects, show_diff=show_diff))
        else:
            print(json.dumps(result, ensure_ascii=False))
        sys.exit(0 if category != 'error' else 1)

    if human:
        for result, category in results:
            print(human_report(result, show_defects=show_defects, show_diff=show_diff))
            print()
        print('Summary: %d file(s), %d watertight, %d with warnings, %d failed.'
              % (len(files), ok, warnings, errors))
        for code, n in issue_counts.items():
            print('  - %s: %d dosya' % (issue_label(code), n))
    else:
        payload = {
            'files': [r for r, _ in results],
            'summary': {
                'total': len(files), 'ok': ok, 'warning': warnings, 'error': errors,
                'issue_counts': issue_counts,
            },
        }
        print(json.dumps(payload, ensure_ascii=False))
    sys.exit(0 if errors == 0 else 1)


if __name__ == '__main__':
    main()