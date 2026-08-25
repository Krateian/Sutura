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
[0,1] built from sigmoids over the geometric metrics (not a single hard
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
  near90 = fraction of adjacent-face dihedral angles in [60, 120] deg (%)
  flat   = fraction of adjacent-face dihedral angles < 1 deg (%)  (true flat)
  gentle = fraction of adjacent-face dihedral angles in [1, 15) deg (%)
The `flat`/`gentle` split is what keeps smooth high-poly organic meshes from
reading mechanical: their dihedrals are ~2-5 deg, i.e. `gentle`, not `flat`.
"""
import numpy as np

# Sigmoid midpoints/widths, re-derived on the CORRECTED dihedral metric (the
# old values were calibrated against the pre-`i % F`-fix garbage distribution).
# After the fix the corrected bands separate cleanly:
#   mechanical: near90 ~50-71, flat ~26-39, gentle ~0-2
#   organic   : near90 ~0-8,   flat  ~0-40, gentle ~20-100
#   cylinder (deliberately unknown): near90 ~33, flat ~50, gentle ~17
# near90 alone now separates the classes, so its sigmoid midpoint sits in the
# wide gap between organic (<=8) and mechanical (>=50). flat is a weak
# mechanical OR-signal (midpoint 50: only truly flat-dominant parts fire it).
# organic is an AND of (low near90) AND (low flat) AND (high gentle): the
# true-flat band (>=1 deg) must NOT block smooth high-poly organic meshes
# (their dihedrals are ~2-5 deg, above the flat band but inside gentle), and
# genuinely flat mechanical regions block organic membership. The gentle
# band (1-15 deg) is what makes a smooth mesh organic.
_MECH_NEAR90 = 35.0
_W_NEAR90 = 8.0
_MECH_FLAT = 50.0
_W_FLAT = 12.0
_ORG_FLAT = 45.0
_W_ORG_FLAT = 6.0
_ORG_GENTLE = 15.0
_W_GENTLE = 5.0
# Decision floors. The mechanical floor (0.7) is well below the corrected
# mechanical confidence (min 0.867) and keeps the near90~33 cylinder out.
# The organic floor (0.5) keeps borderline/low-confidence meshes out (the
# cylinder's organic score is 0.30).
_MECH_FLOOR = 0.7
_ORG_FLOOR = 0.5


def _sigmoid(x):
    """Stable logistic sigmoid."""
    x = np.clip(x, -50.0, 50.0)
    return 1.0 / (1.0 + np.exp(-x))


def _dihedral_stats(verts, tris):
    """Compute (near90_pct, flat_pct, gentle_pct) from face normals' dihedrals.

    The edge adjacency must map position i in the concatenated edge array back
    to face ``i % F`` (the array is stacked as all edge-0s, then all edge-1s,
    then all edge-2s), NOT ``i // 3`` -- the old ``i // 3`` paired up unrelated
    faces and produced garbage near90/coplanar values. Verified against
    trimesh's face_adjacency_angles (matches exactly after the fix).
    """
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
    F = len(f)
    edge_to_faces = defaultdict(list)
    for i, k in enumerate(keys):
        edge_to_faces[int(k)].append(i % F)

    dihedrals = []
    for faces in edge_to_faces.values():
        if len(faces) != 2:
            continue
        i, j = faces
        cos = float(np.clip(np.dot(n[i], n[j]), -1.0, 1.0))
        dihedrals.append(np.degrees(np.arccos(cos)))

    if not dihedrals:
        return 0.0, 0.0, 0.0
    d = np.array(dihedrals)
    near90 = float(np.mean((d >= 60) & (d <= 120))) * 100.0
    flat = float(np.mean(d < 1.0)) * 100.0
    gentle = float(np.mean((d >= 1.0) & (d < 15.0))) * 100.0
    return near90, flat, gentle


def _class_scores(near90, flat, gentle):
    """Smooth class memberships (mechanical, organic) in [0,1] from the three
    corrected bands. mechanical is an OR of the near90 and true-flat signals
    (either is enough), organic is an AND of (low near90) AND (low flat) AND
    (high gentle) -- all three must hold for a mesh to read organic."""
    mech_near = _sigmoid((near90 - _MECH_NEAR90) / _W_NEAR90)
    mech_flat = _sigmoid((flat - _MECH_FLAT) / _W_FLAT)
    mechanical = max(mech_near, mech_flat)

    org_near = 1.0 - mech_near
    org_flat = 1.0 - _sigmoid((flat - _ORG_FLAT) / _W_ORG_FLAT)
    org_gentle = _sigmoid((gentle - _ORG_GENTLE) / _W_GENTLE)
    organic = min(org_near, org_flat, org_gentle)
    return mechanical, organic


def classify_mesh(verts, tris):
    """Return {'type', 'confidence', 'metrics'}.

    type: 'mechanical' | 'organic' | 'unknown'
    confidence: 0..1, the signed-margin strength toward the decided class;
                for 'unknown' it is the proximity to the nearer class (the
                max of the two memberships) so it is never a flat 0.
    metrics: {'near90', 'flat', 'gentle', 'mechanical_score',
              'organic_score', 'leaning' (unknown only)}
    """
    near90, flat, gentle = _dihedral_stats(verts, tris)
    mechanical, organic = _class_scores(near90, flat, gentle)
    metrics = {
        'near90': round(near90, 2),
        'flat': round(flat, 2),
        'gentle': round(gentle, 2),
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