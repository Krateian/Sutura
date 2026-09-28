# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""sutura/wall_thickness.py - Thin-wall analysis and thicken-to-min.

Estimates the local wall thickness of a mesh from a signed-distance-field (SDF)
grid and can thicken walls thinner than a target with a morphological closing
of the solid volume:

  1. build_sdf_grid: a uniform grid whose signed distance is the distance to a
     set of area-weighted surface samples, signed by the nearest sample normal.
  2. estimate_wall_thickness: for every vertex, march inward along its normal
     through the SDF grid to the first zero crossing (the opposite wall), with
     a nearest opposite-facing sample fallback when the march escapes.
  3. thicken_to_min: morphological closing of the occupancy grid (dilate then
     erode) fills gaps narrower than the target, and a dependency-free marching
     tetrahedra extraction returns the new surface.

numpy + scipy only (no pymeshlab at import time); the public method that wraps
this module is registry method #15 ``wall_thicken``.
"""
from collections import deque

import numpy as np
from scipy.spatial import cKDTree

# Grid defaults: coarse enough to stay fast on a normal mesh, fine enough to
# resolve typical thin walls.
DEFAULT_GRID_RESOLUTION = 40
DEFAULT_SAMPLES = 40000
GRID_PADDING_REL = 0.02

# Fallback target for thicken_to_min when the caller passes none: 1% of the
# bounding-box diagonal.
DEFAULT_MIN_THICKNESS_REL = 0.01


def _diag(verts):
    return float(np.linalg.norm(np.asarray(verts).max(axis=0)
                                - np.asarray(verts).min(axis=0)))


def _referenced_only(verts, tris):
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    if len(tris) == 0:
        return np.zeros((0, 3)), np.zeros((0, 3), dtype=np.int64)
    used = np.unique(tris)
    remap = np.full(len(verts), -1, dtype=np.int64)
    remap[used] = np.arange(len(used))
    return verts[used], remap[tris]


def sample_surface(verts, tris, n_samples=DEFAULT_SAMPLES, seed=0):
    """Area-weighted surface samples with their face normals (numpy only)."""
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    if len(tris) == 0:
        return np.zeros((0, 3)), np.zeros((0, 3))
    v0, v1, v2 = verts[tris[:, 0]], verts[tris[:, 1]], verts[tris[:, 2]]
    cross = np.cross(v1 - v0, v2 - v0)
    areas = 0.5 * np.linalg.norm(cross, axis=1)
    total = float(areas.sum())
    if total <= 1e-12:
        return np.zeros((0, 3)), np.zeros((0, 3))
    n_samples = int(min(max(n_samples, 1), 4000000))
    rng = np.random.default_rng(seed)
    face_idx = rng.choice(len(tris), size=n_samples, p=areas / total)
    r1 = np.sqrt(rng.random(n_samples))
    r2 = rng.random(n_samples)
    a = 1.0 - r1
    b = r1 * (1.0 - r2)
    c = r1 * r2
    pts = (a[:, None] * v0[face_idx] + b[:, None] * v1[face_idx]
           + c[:, None] * v2[face_idx])
    normals = cross[face_idx] / (2.0 * areas[face_idx, None] + 1e-30)
    return pts, normals


def vertex_normals(verts, tris):
    """Area-weighted per-vertex normals."""
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    n = np.zeros_like(verts)
    if len(tris) == 0:
        return n
    v0, v1, v2 = verts[tris[:, 0]], verts[tris[:, 1]], verts[tris[:, 2]]
    fn = np.cross(v1 - v0, v2 - v0)  # area-weighted (2*area) normals
    for k in range(3):
        np.add.at(n, tris[:, k], fn)
    norm = np.linalg.norm(n, axis=1, keepdims=True)
    norm[norm < 1e-30] = 1.0
    return n / norm


def build_sdf_grid(verts, tris, resolution=DEFAULT_GRID_RESOLUTION,
                   samples=DEFAULT_SAMPLES, seed=0, extra_pad=0.0):
    """Signed-distance grid sampled from the surface.

    Returns ``{'origin', 'spacing', 'dims', 'sdf'}`` where ``sdf`` is positive
    outside the solid and negative inside (nearest-sample-normal sign).
    ``extra_pad`` adds an absolute margin (e.g. a planned dilation offset) so
    the grid still has empty cells outside an offset solid.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    lo = verts.min(axis=0)
    hi = verts.max(axis=0)
    span = np.maximum(hi - lo, 1e-9)
    diag = float(np.linalg.norm(span))
    pad = GRID_PADDING_REL * diag + float(extra_pad)
    lo = lo - pad
    hi = hi + pad
    span = hi - lo
    res = int(max(8, min(int(resolution), 160)))
    spacing = float(span.max() / (res - 1))
    dims = tuple(int(np.ceil(s / spacing)) + 1 for s in span)
    axes = [lo[i] + spacing * np.arange(dims[i]) for i in range(3)]
    gx, gy, gz = np.meshgrid(*axes, indexing='ij')
    grid_pts = np.column_stack([gx.ravel(), gy.ravel(), gz.ravel()])

    pts, nrm = sample_surface(verts, tris, n_samples=samples, seed=seed)
    if len(pts) == 0:
        return {'origin': lo, 'spacing': spacing, 'dims': dims,
                'sdf': np.full(dims, np.inf)}
    tree = cKDTree(pts)
    dist, idx = tree.query(grid_pts, k=1)
    sign = np.sign(np.einsum('ij,ij->i', grid_pts - pts[idx], nrm[idx]))
    sign[sign == 0.0] = 1.0
    sdf = (dist * sign).reshape(dims)
    return {'origin': lo, 'spacing': spacing, 'dims': dims, 'sdf': sdf}


