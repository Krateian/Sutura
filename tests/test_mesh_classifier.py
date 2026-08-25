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


def test_corrected_dihedral_metric_values():
    # regression for the edge->face pairing bug (i % F, formerly i // 3): the
    # box must read near90=66.67 (12 edge pairs at 90 deg) and flat=33.33 (6
    # coplanar quad diagonals), NOT the buggy 72.22/11.11 the wrong pairing
    # produced.
    from mesh_classifier import _dihedral_stats
    near90, flat, gentle = _dihedral_stats(*_cube())
    assert round(near90, 2) == 66.67, near90
    assert round(flat, 2) == 33.33, flat
    assert round(gentle, 2) == 0.0, gentle


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
        assert 'flat' in r['metrics'] and 'gentle' in r['metrics'], r
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
    # the signed-margin score must be monotonic in each driving metric when
    # the others are held constant: more near90 => not less mechanical and
    # not more organic; more flat => not less mechanical and not more organic;
    # more gentle => not less organic (mechanical is untouched by gentle).
    from mesh_classifier import _class_scores
    prev_m, prev_o = _class_scores(0.0, 0.0, 0.0)
    for near90 in np.linspace(0.0, 95.0, 26):
        m, o = _class_scores(float(near90), 0.0, 0.0)
        assert m >= prev_m - 1e-9, (near90, m, prev_m)
        assert o <= prev_o + 1e-9, (near90, o, prev_o)
        prev_m, prev_o = m, o
    prev_m, prev_o = _class_scores(0.0, 0.0, 0.0)
    for flat in np.linspace(0.0, 100.0, 26):
        m, o = _class_scores(0.0, float(flat), 0.0)
        assert m >= prev_m - 1e-9, (flat, m, prev_m)
        assert o <= prev_o + 1e-9, (flat, o, prev_o)
        prev_m, prev_o = m, o
    prev_m, prev_o = _class_scores(0.0, 0.0, 0.0)
    for gentle in np.linspace(0.0, 100.0, 26):
        m, o = _class_scores(0.0, 0.0, float(gentle))
        assert o >= prev_o - 1e-9, (gentle, o, prev_o)
        assert abs(m - prev_m) <= 1e-9, (gentle, m, prev_m)
        prev_m, prev_o = m, o


def test_tuning_gate_thresholds():
    # the repair confidence gate (Aşama 3): a classified mesh only gets its
    # tuned Stage 1 thresholds when confidence clears the class-specific gate
    # (mechanical 0.75, organic 0.70); below it defaults are used. Unknown
    # never tunes.
    from repair import (MECH_TUNE_GATE, ORG_TUNE_GATE, tuning_applied_for)
    assert MECH_TUNE_GATE == 0.75
    assert ORG_TUNE_GATE == 0.70
    # exactly at the gate -> tuned (>=), just below -> defaults
    assert tuning_applied_for('mechanical', MECH_TUNE_GATE) == True
    assert tuning_applied_for('mechanical', MECH_TUNE_GATE - 0.001) == False
    assert tuning_applied_for('mechanical', 0.92) == True
    assert tuning_applied_for('organic', ORG_TUNE_GATE) == True
    assert tuning_applied_for('organic', ORG_TUNE_GATE - 0.001) == False
    assert tuning_applied_for('organic', 0.71) == True
    # unknown and unclassified -> defaults, even at high confidence
    assert tuning_applied_for('unknown', 0.9) == False
    assert tuning_applied_for(None, 0.9) == False


def test_tuning_gate_low_confidence_mesh_uses_defaults():
    # end-to-end: a real mesh classified below the gate must report
    # tuning_applied=False but keep its detected type (defaults used). The
    # 24k-tri smooth capsule reads ~0.69 organic, below the 0.70 gate.
    from mesh_classifier import classify_mesh
    from repair import tuning_applied_for
    from make_classifier_set import iter_meshes
    capsule = None
    for m in iter_meshes():
        if m['name'] == 'capsule_64':
            capsule = m
            break
    assert capsule is not None, 'capsule_64 missing from calibration set'
    r = classify_mesh(capsule['verts'], capsule['tris'])
    assert r['type'] == 'organic', r
    assert r['confidence'] < 0.70, r
    assert not tuning_applied_for(r['type'], r['confidence'])


def test_tuning_gate_high_confidence_mesh_tuned():
    # a clearly mechanical box (~0.92) is above the gate -> tuned applied.
    from mesh_classifier import classify_mesh
    from repair import tuning_applied_for
    from make_classifier_set import _boxes
    name, v, t, _label = _boxes()[0]
    r = classify_mesh(v, t)
    assert r['type'] == 'mechanical', r
    assert r['confidence'] >= 0.75, r
    assert tuning_applied_for(r['type'], r['confidence'])


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('mesh_classifier tests passed')


if __name__ == '__main__':
    main()
