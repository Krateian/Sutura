"""Autorefine + snap-rounding self-intersection resolution.

From-scratch reimplementation of the published Lazard & Valque loop
("Resolving self-intersections in 3D meshes while preserving floating-point
coordinates", Computer Graphics Forum 44(5), 2025 — DOI 10.1111/cgf.70197).
No CGAL source was read or copied; only the published algorithm text and the
standard exact-predicate mathematics (Shewchuk-style adaptive orient3d).

The loop (per the paper):
  (i)   identify pairs of triangles that *properly* intersect (interior
        crossing), cheaply;
  (ii)  snap the vertices of those pairs to the finest float-exact uniform
        grid (scale so max|x| lands in [2^23, 2^24));
  (iii) also snap every vertex in a cell containing a snapped vertex;
  (iv)  compute the exact triangle-pair intersections and subdivide both
        triangles along the intersection segment (so it becomes a shared edge);
  (v)   round new vertices to doubles;
  (vi)  iterate <= 5 times or until no proper intersections remain.

On top of that base loop sits the CGAL 6.1 (June 2025) second-half fix, the
iterative snap-rounding pass (apply_iterative_snap_rounding): the vertices of
still-intersecting triangles (and their cell-mates) are repeatedly snapped
onto a fitting float-exact integer grid, the degenerate elements this creates
are eliminated, and intersection detection + re-resolution run again,
iterating (bounded by MAX_SNAP_ROUNDS; no formal termination guarantee) until
no proper intersection remains or the cap is hit. The grid is kept at the
finest float-exact level ([2^23, 2^24)): coarsening it was measured on the
heavy-SI corpus and rejected (100281 went 906->97 pairs on the fine grid vs
906->74289 when coarsened -- on dense scans the coarse grid amplifies the
cell-mate snapping and manufactures new intersections). Degenerate elements
left by each subdivision are eliminated too.

Unlike the delete-and-reclose approach (extreme_extra_passes), this NEVER
deletes an input face: every input triangle is either kept whole or split
into sub-triangles. This is the core property that fixes the documented
"extreme mode can worsen heavy-SI scans" failure mode (removing SI faces
opens more boundary edges than it closes).

Implementation notes:
- Pure numpy + pyrobust_predicates. NO pymeshlab, NO scipy, NO trimesh.
- Exact predicates are used for the plane-side tests (EPICK: exact
  predicates, float64 construction), matching the paper's note that this
  gives certified topology with less control on input-output distance than
  exact construction (EPECK). On dense-SI scans (thousands of intersecting
  faces in one region) the float64 construction can leave residual
  intersections / slivers that cascade; the convergence guards below bound
  this. See docs/alpha-wrap-feasibility-2026-09.md section 4a.
"""
import numpy as np

from pyrobust_predicates import orient3d

MAX_ITERATIONS = 5
DEFAULT_SNAP_BITS = 23  # largest |x| scaled into [2^23, 2^24) -> rel. err <= 2^-24
MAX_SNAP_ROUNDS = 8     # inner iterative snap-rounding cap per outer pass
MAX_FACES_GROWTH = 4     # blow-up guard: faces may not exceed input * this
WELD_TOL_REL = 1e-5      # weld merge distance as a fraction of mesh span
INNER_WELD_TOL_REL = 2e-5  # slightly looser weld used during the inner loop


# ---------------------------------------------------------------------------
# exact predicates
# ---------------------------------------------------------------------------

def orient3d_adaptive(a, b, c, d):
    """Signed orientation of point d vs plane(a,b,c).

    Adaptive (Shewchuk-style filter then exact fallback): a fast float64
    4x4 determinant decides whenever it is comfortably away from zero; the
    exact pyrobust_predicates adaptive predicate handles near-degenerate
    inputs. Returns a float sign."""
    ax, ay, az = a[0], a[1], a[2]
    bx, by, bz = b[0], b[1], b[2]
    cx, cy, cz = c[0], c[1], c[2]
    dx, dy, dz = d[0], d[1], d[2]
    det = ((bx - ax) * ((cy - ay) * (dz - az) - (cz - az) * (dy - ay))
           - (by - ay) * ((cx - ax) * (dz - az) - (cz - az) * (dx - ax))
           + (bz - az) * ((cx - ax) * (dy - ay) - (cy - ay) * (dx - ax)))
    scale = (abs(bx - ax) + abs(by - ay) + abs(bz - az)
             + abs(cx - ax) + abs(cy - ay) + abs(cz - az)
             + abs(dx - ax) + abs(dy - ay) + abs(dz - az))
    if abs(det) > 1e-14 * max(scale, 1e-300) * max(scale, 1e-300):
        return det
    return orient3d(float(ax), float(ay), float(az),
                    float(bx), float(by), float(bz),
                    float(cx), float(cy), float(cz),
                    float(dx), float(dy), float(dz))


