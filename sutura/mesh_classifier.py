"""Mesh type classification (organic vs mechanical) for repair tuning.

stdlib + numpy only, by design: `repair.py` (under the PyMeshLab venv) passes
plain `verts`/`tris` arrays in here and this module does pure array work so it
stays lightweight and importable anywhere (same rule as classification.py and
defects.py).

This is a conservative geometric heuristic, NOT an ML model. It distinguishes
only high-confidence cases and reports `unknown` otherwise, so repair can fall
back to its existing fixed parameters instead of risking a wrong parameter
set on a mis-classified mesh.

Note: the mechanical class's `mincomponentsize` in `repair.py` is kept at the
default 8 (not lowered) because lowering it to 4 let small/degenerate meshes
survive the debris cutoff and get "repaired" instead of rejected (a CI
regression in tests/test_adversarial.py 'degenerate'). Do not lower it below
8 without addressing that.

Decision rules (calibrated on synthetic + Thingi10K meshes, see
tests/test_mesh_classifier.py):
  mechanical   : near90 > 60 OR coplanar > 45
  organic      : near90 < 55 AND coplanar < 5
  else         : unknown  (caller keeps default parameters)
where
  near90   = fraction of adjacent-face dihedral angles in [60, 120] deg (%)
  coplanar = fraction of adjacent-face dihedral angles < 3 deg (%)
"""
import numpy as np

# Thresholds from calibration (conservative, wide 'unknown' band).
MECH_NEAR90 = 60.0
MECH_COPLANAR = 45.0
ORG_NEAR90 = 55.0
ORG_COPLANAR = 5.0


def _dihedral_stats(verts, tris):
    """Compute (near90_pct, coplanar_pct) from the face normals' dihedrals."""
    from collections import defaultdict
    v = np.asarray(verts, dtype=np.float64)
    f = np.asarray(tris, dtype=np.int64)
    a = v[f[:, 0]]
    b = v[f[:, 1]]
    c = v[f[:, 2]]
    n = np.cross(b - a, c - a)
    norm = np.linalg.norm(n, axis=1, keepdims=True)
    norm[norm == 0] = 1.0
    n = n / norm

    edges = np.concatenate([f[:, [0, 1]], f[:, [1, 2]], f[:, [2, 0]]], axis=0)
    keys = np.min(edges, axis=1) * (10 ** 7) + np.max(edges, axis=1)
    edge_to_faces = defaultdict(list)
    for i, k in enumerate(keys):
        edge_to_faces[int(k)].append(i // 3)

    dihedrals = []
    for faces in edge_to_faces.values():
        if len(faces) != 2:
            continue
        i, j = faces
        cos = float(np.clip(np.dot(n[i], n[j]), -1.0, 1.0))
        dihedrals.append(np.degrees(np.arccos(cos)))

    if not dihedrals:
        return 0.0, 0.0
    d = np.array(dihedrals)
    near90 = float(np.mean((d >= 60) & (d <= 120))) * 100.0
    coplanar = float(np.mean(d < 3)) * 100.0
    return near90, coplanar


def classify_mesh(verts, tris):
    """Return {'type', 'confidence', 'metrics'}.

    type: 'mechanical' | 'organic' | 'unknown'
    confidence: 0..1, how far the decision is from its threshold boundary
    metrics: {'near90': ..., 'coplanar': ...}
    """
    near90, coplanar = _dihedral_stats(verts, tris)
    metrics = {'near90': round(near90, 2), 'coplanar': round(coplanar, 2)}

    if near90 > MECH_NEAR90 or coplanar > MECH_COPLANAR:
        # mechanical: distance from the nearer mechanical boundary
        if near90 > MECH_NEAR90:
            conf = (near90 - MECH_NEAR90) / (100.0 - MECH_NEAR90)
        else:
            conf = (coplanar - MECH_COPLANAR) / (100.0 - MECH_COPLANAR)
        conf = min(max(conf, 0.0), 1.0)
        return {'type': 'mechanical', 'confidence': round(conf, 3), 'metrics': metrics}

    if near90 < ORG_NEAR90 and coplanar < ORG_COPLANAR:
        # organic: near90 must be comfortably below 55 and coplanar below 5
        conf = min((ORG_NEAR90 - near90) / ORG_NEAR90,
                   (ORG_COPLANAR - coplanar) / ORG_COPLANAR)
        conf = min(max(conf, 0.0), 1.0)
        return {'type': 'organic', 'confidence': round(conf, 3), 'metrics': metrics}

    return {'type': 'unknown', 'confidence': 0.0, 'metrics': metrics}