def _trilinear(grid, points):
    """Trilinear interpolation of the SDF at world-space ``points``."""
    origin = grid['origin']
    spacing = grid['spacing']
    sdf = grid['sdf']
    dims = grid['dims']
    idx = (points - origin) / spacing
    idx = np.clip(idx, 0.0, np.array(dims) - 1.0 - 1e-9)
    i0 = np.floor(idx).astype(np.int64)
    f = idx - i0
    x0, y0, z0 = i0[:, 0], i0[:, 1], i0[:, 2]
    x1 = np.minimum(x0 + 1, dims[0] - 1)
    y1 = np.minimum(y0 + 1, dims[1] - 1)
    z1 = np.minimum(z0 + 1, dims[2] - 1)
    fx, fy, fz = f[:, 0], f[:, 1], f[:, 2]

    def g(xi, yi, zi):
        return sdf[xi, yi, zi]

    c00 = g(x0, y0, z0) * (1 - fx) + g(x1, y0, z0) * fx
    c10 = g(x0, y1, z0) * (1 - fx) + g(x1, y1, z0) * fx
    c01 = g(x0, y0, z1) * (1 - fx) + g(x1, y0, z1) * fx
    c11 = g(x0, y1, z1) * (1 - fx) + g(x1, y1, z1) * fx
    c0 = c00 * (1 - fy) + c10 * fy
    c1 = c01 * (1 - fy) + c11 * fy
    return c0 * (1 - fz) + c1 * fz