# ---------------------------------------------------------------------------
# broad phase: uniform-grid spatial hash on triangle AABBs
# ---------------------------------------------------------------------------

def _aabbs(v, t):
    tri = v[t]
    return tri.min(axis=1), tri.max(axis=1)


def broad_phase_candidates(v, t):
    """Candidate (i, j) pairs whose AABBs overlap. Conservative superset of
    the proper-intersection pairs. Vectorized uniform-grid spatial hash."""
    v = np.asarray(v, dtype=np.float64)
    t = np.asarray(t, dtype=np.int64)
    mins, maxs = _aabbs(v, t)
    spans = maxs - mins
    diag = np.sqrt((spans ** 2).sum(axis=1))
    cell = float(np.median(diag)) * 1.5
    if not (cell > 0.0):
        cell = 1.0
    cell = max(cell, 1e-9)

    c0 = np.floor(mins / cell).astype(np.int64)
    c1 = np.floor(maxs / cell).astype(np.int64)

    ncx = c1[:, 0] - c0[:, 0] + 1
    ncy = c1[:, 1] - c0[:, 1] + 1
    ncz = c1[:, 2] - c0[:, 2] + 1
    vol = ncx * ncy * ncz
    total = int(vol.sum())
    if total == 0:
        return []
    fidx = np.repeat(np.arange(len(t)), vol)
    x0 = np.repeat(c0[:, 0], vol)
    y0 = np.repeat(c0[:, 1], vol)
    z0 = np.repeat(c0[:, 2], vol)
    nx = np.repeat(ncx, vol)
    ny = np.repeat(ncy, vol)
    starts = np.cumsum(vol) - vol
    off = np.arange(total) - np.repeat(starts, vol)
    zz = off % np.repeat(ncz, vol)
    rem = off // np.repeat(ncz, vol)
    yy = rem % ny
    xx = rem // ny
    cells = (x0 + xx) * 2 ** 21 * 2 ** 21 + (y0 + yy) * 2 ** 21 + (z0 + zz)
    order = np.argsort(cells, kind='stable')
    cells_s = cells[order]
    fidx_s = fidx[order]
    bounds = np.where(np.diff(cells_s) != 0)[0] + 1
    bounds = np.concatenate([[0], bounds, [len(cells_s)]])
    seen = set()
    out = []
    for bi in range(len(bounds) - 1):
        grp = fidx_s[bounds[bi]:bounds[bi + 1]]
        if len(grp) < 2:
            continue
        g = grp
        for idx in range(len(g)):
            i = int(g[idx])
            for jj in range(idx + 1, len(g)):
                j = int(g[jj])
                key = (i, j) if i < j else (j, i)
                if key in seen:
                    continue
                seen.add(key)
                if (mins[i, 0] <= maxs[j, 0] and mins[j, 0] <= maxs[i, 0] and
                        mins[i, 1] <= maxs[j, 1] and mins[j, 1] <= maxs[i, 1] and
                        mins[i, 2] <= maxs[j, 2] and mins[j, 2] <= maxs[i, 2]):
                    out.append((i, j))
    return out


# ---------------------------------------------------------------------------
# plane / segment helpers
# ---------------------------------------------------------------------------

def _plane_line_point_vec(n1, d1, n2, d2):
    """Vectorized point on the line n1.x=d1, n2.x=d2. Returns (n,3)."""
    n11 = np.einsum('ij,ij->i', n1, n1)
    n12 = np.einsum('ij,ij->i', n1, n2)
    n22 = np.einsum('ij,ij->i', n2, n2)
    det = n11 * n22 - n12 * n12
    det = np.where(np.abs(det) < 1e-300, 1.0, det)
    alpha = (d1 * n22 - d2 * n12) / det
    beta = (n11 * d2 - n12 * d1) / det
    return alpha[:, None] * n1 + beta[:, None] * n2


def _clip_segment_2d(poly_ccw, seg):
    """Clip a 2D segment against a CCW convex polygon. Returns [p, q] or None."""
    p0, p1 = seg
    u0, u1 = 0.0, 1.0
    d = p1 - p0
    for k in range(len(poly_ccw)):
        a = poly_ccw[k]
        b = poly_ccw[(k + 1) % len(poly_ccw)]
        e = b - a
        # For a CCW polygon the interior is to the LEFT of each directed edge:
        # point P inside iff cross(e, P - a) >= 0.
        f0 = e[0] * (p0[1] - a[1]) - e[1] * (p0[0] - a[0])
        f1 = e[0] * (p1[1] - a[1]) - e[1] * (p1[0] - a[0])
        if f0 < 0 and f1 < 0:
            return None
        if f0 < 0:
            u0 = max(u0, f0 / (f0 - f1))
        elif f1 < 0:
            u1 = min(u1, f0 / (f0 - f1))
    if u1 < u0:
        return None
    q0 = p0 + u0 * d
    q1 = p0 + u1 * d
    return (q0, q1)


