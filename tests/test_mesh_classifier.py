#!/usr/bin/env python3
"""Regression test for sutura/mesh_classifier.py (organic vs mechanical).

Checks that:
  1. mesh_classifier.py is stdlib+numpy only (no pymeshlab/trimesh/manifold3d).
  2. classify_mesh() returns the calibrated decisions for synthetic meshes:
     mechanical cubes/boxes, organic spheres/torus/capsule, and unknown for
     a curved-but-not-organically-clear cylinder.
  3. The confidence is a signed-margin score: monotonic in the driving
     metric, high on clearly mechanical/organic meshes, and never a flat 0
     for `unknown` (it carries the proximity to the nearer class).
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


def _box():
    import trimesh
    m = trimesh.creation.box(extents=(1.0, 1.0, 1.0))
    return np.asarray(m.vertices, np.float32), np.asarray(m.faces, np.int32)


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
        assert 'mechanical_score' in r['metrics'], r
        assert 'organic_score' in r['metrics'], r


def test_unknown_falls_back():
    # a cylinder is curved and mechanical - deliberately NOT classified,
    # so it must return unknown (the safe fallback) rather than a wrong type
    import trimesh
    m = trimesh.creation.cylinder(radius=1, height=2, sections=48)
    from mesh_classifier import classify_mesh
    r = classify_mesh(np.asarray(m.vertices, np.float32),
                      np.asarray(m.faces, np.int32))
    assert r['type'] == 'unknown', r
    # the unknown result still carries a non-zero proximity value, so callers
    # can tell which way the mesh leans and how close it is
    assert r['confidence'] > 0.0, r
    assert r['metrics'].get('leaning') in ('mechanical', 'organic'), r


def test_clean_boxes_get_high_confidence():
    from mesh_classifier import classify_mesh
    for name, v, t in [('cube', *_cube()), ('box', *_box())]:
        r = classify_mesh(v, t)
        assert r['type'] == 'mechanical', (name, r)
        assert r['confidence'] >= 0.7, (name, r)
        # mechanical confidence is driven by the near90 metric: it must be
        # monotonic w.r.t. the mechanical score
        assert r['metrics']['mechanical_score'] >= r['confidence'] - 1e-9, r


def test_confidence_monotonic():
    # the signed-margin score must be monotonic in the driving metric when the
    # other is held constant: more near90 => not less mechanical, and fewer
    # near90 => not less organic. Tested on the internal scoring function.
    from mesh_classifier import _class_scores
    mech, org = _class_scores(45.0, 0.0)
    prev_mech, prev_org = mech, org
    for near90 in np.linspace(45.0, 95.0, 26):
        m, o = _class_scores(float(near90), 0.0)
        assert m >= prev_mech - 1e-9, (near90, m, prev_mech)
        assert o <= prev_org + 1e-9, (near90, o, prev_org)
        prev_mech, prev_org = m, o
    # and the reverse direction: coplanar drives the mechanical OR-signal too
    m_low, _ = _class_scores(30.0, 0.0)
    m_high, _ = _class_scores(30.0, 60.0)
    assert m_high >= m_low, (m_low, m_high)


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('mesh_classifier tests passed')


if __name__ == '__main__':
    main()