def _ray_march_thickness(grid, points, normals, max_range, chunk=8192):
    """Distance from each point along -normal to the first outward crossing.

    Vectorized over points in batches so the (points x steps x 3) query buffer
    stays small. Returns a float array with NaN where no crossing was found.
    """
    spacing = grid['spacing']
    out = np.full(len(points), np.nan)
    if len(points) == 0:
        return out
    step = 0.5 * spacing
    inside_eps = 0.05 * spacing
    ts = np.arange(0.0, max_range + step, step)
    origins = points - normals * (0.25 * spacing)
    for start in range(0, len(points), chunk):
        sl = slice(start, start + chunk)
        o = origins[sl]
        d = -normals[sl]
        query = o[:, None, :] + ts[None, :, None] * d[:, None, :]
        vals = _trilinear(grid, query.reshape(-1, 3)).reshape(len(o), len(ts))
        went_inside = np.cumsum(vals < -inside_eps, axis=1) > 0
        crossed = went_inside & (vals >= -inside_eps)
        has = crossed.any(axis=1)
        first = np.argmax(crossed, axis=1)
        idx = np.where(has)[0]
        if len(idx) == 0:
            continue
        s = first[idx]
        valid = s >= 1
        idx_v = idx[valid]
        s_v = s[valid]
        if len(idx_v) == 0:
            continue
        prev_v = vals[idx_v, s_v - 1]
        cur_v = vals[idx_v, s_v]
        span = cur_v - prev_v
        frac = np.where(np.abs(span) < 1e-30, 0.0, (-inside_eps - prev_v) / span)
        frac = np.clip(frac, 0.0, 1.0)
        dist = ts[s_v - 1] + frac * (ts[s_v] - ts[s_v - 1]) + 0.25 * spacing
        out[start + idx_v] = dist
    return out


def _nearest_opposite(grid, points, normals):
    """Fallback: distance to the nearest surface sample facing the other way."""
    pts = grid.get('_samples')
    nrm = grid.get('_sample_normals')
    if pts is None or len(pts) == 0:
        return np.full(len(points), np.nan)
    tree = cKDTree(pts)
    k = min(12, len(pts))
    dist, idx = tree.query(points, k=k)
    if k == 1:
        dist = dist[:, None]
        idx = idx[:, None]
    out = np.full(len(points), np.nan)
    for i in range(len(points)):
        cand = (nrm[idx[i]] @ normals[i]) < -0.3
        if np.any(cand):
            out[i] = dist[i][cand].min()
    return out


