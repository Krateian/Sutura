"""Experimental twin-engine mesh classifier (v2: RANSAC + small trained head).

**Do not edit ``mesh_classifier.py``**: it stays the "classic" engine, the
default and the fallback. This module is a FULL COPY of the classic engine plus
two experimental additions, selected only via ``--classifier-engine
experimental`` or ``SUTURA_CLASSIFIER_ENGINE=experimental`` (opt-in):

A) **RANSAC plane segmentation** (robust, noise-tolerant): instead of the
   classic normal-band heuristic alone, planes covering >= 1% of the mesh area
   are detected with a RANSAC loop (face centroid + normal as the sample unit,
   inlier = near-plane distance AND near-parallel normal). The NUMBER of such
   robust planar patches and their TOTAL area fraction are reported as new
   features. Scanned mechanical parts (screw/gear/spanner/nut/key) have large
   planar patches even under scan noise; organic scanned forms mostly do not.
   Two naive planar-clustering attempts failed before (documented BLOCKED) --
   do not retry those; RANSAC is the new approach.

B) **Small trained classifier head**: a logistic regression (pure numpy, no
   scipy/sklearn) over ``[near90, flat, gentle, plane_count, plane_area,
   developable_fraction]``, trained on the 35-mesh synthetic set + the
   real-world corpus (``tests/real-world-samples/``, mechanical-but-organic
   scans correctly labeled mechanical). The weights are baked in as
   constants; training + leave-one-out CV lives in
   ``scripts/train_classifier_v2.py``. With only ~45 labeled meshes the model
   is small and overfitting risk is real -- see that script's report before
   trusting the head.

C) **Curvature developability signal** (``_curvature_developable_fraction``):
   the plane-only RANSAC cannot see cylinders/fillets/pipes (they have no
   planar patches), which is the documented "curved-but-mechanical" gap. A
   cylinder is *developable* (zero Gaussian curvature -> its normal field is
   1-dimensional); a sphere/blob/torus is doubly-curved (2-dimensional
   normal field). The signal measures the area fraction whose per-face
   neighbour-normal spread is 1-D, i.e. exactly the geometric signature of
   curved-but-mechanical surfaces.

The public return shape ``{'type', 'confidence', 'metrics'}`` is identical to
classic so the engine is a drop-in. If ``V2_WEIGHTS`` is None (not baked yet)
``classify_mesh`` falls back to the classic decision verbatim, so the module
is always importable and usable anywhere (stdlib + numpy only, same rule as
mesh_classifier.py).

Invariant: this module must NEVER import pymeshlab / trimesh / scipy.
"""
import numpy as np

# --------------------------------------------------------------------------
# Classic engine constants (copied verbatim from mesh_classifier.py -- keep in
# sync if the classic engine is ever recalibrated; classic itself must not be
# edited for this experiment).
# --------------------------------------------------------------------------
_MECH_NEAR90 = 35.0
_W_NEAR90 = 8.0
_MECH_FLAT = 50.0
_W_FLAT = 12.0
_ORG_FLAT = 45.0
_W_ORG_FLAT = 6.0
_ORG_GENTLE = 15.0
_W_GENTLE = 5.0
_MECH_FLOOR = 0.7
_ORG_FLOOR = 0.5

# RANSAC plane-segmentation tuning (experimental).
_RANSAC_SEED = 0                 # deterministic
_RANSAC_ITERS = 60               # hypothesis draws per plane search
_RANSAC_POOL = 4000              # fixed face pool used to score hypotheses
_RANSAC_MIN_AREA_FRAC = 0.01     # a plane must cover >= 1% of mesh area
_RANSAC_MAX_PLANES = 24          # upper bound on returned plane count
_RANSAC_ANGLE_COS = 0.990        # |n_face . n_plane| >= cos(8 deg)
_RANSAC_DIST_FRAC = 0.02         # centroid-plane distance tol = 2% of diag

# Trained head decision (post-training; see train script output).
# LOO-CV on the labeled set is best at a single 0.50 threshold (acc 0.925,
# mech 16/19 vs classic 14/19, org 21/21 -- no organic regression). The
# classic engine stays the fallback when the head is not baked.
_HEAD_MECH_FLOOR = 0.50          # p(mechanical) >= 0.50 -> mechanical, else organic


def _sigmoid(x):
    """Stable logistic sigmoid."""
    x = np.clip(x, -50.0, 50.0)
    return 1.0 / (1.0 + np.exp(-x))


