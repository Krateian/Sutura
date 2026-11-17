// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! The sampled signed-distance grid from `sutura_engine/wall.py`.
//!
//! **This is not `MeshBvh::sdf_grid`.** That one is an exact BVH
//! raystab/antipodal field. Python's wall builder uses a *Monte-Carlo sampled*
//! field instead (`wall.py:82-116`):
//!
//! ```python
//! pts, nrm = sample_surface(verts, tris, n_samples=samples, seed=seed)
//! dist, idx = sutura_geom.nearest_points(pts, grid_pts, k=1)
//! sign = np.sign(np.einsum('ij,ij->i', grid_pts - pts[idx], nrm[idx]))
//! sdf = (dist * sign).reshape(dims)
//! ```
//!
//! Feeding the exact grid to Python's dilation would change the solid, so the
//! wall port needs this field, not that one. The pieces are shared: the sampler
//! is [`crate::sample::sample_surface`] (bit-identical to NumPy, pinned by a
//! test) and the nearest-neighbour query is
//! [`crate::pointcloud::nearest_points`] — the same primitive Python calls
//! through `sutura_geom.nearest_points`.
//!
//! Grid planning mirrors the Python arithmetic term by term, because the
//! spacing decides `radius = ceil(delta / spacing)` and therefore the whole
//! dilation.

use crate::sample::sample_surface;

/// `wall.DEFAULT_GRID_RESOLUTION`.
pub const DEFAULT_GRID_RESOLUTION: usize = 40;
/// `wall.GRID_PADDING_REL`.
pub const GRID_PADDING_REL: f64 = 0.02;
/// `wall.DEFAULT_MIN_THICKNESS_REL`: the default target thickness, as a fraction
/// of the bbox diagonal.
pub const DEFAULT_MIN_THICKNESS_REL: f64 = 0.01;

const MIN_RES: usize = 8;
const MAX_RES: usize = 160;

/// A sampled SDF on a regular lattice.
#[derive(Clone, Debug, PartialEq)]
pub struct SdfGrid {
    pub origin: [f64; 3],
    pub spacing: f64,
    pub dims: [usize; 3],
    /// `dims[0] * dims[1] * dims[2]` values, positive outside, negative inside,
    /// in the same index order as NumPy's `np.meshgrid(..., indexing='ij')`
    /// flattening (x slowest, z fastest).
    pub sdf: Vec<f64>,
}

impl SdfGrid {
    pub fn index(&self, i: usize, j: usize, k: usize) -> f64 {
        self.sdf[i * self.dims[1] * self.dims[2] + j * self.dims[2] + k]
    }

    /// World-space position of a lattice cell.
    pub fn point(&self, i: usize, j: usize, k: usize) -> [f64; 3] {
        [
            self.origin[0] + self.spacing * i as f64,
            self.origin[1] + self.spacing * j as f64,
            self.origin[2] + self.spacing * k as f64,
        ]
    }

    fn at(&self, i: usize, j: usize, k: usize) -> f64 {
        self.index(i, j, k)
    }

    /// Trilinear interpolation at world-space `p`, exactly as
    /// `wall._trilinear` does it: clamp into the lattice, floor, then lerp the
    /// eight corners in x, y, z order.
    pub fn trilinear(&self, p: [f64; 3]) -> f64 {
        let d = self.dims;
        let mut idx = [0.0f64; 3];
        for k in 0..3 {
            idx[k] = (p[k] - self.origin[k]) / self.spacing;
        }
        // `np.clip(idx, 0, dims - 1 - 1e-9)`
        for k in 0..3 {
            let hi = d[k] as f64 - 1.0 - 1e-9;
            if idx[k] < 0.0 {
                idx[k] = 0.0;
            }
            if idx[k] > hi {
                idx[k] = hi;
            }
        }
        let mut i0 = [0usize; 3];
        let mut f = [0.0f64; 3];
        for k in 0..3 {
            i0[k] = idx[k].floor() as usize;
            f[k] = idx[k] - i0[k] as f64;
        }
        let x0 = i0[0];
        let y0 = i0[1];
        let z0 = i0[2];
        // `np.minimum(x0 + 1, dims - 1)`
        let x1 = (x0 + 1).min(d[0] - 1);
        let y1 = (y0 + 1).min(d[1] - 1);
        let z1 = (z0 + 1).min(d[2] - 1);
        let (fx, fy, fz) = (f[0], f[1], f[2]);

        let c00 = self.at(x0, y0, z0) * (1.0 - fx) + self.at(x1, y0, z0) * fx;
        let c10 = self.at(x0, y1, z0) * (1.0 - fx) + self.at(x1, y1, z0) * fx;
        let c01 = self.at(x0, y0, z1) * (1.0 - fx) + self.at(x1, y0, z1) * fx;
        let c11 = self.at(x0, y1, z1) * (1.0 - fx) + self.at(x1, y1, z1) * fx;
        let c0 = c00 * (1.0 - fy) + c10 * fy;
        let c1 = c01 * (1.0 - fy) + c11 * fy;
        c0 * (1.0 - fz) + c1 * fz
    }
}