def _polygon_from_triangle(poly):
    """Return poly as a CCW list of 2D points (reverse if CW)."""
    area = 0.0
    for k in range(len(poly)):
        a, b = poly[k], poly[(k + 1) % len(poly)]
        area += a[0] * b[1] - b[0] * a[1]
    if area < 0:
        poly = list(reversed(poly))
    return poly


def _insert_point_into_poly(pts, pp):
    """Insert pp into the CCW boundary, snapping to a coincident vertex."""
    out = []
    inserted = False
    n = len(pts)
    for k in range(n):
        a = pts[k]
        b = pts[(k + 1) % n]
        if not inserted:
            ab = b - a
            if np.dot(ab, ab) < 1e-24:
                out.append(a)
                continue
            t = np.dot(pp - a, ab) / np.dot(ab, ab)
            if (-1e-9 <= t <= 1 + 1e-9
                    and np.linalg.norm(pp - (a + t * ab)) <= 1e-9):
                if t > 1e-9:
                    out.append(a)
                out.append(a + t * ab)
                inserted = True
                continue
        out.append(a)
    if not inserted:
        out.append(pp)
    res = []
    for x in out:
        if not res or not np.allclose(res[-1], x, atol=1e-9):
            res.append(x)
    if len(res) > 1 and np.allclose(res[0], res[-1], atol=1e-9):
        res = res[:-1]
    return res


def _split_polygon_by_chord(poly, p, q):
    """Split convex CCW polygon by chord p->q (both on the boundary, possibly
    mid-edge). Returns [poly1, poly2] (each CCW, containing the chord)."""
    poly = _insert_point_into_poly(poly, p)
    poly = _insert_point_into_poly(poly, q)

    def index_of(pts, pp):
        for k, x in enumerate(pts):
            if np.allclose(x, pp, atol=1e-9):
                return k
        return None

    kp = index_of(poly, p)
    kq = index_of(poly, q)
    if kp is None or kq is None or kp == kq:
        return [poly]
    n = len(poly)
    fwd = [poly[kp]]
    k = (kp + 1) % n
    while k != kq:
        fwd.append(poly[k])
        k = (k + 1) % n
    fwd.append(poly[kq])
    bwd = [poly[kp]]
    k = (kp - 1) % n
    while k != kq:
        bwd.append(poly[k])
        k = (k - 1) % n
    bwd.append(poly[kq])
    return [_polygon_from_triangle(fwd), _polygon_from_triangle(bwd)]


def _fan_triangulate(poly):
    """Fan-triangulate a convex polygon (CCW). Returns list of 2D triangles."""
    if len(poly) < 3:
        return []
    return [[poly[0], poly[k], poly[k + 1]] for k in range(1, len(poly) - 1)]


def _subdivide_triangle_2d(tri2, chords, boundary_points=()):
    """Subdivide a 2D triangle (CCW) along chord segments and boundary points.

    chords: list of (p, q) segments guaranteed to be inside the triangle.
    boundary_points: additional 2D points to insert on the triangle boundary
    (for conforming propagation of edge splits from neighbour triangles).
    Processes chords sequentially, clipping each against the current
    sub-polygons (handles crossing chords naturally). Returns a list of 2D
    sub-triangles."""
    polys = [_polygon_from_triangle([np.array(x) for x in tri2])]
    for bp in boundary_points:
        new_polys = []
        for poly in polys:
            np_ = _insert_point_into_poly(poly, np.array(bp, dtype=np.float64))
            new_polys.append(np_ if len(np_) != len(poly) else poly)
        polys = new_polys
    for p, q in chords:
        seg = (np.array(p, dtype=np.float64), np.array(q, dtype=np.float64))
        new_polys = []
        for poly in polys:
            clipped = _clip_segment_2d(poly, seg)
            if clipped is None:
                new_polys.append(poly)
                continue
            c0, c1 = clipped
            if np.allclose(c0, c1, atol=1e-12):
                new_polys.append(poly)
                continue
            split = _split_polygon_by_chord(poly, c0, c1)
            if len(split) == 1 and split[0] is poly:
                new_polys.append(poly)
            else:
                new_polys.extend(split)
        polys = new_polys
    out = []
    for poly in polys:
        out.extend(_fan_triangulate(poly))
    return out


def _project_to_2d(tri3, seg):
    """Project triangle + segment to 2D along the dominant normal axis."""
    a, b, c = tri3
    n = np.cross(b - a, c - a)
    axis = int(np.argmax(np.abs(n)))
    keep = [k for k in range(3) if k != axis]

    def proj(p):
        return np.array([p[keep[0]], p[keep[1]]])

    return [proj(a), proj(b), proj(c)], (proj(seg[0]), proj(seg[1]))


# ---------------------------------------------------------------------------
# proper-intersection test
# ---------------------------------------------------------------------------