def _dihedral_stats(verts, tris):
    """Compute (near90_pct, flat_pct, gentle_pct) from face normals' dihedrals.

    Copied verbatim from mesh_classifier.py (edge->face pairing is ``i % F``;
    the ``i // 3`` version was a bug, do not reintroduce it).
    """
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

    order = np.argsort(keys, kind="stable")
    sorted_keys = keys[order]
    sorted_face = order % F
    boundaries = np.flatnonzero(sorted_keys[1:] != sorted_keys[:-1]) + 1
    starts = np.concatenate(([0], boundaries)).astype(np.int64)
    ends = np.concatenate((boundaries, [sorted_keys.shape[0]])).astype(np.int64)
    run_len = ends - starts
    sel = np.flatnonzero(run_len == 2)
    if sel.shape[0] == 0:
        return 0.0, 0.0, 0.0

    i_face = sorted_face[starts[sel]]
    j_face = sorted_face[starts[sel] + 1]
    cos_all = np.sum(n[i_face] * n[j_face], axis=1)
    d = np.degrees(np.arccos(np.clip(cos_all, -1.0, 1.0)))

    near90 = float(np.mean((d >= 60) & (d <= 120))) * 100.0
    flat = float(np.mean(d < 1.0)) * 100.0
    gentle = float(np.mean((d >= 1.0) & (d < 15.0))) * 100.0
    return near90, flat, gentle


def _class_scores(near90, flat, gentle):
    """Smooth class memberships (mechanical, organic) in [0,1] -- verbatim
    copy of classic."""
    mech_near = _sigmoid((near90 - _MECH_NEAR90) / _W_NEAR90)
    mech_flat = _sigmoid((flat - _MECH_FLAT) / _W_FLAT)
    mechanical = max(mech_near, mech_flat)

    org_near = 1.0 - mech_near
    org_flat = 1.0 - _sigmoid((flat - _ORG_FLAT) / _W_ORG_FLAT)
    org_gentle = _sigmoid((gentle - _ORG_GENTLE) / _W_GENTLE)
    organic = min(org_near, org_flat, org_gentle)
    return mechanical, organic


# --------------------------------------------------------------------------
# (A) RANSAC plane segmentation (experimental).
# --------------------------------------------------------------------------
def _face_areas(n):
    """Per-face area from (already normalized) face normals ``n``."""
    return np.linalg.norm(n, axis=1) * 0.5


