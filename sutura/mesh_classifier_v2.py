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

# Near-boundary classic-agreement fallback (FAZ4 regression fix). The head's
# confidence is the margin from the 0.50 boundary, so p_mech near 0.5 is "no
# opinion". The metal-nut scan sits at p_mech=0.489 -> conf 0.022, below
# ORG_TUNE_GATE, which silently disabled organic tuning and left the mesh
# open (the 103 -> 102 FAZ2 corpus regression). When the head is a coin-flip
# AND classic strongly agrees with the barely-chosen class, keep the head's
# class but inherit classic's confidence, so a strong agreeing signal is
# never lost to the repair tuning gate just because the head sat exactly on
# its decision boundary. Confident head decisions are NEVER overridden (no
# classic veto -- that was tried and reverted as counterproductive).
_HEAD_MARGIN = 0.15              # |p_mech - 0.5| below this counts as no opinion
_CLASSIC_STRONG = 0.70           # classic class-score floor for the fallback


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
V2_WEIGHTS = [2.3823, 2.9866, -0.9694, -0.5230, 0.5083, 2.2289, 1.0432]
V2_MEAN = [20.9532, 30.5532, 35.5085, 4.9296, 0.2917, 0.5122]
V2_STD = [24.1622, 17.6785, 30.5004, 6.4313, 0.3129, 0.2819]

# Opt-in 'edge-tiebreak' head (FAZ11, --experimental-edge-tiebreak): the 6 base
# features PLUS the five strongest single signals from the FAZ10 retrospective
# scan (all |r| > 0.4 on the 71-mesh labeled set, sign-stable over 30 bootstrap
# subsamples): [oppmax_std, don_mean, dihed_mean, rmin_mean, gc_mean]. Trained
# on the same 71 labeled meshes. LOO-CV is neutral vs the base head (0.845;
# 5-fold x20 mean 0.852 vs 0.850) -- the strong correlates are largely
# redundant with the dihedral features, so this stays an explicit opt-in, NEVER
# the default. OFF by default; enabled only via classify_mesh(extra_features=True).
V2_EXTRA_WEIGHTS = [2.3710, 1.2358, -0.7830, 0.0135, 0.5777, 2.1649, 1.1438,
                    0.2955, 1.9530, 0.0802, 0.0848, 0.2433]
V2_EXTRA_MEAN = [20.9532, 30.5532, 35.5085, 4.9296, 0.2917, 0.5122,
                 8880301.1344, 0.2947, 2.6936, 286513.4431, 0.5724]
V2_EXTRA_STD = [24.1622, 17.6785, 30.5004, 6.4313, 0.3129, 0.2819,
                44317875.2737, 0.2680, 0.3578, 1277023.6886, 0.8934]


def _edge_extra_features(verts, tris):
    """The five strongest single signals from the FAZ10 retrospective scan,
    as a 5-vector [oppmax_std, don_mean, dihed_mean, rmin_mean, gc_mean].

    - ``oppmax_std`` / ``dihed_mean`` / ``rmin_mean``: MeshCNN per-edge
      statistics (dihedral = pi - arccos(n1.n2); the sorted apex-height/base
      ratios' max-std and min-mean), implemented from the formula (FAZ9).
    - ``don_mean``: Difference of Normals mean over the 1-ring (Ioannou 2012,
      FAZ8).
    - ``gc_mean``: mean Gaussian-curvature roughness from the per-vertex angle
      deficit (Wang 2012, FAZ8).

    Pure numpy, 1-ring/edge-connectivity only. Returns zeros for degenerate
    input. Used ONLY when the opt-in edge-tiebreak head is enabled."""
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    V = len(v); F = len(t)
    if F == 0 or V == 0:
        return np.zeros(5)
    a = v[t[:, 0]]; b = v[t[:, 1]]; c = v[t[:, 2]]
    cr = np.cross(b - a, c - a)
    mag = np.linalg.norm(cr, axis=1, keepdims=True)
    mag[mag == 0] = 1.0
    n = cr / mag
    areas = 0.5 * np.linalg.norm(cr, axis=1)
    edge_len = np.linalg.norm(b - a, axis=1) + np.linalg.norm(c - b, axis=1) + \
        np.linalg.norm(a - c, axis=1)

    # --- MeshCNN per-edge stats -------------------------------------------
    edges = np.concatenate([t[:, [0, 1]], t[:, [1, 2]], t[:, [2, 0]]], axis=0)
    face_of = np.concatenate([np.arange(F), np.arange(F), np.arange(F)])
    emin = np.minimum(edges[:, 0], edges[:, 1])
    emax = np.maximum(edges[:, 0], edges[:, 1])
    keys = emin * (V + 1) + emax
    order = np.argsort(keys, kind='stable')
    sk = keys[order]; sf = face_of[order]
    bd = np.flatnonzero(sk[1:] != sk[:-1]) + 1
    st = np.concatenate(([0], bd)).astype(np.int64)
    en = np.concatenate((bd, [len(sk)])).astype(np.int64)
    interior = np.flatnonzero(en - st == 2)
    if interior.size == 0:
        dihed_mean = oppmax_std = rmin_mean = 0.0
    else:
        f1 = sf[st[interior]]; f2 = sf[st[interior] + 1]
        ei = sk[st[interior]] // (V + 1); ej = sk[st[interior]] % (V + 1)
        dihed = np.pi - np.arccos(np.clip(np.sum(n[f1] * n[f2], axis=1), -1, 1))
        def _apex(f, i, j):
            tri = t[f]
            msk = (tri != i[:, None]) & (tri != j[:, None])
            col = np.argmax(msk, axis=1)
            vp = v[np.take_along_axis(tri, col[:, None], 1)[:, 0]]
            e1 = v[i] - vp; e2 = v[j] - vp
            nn1 = np.linalg.norm(e1, axis=1); nn2 = np.linalg.norm(e2, axis=1)
            base = np.linalg.norm(v[j] - v[i], axis=1)
            hgt = 2.0 * areas[f] / (base + 1e-12)
            return hgt / (base + 1e-12)
        r1 = _apex(f1, ei, ej); r2 = _apex(f2, ei, ej)
        rr = np.sort(np.stack([r1, r2], axis=1), axis=1)
        dihed_mean = float(dihed.mean())
        rmin_mean = float(rr[:, 0].mean())
        oppmax_std = float(rr[:, 1].std())

    # --- DON + GC roughness (FAZ8) -----------------------------------------
    adj = [[] for _ in range(V)]
    for f in range(F):
        x, y, z = int(t[f][0]), int(t[f][1]), int(t[f][2])
        for p, q in ((x, y), (y, z), (z, x)):
            adj[p].append(q); adj[q].append(p)
    vn = np.zeros((V, 3)); vcnt = np.zeros(V)
    for f in range(F):
        for vi in (int(t[f][0]), int(t[f][1]), int(t[f][2])):
            vn[vi] += n[f]; vcnt[vi] += 1
    vcnt[vcnt == 0] = 1
    vn /= vcnt[:, None]
    vmag = np.linalg.norm(vn, axis=1, keepdims=True)
    vmag[vmag == 0] = 1.0
    vn = vn / vmag
    don = np.zeros(V)
    gc = np.zeros(V)
    for i in range(V):
        nb = adj[i]
        if len(nb) >= 1:
            don[i] = float(np.linalg.norm(vn[i] - vn[nb].mean(axis=0)))
    # Gaussian curvature via angle deficit
    asum = np.zeros(V)
    for f in range(F):
        x, y, z = int(t[f][0]), int(t[f][1]), int(t[f][2])
        for (p, q, r) in ((x, y, z), (y, z, x), (z, x, y)):
            e1 = v[q] - v[p]; e2 = v[r] - v[p]
            n1 = np.linalg.norm(e1); n2 = np.linalg.norm(e2)
            if n1 and n2:
                asum[p] += np.arccos(np.clip(np.dot(e1, e2) / (n1 * n2), -1, 1))
    gc = np.abs(2.0 * np.pi - asum)
    don_mean = float(don.mean()) if len(don) else 0.0
    gc_mean = float(gc.mean()) if len(gc) else 0.0

    return np.array([oppmax_std, don_mean, dihed_mean, rmin_mean, gc_mean])


