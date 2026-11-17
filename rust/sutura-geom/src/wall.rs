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
}
