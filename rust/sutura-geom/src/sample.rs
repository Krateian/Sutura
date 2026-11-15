// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Area-weighted surface sampling, shared by the Python engine and the Rust CLI.
//!
//! Port of `wall.sample_surface`:
//!
//! ```python
//! face_idx = rng.choice(len(tris), size=n, p=areas / total)
//! r1 = np.sqrt(rng.random(n))
//! r2 = rng.random(n)
//! a = 1 - r1; b = r1 * (1 - r2); c = r1 * r2
//! pts = a*v0[face_idx] + b*v1[face_idx] + c*v2[face_idx]
//! normals = cross[face_idx] / (2 * areas[face_idx][:, None] + 1e-30)
//! ```
//!
//! The three draws share ONE generator and must stay in that order: `choice`
//! consumes a variable number of words per draw, so the stream position when
//! `r1` is taken depends on how many faces were drawn. `sutura-geom::choice`
//! already reproduces NumPy's `choice(p=...)` and `sutura-geom::rng` its
//! `SeedSequence`/PCG64, so the whole sequence is bit-identical.

use crate::choice::{choice_weighted, next_double};
use crate::measures::sum_f64;
use crate::rng::Pcg64;

/// `wall.DEFAULT_SAMPLES`; the sampler clamps to `[1, 4_000_000]`.
pub const DEFAULT_SAMPLES: usize = 5000;
const MAX_SAMPLES: usize = 4_000_000;

/// `(points, normals)` for `n_samples` area-weighted draws, exactly as
/// `wall.sample_surface` produces them.
///
/// Returns empty vectors for an empty or zero-area mesh, matching the Python
/// early returns.
pub fn sample_surface(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    n_samples: usize,
    seed: u64,
) -> (Vec<[f64; 3]>, Vec<[f64; 3]>) {
    if tris.is_empty() {
        return (Vec::new(), Vec::new());
    }
    // `cross = np.cross(v1 - v0, v2 - v0)`; `areas = 0.5 * |cross|`.
    let mut cross: Vec<[f64; 3]> = Vec::with_capacity(tris.len());
    let mut areas: Vec<f64> = Vec::with_capacity(tris.len());
    for f in tris {
        let a = verts[f[0]];
        let b = verts[f[1]];
        let c = verts[f[2]];
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        areas.push(0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt());
        cross.push(n);
    }
    let total = sum_f64(&areas);
    if total <= 1e-12 {
        return (Vec::new(), Vec::new());
    }
    let n = n_samples.clamp(1, MAX_SAMPLES);

    let mut rng = Pcg64::new(seed);
    // Same order and same generator as the Python body.
    let face_idx = choice_weighted(&mut rng, &areas, n);
    let r1: Vec<f64> = (0..n).map(|_| next_double(&mut rng).sqrt()).collect();
    let r2: Vec<f64> = (0..n).map(|_| next_double(&mut rng)).collect();

    let mut pts = Vec::with_capacity(n);
    let mut normals = Vec::with_capacity(n);
    for i in 0..n {
        let f = face_idx[i];
        let tri = tris[f];
        let (v0, v1, v2) = (verts[tri[0]], verts[tri[1]], verts[tri[2]]);
        let a = 1.0 - r1[i];
        let b = r1[i] * (1.0 - r2[i]);
        let c = r1[i] * r2[i];
        pts.push([
            a * v0[0] + b * v1[0] + c * v2[0],
            a * v0[1] + b * v1[1] + c * v2[1],
            a * v0[2] + b * v1[2] + c * v2[2],
        ]);
        let denom = 2.0 * areas[f] + 1e-30;
        let cr = cross[f];
        normals.push([cr[0] / denom, cr[1] / denom, cr[2] / denom]);
    }
    (pts, normals)
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

    #[test]
    fn empty_mesh_samples_nothing() {
        let (p, n) = sample_surface(&[], &[], 10, 0);
        assert!(p.is_empty() && n.is_empty());
    }

    #[test]
    fn zero_area_mesh_samples_nothing() {
        let verts = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]];
        let tris = vec![[0usize, 1, 2]];
        let (p, _) = sample_surface(&verts, &tris, 10, 0);
        assert!(p.is_empty());
    }

    #[test]
    fn is_deterministic_for_a_seed() {
        let (verts, tris) = unit_square();
        let a = sample_surface(&verts, &tris, 50, 42);
        let b = sample_surface(&verts, &tris, 50, 42);
        assert_eq!(a.0, b.0);
        assert_eq!(a.1, b.1);
    }

    #[test]
    fn different_seeds_differ() {
        let (verts, tris) = unit_square();
        let a = sample_surface(&verts, &tris, 50, 42);
        let b = sample_surface(&verts, &tris, 50, 43);
        assert_ne!(a.0, b.0);
    }

    /// Every sample must land inside the unit square's z = 0 plane, and every
    /// normal must be the +z face normal (the sampler divides by
    /// `2*area + 1e-30`, which is exactly 1 for these triangles).
    #[test]
    fn samples_lie_on_the_surface_with_unit_normals() {
        let (verts, tris) = unit_square();
        let (pts, normals) = sample_surface(&verts, &tris, 200, 7);
        assert_eq!(pts.len(), 200);
        for p in &pts {
            assert!((0.0..=1.0).contains(&p[0]), "{p:?}");
            assert!((0.0..=1.0).contains(&p[1]), "{p:?}");
            assert!((0.0..=1.0).contains(&p[2]), "{p:?}");
        }
        for n in &normals {
            assert!((n[2].abs() - 1.0).abs() < 1e-12, "{n:?}");
        }
    }

    #[test]
    fn count_is_clamped_and_honoured() {
        let (verts, tris) = unit_square();
        assert_eq!(sample_surface(&verts, &tris, 0, 0).0.len(), 1);
        assert_eq!(sample_surface(&verts, &tris, 17, 0).0.len(), 17);
    }
}