def _head_p_mechanical_extra(base, extra):
    """p(mechanical) from the opt-in 11-feature head (base 6 + 5 extras)."""
    if V2_EXTRA_WEIGHTS is None:
        return None
    x = np.concatenate([np.asarray(base, dtype=np.float64),
                        np.asarray(extra, dtype=np.float64)])
    xs = (x - np.asarray(V2_EXTRA_MEAN, dtype=np.float64)) / np.asarray(
        V2_EXTRA_STD, dtype=np.float64)
    w = np.asarray(V2_EXTRA_WEIGHTS, dtype=np.float64)
    z = float(w[0] + np.dot(xs, w[1:]))
    return float(_sigmoid(z))


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
def classify_mesh(verts, tris, extra_features=False):
    """Return {'type', 'confidence', 'metrics'} with the same shape as classic.

    Decision (when the trained head is baked): the head's p(mechanical) drives
    the verdict, with classic acting as a safety net -- a strong classic
    opinion (>= _STRONG_CLASSIC) forces a classic-side verdict on the
    opposite-category call, guarding against organic regression.

    ``extra_features=True`` enables the OPT-IN 'edge-tiebreak' 11-feature head
    (base 6 + the five strong FAZ10 scan signals); NEVER the default. Without
    baked weights: the classic decision verbatim (metrics gain the RANSAC
    features).
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

    extra = None
    if extra_features:
        extra = _edge_extra_features(verts, tris)
        metrics['edge_oppmax_std'] = round(float(extra[0]), 4)
        metrics['edge_don_mean'] = round(float(extra[1]), 4)
        metrics['edge_dihed_mean'] = round(float(extra[2]), 4)
        metrics['edge_rmin_mean'] = round(float(extra[3]), 4)
        metrics['edge_gc_mean'] = round(float(extra[4]), 4)

    p_mech = _head_p_mechanical(near90, flat, gentle, plane_count, plane_area,
                                developable)
    if extra is not None:
        p_mech = _head_p_mechanical_extra(
            [near90, flat, gentle, plane_count, plane_area, developable], extra)

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

    # FAZ4 near-boundary fallback: a coin-flip head with a strongly agreeing
    # classic keeps its class but inherits classic's confidence (see the
    # _HEAD_MARGIN comment). Never changes a confident head decision.
    if head_conf < _HEAD_MARGIN:
        classic_type, classic_score = _classic_verdict(mechanical, organic)
        if classic_type == head_type and classic_score >= _CLASSIC_STRONG:
            head_conf = classic_score

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


def _classic_verdict(mechanical, organic):
    """The classic engine's (type, class-score) WITHOUT mutating ``metrics``.

    Unlike ``_classic_decision`` (which writes the ``leaning`` key on
    unknown), this is a pure read used by the near-boundary fallback so the
    reported ``metrics`` dict is never polluted by the fallback path."""
    if mechanical >= _MECH_FLOOR:
        return 'mechanical', mechanical
    if organic >= _ORG_FLOOR:
        return 'organic', organic
    return 'unknown', max(mechanical, organic)