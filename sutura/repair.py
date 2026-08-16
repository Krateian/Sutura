#!/usr/bin/env python3
"""Sutura - two-stage STL/3MF mesh repair engine.

Stage 1 (PyMeshLab/VCG): removes duplicates and degenerates, repairs
non-manifold edges, orients faces coherently, closes holes of any size,
drops small open debris components.
Stage 2 (manifold3d): rebuilds the (now closed) mesh as a valid manifold
solid, resolving overlaps between shells via a boolean union.

The output is written to a new file; the input is never overwritten.
"""
import sys
import os
import json
import tempfile
import shutil
import subprocess

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


def run_stage1(src):
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.load_new_mesh(src)

    before = ms.apply_filter('get_topological_measures')
    # The topological-measures filter can mutate the mesh on non-manifold
    # input; reload so the repair chain sees the pristine model.
    ms.load_new_mesh(src)

    stats = {'stage1': {}}
    holes_before = max(before.get('boundary_edges', 0) // 2, 0)
    nm_before = before.get('non_two_manifold_edges', 0)

    applied = 0
    for name, params in stage1_chain(ml):
        try:
            ms.apply_filter(name, **params)
            applied += 1
        except Exception as e:
            stats['stage1']['skipped'] = stats['stage1'].get('skipped', {})
            stats['stage1']['skipped'][name] = str(e)

    after = ms.apply_filter('get_topological_measures')
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

    tmp = tempfile.mkdtemp(prefix='sutura-')
    inter = os.path.join(tmp, 'intermediate.obj')
    ms.save_current_mesh(inter)
    return inter, stats['stage1'], tmp


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
    return '\n'.join(lines)


def main():
    args = sys.argv[1:]
    src = None
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
            src = a
            i += 1

    if not src or not os.path.exists(src):
        if src in ('-h', '--help', 'help'):
            print('Usage: sutura [FILE] [-o OUTPUT] [--human]')
            print('Repair an STL/3MF mesh. Output is written with a "_fixed" suffix.')
            sys.exit(0)
        print(json.dumps({'error': 'file not found: %s' % src}))
        sys.exit(1)

    stem, ext = os.path.splitext(src)
    if out is None:
        out = stem + '_fixed' + ext

    result = {'input': src, 'output': out}
    try:
        inter, s1, tmpdir = run_stage1(src)
    except Exception as e:
        print(json.dumps({'error': 'stage 1 error: %s' % e}))
        sys.exit(1)

    result['stage1'] = s1

    final_obj = inter
    if os.path.exists(VENV311) and os.path.exists(BRIDGE):
        out_obj = os.path.join(tmpdir, 'stage2.obj')
        mrep, _ = run_stage2(inter, out_obj)
        if mrep is not None:
            result['stage2'] = mrep
            if 'error' not in mrep and os.path.exists(out_obj):
                final_obj = out_obj

    try:
        import pymeshlab as ml
        ms = ml.MeshSet()
        ms.load_new_mesh(final_obj)
        geom = ms.apply_filter('get_geometric_measures')
        if geom.get('mesh_volume', 0) < 0:
            ms.apply_filter('meshing_invert_face_orientation')
        ms.save_current_mesh(out)
        after = ms.apply_filter('get_topological_measures')
        result['stage1'].update({
            'two_manifold': bool(after.get('is_mesh_two_manifold')),
            'holes_remaining': max(after.get('boundary_edges', 0) // 2, 0),
            'components': after.get('connected_components_number'),
            'faces_after': after.get('faces_number'),
        })
    except Exception as e:
        result['error'] = 'save error: %s' % e

    shutil.rmtree(tmpdir, ignore_errors=True)

    if human:
        print(human_report(result))
    else:
        print(json.dumps(result, ensure_ascii=False))
    sys.exit(0)


if __name__ == '__main__':
    main()