// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! H7: rebuild the tet basis from the INPUT, fTetWild-style, instead of the
//! dual-contour proxy pillow.
//!
//! The point set is fully deterministic and RNG-free: the input vertices, a
//! barycentric Steiner lattice on each input triangle (spacing derived from the
//! bbox diagonal), a coarse interior lattice in a padded box, and the eight
//! corners of that padded box. Points are deduplicated on the `f32` grid and
//! sorted canonically, so the Delaunay and every index-ordered downstream pass
//! are a pure function of the input mesh and the two spacing knobs.

use std::collections::{HashMap, HashSet};

/// Fraction of the bbox diagonal used as the padded-box margin.
const PAD_FRAC: f64 = 0.05;
/// Upper bound on the per-triangle barycentric order (work-budget safety).
const MAX_STEINER_ORDER: usize = 128;

pub(crate) struct InputBasis {
    /// Deduplicated, canonically sorted point set.
    pub(crate) points: Vec<[f64; 3]>,
    /// Representative sample spacing (normalized units), used as `voxel`.
    pub(crate) voxel: f64,
    /// Per point: true iff it is one of the eight padded-box corners.
    pub(crate) corner: Vec<bool>,
}

fn f32_key(p: [f64; 3]) -> [u32; 3] {
    [
        (p[0] as f32).to_bits(),
        (p[1] as f32).to_bits(),
        (p[2] as f32).to_bits(),
    ]
}

/// Map each input vertex to the basis point that carries it (same `f32` grid
/// key), or `None` if the vertex was absorbed by a duplicate. Deterministic:
/// first occurrence in `points` (canonical order) wins.
pub(crate) fn map_input_vertices(
    points: &[[f64; 3]],
    nverts: &[[f64; 3]],
) -> Vec<Option<usize>> {
    let mut idx: HashMap<[u32; 3], usize> = HashMap::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        idx.entry(f32_key(*p)).or_insert(i);
    }
    nverts
        .iter()
        .map(|p| idx.get(&f32_key(*p)).copied())
        .collect()
}

/// Build the H7 point basis in normalized coordinates. `steiner_frac` is the
/// Steiner spacing as a fraction of the bbox diagonal; `lattice_div` is the
/// number of cells per axis of the coarse interior lattice.
pub(crate) fn build(
    nverts: &[[f64; 3]],
    tris: &[[usize; 3]],
    steiner_frac: f64,
    lattice_div: usize,
) -> InputBasis {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in nverts {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    let d2 = (hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2);
    let diag = d2.sqrt().max(1e-30);
    let pad = PAD_FRAC * diag;
    let bl = [lo[0] - pad, lo[1] - pad, lo[2] - pad];
    let bh = [hi[0] + pad, hi[1] + pad, hi[2] + pad];

    let mut pts: Vec<[f64; 3]> = Vec::with_capacity(nverts.len() + 512);
    pts.extend_from_slice(nverts);

    // Barycentric Steiner lattice on each input triangle. The order is
    // ceil(longest_edge / spacing), clamped so the sample count stays bounded.
    let spacing = (diag * steiner_frac).max(1e-9);
    for t in tris {
        let p0 = nverts[t[0]];
        let p1 = nverts[t[1]];
        let p2 = nverts[t[2]];
        let elen = |a: [f64; 3], b: [f64; 3]| -> f64 {
            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
        };
        let lmax = elen(p0, p1).max(elen(p1, p2)).max(elen(p2, p0));
        let m = ((lmax / spacing).ceil() as usize).clamp(1, MAX_STEINER_ORDER);
        let inv = 1.0 / m as f64;
        for i in 0..=m {
            for j in 0..=(m - i) {
                let k = m - i - j;
                let w0 = i as f64 * inv;
                let w1 = j as f64 * inv;
                let w2 = k as f64 * inv;
                pts.push([
                    w0 * p0[0] + w1 * p1[0] + w2 * p2[0],
                    w0 * p0[1] + w1 * p1[1] + w2 * p2[1],
                    w0 * p0[2] + w1 * p1[2] + w2 * p2[2],
                ]);
            }
        }
    }

    // Coarse interior lattice so far-field tets are not slivers.
    let div = lattice_div.max(2);
    let inv_div = 1.0 / div as f64;
    for i in 1..div {
        for j in 1..div {
            for k in 1..div {
                pts.push([
                    bl[0] + (bh[0] - bl[0]) * i as f64 * inv_div,
                    bl[1] + (bh[1] - bl[1]) * j as f64 * inv_div,
                    bl[2] + (bh[2] - bl[2]) * k as f64 * inv_div,
                ]);
            }
        }
    }

    // Eight padded box corners.
    let mut corner_keys: HashSet<[u32; 3]> = HashSet::with_capacity(8);
    for &x in &[bl[0], bh[0]] {
        for &y in &[bl[1], bh[1]] {
            for &z in &[bl[2], bh[2]] {
                let c = [x, y, z];
                pts.push(c);
                corner_keys.insert(f32_key(c));
            }
        }
    }

    // Deduplicate on the f32 grid (first occurrence wins), then sort by the
    // f32 key so the order is canonical regardless of HashMap iteration order.
    let mut map: HashMap<[u32; 3], [f64; 3]> = HashMap::with_capacity(pts.len());
    for p in &pts {
        map.entry(f32_key(*p)).or_insert(*p);
    }
    let mut unique: Vec<[f64; 3]> = map.values().copied().collect();
    unique.sort_by(|a, b| f32_key(*a).cmp(&f32_key(*b)));
    let corner = unique
        .iter()
        .map(|p| corner_keys.contains(&f32_key(*p)))
        .collect();

    InputBasis {
        points: unique,
        voxel: spacing,
        corner,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let v = vec![
            [-0.5, -0.5, -0.5],
            [0.5, -0.5, -0.5],
            [0.5, 0.5, -0.5],
            [-0.5, 0.5, -0.5],
            [-0.5, -0.5, 0.5],
            [0.5, -0.5, 0.5],
            [0.5, 0.5, 0.5],
            [-0.5, 0.5, 0.5],
        ];
        let t = vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [3, 7, 6],
            [3, 6, 2],
            [0, 4, 7],
            [0, 7, 3],
            [1, 2, 6],
            [1, 6, 5],
        ];
        (v, t)
    }

    #[test]
    fn points_are_deterministic_and_deduplicated() {
        let (v, t) = cube();
        let a = build(&v, &t, 1.0 / 16.0, 6);
        let b = build(&v, &t, 1.0 / 16.0, 6);
        assert_eq!(a.points.len(), b.points.len());
        for (p, q) in a.points.iter().zip(b.points.iter()) {
            assert_eq!(f32_key(*p), f32_key(*q));
        }
        // Canonical order: strictly increasing f32 keys.
        for w in a.points.windows(2) {
            assert!(f32_key(w[0]) < f32_key(w[1]));
        }
        assert_eq!(a.corner.iter().filter(|&&c| c).count(), 8);
    }
}
