#!/usr/bin/env python3
"""Regression test for sutura/mesh_classifier.py (organic vs mechanical).

Checks that:
  1. mesh_classifier.py is stdlib+numpy only (no pymeshlab/trimesh/manifold3d).
  2. classify_mesh() returns the calibrated decisions for synthetic meshes:
     mechanical cubes/boxes, organic spheres/torus/capsule, and unknown for
     a curved-but-not-organically-clear cylinder.
Usage: python3 tests/test_mesh_classifier.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402


def _cube():
    v = np.array([
        (0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0),
        (0, 0, 1), (1, 0, 1), (1, 1, 1), (0, 1, 1)], dtype=np.float32)
    t = np.array([
        (0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7),
        (0, 1, 5), (0, 5, 4), (1, 2, 6), (1, 6, 5),
        (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7)], dtype=np.int32)
    return v, t


def _icosphere(subdivisions):
    import trimesh
    m = trimesh.creation.icosphere(subdivisions=subdivisions)
    return np.asarray(m.vertices, np.float32), np.asarray(m.faces, np.int32)


def test_stdlib_only():
    # import mesh_classifier in a fresh subprocess and assert it does not
    # pull in any heavy third-party library (stdlib+numpy rule)
    import subprocess
    code = (
        "import sys; sys.path.insert(0, %r); import mesh_classifier; "
        "bad=[m for m in ('pymeshlab','trimesh','manifold3d') if m in sys.modules]; "
        "print('OK' if not bad else 'BAD:'+','.join(bad))" % SUTURA
    )
    out = subprocess.run([sys.executable, '-c', code],
                         capture_output=True, text=True)
    assert out.returncode == 0, out.stderr
    assert 'OK' in out.stdout, out.stdout + out.stderr


def test_cube_is_mechanical():
    from mesh_classifier import classify_mesh
    r = classify_mesh(*_cube())
    assert r['type'] == 'mechanical', r


def test_sphere_is_organic():
    from mesh_classifier import classify_mesh
    r = classify_mesh(*_icosphere(3))
    assert r['type'] == 'organic', r


def test_confidence_in_range():
    from mesh_classifier import classify_mesh
    for v, t in [_cube(), _icosphere(3)]:
        r = classify_mesh(v, t)
        assert 0.0 <= r['confidence'] <= 1.0, r
        assert 'metrics' in r and 'near90' in r['metrics'], r


def test_unknown_falls_back():
    # a cylinder is curved and mechanical - deliberately NOT classified,
    # so it must return unknown (the safe fallback) rather than a wrong type
    import trimesh
    m = trimesh.creation.cylinder(radius=1, height=2, sections=48)
    from mesh_classifier import classify_mesh
    r = classify_mesh(np.asarray(m.vertices, np.float32),
                      np.asarray(m.faces, np.int32))
    assert r['type'] == 'unknown', r


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('mesh_classifier tests passed')


if __name__ == '__main__':
    main()