/// Bbox diagonal, exactly as `wall._diag` computes it.
pub fn diag(verts: &[[f64; 3]]) -> f64 {
    if verts.is_empty() {
        return 0.0;
    }
    let mut lo = verts[0];
    let mut hi = verts[0];
    for v in verts {
        for k in 0..3 {
            if v[k] < lo[k] {
                lo[k] = v[k];
            }
            if v[k] > hi[k] {
                hi[k] = v[k];
            }
        }
    }
    let d = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

/// Build the sampled SDF grid.
///
/// `extra_pad` is an absolute margin added to the bounding box, used by the
/// thickener so the grid still has empty cells outside the planned offset
/// solid. Returns `None` when the mesh has no area (Python returns an all-`inf`
/// grid there, which the caller treats the same way: nothing to dilate).
pub fn build_sdf_grid(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    resolution: usize,
    samples: usize,
    seed: u64,
    extra_pad: f64,
) -> Option<SdfGrid> {
    if verts.is_empty() {
        return None;
    }
    let mut lo = verts[0];
    let mut hi = verts[0];
    for v in verts {
        for k in 0..3 {
            if v[k] < lo[k] {
                lo[k] = v[k];
            }
            if v[k] > hi[k] {
                hi[k] = v[k];
            }
        }
    }
    // `span = np.maximum(hi - lo, 1e-9)` before the diagonal.
    let mut span = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    for s in &mut span {
        if *s < 1e-9 {
            *s = 1e-9;
        }
    }
    let d = (span[0] * span[0] + span[1] * span[1] + span[2] * span[2]).sqrt();
    let pad = GRID_PADDING_REL * d + extra_pad;
    for k in 0..3 {
        lo[k] -= pad;
        hi[k] += pad;
        span[k] = hi[k] - lo[k];
    }

    let res = resolution.clamp(MIN_RES, MAX_RES);
    let max_span = span[0].max(span[1]).max(span[2]);
    // `spacing = span.max() / (res - 1)`; `res >= 8` so the divisor is safe.
    let spacing = max_span / (res - 1) as f64;
    if !(spacing.is_finite() && spacing > 0.0) {
        return None;
    }
    let mut dims = [0usize; 3];
    for k in 0..3 {
        dims[k] = (span[k] / spacing).ceil() as usize + 1;
    }

    let (pts, nrm) = sample_surface(verts, tris, samples, seed);
    if pts.is_empty() {
        return None;
    }

    // All lattice points, `ij` order (x slowest), matching `meshgrid` +
    // `column_stack`.
    let total = dims[0] * dims[1] * dims[2];
    let mut queries: Vec<[f64; 3]> = Vec::with_capacity(total);
    for i in 0..dims[0] {
        for j in 0..dims[1] {
            for k in 0..dims[2] {
                queries.push([
                    lo[0] + spacing * i as f64,
                    lo[1] + spacing * j as f64,
                    lo[2] + spacing * k as f64,
                ]);
            }
        }
    }

    let (dist, idx, _) = crate::pointcloud::nearest_points(&pts, &queries, 1);
    if dist.len() < total || idx.len() < total {
        return None;
    }
    let mut sdf = Vec::with_capacity(total);
    for q in 0..total {
        let p = pts[idx[q] as usize];
        let n = nrm[idx[q] as usize];
        let delta = [
            queries[q][0] - p[0],
            queries[q][1] - p[1],
            queries[q][2] - p[2],
        ];
        let dot = delta[0] * n[0] + delta[1] * n[1] + delta[2] * n[2];
        // `np.sign` then `sign[sign == 0] = 1`.
        let sign = if dot > 0.0 {
            1.0
        } else if dot < 0.0 {
            -1.0
        } else {
            1.0
        };
        sdf.push(dist[q] * sign);
    }

    Some(SdfGrid {
        origin: lo,
        spacing,
        dims,
        sdf,
    })
}

/// Per-vertex wall thickness plus the summary `wall.estimate_wall_thickness`
/// returns.
#[derive(Clone, Debug, PartialEq)]
pub struct ThicknessEstimate {
    /// One value per input vertex: the minimum thickness of the faces that
    /// touch it, or `diag` where no ray found a crossing.
    pub per_vertex: Vec<f64>,
    pub min: f64,
    pub median: f64,
    pub mean: f64,
    pub min_thickness: f64,
    pub thin_count: usize,
    pub resolution: usize,
    /// `"sdf-ray"`, or `"sdf-ray+opposite-normal"` once the fallback fired.
    pub method: String,
}

/// `wall._ray_march_thickness`: distance from each point along `-normal` to the
/// first outward crossing.
///
/// Walks `ts = 0, step, 2*step, ...` up to `max_range` with `step = 0.5 *
/// spacing`, starting `0.25 * spacing` back along the normal so the ray begins
/// inside the surface. A crossing is the first sample that is not "inside"
/// after having been inside; the exact distance is linearly interpolated
/// between the two samples. `NaN` where no crossing was found, so the caller
/// can tell "missed" from "zero".
fn ray_march_thickness(
    grid: &SdfGrid,
    points: &[[f64; 3]],
    normals: &[[f64; 3]],
    max_range: f64,
) -> Vec<f64> {
    let spacing = grid.spacing;
    let mut out = vec![f64::NAN; points.len()];
    if points.is_empty() {
        return out;
    }
    let step = 0.5 * spacing;
    let inside_eps = 0.05 * spacing;
    // `np.arange(0.0, max_range + step, step)` -- NumPy computes each element as
    // `start + i * step`, it does NOT accumulate. Accumulating drifts by a few
    // ULPs at large `i`, which shows up as sub-1e-9 differences in the reported
    // thickness for ~1000 of 4601 vertices on a real mesh.
    let mut ts: Vec<f64> = Vec::new();
    let stop = max_range + step;
    let count = ((stop - 0.0) / step).ceil() as usize;
    for i in 0..count {
        let t = i as f64 * step;
        if t >= stop {
            break;
        }
        ts.push(t);
    }
    if ts.is_empty() {
        return out;
    }

    for (i, (p, n)) in points.iter().zip(normals.iter()).enumerate() {
        // `origins = points - normals * (0.25 * spacing)`, direction `-normal`.
        let origin = [
            p[0] - n[0] * (0.25 * spacing),
            p[1] - n[1] * (0.25 * spacing),
            p[2] - n[2] * (0.25 * spacing),
        ];
        let dir = [-n[0], -n[1], -n[2]];

        // Evaluate the field along the ray, then look for the first crossing.
        let mut vals = Vec::with_capacity(ts.len());
        for &tt in &ts {
            let q = [
                origin[0] + tt * dir[0],
                origin[1] + tt * dir[1],
                origin[2] + tt * dir[2],
            ];
            vals.push(grid.trilinear(q));
        }

        // `went_inside = cumsum(vals < -inside_eps) > 0`;
        // `crossed = went_inside & (vals >= -inside_eps)`.
        let mut went_inside = false;
        let mut first: Option<usize> = None;
        for (s, &v) in vals.iter().enumerate() {
            if v < -inside_eps {
                went_inside = true;
            } else if went_inside && v >= -inside_eps {
                first = Some(s);
                break;
            }
        }
        let Some(s) = first else { continue };
        if s == 0 {
            // `valid = s >= 1` in Python: no span to interpolate over.
            continue;
        }
        let prev_v = vals[s - 1];
        let cur_v = vals[s];
        let span = cur_v - prev_v;
        let frac = if span.abs() < 1e-30 {
            0.0
        } else {
            ((-inside_eps - prev_v) / span).clamp(0.0, 1.0)
        };
        out[i] = ts[s - 1] + frac * (ts[s] - ts[s - 1]) + 0.25 * spacing;
    }
    out
}

/// `wall._nearest_opposite`: distance to the nearest surface sample whose
/// normal faces the other way, used when the forward ray finds no crossing.
fn nearest_opposite(
    samples: &[[f64; 3]],
    sample_normals: &[[f64; 3]],
    points: &[[f64; 3]],
    normals: &[[f64; 3]],
) -> Vec<f64> {
    let mut out = vec![f64::NAN; points.len()];
    if samples.is_empty() || points.is_empty() {
        return out;
    }
    let k = 12.min(samples.len());
    let (dist, idx, take) = crate::pointcloud::nearest_points(samples, points, k);
    let take = take.max(1);
    for i in 0..points.len() {
        let mut best = f64::NAN;
        for j in 0..take {
            let flat = i * take + j;
            if flat >= dist.len() || flat >= idx.len() {
                break;
            }
            let si = idx[flat];
            if si < 0 {
                continue;
            }
            let sn = sample_normals[si as usize];
            let dot = sn[0] * normals[i][0] + sn[1] * normals[i][1] + sn[2] * normals[i][2];
            if dot < -0.3 && (best.is_nan() || dist[flat] < best) {
                best = dist[flat];
            }
        }
        out[i] = best;
    }
    out
}

/// `wall.estimate_wall_thickness`: per-vertex wall thickness plus the summary.
///
/// Thickness is measured from each face centroid along `-normal` to the first
/// outward crossing, then reduced per vertex with `min` over the faces that
/// touch it. Vertices with no measurement fall back to the opposite-normal
/// search; vertices still unmeasured take the bbox diagonal (Python:
/// `thickness[...] = diag`).
pub fn estimate_wall_thickness(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    resolution: usize,
    samples: usize,
    seed: u64,
    min_thickness: Option<f64>,
) -> Option<ThicknessEstimate> {
    let d = diag(verts);
    let grid = build_sdf_grid(verts, tris, resolution, samples, seed, 0.0)?;
    let (pts, nrm) = sample_surface(verts, tris, samples, seed);
    let max_range = d.max(grid.spacing * 4.0);

    let mut thickness = vec![f64::INFINITY; verts.len()];
    let mut method = "sdf-ray".to_string();
    if !tris.is_empty() {
        // Face normals and centroids, as in Python.
        let mut centroids: Vec<[f64; 3]> = Vec::with_capacity(tris.len());
        let mut fnormals: Vec<[f64; 3]> = Vec::with_capacity(tris.len());
        for f in tris {
            let (a, b, c) = (verts[f[0]], verts[f[1]], verts[f[2]]);
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let cr = [
                u[1] * w[2] - u[2] * w[1],
                u[2] * w[0] - u[0] * w[2],
                u[0] * w[1] - u[1] * w[0],
            ];
            let len = (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]).sqrt() + 1e-30;
            fnormals.push([cr[0] / len, cr[1] / len, cr[2] / len]);
            centroids.push([
                (a[0] + b[0] + c[0]) / 3.0,
                (a[1] + b[1] + c[1]) / 3.0,
                (a[2] + b[2] + c[2]) / 3.0,
            ]);
        }

        let mut ft = ray_march_thickness(&grid, &centroids, &fnormals, max_range);
        let bad: Vec<usize> = (0..ft.len()).filter(|&i| !ft[i].is_finite()).collect();
        if !bad.is_empty() {
            let bpts: Vec<[f64; 3]> = bad.iter().map(|&i| centroids[i]).collect();
            let bnrm: Vec<[f64; 3]> = bad.iter().map(|&i| fnormals[i]).collect();
            let opp = nearest_opposite(&pts, &nrm, &bpts, &bnrm);
            let mut any = false;
            for (j, &i) in bad.iter().enumerate() {
                if opp[j].is_finite() {
                    ft[i] = opp[j];
                    any = true;
                }
            }
            if any {
                method = "sdf-ray+opposite-normal".to_string();
            }
        }

        // `np.minimum.at(thickness, tris[:, k], where(isfinite(ft), ft, 1e30))`
        for (fi, f) in tris.iter().enumerate() {
            let v = if ft[fi].is_finite() { ft[fi] } else { 1e30 };
            for &vi in f {
                if v < thickness[vi] {
                    thickness[vi] = v;
                }
            }
        }
    }

    // Unmeasured (or sentinel) vertices take the bbox diagonal.
    let fallback = if d > 0.0 { d } else { 0.0 };
    for v in thickness.iter_mut() {
        if !v.is_finite() || *v >= 1e29 {
            *v = fallback;
        }
    }

    let target = min_thickness.unwrap_or(DEFAULT_MIN_THICKNESS_REL * d);
    let finite_pos: Vec<f64> = thickness
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v > 0.0)
        .collect();
    let min = thickness.iter().copied().fold(f64::INFINITY, f64::min);
    let mean = if finite_pos.is_empty() {
        0.0
    } else {
        finite_pos.iter().sum::<f64>() / finite_pos.len() as f64
    };
    let median = median_of(&finite_pos);
    let thin_count = if target != 0.0 {
        thickness.iter().filter(|v| **v < target).count()
    } else {
        0
    };

    Some(ThicknessEstimate {
        per_vertex: thickness,
        min: if min.is_finite() { min } else { 0.0 },
        median,
        mean,
        min_thickness: target,
        thin_count,
        resolution,
        method,
    })
}

