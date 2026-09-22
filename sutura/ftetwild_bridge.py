#!/usr/bin/env python3
"""fTetWild fallback bridge: tetrahedralize a broken surface and extract the
boundary as a watertight, self-intersection-free triangle mesh.

Guaranteed-correct fallback tier (FAZ17) for meshes where the stage-1 chain
(and --experimental-autorefine) still leave residual self-intersections.
fTetWild (MPL-2.0, via the pytetwild wrapper) tetrahedralizes within an
epsilon-envelope using floating-point construction, then the boundary surface
of the tet mesh is extracted here as the repaired output.

Runs under the python3.11 virtualenv (like manifold_bridge.py) or in-process
in a single-env install. Reads an input surface mesh (STL/OBJ), calls
pytetwild.tetrahedralize(), keeps the triangle faces incident to exactly ONE
tetrahedron (the boundary), orients them outward/consistently, writes an OBJ
for the caller and prints a JSON report.

Both pytetwild and pyvista are required (pytetwild imports pyvista at import
time despite declaring it optional); when either is missing the bridge reports
an explicit skip, never a crash.
"""
import json
import sys
import time

import numpy as np


def write_obj(path, verts, tris):
    with open(path, 'w') as f:
        f.write('# ftetwild bridge output\n')
        for v in verts:
            f.write('v %.9g %.9g %.9g\n' % (v[0], v[1], v[2]))
        for t in tris:
            f.write('f %d %d %d\n' % (t[0] + 1, t[1] + 1, t[2] + 1))


def extract_boundary(verts, tets):
    """Boundary triangles of a tet mesh: faces incident to exactly one tet,
    wound outward (normal pointing away from the tet's 4th vertex)."""
    verts = np.asarray(verts, dtype=np.float64)
    tets = np.asarray(tets, dtype=np.int64)
    keyed = {}
    for ti, tet in enumerate(tets):
        a, b, c, d = tet
        for v0, v1, v2, opp in ((a, b, c, d), (a, b, d, c),
                                (a, c, d, b), (b, c, d, a)):
            key = tuple(sorted((int(v0), int(v1), int(v2))))
            keyed.setdefault(key, []).append((v0, v1, v2, opp))
    out = []
    for key, recs in keyed.items():
        if len(recs) != 1:
            continue
        v0, v1, v2, opp = recs[0]
        n = np.cross(verts[v1] - verts[v0], verts[v2] - verts[v0])
        if np.dot(n, verts[opp] - verts[v0]) > 0:
            v1, v2 = v2, v1
        out.append((int(v0), int(v1), int(v2)))
    return out


def run_bridge(src, dst):
    """Tetrahedralize the surface mesh at src and write the boundary surface
    to dst. Returns the report dict; ``ok`` is True on success. Never raises
    for missing dependencies (reports an explicit skip instead)."""
    report = {}
    try:
        import trimesh
        import pytetwild  # noqa: F401  (imports pyvista internally)
    except ImportError as e:
        report['error'] = ('fTetWild fallback skipped: pytetwild/pyvista not '
                           'available in this environment (%s)' % e)
        return report

    t0 = time.perf_counter()
    try:
        mesh = trimesh.load(src, force='mesh')
        verts = np.asarray(mesh.vertices, dtype=np.float64)
        tris = np.asarray(mesh.faces, dtype=np.int32)
        report['input_vertices'] = int(len(verts))
        report['input_faces'] = int(len(tris))
        tmesh_v, tmesh_c = pytetwild.tetrahedralize(
            verts, tris, quiet=True)
        report['time'] = round(time.perf_counter() - t0, 2)
        report['tet_vertices'] = int(len(tmesh_v))
        report['tet_cells'] = int(len(tmesh_c))
        boundary = extract_boundary(tmesh_v, tmesh_c)
        report['output_vertices'] = int(len(tmesh_v))
        report['output_faces'] = int(len(boundary))
        write_obj(dst, tmesh_v, boundary)
        report['ok'] = True
    except Exception as e:  # noqa: BLE001 - a bad mesh must never crash a repair
        report['error'] = '%s: %s' % (type(e).__name__, e)
        report['time'] = round(time.perf_counter() - t0, 2)
    return report


def main():
    src, dst = sys.argv[1], sys.argv[2]
    report = run_bridge(src, dst)
    print(json.dumps(report))


if __name__ == '__main__':
    main()