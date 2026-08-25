#!/usr/bin/env python3
"""Generate a deliberately broken OBJ for testing Sutura (mirrors
make_broken_stl.py's defect set, as ASCII OBJ).

The output mesh has:
  * a missing face (hole),
  * one inverted winding (wrong normal),
  * a duplicated face (non-manifold edges),
  * a fin triangle attached by an edge (non-manifold edge),
  * a triangle crossing through the body (self-intersection).

With --material the OBJ also references mtllib/usemtl (a broken.mtl file is
written next to it) so the material_discarded warning can be exercised.

Usage: make_broken_obj.py [OUTPUT] [--material]
Default output: broken.obj in the current directory.
"""
import os
import sys

V = {
    'lb': (-1, -1, -1), 'rb': (1, -1, -1), 'rt': (1, 1, -1), 'lt': (-1, 1, -1),
    'lT': (-1, -1, 1), 'rT': (1, -1, 1), 'RT': (1, 1, 1), 'LT': (-1, 1, 1),
}


def tri(a, b, c):
    return (V[a], V[b], V[c])


def build_broken_triangles():
    faces = [
        tri('lb', 'rb', 'rt'), tri('lb', 'rt', 'lt'),   # bottom
        tri('lT', 'LT', 'RT'), tri('lT', 'RT', 'rT'),   # top (removed)
        tri('lb', 'lT', 'rT'), tri('lb', 'rT', 'rb'),   # back
        tri('rb', 'rT', 'RT'), tri('rb', 'RT', 'rt'),   # right
        tri('rt', 'RT', 'LT'), tri('rt', 'LT', 'lt'),   # front
        tri('lt', 'LT', 'lT'), tri('lt', 'lT', 'lb'),   # left
    ]
    broken = faces[:2] + faces[4:]          # cube without the top face (hole)
    broken[2] = (broken[2][2], broken[2][1], broken[2][0])  # inverted winding
    broken += [broken[4]]                   # duplicated face
    broken += [((0, 0, 0), (1, -1, 1), (-1, -1, 1))]        # fin on an edge
    broken += [((2, 0, 0), (0, 2, 0), (0, 0, 2))]           # crossing triangle
    return broken


def _as_obj(triangles):
    """Deduplicate vertices, return (vertices, 1-based face index list)."""
    verts = []
    index = {}
    faces = []
    for t in triangles:
        face = []
        for v in t:
            k = tuple(round(x, 6) for x in v)
            if k not in index:
                index[k] = len(verts)
                verts.append(v)
            face.append(index[k] + 1)   # OBJ indices are 1-based
        faces.append(face)
    return verts, faces


def write_obj(path, material=False):
    triangles = build_broken_triangles()
    verts, faces = _as_obj(triangles)
    with open(path, 'w') as f:
        f.write('# broken mesh for repair test\n')
        if material:
            f.write('mtllib broken.mtl\n')
        for v in verts:
            f.write('v %.6f %.6f %.6f\n' % (v[0], v[1], v[2]))
        if material:
            f.write('usemtl redmat\n')
        for face in faces:
            f.write('f %d %d %d\n' % tuple(face))
    if material:
        mtl = os.path.join(os.path.dirname(os.path.abspath(path)) or '.',
                           'broken.mtl')
        with open(mtl, 'w') as f:
            f.write('newmtl redmat\nKd 1 0 0\n')
    print('wrote %d triangles to %s' % (len(triangles), path))


def main():
    args = [a for a in sys.argv[1:] if not a.startswith('--')]
    material = '--material' in sys.argv[1:]
    out = args[0] if args else 'broken.obj'
    write_obj(out, material=material)


if __name__ == '__main__':
    main()