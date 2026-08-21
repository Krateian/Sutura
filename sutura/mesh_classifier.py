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

Confidence is a signed-margin score: each class gets a smooth membership in
[0,1] built from sigmoids over BOTH geometric metrics (not a single hard
threshold), and the decision is taken on the margin between the two
memberships. This removes the hard `[55,60]` near90 discontinuity: the type
flip is now a smooth logistic transition, and an `unknown` result still
carries a non-zero proximity value (which class it leans toward, and how
close) instead of a flat 0. The public return shape
``{'type', 'confidence', 'metrics'}`` is unchanged.

Decision rules (calibrated on synthetic + Thingi10K meshes, see
tests/test_mesh_classifier.py and scripts/calibrate_classifier.py):
  mechanical   : mechanical membership >= 0.7
  organic      : organic membership >= 0.5
  else         : unknown  (caller keeps default parameters)
where
  near90   = fraction of adjacent-face dihedral angles in [60, 120] deg (%)
  coplanar = fraction of adjacent-face dihedral angles < 3 deg (%)
"""
import numpy as np

# Sigmoid midpoints (the historical hard thresholds become the 50% points).
_MECH_NEAR90 = 60.0
_MECH_COPLANAR = 45.0
_ORG_NEAR90 = 55.0
_ORG_COPLANAR = 5.0
# Sigmoid widths: how fast each metric saturates around its midpoint.
_W_NEAR90 = 5.0
_W_COPLANAR = 10.0
# Decision floors. The mechanical floor (0.7) is deliberately above the 60
# near90 midpoint so barely-over-60 organic meshes (low-poly spheres/tori)
# fall into `unknown` instead of being wrongly tuned as mechanical. The
# organic floor (0.5) reproduces the historical `near90 < 55 AND coplanar < 5`
# rule exactly.
_MECH_FLOOR = 0.7
_ORG_FLOOR = 0.5


def _sigmoid(x):
    """Stable logistic sigmoid."""
    x = np.clip(x, -50.0, 50.0)
    return 1.0 / (1.0 + np.exp(-x))


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


def _class_scores(near90, coplanar):
    """Smooth class memberships (mechanical, organic) in [0,1] from both
    metrics. mechanical is an OR of the two signals (either is enough),
    organic is an AND (both must be low)."""
    mech_near = _sigmoid((near90 - _MECH_NEAR90) / _W_NEAR90)
    mech_cop = _sigmoid((coplanar - _MECH_COPLANAR) / _W_COPLANAR)
    mechanical = max(mech_near, mech_cop)

    org_near = 1.0 - _sigmoid((near90 - _ORG_NEAR90) / _W_NEAR90)
    org_cop = 1.0 - _sigmoid((coplanar - _ORG_COPLANAR) / _W_COPLANAR)
    organic = min(org_near, org_cop)
    return mechanical, organic


def classify_mesh(verts, tris):
    """Return {'type', 'confidence', 'metrics'}.

    type: 'mechanical' | 'organic' | 'unknown'
    confidence: 0..1, the signed-margin strength toward the decided class;
                for 'unknown' it is the proximity to the nearer class (the
                max of the two memberships) so it is never a flat 0.
    metrics: {'near90', 'coplanar', 'mechanical_score', 'organic_score',
              'leaning' (unknown only)}
    """
    near90, coplanar = _dihedral_stats(verts, tris)
    mechanical, organic = _class_scores(near90, coplanar)
    metrics = {
        'near90': round(near90, 2),
        'coplanar': round(coplanar, 2),
        'mechanical_score': round(mechanical, 3),
        'organic_score': round(organic, 3),
    }

    if mechanical >= _MECH_FLOOR:
        return {'type': 'mechanical',
                'confidence': round(mechanical, 3),
                'metrics': metrics}

    if organic >= _ORG_FLOOR:
        return {'type': 'organic',
                'confidence': round(organic, 3),
                'metrics': metrics}

    # unknown: keep the proximity to the nearer class (signed margin sign)
    metrics['leaning'] = 'mechanical' if mechanical >= organic else 'organic'
    return {'type': 'unknown',
            'confidence': round(max(mechanical, organic), 3),
            'metrics': metrics}