#!/usr/bin/env python3
"""CSG bridge: execute polyhedral boolean operations for repeated-element repair.

Runs under the python3.11 virtualenv (where manifold3d is installed). Reads an input
mesh and a JSON operation list (box creation, intersection, difference, union, transform)
from a temporary directory or arguments, performs exact manifold3d CSG booleans,
and writes the repaired result mesh.
"""

import sys
import os
import json
import numpy as np
import trimesh
import manifold3d as m3d


def write_obj(path, verts, tris):
    """Write vertices and triangles to Wavefront OBJ format."""
    with open(path, 'w') as f:
        f.write('# sutura CSG bridge output\n')
        for v in verts:
            f.write('v %.9g %.9g %.9g\n' % (v[0], v[1], v[2]))
        for t in tris:
            f.write('f %d %d %d\n' % (t[0] + 1, t[1] + 1, t[2] + 1))


def load_mesh(path):
    """Load vertices and triangle faces from NPZ or OBJ/STL file."""
    if path.endswith('.npz'):
        data = np.load(path)
        return np.asarray(data['verts'], dtype=np.float64), np.asarray(data['tris'], dtype=np.int32)
    mesh = trimesh.load(path, force='mesh')
    return np.asarray(mesh.vertices, dtype=np.float64), np.asarray(mesh.faces, dtype=np.int32)


def save_mesh(path, verts, tris):
    """Save vertices and triangles to NPZ or OBJ file."""
    if path.endswith('.npz'):
        np.savez(path, verts=np.asarray(verts, dtype=np.float64), tris=np.asarray(tris, dtype=np.int32))
    else:
        write_obj(path, verts, tris)


def run_csg_bridge(spec, work_dir=None):
    """Execute CSG operation list defined in spec.

    spec format:
    {
        "input_mesh": "input.npz" or "input.obj",
        "output_mesh": "output.npz" or "output.obj",
        "ops": [
            {"op": "box", "result": "src_box", "center": [...], "extents": [...], "frame": [...]},
            {"op": "intersection", "a": "base", "b": "src_box", "result": "element"},
            {"op": "transform", "input": "src_box", "matrix": [...], "result": "dst_box_0"},
            {"op": "difference", "a": "base", "b": "dst_box_0", "result": "base"},
            {"op": "transform", "input": "element", "matrix": [...], "result": "elem_0"},
            {"op": "union", "a": "base", "b": "elem_0", "result": "base"}
        ]
    }
    """
    in_path = spec['input_mesh']
    out_path = spec['output_mesh']
    if work_dir:
        if not os.path.isabs(in_path):
            in_path = os.path.join(work_dir, in_path)
        if not os.path.isabs(out_path):
            out_path = os.path.join(work_dir, out_path)

    verts, tris = load_mesh(in_path)
    if len(verts) < 4 or len(tris) < 4:
        return {'ok': False, 'error': 'Input mesh has too few vertices or triangles'}

    mf = m3d.Manifold(m3d.Mesh(
        vert_properties=np.asarray(verts, dtype=np.float64),
        tri_verts=np.asarray(tris, dtype=np.int32),
    ))
    if mf.status() != m3d.Error.NoError or mf.is_empty():
        return {'ok': False, 'error': f'manifold3d could not process input mesh ({mf.status()})'}

    initial_vol = float(mf.volume())
    solids = {'base': mf}

    for op in spec.get('ops', []):
        op_type = op.get('op')
        res_name = op.get('result', 'base')

        if op_type == 'box':
            extents = op['extents']
            cube = m3d.Manifold.cube(extents, center=True)
            T = np.eye(4)
            T[:3, :3] = np.asarray(op.get('frame', np.eye(3)), dtype=np.float64)
            T[:3, 3] = np.asarray(op.get('center', [0, 0, 0]), dtype=np.float64)
            solids[res_name] = cube.transform(T[:3, :])

        elif op_type in ('intersection', 'intersect'):
            solids[res_name] = solids[op['a']] ^ solids[op['b']]

        elif op_type in ('difference', 'diff'):
            solids[res_name] = solids[op['a']] - solids[op['b']]

        elif op_type in ('union', 'add'):
            solids[res_name] = solids[op['a']] + solids[op['b']]

        elif op_type == 'transform':
            T = np.asarray(op['matrix'], dtype=np.float64)
            solids[res_name] = solids[op['input']].transform(T[:3, :])

        else:
            return {'ok': False, 'error': f'Unsupported CSG operation: {op_type}'}

    target_name = spec.get('target', 'base')
    final_solid = solids.get(target_name)
    if final_solid is None:
        return {'ok': False, 'error': f'Result solid {target_name} not found after operations'}

    out_m = final_solid.to_mesh()
    out_verts = np.asarray(out_m.vert_properties)[:, :3]
    out_tris = np.asarray(out_m.tri_verts)
    save_mesh(out_path, out_verts, out_tris)

    report = {
        'ok': True,
        'input_vertices': int(len(verts)),
        'input_faces': int(len(tris)),
        'output_vertices': int(len(out_verts)),
        'output_faces': int(len(out_tris)),
        'volume_before': initial_vol,
        'volume_after': float(final_solid.volume()),
        'volume_change': float(abs(final_solid.volume() - initial_vol)),
        'is_watertight': bool(final_solid.status() == m3d.Error.NoError) and not final_solid.is_empty(),
        'status': str(final_solid.status()),
    }
    if work_dir:
        report_path = os.path.join(work_dir, 'report.json')
        try:
            with open(report_path, 'w') as f:
                json.dump(report, f)
        except Exception:
            pass

    return report


def main():
    if len(sys.argv) == 2:
        arg = sys.argv[1]
        if os.path.isdir(arg):
            work_dir = arg
            spec_path = os.path.join(arg, 'spec.json')
            if not os.path.isfile(spec_path):
                spec_path = os.path.join(arg, 'ops.json')
        else:
            work_dir = os.path.dirname(os.path.abspath(arg))
            spec_path = arg

        with open(spec_path, 'r') as f:
            spec = json.load(f)

    elif len(sys.argv) == 4:
        in_mesh, out_mesh, ops_json = sys.argv[1], sys.argv[2], sys.argv[3]
        work_dir = os.path.dirname(os.path.abspath(ops_json))
        with open(ops_json, 'r') as f:
            ops_data = json.load(f)
        spec = {
            'input_mesh': in_mesh,
            'output_mesh': out_mesh,
            'ops': ops_data if isinstance(ops_data, list) else ops_data.get('ops', [])
        }

    else:
        print("Usage: csg_bridge.py <work_dir | spec.json | in_mesh out_mesh ops.json>", file=sys.stderr)
        sys.exit(1)

    report = run_csg_bridge(spec, work_dir=work_dir)
    print(json.dumps(report))
    if not report.get('ok'):
        sys.exit(1)


if __name__ == '__main__':
    main()