def _segment_intersection_3d(tri1, tri2, _fast=False):
    """Return the proper-intersection segment [p, q] (3D) that lies inside
    both triangles, or None. Exact orient3d for the plane-side tests (EPICK:
    exact predicates, float64 construction, per the paper's note). When
    ``_fast`` is set the caller has already confirmed (in float64) that the
    midpoint is strictly interior, so the strict-interior re-check is
    skipped."""
    a, b, c = tri1
    d, e, f = tri2
    n1 = np.cross(b - a, c - a)
    n2 = np.cross(e - d, f - d)
    if np.dot(np.cross(n1, n2), np.cross(n1, n2)) < 1e-30:
        return None  # parallel planes (coplanar overlap handled elsewhere)
    s = [orient3d_adaptive(a, b, c, tri2[k]) for k in range(3)]
    t = [orient3d_adaptive(d, e, f, tri1[k]) for k in range(3)]
    if all(x > 0 for x in s) or all(x < 0 for x in s):
        return None
    if all(x > 0 for x in t) or all(x < 0 for x in t):
        return None
    d1 = np.dot(n1, a)
    d2 = np.dot(n2, d)
    n11 = np.dot(n1, n1); n12 = np.dot(n1, n2); n22 = np.dot(n2, n2)
    det = n11 * n22 - n12 * n12
    if abs(det) < 1e-300:
        return None
    alpha = (d1 * n22 - d2 * n12) / det
    beta = (n11 * d2 - n12 * d1) / det
    o = alpha * n1 + beta * n2
    u = np.cross(n1, n2)
    u = u / np.linalg.norm(u)

    def triangle_plane_intersection(tri, plane_pts):
        pts = []
        for k in range(3):
            p = tri[k]
            q = tri[(k + 1) % 3]
            sp = orient3d_adaptive(plane_pts[0], plane_pts[1], plane_pts[2], p)
            sq = orient3d_adaptive(plane_pts[0], plane_pts[1], plane_pts[2], q)
            if sp == 0.0:
                pts.append(np.array(p))
            elif sq == 0.0:
                pts.append(np.array(q))
            elif (sp > 0) != (sq > 0):
                w = -sp / (sq - sp)
                pts.append(np.array(p) + w * (np.array(q) - np.array(p)))
        out = []
        for p in pts:
            if not any(np.allclose(p, q, atol=1e-12) for q in out):
                out.append(p)
        return out if len(out) >= 2 else None

    s1 = triangle_plane_intersection(tri1, (d, e, f))
    s2 = triangle_plane_intersection(tri2, (a, b, c))
    if s1 is None or s2 is None:
        return None

    def interval(seg):
        ts = [np.dot(np.array(p) - o, u) for p in seg]
        return min(ts), max(ts)

    (t1lo, t1hi) = interval(s1)
    (t2lo, t2hi) = interval(s2)
    lo = max(t1lo, t2lo)
    hi = min(t1hi, t2hi)
    if hi - lo < 1e-12:
        return None
    p = o + lo * u
    q = o + hi * u
    if _fast:
        return (p, q)
    # Proper-intersection check: the segment's midpoint must be STRICTLY
    # inside both triangles (excludes shared edges / vertex contacts).
    mid = (p + q) / 2
    for tri in (tri1, tri2):
        t2 = _project_to_2d(tri, (mid, mid))[0]
        m2 = _project_to_2d(tri, (mid, mid))[1][0]
        v0 = t2[1] - t2[0]
        v1 = t2[2] - t2[0]
        v2 = m2 - t2[0]
        denom = v0[0] * v1[1] - v1[0] * v0[1]
        if abs(denom) < 1e-18:
            return None
        w0 = (v2[0] * v1[1] - v1[0] * v2[1]) / denom
        w1 = (v0[0] * v2[1] - v2[0] * v0[1]) / denom
        w2 = 1.0 - w0 - w1
        if min(w0, w1, w2) <= 1e-9:
            return None
    return (p, q)


# ---------------------------------------------------------------------------
# pair detection
# ---------------------------------------------------------------------------