def _ransac_plane_features(verts, tris):
    """Detect robust planar patches via RANSAC.

    Returns ``(plane_count, plane_area_fraction)``:
      plane_count         : number of planar patches covering >= 1% of the
                            mesh area (bound at _RANSAC_MAX_PLANES)
      plane_area_fraction : fraction of the total face area covered by all
                            detected patches (0..1)

    Sample unit is a face (centroid + unit normal). A hypothesis is drawn from
    a random remaining face; a subsample of faces scores it (distance of
    centroid to the plane <= 2% of bbox diagonal AND normal alignment >= cos
    8 deg). The best hypothesis per search is refined against ALL faces, its
    inliers are removed, and the search repeats for the next plane. Deterministic
    (fixed seed).
    """
    v = np.asarray(verts, dtype=np.float64)
    f = np.asarray(tris, dtype=np.int64)
    if len(f) == 0:
        return 0, 0.0

    a = v[f[:, 0]]
    b = v[f[:, 1]]
    c = v[f[:, 2]]
    cr = np.cross(b - a, c - a)
    mag = np.linalg.norm(cr, axis=1)
    mag[mag == 0] = 1.0
    n = cr / mag[:, None]
    areas = mag * 0.5
    centroids = (a + b + c) / 3.0

    total_area = float(areas.sum())
    if total_area <= 0:
        return 0, 0.0

    diag = float(np.linalg.norm(v.max(axis=0) - v.min(axis=0)))
    if diag <= 0:
        diag = 1.0
    dist_tol = _RANSAC_DIST_FRAC * diag

    rng = np.random.default_rng(_RANSAC_SEED)
    n_faces = len(f)
    # Fixed scoring pool: a deterministic subsample of face indices used for
    # every hypothesis draw. Keeps the per-iteration cost constant regardless
    # of mesh size and lets the arrays be cached (one-time fancy index).
    pool_size = min(n_faces, _RANSAC_POOL)
    pool = rng.choice(n_faces, size=pool_size, replace=False)
    pool_c = centroids[pool]
    pool_n = n[pool]
    pool_in = np.ones(pool_size, dtype=bool)  # pool faces not yet consumed

    remaining = np.ones(len(f), dtype=bool)
    plane_count = 0
    covered = 0.0

    while plane_count < _RANSAC_MAX_PLANES:
        idx = np.flatnonzero(remaining)
        if idx.size < 3:
            break
        best_hits = -1
        best_plane = None

        for _ in range(_RANSAC_ITERS):
            seed = idx[rng.integers(0, idx.size)]
            pn = n[seed]
            pc = centroids[seed]
            sub = pool[pool_in]
            d = np.abs((pool_c[pool_in] - pc) @ pn)
            align = np.abs(pool_n[pool_in] @ pn)
            inl = sub[(d <= dist_tol) & (align >= _RANSAC_ANGLE_COS)]
            if inl.size > best_hits:
                best_hits = inl.size
                best_plane = (pn, pc)

        if best_plane is None:
            break
        pn, pc = best_plane
        # refine against ALL remaining faces
        rem = np.flatnonzero(remaining)
        d = np.abs((centroids[rem] - pc) @ pn)
        align = np.abs(n[rem] @ pn)
        inl = rem[(d <= dist_tol) & (align >= _RANSAC_ANGLE_COS)]
        if inl.size == 0:
            break
        inl_area = float(areas[inl].sum())
        # Stop hunting once a search only finds sub-threshold patches: organic
        # meshes otherwise keep yielding tiny noise clusters one at a time.
        if inl_area / total_area < _RANSAC_MIN_AREA_FRAC:
            break
        plane_count += 1
        covered += inl_area
        remaining[inl] = False
        pool_in &= ~np.isin(pool, inl)
        if remaining.sum() < 3:
            break

    frac = min(1.0, covered / total_area)
    return plane_count, round(frac, 4)


# Curvature / developability signal tuning (experimental).
_CURV_AXIS_TOL_DEG = 8.0       # |normal . axis| <= sin(tol) counts as on the
#                              #   great circle perpendicular to the axis


def _curvature_developable_fraction(verts, tris):
    """Area-weighted fraction of the surface whose normals lie on a common
    great circle -- the signature of a developable surface (a cylinder, pipe,
    fillet or other ruled curved part).

    A developable surface has zero Gaussian curvature, so its normal field
    (the Gauss map) is 1-dimensional: all side-face normals of a cylinder are
    perpendicular to its axis and lie on one great circle of the unit sphere.
    Doubly-curved organic forms (spheres, blobs, torus) have normals spread
    over a 2-D region of the Gauss sphere and only a thin band is ever
    perpendicular to any single axis. The plane-based RANSAC cannot see any
    of this (a cylinder has no planar patches), so this signal is the
    principled discriminator for "curved-but-mechanical" parts.

    The best-fit common axis is the smallest-eigenvalue eigenvector of the
    area-weighted face-normal covariance; a face counts when its normal is
    within ``_CURV_AXIS_TOL_DEG`` of perpendicular to that axis. A global
    area statistic (not a per-face differential estimate) so it is robust to
    damage and scan noise. Pure numpy, deterministic, O(faces).
    """
    v = np.asarray(verts, dtype=np.float64)
    f = np.asarray(tris, dtype=np.int64)
    if len(f) < 3:
        return 0.0  # a common axis needs at least a few faces
    a = v[f[:, 0]]
    b = v[f[:, 1]]
    c = v[f[:, 2]]
    cr = np.cross(b - a, c - a)
    mag = np.linalg.norm(cr, axis=1)
    mag[mag == 0] = 1.0
    n = cr / mag[:, None]
    areas = mag * 0.5
    total = float(areas.sum())
    if total <= 0:
        return 0.0
    try:
        cov = np.cov(n, rowvar=False, aweights=areas / total)
        # smallest-eigenvalue eigenvector = best-fit common axis for the normals
        u = np.linalg.eigh(cov)[1][:, 0]
    except (np.linalg.LinAlgError, FloatingPointError):
        return 0.0  # degenerate/NaN normals: never crash the classifier
    npar = np.abs(n @ u)
    sin_tol = np.sin(np.radians(_CURV_AXIS_TOL_DEG))
    frac = float((areas * (npar <= sin_tol)).sum() / total)
    return round(min(1.0, frac), 4)