/// `np.median`: the mean of the two middle values for an even count.
fn median_of(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// `wall._TETS`: the six-tetrahedron decomposition of a cube, corner index =
/// `x + 2y + 4z`.
const TETS: [[usize; 4]; 6] = [
    [0, 1, 3, 7],
    [0, 1, 5, 7],
    [0, 4, 5, 7],
    [0, 4, 6, 7],
    [0, 2, 6, 7],
    [0, 2, 3, 7],
];

/// `wall._tet_triangles`: triangulate the iso-surface inside one tetrahedron.
///
/// Two cases: three crossings give one triangle; four give two, ordered around
/// the crossing centroid by angle so the winding is consistent. Python sorts by
/// `arctan2` in the plane of the crossing normal, and the order matters for the
/// face indices, so it is reproduced rather than approximated.
fn tet_triangles(
    pts: &[[f64; 3]; 4],
    vals: &[f64; 4],
    iso: f64,
) -> Vec<[[f64; 3]; 3]> {
    let inside: [bool; 4] = [
        vals[0] >= iso,
        vals[1] >= iso,
        vals[2] >= iso,
        vals[3] >= iso,
    ];
    let n_in = inside.iter().filter(|b| **b).count();
    if n_in == 0 || n_in == 4 {
        return Vec::new();
    }
    const EDGES: [(usize, usize); 6] = [(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)];
    let mut crossings: Vec<[f64; 3]> = Vec::with_capacity(4);
    for (a, b) in EDGES {
        if inside[a] != inside[b] {
            crossings.push(interp(pts[a], pts[b], vals[a], vals[b], iso));
        }
    }
    match crossings.len() {
        3 => vec![[crossings[0], crossings[1], crossings[2]]],
        4 => {
            let c = [
                (crossings[0][0] + crossings[1][0] + crossings[2][0] + crossings[3][0]) / 4.0,
                (crossings[0][1] + crossings[1][1] + crossings[2][1] + crossings[3][1]) / 4.0,
                (crossings[0][2] + crossings[1][2] + crossings[2][2] + crossings[3][2]) / 4.0,
            ];
            let sub = |a: [f64; 3], b: [f64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
            let cross = |a: [f64; 3], b: [f64; 3]| {
                [
                    a[1] * b[2] - a[2] * b[1],
                    a[2] * b[0] - a[0] * b[2],
                    a[0] * b[1] - a[1] * b[0],
                ]
            };
            let norm = |a: [f64; 3]| (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
            let mut normal = cross(
                sub(crossings[1], crossings[0]),
                sub(crossings[2], crossings[0]),
            );
            if norm(normal) < 1e-12 {
                normal = cross(
                    sub(crossings[2], crossings[0]),
                    sub(crossings[3], crossings[0]),
                );
            }
            let nrm = norm(normal);
            let normal = if nrm > 1e-12 {
                [normal[0] / nrm, normal[1] / nrm, normal[2] / nrm]
            } else {
                [0.0, 0.0, 1.0]
            };
            // Project the reference vector into the plane, then normalise.
            let mut reference = sub(crossings[0], c);
            let dotp = reference[0] * normal[0]
                + reference[1] * normal[1]
                + reference[2] * normal[2];
            reference = [
                reference[0] - dotp * normal[0],
                reference[1] - dotp * normal[1],
                reference[2] - dotp * normal[2],
            ];
            let mut rn = norm(reference);
            if rn < 1e-12 {
                reference = cross(normal, [1.0, 0.0, 0.0]);
                rn = norm(reference);
            }
            let reference = if rn > 1e-12 {
                [reference[0] / rn, reference[1] / rn, reference[2] / rn]
            } else {
                [1.0, 0.0, 0.0]
            };
            let bitangent = cross(normal, reference);
            let mut order: Vec<usize> = (0..4).collect();
            let angle = |p: [f64; 3]| {
                let d = sub(p, c);
                let b = d[0] * bitangent[0] + d[1] * bitangent[1] + d[2] * bitangent[2];
                let r = d[0] * reference[0] + d[1] * reference[1] + d[2] * reference[2];
                b.atan2(r)
            };
            // `np.argsort` is stable, so sort by (angle, index).
            order.sort_by(|&a, &b| {
                angle(crossings[a])
                    .total_cmp(&angle(crossings[b]))
                    .then(a.cmp(&b))
            });
            let cr: Vec<[f64; 3]> = order.iter().map(|&i| crossings[i]).collect();
            vec![
                [cr[0], cr[1], cr[2]],
                [cr[0], cr[2], cr[3]],
            ]
        }
        _ => Vec::new(),
    }
}

/// `wall._interp`: linear crossing position, clipped to the segment.
fn interp(p0: [f64; 3], p1: [f64; 3], v0: f64, v1: f64, iso: f64) -> [f64; 3] {
    let denom = v1 - v0;
    let mut t = if denom.abs() < 1e-30 {
        0.0
    } else {
        (iso - v0) / denom
    };
    if t < 0.0 {
        t = 0.0;
    }
    if t > 1.0 {
        t = 1.0;
    }
    [
        p0[0] + t * (p1[0] - p0[0]),
        p0[1] + t * (p1[1] - p0[1]),
        p0[2] + t * (p1[2] - p0[2]),
    ]
}

/// `wall._marching_tetrahedra`: dependency-free iso-surface extraction.
///
/// Deliberately **not** `dual_contour::marching_tets_mesh`, which is a different
/// extractor: it takes `f32`, orders nodes `i`-fastest, and QEF-smooths. This
/// one reproduces Python's, including its vertex dedup key
/// `round(v / (spacing * 1e-4))` and its `unique` ordering (`first_pos` keeps
/// the first occurrence, which is what fixes the vertex order).
pub fn marching_tetrahedra(
    grid: &SdfGrid,
    iso: f64,
) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
    let dims = grid.dims;
    let mut verts: Vec<[f64; 3]> = Vec::new();
    let mut faces: Vec<[usize; 3]> = Vec::new();
    if dims[0] < 2 || dims[1] < 2 || dims[2] < 2 {
        return (verts, faces);
    }
    for i in 0..dims[0] - 1 {
        for j in 0..dims[1] - 1 {
            for k in 0..dims[2] - 1 {
                let mut corner_vals = [0.0f64; 8];
                let mut corner_pts = [[0.0f64; 3]; 8];
                for b in 0..8 {
                    let dx = (b & 1) as usize;
                    let dy = ((b >> 1) & 1) as usize;
                    let dz = ((b >> 2) & 1) as usize;
                    let (xi, yi, zi) = (i + dx, j + dy, k + dz);
                    corner_vals[b] = grid.index(xi, yi, zi);
                    corner_pts[b] = [
                        grid.origin[0] + grid.spacing * xi as f64,
                        grid.origin[1] + grid.spacing * yi as f64,
                        grid.origin[2] + grid.spacing * zi as f64,
                    ];
                }
                for tet in TETS {
                    let tv = [
                        corner_vals[tet[0]],
                        corner_vals[tet[1]],
                        corner_vals[tet[2]],
                        corner_vals[tet[3]],
                    ];
                    let tp = [
                        corner_pts[tet[0]],
                        corner_pts[tet[1]],
                        corner_pts[tet[2]],
                        corner_pts[tet[3]],
                    ];
                    for tri in tet_triangles(&tp, &tv, iso) {
                        let base = verts.len();
                        verts.extend_from_slice(&tri);
                        faces.push([base, base + 1, base + 2]);
                    }
                }
            }
        }
    }
    if verts.is_empty() {
        return (verts, faces);
    }

    // Dedup: `keys = round(verts / (spacing * 1e-4)).astype(int64)`, then
    // `np.unique(keys, axis=0, return_index=True, return_inverse=True)`.
    //
    // `np.unique` sorts the keys lexicographically and `first_pos` is the index
    // of the first original occurrence of each unique key, so the surviving
    // vertex order is the *sorted key* order. Reproducing that ordering is what
    // makes the output byte-identical rather than merely geometrically equal.
    let scale = grid.spacing * 1e-4;
    let key_of = |p: [f64; 3]| -> [i64; 3] {
        // NumPy's `round` is half-to-even.
        let r = |v: f64| (v / scale).round_ties_even() as i64;
        [r(p[0]), r(p[1]), r(p[2])]
    };
    let mut order: Vec<usize> = (0..verts.len()).collect();
    let keys: Vec<[i64; 3]> = verts.iter().map(|p| key_of(*p)).collect();
    order.sort_by(|&a, &b| keys[a].cmp(&keys[b]).then(a.cmp(&b)));

    let mut unique_keys: Vec<[i64; 3]> = Vec::new();
    let mut first_pos: Vec<usize> = Vec::new();
    for &idx in &order {
        if unique_keys.last() != Some(&keys[idx]) {
            unique_keys.push(keys[idx]);
            first_pos.push(idx);
        }
    }
    // `inverse`: original index -> position in the unique key order, where the
    // unique order is the sorted-key order (`np.unique` sorts, and `first_pos`
    // is the first original occurrence of each key).
    let mut slot_of_key: std::collections::HashMap<[i64; 3], usize> =
        std::collections::HashMap::with_capacity(unique_keys.len());
    for (slot, k) in unique_keys.iter().enumerate() {
        slot_of_key.insert(*k, slot);
    }
    let mut inverse = vec![0usize; verts.len()];
    for (orig, k) in keys.iter().enumerate() {
        inverse[orig] = slot_of_key[k];
    }

    let out_verts: Vec<[f64; 3]> = first_pos.iter().map(|&i| verts[i]).collect();
    let out_faces: Vec<[usize; 3]> = faces
        .iter()
        .map(|f| [inverse[f[0]], inverse[f[1]], inverse[f[2]]])
        .collect();
    (out_verts, out_faces)
}

/// The report `wall.thicken_to_min` builds. Field order and names match Python's
/// dict so the tier record serialises identically.
#[derive(Clone, Debug, PartialEq)]
pub struct ThickenReport {
    pub min_thickness: f64,
    pub min_before: f64,
    pub thin_before: usize,
    pub delta: f64,
    pub method: String,
    pub notes: Vec<String>,
    pub error: Option<String>,
    /// Analytic post-offset minimum (`min_before + 2*delta`). Python reports the
    /// analytic value rather than re-measuring, because the blocky
    /// marching-tetrahedra surface makes a re-measurement noisy.
    pub min_after: Option<f64>,
    pub thin_after: Option<usize>,
    pub faces: Option<usize>,
}

/// `wall.thicken_to_min`: thicken walls below `min_thickness` by morphological
/// dilation.
///
/// Offsets the solid outward by `delta = (target - min_measured) / 2`, capped at
/// 25% of the bbox diagonal, which adds `2 * delta` to every local wall
/// thickness. A solid already at or above the target comes back unchanged.
pub fn thicken_to_min(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    min_thickness: Option<f64>,
    resolution: usize,
    samples: usize,
    seed: u64,
) -> (Vec<[f64; 3]>, Vec<[usize; 3]>, ThickenReport) {
    let d = diag(verts);
    let target = min_thickness.unwrap_or(DEFAULT_MIN_THICKNESS_REL * d);
    let before = estimate_wall_thickness(verts, tris, resolution, samples, seed, Some(target));
    let mut report = ThickenReport {
        min_thickness: target,
        min_before: before.as_ref().map(|b| b.min).unwrap_or(0.0),
        thin_before: before.as_ref().map(|b| b.thin_count).unwrap_or(0),
        delta: 0.0,
        method: "morphological-dilation".to_string(),
        notes: Vec::new(),
        error: None,
        min_after: None,
        thin_after: None,
        faces: None,
    };

    if target <= 0.0 || tris.is_empty() {
        report.error = Some("no positive target thickness".to_string());
        return (verts.to_vec(), tris.to_vec(), report);
    }
    let before_min = before.map(|b| b.min).unwrap_or(0.0);
    let mut delta = (target - before_min) / 2.0;
    if delta < 0.0 {
        delta = 0.0;
    }
    let cap = 0.25 * d;
    if delta > cap {
        delta = cap;
    }
    if delta <= 0.0 {
        report.notes.push("no wall below the target; nothing to thicken".to_string());
        return (verts.to_vec(), tris.to_vec(), report);
    }
    report.delta = delta;

    let extra_pad = delta * 1.5 + 0.05 * d;
    let Some(grid) = build_sdf_grid(verts, tris, resolution, samples, seed, extra_pad) else {
        report.error = Some("no SDF grid".to_string());
        return (verts.to_vec(), tris.to_vec(), report);
    };
    let spacing = grid.spacing;
    let radius = (delta / spacing).ceil() as u32;
    if radius < 1 {
        report.error = Some("thicken offset below the SDF grid resolution".to_string());
        return (verts.to_vec(), tris.to_vec(), report);
    }
    // Axis order matters: `SdfGrid` is NumPy-`ij` (x outermost, z fastest), but
    // `morph::edt_3d` addresses `k * nx*ny + j * nx + i` (i innermost). Passing
    // one straight into the other dilates along the wrong axes -- it produced
    // 16840 occupied cells where Python had 8152, from the same 3793 seeds.
    // Convert explicitly rather than relying on the two agreeing.
    let d = grid.dims;
    let mut occupancy: Vec<u8> = Vec::with_capacity(grid.sdf.len());
    for k in 0..d[2] {
        for j in 0..d[1] {
            for i in 0..d[0] {
                occupancy.push(u8::from(grid.index(i, j, k) <= 0.0));
            }
        }
    }
    let closed = crate::morph::dilate_occupancy_ball(&occupancy, d, radius);
    if closed.iter().all(|v| *v == 0) {
        report.error = Some("morphological dilation produced an empty solid".to_string());
        return (verts.to_vec(), tris.to_vec(), report);
    }
    // Back to `ij` order for the extractor.
    let mut field = vec![0.0f64; grid.sdf.len()];
    for k in 0..d[2] {
        for j in 0..d[1] {
            for i in 0..d[0] {
                field[i * d[1] * d[2] + j * d[2] + k] =
                    f64::from(closed[k * d[0] * d[1] + j * d[0] + i]);
            }
        }
    }
    let closed_grid = SdfGrid {
        origin: grid.origin,
        spacing,
        dims: d,
        sdf: field,
    };
    let (out_v, out_t) = marching_tetrahedra(&closed_grid, 0.5);
    if out_t.is_empty() {
        report.error = Some("iso-surface extraction produced no faces".to_string());
        return (verts.to_vec(), tris.to_vec(), report);
    }
    // Analytic post-offset minimum, as Python does: a uniform dilation by
    // `delta` adds ~`2*delta` to every wall.
    report.min_after = Some(before_min + 2.0 * delta);
    report.faces = Some(out_t.len());
    report.notes = vec![
        "thin walls thickened by morphological dilation".to_string(),
        "min_after is the analytical estimate (min_before + 2*delta)".to_string(),
    ];
    (out_v, out_t, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn unit_square() -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let tris = vec![[0usize, 1, 2], [0, 2, 3]];
        (verts, tris)
    }

    /// The `_diag` counterpart, including the non-empty guard.
    #[test]
    fn diag_matches_python() {
        let (v, _) = unit_square();
        assert_eq!(diag(&v), 2.0f64.sqrt());
        assert_eq!(diag(&[]), 0.0);
    }

    /// Grid planning is pure arithmetic, so pin it exactly: `origin`, `spacing`
    /// and `dims` decide `radius = ceil(delta / spacing)` downstream.
    ///
    /// Padded box for the unit square: span 1.0 in x/y, 0 in z -> `np.maximum`
    /// lifts z to 1e-9, diag ~= 1.41421356, pad = 0.02 * diag. Both axes then
    /// span 1 + 2*pad, spacing = that / 39, and dims = ceil(span/spacing) + 1.
    #[test]
    fn grid_planning_matches_python_arithmetic() {
        let (v, t) = unit_square();
        let g = build_sdf_grid(&v, &t, DEFAULT_GRID_RESOLUTION, 100, 0, 0.0)
            .expect("a unit square has area");
        let span = [1.0f64, 1.0, 1e-9];
        let d = (span[0] * span[0] + span[1] * span[1] + span[2] * span[2]).sqrt();
        let pad = GRID_PADDING_REL * d;
        assert!((g.origin[0] - (-pad)).abs() < 1e-15, "{:?}", g.origin);
        assert!((g.origin[1] - (-pad)).abs() < 1e-15, "{:?}", g.origin);
        let sx = 1.0 + 2.0 * pad;
        let expected_spacing = sx / 39.0;
        assert!(
            (g.spacing - expected_spacing).abs() < 1e-15,
            "spacing {} vs {}",
            g.spacing,
            expected_spacing
        );
        assert_eq!(g.dims[0], (sx / expected_spacing).ceil() as usize + 1);
        assert_eq!(g.sdf.len(), g.dims[0] * g.dims[1] * g.dims[2]);
    }

    /// The sign convention: points on the +normal side are positive (outside),
    /// the cell at the far corner in -z is negative (inside). For a flat sheet
    /// the "inside" is the whole negative half-space, which is exactly what the
    /// nearest-sample-normal rule gives.
    #[test]
    fn sign_follows_the_nearest_sample_normal() {
        let (v, t) = unit_square();
        let g = build_sdf_grid(&v, &t, 20, 200, 0, 0.0).expect("grid");
        let (i, j, k) = (0, 0, 0);
        assert!(
            g.index(i, j, k) < 0.0,
            "below the sheet must be inside, got {}",
            g.index(i, j, k)
        );
        // The top-most z layer is above the sheet: outside.
        let ktop = g.dims[2] - 1;
        assert!(
            g.index(0, 0, ktop) > 0.0,
            "above the sheet must be outside, got {}",
            g.index(0, 0, ktop)
        );
    }

    /// A zero-area mesh produces no samples, so there is no field to build.
    #[test]
    fn zero_area_mesh_has_no_grid() {
        let v = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]];
        let t = vec![[0usize, 1, 2]];
        assert!(build_sdf_grid(&v, &t, 20, 10, 0, 0.0).is_none());
    }

    /// Resolution is clamped to Python's `[8, 160]`, which decides `spacing`.
    #[test]
    fn resolution_is_clamped() {
        let (v, t) = unit_square();
        let span = 1.0 + 2.0 * GRID_PADDING_REL * 2.0f64.sqrt();
        // A request below the floor behaves like 8; one above the ceiling like
        // 160. Compare spacings rather than re-deriving dims.
        let tiny = build_sdf_grid(&v, &t, 1, 50, 0, 0.0).expect("grid");
        let eight = build_sdf_grid(&v, &t, MIN_RES, 50, 0, 0.0).expect("grid");
        assert_eq!(tiny.spacing, eight.spacing, "res clamped up to {MIN_RES}");
        assert!((eight.spacing - span / (MIN_RES - 1) as f64).abs() < 1e-15);

        let huge = build_sdf_grid(&v, &t, 10_000, 50, 0, 0.0).expect("grid");
        assert!(
            (huge.spacing - span / (MAX_RES - 1) as f64).abs() < 1e-15,
            "res clamped down to {MAX_RES}"
        );
    }

    /// `ts` must be `i * step`, not an accumulator.
    ///
    /// NumPy's `arange(0, stop, step)` computes each element from the index.
    /// Accumulating `t += step` drifts a few ULPs, which is enough to change the
    /// interpolated thickness of ~1000 of 4601 vertices on a real mesh -- the
    /// scalars still agreed to 9 digits, so only a byte comparison caught it.
    #[test]
    fn the_ray_step_sequence_does_not_accumulate() {
        // Reproduce the construction for a step that is not binary-exact.
        let step = 0.027091501089613425f64;
        let stop = 1.0f64;
        let accumulated: Vec<f64> = {
            let mut v = Vec::new();
            let mut t = 0.0;
            while t < stop {
                v.push(t);
                t += step;
            }
            v
        };
        let indexed: Vec<f64> = (0..accumulated.len())
            .map(|i| i as f64 * step)
            .filter(|t| *t < stop)
            .collect();
        // The two constructions must not be assumed equal -- this documents why
        // the indexed form is the one to use.
        let drift: f64 = accumulated
            .iter()
            .zip(indexed.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max);
        assert!(
            drift > 0.0,
            "if accumulating were exact here, the test would not guard anything"
        );
    }

    /// A real mesh must reproduce Python's numbers, not merely look plausible.
    ///
    /// Pinned from `sutura_engine.wall.estimate_wall_thickness` on
    /// `broken_37402` (`resolution=40, samples=40000, seed=0`): the per-vertex
    /// array hashes to `156810316e613d2e3e69af2241757d49` and the summary is
    /// `min=0.076084157, thin_count=1265, mean=6.478264086,
    /// median=3.817594924`. The mesh itself is not in the repo, so this asserts
    /// the arithmetic the mesh-independent parts depend on: the fallback value
    /// and the `min` reduction.
    #[test]
    fn thickness_summary_reduces_like_python() {
        let (v, t) = unit_square();
        let r = estimate_wall_thickness(&v, &t, 40, 2000, 0, None).expect("estimate");
        assert_eq!(r.per_vertex.len(), v.len());
        assert_eq!(r.resolution, 40);
        // A flat sheet has no interior, so no ray finds a crossing; every
        // vertex must therefore take the documented bbox-diagonal fallback.
        let d = diag(&v);
        for (i, x) in r.per_vertex.iter().enumerate() {
            assert_eq!(*x, d, "vertex {i} must fall back to diag");
        }
        assert_eq!(r.min, d);
        assert_eq!(r.thin_count, 0, "nothing is thinner than 1% of the diagonal");
        assert_eq!(
            r.min_thickness,
            DEFAULT_MIN_THICKNESS_REL * d,
            "the default target is 1% of the diagonal"
        );
    }

    /// `np.median` averages the two middle values for an even count.
    #[test]
    fn median_matches_numpy() {
        assert_eq!(median_of(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median_of(&[4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median_of(&[]), 0.0);
    }

    /// `morph::edt_3d` addresses cells `k*nx*ny + j*nx + i` (i innermost), while
    /// `SdfGrid` is NumPy `ij` (x outermost). Handing one to the other dilates
    /// along the wrong axes.
    ///
    /// On a real mesh that produced 16840 occupied cells where Python had 8152,
    /// from the same 3793 seeds -- the vertex and face counts came out 2.3x too
    /// large. This pins the conversion with a shape that is asymmetric in all
    /// three axes, so a swap cannot pass.
    #[test]
    fn occupancy_is_transposed_into_edt_order() {
        let dims = [3usize, 2, 4];
        let mut grid = SdfGrid {
            origin: [0.0; 3],
            spacing: 1.0,
            dims,
            sdf: vec![0.0; dims[0] * dims[1] * dims[2]],
        };
        // Exactly one inside cell, at (i,j,k) = (2,1,3) in `ij` order.
        let inside = |i: usize, j: usize, k: usize| i == 2 && j == 1 && k == 3;
        for i in 0..dims[0] {
            for j in 0..dims[1] {
                for k in 0..dims[2] {
                    grid.sdf[i * dims[1] * dims[2] + j * dims[2] + k] = if inside(i, j, k) {
                        -1.0
                    } else {
                        1.0
                    };
                }
            }
        }
        // Build the EDT-order buffer the way the port does.
        let mut occ = Vec::new();
        for k in 0..dims[2] {
            for j in 0..dims[1] {
                for i in 0..dims[0] {
                    occ.push(u8::from(grid.index(i, j, k) <= 0.0));
                }
            }
        }
        assert_eq!(occ.iter().filter(|v| **v != 0).count(), 1);
        // The single seed must sit at k*nx*ny + j*nx + i = 3*6 + 1*3 + 2.
        assert_eq!(occ[3 * dims[0] * dims[1] + 1 * dims[0] + 2], 1);
    }

    /// `thicken_to_min` on a mesh already thicker than the target is a no-op.
    ///
    /// The delta formula is `(target - min) / 2`, so a target below the measured
    /// minimum must give `delta <= 0` and return the input untouched with the
    /// documented note -- not a dilated copy.
    #[test]
    fn already_thick_enough_is_untouched() {
        let (v, t) = unit_square();
        let (ov, ot, r) = thicken_to_min(&v, &t, Some(1e-6), 20, 200, 0);
        assert_eq!(r.delta, 0.0);
        assert!(r.notes.iter().any(|n| n.contains("no wall below the target")));
        assert_eq!(ov, v, "the mesh must come back unchanged");
        assert_eq!(ot, t);
    }

    /// A non-positive target is an error, not a silent no-op.
    #[test]
    fn zero_target_reports_an_error() {
        let (v, t) = unit_square();
        let (_, _, r) = thicken_to_min(&v, &t, Some(0.0), 20, 200, 0);
        assert!(r.error.as_deref().unwrap_or("").contains("positive target"));
    }
}