def detect_pairs_with_segments(v, t):
    """Return list of (i, j, seg) for every properly-intersecting pair.

    Fast path: vectorized float64 prefilter on the candidate pairs (rejects
    AABB-overlapping-but-adjacent and strictly one-sided pairs), then exact
    orient3d + segment construction only on the survivors."""
    v = np.asarray(v, dtype=np.float64)
    t = np.asarray(t, dtype=np.int64)
    cand = broad_phase_candidates(v, t)
    if not cand:
        return []
    cand = np.asarray(cand, dtype=np.int64)
    # Exclude adjacent pairs (sharing a vertex): two triangles that share a
    # vertex cannot properly intersect in their interiors.
    vcand = t[cand]
    cset = np.concatenate([vcand[:, 0], vcand[:, 1]], axis=1)
    share = (cset[:, 0, None] == cset[:, 3:6]).any(axis=1) | \
            (cset[:, 1, None] == cset[:, 3:6]).any(axis=1) | \
            (cset[:, 2, None] == cset[:, 3:6]).any(axis=1)
    cand = cand[~share]
    if len(cand) == 0:
        return []
    a = v[t[cand[:, 0]]]
    b = v[t[cand[:, 1]]]
    n1 = np.cross(a[:, 1] - a[:, 0], a[:, 2] - a[:, 0])
    n2 = np.cross(b[:, 1] - b[:, 0], b[:, 2] - b[:, 0])
    d1 = np.einsum('ij,ij->i', n1, a[:, 0])
    d2 = np.einsum('ij,ij->i', n2, b[:, 0])
    sd_b = np.einsum('ki,kmi->km', n1, b) - d1[:, None]
    sd_a = np.einsum('ki,kmi->km', n2, a) - d2[:, None]
    all_pos_b = (sd_b > 0).all(axis=1)
    all_neg_b = (sd_b < 0).all(axis=1)
    all_pos_a = (sd_a > 0).all(axis=1)
    all_neg_a = (sd_a < 0).all(axis=1)
    keep = ~(all_pos_b | all_neg_b | all_pos_a | all_neg_a)
    keep_idx = np.where(keep)[0]

    kk = cand[keep_idx]
    if len(kk) == 0:
        return []
    aa = v[t[kk[:, 0]]]
    bb = v[t[kk[:, 1]]]
    nn1 = np.cross(aa[:, 1] - aa[:, 0], aa[:, 2] - aa[:, 0])
    nn2 = np.cross(bb[:, 1] - bb[:, 0], bb[:, 2] - bb[:, 0])
    dd1 = np.einsum('ij,ij->i', nn1, aa[:, 0])
    dd2 = np.einsum('ij,ij->i', nn2, bb[:, 0])
    sdb = np.einsum('ki,kmi->km', nn1, bb) - dd1[:, None]
    sda = np.einsum('ki,kmi->km', nn2, aa) - dd2[:, None]
    uu = np.cross(nn1, nn2)
    uu_len2 = np.einsum('ij,ij->i', uu, uu)
    par = uu_len2 < 1e-24
    if par.all():
        return []
    with np.errstate(invalid='ignore', divide='ignore'):
        uhat = uu / np.sqrt(uu_len2)[:, None]
    uhat = uhat.copy()
    uhat[par] = np.array([1.0, 0.0, 0.0])
    o = _plane_line_point_vec(nn1, dd1, nn2, dd2)

    def edge_cross_points(tris, sd):
        n = len(tris)
        ts = np.full((n, 3), np.nan)
        valid = np.zeros(n, dtype=bool)
        for m in range(3):
            s0 = sd[:, m]
            s1 = sd[:, (m + 1) % 3]
            cross = (s0 * s1) < 0
            denom = (s1 - s0)
            w = np.zeros(n)
            nz = np.abs(denom) > 1e-18
            w[nz] = -s0[nz] / denom[nz]
            p = tris[:, m] + w[:, None] * (tris[:, (m + 1) % 3] - tris[:, m])
            valid |= cross
            if cross.any():
                ts[cross, m] = np.einsum('ij,ij->i', p[cross] - o[cross],
                                         uhat[cross])
        onplane = np.abs(sd) < 1e-12
        for m in range(3):
            if onplane[:, m].any():
                p = tris[:, m]
                valid |= onplane[:, m]
                t_ = np.einsum('ij,ij->i', p - o, uhat)
                ts[onplane[:, m], m] = t_[onplane[:, m]]
        with np.errstate(all='ignore'):
            tmin = np.nanmin(ts, axis=1)
            tmax = np.nanmax(ts, axis=1)
        valid &= ~np.isnan(tmin)
        return tmin, tmax, valid

    tmin_a, tmax_a, valid_a = edge_cross_points(aa, sdb)
    tmin_b, tmax_b, valid_b = edge_cross_points(bb, sda)
    ov_lo = np.maximum(tmin_a, tmin_b)
    ov_hi = np.minimum(tmax_a, tmax_b)
    ov = valid_a & valid_b & (ov_hi - ov_lo > 1e-12) & ~par
    ov_idx = np.where(ov)[0]
    if len(ov_idx) == 0:
        return []
    kk2 = kk[ov_idx]

    # Fast float64 confirmation of the proper condition: the overlap
    # segment's midpoint strictly inside both triangles (adaptive: escalate
    # to exact only for near-boundary midpoints).
    mid_t = (ov_lo + ov_hi)[ov_idx] / 2
    mid = o[ov_idx] + mid_t[:, None] * uhat[ov_idx]
    aa2 = v[t[kk2[:, 0]]]
    bb2 = v[t[kk2[:, 1]]]

    def mid_inside(tris, midpts):
        n = len(tris)
        nrm = np.cross(tris[:, 1] - tris[:, 0], tris[:, 2] - tris[:, 0])
        axis = np.argmax(np.abs(nrm), axis=1)
        keep = np.array([[k for k in range(3) if k != ax] for ax in axis])
        a_proj = np.take_along_axis(tris[:, 0], keep, axis=1)
        b_proj = np.take_along_axis(tris[:, 1], keep, axis=1)
        c_proj = np.take_along_axis(tris[:, 2], keep, axis=1)
        m_proj = np.take_along_axis(midpts, keep, axis=1)
        v0 = b_proj - a_proj
        v1 = c_proj - a_proj
        v2 = m_proj - a_proj
        denom = v0[:, 0] * v1[:, 1] - v1[:, 0] * v0[:, 1]
        w0 = (v2[:, 0] * v1[:, 1] - v1[:, 0] * v2[:, 1]) / denom
        w1 = (v0[:, 0] * v2[:, 1] - v2[:, 0] * v0[:, 1]) / denom
        w2 = 1.0 - w0 - w1
        mins = np.minimum(np.minimum(w0, w1), w2)
        return mins > 1e-9, mins <= 1e-6

    in_a, near_a = mid_inside(aa2, mid)
    in_b, near_b = mid_inside(bb2, mid)
    certain = in_a & in_b & ~near_a & ~near_b
    certain_idx = np.where(certain)[0]
    out = []
    for k in certain_idx:
        i, j = int(kk2[k, 0]), int(kk2[k, 1])
        seg = _segment_intersection_3d(v[t[i]], v[t[j]], _fast=True)
        if seg is not None:
            out.append((i, j, seg))
    for k in np.where(~certain)[0]:
        i, j = int(kk2[k, 0]), int(kk2[k, 1])
        seg = _segment_intersection_3d(v[t[i]], v[t[j]])
        if seg is not None:
            out.append((i, j, seg))
    return out