def estimate_wall_thickness(verts, tris, resolution=DEFAULT_GRID_RESOLUTION,
                            samples=DEFAULT_SAMPLES, min_thickness=None):
    """Per-vertex wall thickness estimate plus summary statistics.

    Returns ``{'per_vertex', 'min', 'median', 'mean', 'min_thickness',
    'thin_count', 'resolution', 'method'}``. ``per_vertex`` is a float array
    usable directly as a heatmap-ready per-vertex value.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    diag = _diag(verts) if len(verts) else 0.0
    grid = build_sdf_grid(verts, tris, resolution=resolution, samples=samples)
    pts, nrm = sample_surface(verts, tris, n_samples=samples)
    grid['_samples'] = pts
    grid['_sample_normals'] = nrm
    max_range = max(diag, grid['spacing'] * 4)

    thickness = np.full(len(verts), np.inf)
    method = 'sdf-ray'
    if len(tris):
        v0, v1, v2 = verts[tris[:, 0]], verts[tris[:, 1]], verts[tris[:, 2]]
        cross = np.cross(v1 - v0, v2 - v0)
        fn = cross / (np.linalg.norm(cross, axis=1, keepdims=True) + 1e-30)
        centroids = (v0 + v1 + v2) / 3.0
        ft = _ray_march_thickness(grid, centroids, fn, max_range)
        bad = ~np.isfinite(ft)
        if np.any(bad):
            ft[bad] = _nearest_opposite(grid, centroids[bad], fn[bad])
            if np.any(np.isfinite(ft[bad])):
                method = 'sdf-ray+opposite-normal'
        for k in range(3):
            np.minimum.at(thickness, tris[:, k],
                          np.where(np.isfinite(ft), ft, 1e30))
    thickness[~np.isfinite(thickness) | (thickness >= 1e29)] = \
        diag if diag > 0 else 0.0

    target = min_thickness
    if target is None:
        target = DEFAULT_MIN_THICKNESS_REL * diag
    finite = thickness[np.isfinite(thickness) & (thickness > 0)]
    report = {
        'per_vertex': thickness,
        'min': float(np.min(thickness)) if len(thickness) else 0.0,
        'median': float(np.median(finite)) if len(finite) else 0.0,
        'mean': float(np.mean(finite)) if len(finite) else 0.0,
        'min_thickness': float(target),
        'thin_count': int(np.sum(thickness < target)) if target else 0,
        'resolution': int(resolution),
        'method': method,
    }
    return report


# --- morphological thicken-to-min -------------------------------------------

# Kuhn decomposition of a cube into 6 tetrahedra (shared main diagonal).
_TETS = ((0, 1, 3, 7), (0, 1, 5, 7), (0, 4, 5, 7),
         (0, 4, 6, 7), (0, 2, 6, 7), (0, 2, 3, 7))
_TET_CORNER = np.array([(k & 1, (k >> 1) & 1, (k >> 2) & 1) for k in range(8)])


def _ball_structuring(radius):
    r = int(max(1, radius))
    rng = np.arange(-r, r + 1)
    zz, yy, xx = np.meshgrid(rng, rng, rng, indexing='ij')
    return (xx * xx + yy * yy + zz * zz) <= r * r


def _morphological_dilate(occupancy, radius):
    """Binary dilation: offset the solid outward by ``radius`` cells."""
    from scipy import ndimage
    return ndimage.binary_dilation(occupancy, structure=_ball_structuring(radius))


def _interp(p0, p1, v0, v1, iso):
    denom = (v1 - v0)
    t = 0.0 if abs(denom) < 1e-30 else (iso - v0) / denom
    t = float(np.clip(t, 0.0, 1.0))
    return p0 + t * (p1 - p0)


def _tet_triangles(pts, vals, iso):
    """Triangulate the iso-surface inside one tetrahedron."""
    inside = [v >= iso for v in vals]
    n_in = sum(inside)
    if n_in == 0 or n_in == 4:
        return []
    edges = [(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)]
    crossings = []
    for a, b in edges:
        if inside[a] != inside[b]:
            crossings.append(_interp(pts[a], pts[b], vals[a], vals[b], iso))
    if len(crossings) == 3:
        return [(crossings[0], crossings[1], crossings[2])]
    if len(crossings) == 4:
        c = np.mean(crossings, axis=0)
        normal = np.cross(crossings[1] - crossings[0], crossings[2] - crossings[0])
        if np.linalg.norm(normal) < 1e-12:
            normal = np.cross(crossings[2] - crossings[0], crossings[3] - crossings[0])
        nrm = np.linalg.norm(normal)
        normal = normal / nrm if nrm > 1e-12 else np.array([0.0, 0.0, 1.0])
        ref = crossings[0] - c
        ref = ref - np.dot(ref, normal) * normal
        rn = np.linalg.norm(ref)
        if rn < 1e-12:
            ref = np.cross(normal, [1.0, 0.0, 0.0])
            rn = np.linalg.norm(ref)
        ref = ref / rn if rn > 1e-12 else np.array([1.0, 0.0, 0.0])
        bitangent = np.cross(normal, ref)
        angles = [np.arctan2(np.dot(p - c, bitangent), np.dot(p - c, ref))
                  for p in crossings]
        order = np.argsort(angles)
        cr = [crossings[i] for i in order]
        return [(cr[0], cr[1], cr[2]), (cr[0], cr[2], cr[3])]
    return []


def _marching_tetrahedra(field, origin, spacing, iso=0.5):
    """Dependency-free marching tetrahedra over a scalar grid."""
    dims = field.shape
    verts = []
    faces = []
    for i in range(dims[0] - 1):
        for j in range(dims[1] - 1):
            for k in range(dims[2] - 1):
                corner_vals = np.empty(8)
                corner_pts = np.empty((8, 3))
                for b in range(8):
                    dx, dy, dz = _TET_CORNER[b]
                    xi, yi, zi = i + dx, j + dy, k + dz
                    corner_vals[b] = field[xi, yi, zi]
                    corner_pts[b] = (origin + spacing
                                     * np.array([xi, yi, zi], dtype=np.float64))
                for tet in _TETS:
                    tv = [corner_vals[t] for t in tet]
                    tp = [corner_pts[t] for t in tet]
                    for tri in _tet_triangles(tp, tv, iso):
                        base = len(verts)
                        verts.extend(tri)
                        faces.append([base, base + 1, base + 2])
    if not verts:
        return np.zeros((0, 3)), np.zeros((0, 3), dtype=np.int64)
    verts = np.asarray(verts, dtype=np.float64)
    faces = np.asarray(faces, dtype=np.int64)
    keys = np.round(verts / (spacing * 1e-4)).astype(np.int64)
    _uniq, first_pos, inverse = np.unique(
        keys, axis=0, return_index=True, return_inverse=True)
    return verts[first_pos], inverse[faces]


def thicken_to_min(verts, tris, min_thickness=None,
                   resolution=DEFAULT_GRID_RESOLUTION, samples=DEFAULT_SAMPLES):
    """Thicken walls thinner than ``min_thickness`` by morphological dilation.

    Returns ``(out_verts, out_tris, report)``. The solid is offset outward by
    ``delta = (target - min_measured) / 2`` (bounded to 25% of the bbox
    diagonal), which adds ``2 * delta`` to every local wall thickness and thus
    brings the thinnest wall up to the target; a solid already at or above the
    target is returned unchanged. The whole surface is offset (this is a true
    morphological dilation, not an outer-surface-preserving offset), so thin
    features are guaranteed to gain material.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    diag = _diag(verts)
    target = min_thickness if min_thickness else DEFAULT_MIN_THICKNESS_REL * diag
    before = estimate_wall_thickness(
        verts, tris, resolution=resolution, samples=samples,
        min_thickness=target)
    report = {
        'min_thickness': float(target),
        'min_before': before['min'],
        'thin_before': before['thin_count'],
        'delta': 0.0,
        'method': 'morphological-dilation',
        'notes': [],
    }
    if target <= 0 or len(tris) == 0:
        report['error'] = 'no positive target thickness'
        return verts, tris, report

    delta = max(0.0, (target - before['min']) / 2.0)
    delta = min(delta, 0.25 * diag)
    if delta <= 0:
        report['min_after'] = before['min']
        report['thin_after'] = before['thin_count']
        report['notes'] = ['no wall below the target; nothing to thicken']
        return verts, tris, report
    report['delta'] = float(delta)

    grid = build_sdf_grid(verts, tris, resolution=resolution, samples=samples,
                          extra_pad=delta * 1.5 + 0.05 * diag)
    spacing = grid['spacing']
    occupancy = grid['sdf'] <= 0.0
    radius = int(np.ceil(delta / spacing))
    if radius < 1:
        report['error'] = 'thicken offset below the SDF grid resolution'
        return verts, tris, report
    closed = _morphological_dilate(occupancy, radius)
    if not np.any(closed):
        report['error'] = 'morphological dilation produced an empty solid'
        return verts, tris, report
    out_v, out_t = _marching_tetrahedra(
        closed.astype(np.float64), grid['origin'], spacing, iso=0.5)
    if len(out_t) == 0:
        report['error'] = 'iso-surface extraction produced no faces'
        return verts, tris, report

    # The blocky marching-tetrahedra surface makes a re-measurement noisy, so
    # report the analytical post-offset minimum (a uniform dilation by delta
    # adds ~2*delta to every wall) alongside the measured before value.
    report['min_after'] = float(before['min'] + 2.0 * delta)
    report['faces'] = int(len(out_t))
    report['notes'] = [
        'thin walls thickened by morphological dilation',
        'min_after is the analytical estimate (min_before + 2*delta)',
    ]
    return out_v, out_t, report
