#!/usr/bin/env python3
"""Generate a deliberately broken STL for testing Sutura.

The output mesh has:
  * a missing face (hole),
  * one inverted winding (wrong normal),
  * a duplicated face (non-manifold edges),
  * a fin triangle attached by an edge (non-manifold edge),
  * a triangle crossing through the body (self-intersection).

Usage: make_broken_stl.py [OUTPUT]
Default output: broken.stl in the current directory.
"""
import struct
import sys

V = {
    'lb': (-1, -1, -1), 'rb': (1, -1, -1), 'rt': (1, 1, -1), 'lt': (-1, 1, -1),
    'lT': (-1, -1, 1), 'rT': (1, -1, 1), 'RT': (1, 1, 1), 'LT': (-1, 1, 1),
}


def tri(a, b, c):
    return (V[a], V[b], V[c])


def write_stl(path, triangles):
    with open(path, 'wb') as f:
        f.write(b'broken mesh for repair test'.ljust(80, b'\0'))
        f.write(struct.pack('<I', len(triangles)))
        for a, b, c in triangles:
            f.write(struct.pack('<3f', 0, 0, 0))
            f.write(struct.pack('<3f', *a))
            f.write(struct.pack('<3f', *b))
            f.write(struct.pack('<3f', *c))
            f.write(struct.pack('<H', 0))


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else 'broken.stl'

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

    write_stl(out, broken)
    print('wrote %d triangles to %s' % (len(broken), out))


if __name__ == '__main__':
    main()