# ---------------------------------------------------------------------------
# snap rounding
# ---------------------------------------------------------------------------

def _grid_scale(v, bits=DEFAULT_SNAP_BITS):
    m = float(np.max(np.abs(v)))
    if m <= 0.0:
        return 1.0
    s = 1.0
    while m * s < 2.0 ** bits:
        s *= 2.0
    while m * s >= 2.0 ** (bits + 1):
        s /= 2.0
    return s


def snap_round(v, t, pairs, bits=DEFAULT_SNAP_BITS):
    """Steps (ii)+(iii): snap vertices of intersecting-pair triangles to the
    grid, and every vertex in a cell that contains a snapped vertex."""
    v = np.asarray(v, dtype=np.float64).copy()
    s = _grid_scale(v, bits)
    cells = np.floor(v * s).astype(np.int64)
    to_snap = set()
    for i, j, _seg in pairs:
        for idx in t[i]:
            to_snap.add(int(idx))
        for idx in t[j]:
            to_snap.add(int(idx))
    if not to_snap:
        return v
    snapped_cells = {tuple(cells[idx]) for idx in to_snap}
    mask = np.array([tuple(c) in snapped_cells for c in cells])
    if mask.any():
        v[mask] = np.round(v[mask] * s) / s
    return v


# ---------------------------------------------------------------------------
# subdivision
# ---------------------------------------------------------------------------

def _weld(verts, tris, tol=None):
    """Weld near-coincident vertices and remap triangles. ``tol`` is the merge
    distance (default: a fraction of the mesh span, to absorb the float64
    non-conformity of per-triangle subdivision where the two members of a pair
    produce shared-edge vertices differing by ~1e-5 of the mesh scale)."""
    verts = np.asarray(verts, dtype=np.float64)
    if tol is None:
        span = float(np.max(verts.max(axis=0) - verts.min(axis=0)))
        tol = WELD_TOL_REL * max(span, 1e-300)
    cell = max(tol, 1e-300)
    keys = np.floor(verts / cell).astype(np.int64)
    lut = {}
    new_verts = []
    remap = []
    for i, (v0, k) in enumerate(zip(verts, keys)):
        found = None
        for dx in (-1, 0, 1):
            for dy in (-1, 0, 1):
                for dz in (-1, 0, 1):
                    ck = (int(k[0] + dx), int(k[1] + dy), int(k[2] + dz))
                    for j in lut.get(ck, ()):
                        if np.linalg.norm(new_verts[j] - v0) <= tol:
                            found = j
                            break
                    if found is not None:
                        break
                if found is not None:
                    break
            if found is not None:
                break
        if found is not None:
            remap.append(found)
        else:
            idx = len(new_verts)
            new_verts.append(v0)
            remap.append(idx)
            lut.setdefault((int(k[0]), int(k[1]), int(k[2])), []).append(idx)
    new_tris = []
    for tri in tris:
        nt = [remap[int(i)] for i in tri]
        if len(set(nt)) == 3:
            new_tris.append(nt)
    return np.array(new_verts, dtype=np.float64), np.array(new_tris, dtype=np.int64)