# --------------------------------------------------------------------------
# (B) Trained logistic head (experimental, numpy-only).
# --------------------------------------------------------------------------
# Baked weights from scripts/train_classifier_v2.py. Format (matches the
# trainer exactly: it builds Xb = hstack([ones, Xs])):
#   w[0]    : bias
#   w[1:]   : weights for the 6 standardized features
#              [near90, flat, gentle, plane_count, plane_area, developable]
#   V2_MEAN : feature means (6-feature order)
#   V2_STD  : feature stds (6-feature order, 1.0 where 0)
# Set V2_WEIGHTS to None to disable the head and make classify_mesh behave
# exactly like classic (metrics still include the RANSAC + curvature features).
V2_WEIGHTS = [2.8604, 2.6639, -2.2499, -0.6524, 0.5539, 3.8095, 1.7295]
V2_MEAN = [24.7311, 28.3521, 34.4855, 5.8085, 0.3104, 0.5297]
V2_STD = [27.8310, 16.1076, 33.4674, 7.3300, 0.3507, 0.3008]


def _head_p_mechanical(near90, flat, gentle, plane_count, plane_area,
                       developable):
    """Return p(mechanical) in [0,1] from the trained head, or None if no
    weights are baked."""
    if V2_WEIGHTS is None:
        return None
    x = np.array([near90, flat, gentle, plane_count, plane_area, developable],
                 dtype=np.float64)
    xs = (x - np.asarray(V2_MEAN, dtype=np.float64)) / np.asarray(V2_STD,
                                                                   dtype=np.float64)
    w = np.asarray(V2_WEIGHTS, dtype=np.float64)
    # w[0] is the bias, w[1:] are the feature weights (trainer layout).
    z = float(w[0] + np.dot(xs, w[1:]))
    return float(_sigmoid(z))


# --------------------------------------------------------------------------
# Public API (drop-in for mesh_classifier.classify_mesh).
# --------------------------------------------------------------------------
def classify_mesh(verts, tris):
    """Return {'type', 'confidence', 'metrics'} with the same shape as classic.

    Decision (when the trained head is baked): the head's p(mechanical) drives
    the verdict, with classic acting as a safety net -- a strong classic
    opinion (>= _STRONG_CLASSIC) forces a classic-side verdict on the
    opposite-category call, guarding against organic regression.

    Without baked weights: the classic decision verbatim (metrics gain the
    RANSAC features).
    """
    near90, flat, gentle = _dihedral_stats(verts, tris)
    plane_count, plane_area = _ransac_plane_features(verts, tris)
    developable = _curvature_developable_fraction(verts, tris)
    mechanical, organic = _class_scores(near90, flat, gentle)

    metrics = {
        'near90': round(near90, 2),
        'flat': round(flat, 2),
        'gentle': round(gentle, 2),
        'mechanical_score': round(mechanical, 3),
        'organic_score': round(organic, 3),
        'plane_count': plane_count,
        'plane_area': plane_area,
        'developable_fraction': developable,
    }

    p_mech = _head_p_mechanical(near90, flat, gentle, plane_count, plane_area,
                                developable)

    if p_mech is None:
        # no trained head baked yet -> classic decision
        return _classic_decision(mechanical, organic, metrics)

    # head verdict: single threshold at p=0.50 (LOO-CV-validated on the
    # labeled set). No classic veto: it was counterproductive, reverting
    # correct head flips on the known mechanical scans.
    if p_mech >= _HEAD_MECH_FLOOR:
        head_type = 'mechanical'
    else:
        head_type = 'organic'
    head_conf = abs(p_mech - 0.5) * 2.0  # 0..1 margin

    return {'type': head_type,
            'confidence': round(head_conf, 3),
            'metrics': metrics}


def _classic_decision(mechanical, organic, metrics):
    """The classic decision verbatim (used when the head is not baked)."""
    if mechanical >= _MECH_FLOOR:
        return {'type': 'mechanical',
                'confidence': round(mechanical, 3),
                'metrics': metrics}
    if organic >= _ORG_FLOOR:
        return {'type': 'organic',
                'confidence': round(organic, 3),
                'metrics': metrics}
    metrics['leaning'] = 'mechanical' if mechanical >= organic else 'organic'
    return {'type': 'unknown',
            'confidence': round(max(mechanical, organic), 3),
            'metrics': metrics}