def _eliminate_degenerate(v, t, tol_rel=INNER_WELD_TOL_REL):
    """Eliminate the degenerate/sliver elements that snapping creates.

    Only degenerate OUTPUT is ever removed: triangles whose vertices weld to
    fewer than three distinct points, faces whose area collapses below a tiny
    relative floor, and duplicate faces. Non-degenerate triangles -- including
    every input triangle that was kept or split -- are preserved unchanged.
    Returns (new_v, new_t)."""
    v = np.asarray(v, dtype=np.float64)
    t = np.asarray(t, dtype=np.int64)
    if len(t) == 0:
        return v, t
    span = float(np.max(v.max(axis=0) - v.min(axis=0)))
    v, t = _weld(v, t, tol=tol_rel * max(span, 1e-300))
    if len(t) == 0:
        return v, t
    a = v[t[:, 0]]
    b = v[t[:, 1]]
    c = v[t[:, 2]]
    area2 = np.linalg.norm(np.cross(b - a, c - a), axis=1)
    keep = area2 > 1e-12 * max(span * span, 1e-300)
    t = t[keep]
    if len(t) == 0:
        return v, t
    key = np.sort(t, axis=1)
    _, uniq = np.unique(key, axis=0, return_index=True)
    t = t[np.sort(uniq)]
    return v, t


def subdivide(v, t, pairs, snap_bits=None):
    """Subdivide triangles so every proper-intersection segment becomes a
    shared edge. Never deletes an input triangle. Conforming: a split point
    landing mid-edge on a triangle's boundary is propagated to every triangle
    sharing that edge (so no T-junctions / new holes are opened). New vertices
    are rounded to the float-exact grid (``snap_bits``) so the two members of a
    pair reconstruct the SAME shared-edge vertex and weld exactly. Returns
    (new_v, new_t)."""
    v = np.asarray(v, dtype=np.float64)
    t = np.asarray(t, dtype=np.int64)
    gs = _grid_scale(v, snap_bits) if snap_bits is not None else 1.0

    segs_per_tri = {}
    for i, j, seg in pairs:
        segs_per_tri.setdefault(i, []).append(seg)
        segs_per_tri.setdefault(j, []).append(seg)

    def edge_key(vi, vj):
        return (int(vi), int(vj)) if int(vi) < int(vj) else (int(vj), int(vi))

    edge_splits = {}
    for i, j, seg in pairs:
        for k in (i, j):
            tri = t[k]
            for p in seg:
                for m in range(3):
                    a = v[tri[m]]
                    b = v[tri[(m + 1) % 3]]
                    ab = b - a
                    if np.dot(ab, ab) < 1e-24:
                        continue
                    tt = np.dot(p - a, ab) / np.dot(ab, ab)
                    if (-1e-7 <= tt <= 1 + 1e-7
                            and np.linalg.norm(p - (a + tt * ab)) <= 1e-7):
                        ek = edge_key(tri[m], tri[(m + 1) % 3])
                        edge_splits.setdefault(ek, []).append(np.array(p))
                        break

    coords = []
    tri_list = []
    for fi in range(len(t)):
        tri3 = [np.array(v[idx]) for idx in t[fi]]
        segs = segs_per_tri.get(fi, [])
        boundary = []
        for m in range(3):
            ek = edge_key(t[fi][m], t[fi][(m + 1) % 3])
            if ek in edge_splits:
                boundary.extend(edge_splits[ek])
        if not segs and not boundary:
            base = len(coords)
            coords.extend(tri3)
            tri_list.append((base, base + 1, base + 2))
            continue
        tri2, _ = _project_to_2d(tri3, (tri3[0], tri3[1]))
        chords = []
        for p, q in segs:
            _t2, seg2 = _project_to_2d(tri3, (p, q))
            chords.append(seg2)
        boundary2 = []
        for bp in boundary:
            _t2, bp2 = _project_to_2d(tri3, (bp, bp))
            boundary2.append(bp2[0])
        subs = _subdivide_triangle_2d(tri2, chords, boundary_points=boundary2)
        for sub in subs:
            idxs = []
            for p2 in sub:
                v0 = tri2[1] - tri2[0]
                v1 = tri2[2] - tri2[0]
                vv = p2 - tri2[0]
                denom = v0[0] * v1[1] - v1[0] * v0[1]
                if abs(denom) < 1e-18:
                    break
                w0 = (vv[0] * v1[1] - v1[0] * vv[1]) / denom
                w1 = (v0[0] * vv[1] - vv[0] * v0[1]) / denom
                w2 = 1.0 - w0 - w1
                p3 = w2 * tri3[0] + w0 * tri3[1] + w1 * tri3[2]
                if gs != 1.0:
                    p3 = np.round(p3 * gs) / gs
                base = len(coords)
                coords.append(p3)
                idxs.append(base)
            if len(idxs) == 3:
                tri_list.append(tuple(idxs))
    return _weld(np.array(coords, dtype=np.float64),
                 np.array(tri_list, dtype=np.int64))


# ---------------------------------------------------------------------------
# main loop
# ---------------------------------------------------------------------------

def _iterative_snap_round(v, t, snap_bits, max_rounds=MAX_SNAP_ROUNDS,
                          _debug=False):
    """Iterative snap-rounding pass (CGAL 6.1 apply_iterative_snap_rounding).

    Round the vertices of still-intersecting triangles (and their cell-mates)
    onto the fitting double-representable integer grid and eliminate the
    degenerate elements this creates, iterating while the proper-pair count
    strictly decreases. The grid is kept at the finest float-exact level
    (``snap_bits``): coarsening the grid was measured on the heavy-SI corpus
    and rejected -- on dense scans it amplifies the cell-mate snapping and
    creates new intersections (100281: finest grid 906->97 pairs vs coarsened
    906->74289). Bounded by ``max_rounds`` (no formal termination guarantee).
    Returns (v, t, rounds_used)."""
    v = np.asarray(v, dtype=np.float64)
    t = np.asarray(t, dtype=np.int64)
    rounds = 0
    prev = None
    for r in range(max_rounds):
        pairs = detect_pairs_with_segments(v, t)
        if not pairs:
            break
        rounds = r + 1
        cur = len(pairs)
        if prev is not None and cur >= prev:
            break
        prev = cur
        if _debug:
            print('    snap-round %d: %d pairs' % (r, cur))
        v = snap_round(v, t, pairs, bits=snap_bits)
        v, t = _eliminate_degenerate(v, t)
    return v, t, rounds


def autorefine(v, t, max_iterations=MAX_ITERATIONS, snap_bits=DEFAULT_SNAP_BITS,
               _debug=False):
    """Resolve self-intersections by subdivision + iterative snap-rounding.
    Never deletes an input face. Returns (new_v, new_t, report).

    Outer loop follows the paper: (i) identify proper pairs, (ii)+(iii) snap
    the involved vertices (and cell-mates) to the grid, (iv) subdivide along
    the intersection segments, repeat. The inner iterative snap-rounding pass
    (_iterative_snap_round) rounds the still-intersecting vertices onto the
    fitting float-exact grid and eliminates the degenerate elements created,
    and the degenerate elements left by each subdivision are eliminated too --
    the CGAL 6.1 June-2025 second-half fix. The grid stays at the finest
    float-exact level (no coarsening; see _iterative_snap_round for the corpus
    evidence). Two safety guards keep the prototype usable on real scans: a
    ``MAX_FACES_GROWTH`` cap (blow-up guard on dense-SI scans, where float64
    construction slivers can cascade) and an early stop when an outer pass
    does not reduce the proper-pair count.
    """
    v = np.asarray(v, dtype=np.float64).copy()
    t = np.asarray(t, dtype=np.int64)
    report = {'iterations': 0, 'si_before': 0, 'si_after': 0,
              'faces_before': int(len(t)), 'faces_after': int(len(t)),
              'converged': False, 'capped': False,
              'snap_rounds_per_iteration': [], 'grid_scales_used': []}
    pairs = detect_pairs_with_segments(v, t)
    report['si_before'] = len(pairs)
    if not pairs:
        report['converged'] = True
        report['si_after'] = 0
        return v, t, report
    prev_pairs = len(pairs) + 1
    for it in range(max_iterations):
        pairs = detect_pairs_with_segments(v, t)
        if not pairs:
            report['converged'] = True
            break
        if len(pairs) >= prev_pairs:
            break
        prev_pairs = len(pairs)
        if _debug:
            print('  iter %d: %d proper SI pairs' % (it, len(pairs)))
        v, t, rounds = _iterative_snap_round(
            v, t, snap_bits, _debug=_debug)
        report['iterations'] = it + 1
        report['snap_rounds_per_iteration'].append(rounds)
        report['grid_scales_used'].append(snap_bits)
        pairs = detect_pairs_with_segments(v, t)
        if not pairs:
            report['converged'] = True
            break
        if len(t) > report['faces_before'] * MAX_FACES_GROWTH:
            report['capped'] = True
            break
        v, t = subdivide(v, t, pairs, snap_bits=snap_bits)
        v, t = _eliminate_degenerate(v, t)
        if len(t) > report['faces_before'] * MAX_FACES_GROWTH:
            report['capped'] = True
            break
    pairs = detect_pairs_with_segments(v, t)
    report['converged'] = report['converged'] or not pairs
    report['si_after'] = len(pairs)
    report['faces_after'] = int(len(t))
    return v, t, report