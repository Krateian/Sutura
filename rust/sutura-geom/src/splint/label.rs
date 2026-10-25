// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-tet initial labels from the signed UDF/GWN field.
//!
//! This is the only parallel stage in the core. Every tet is independent, so
//! the rayon map preserves the tet index order and produces bit-identical
//! results to the sequential loop.

use rayon::prelude::*;

use crate::winding::MeshBvh;

pub(crate) struct Labels {
    pub(crate) phi: Vec<f64>,
    pub(crate) init_label: Vec<u8>,
    pub(crate) interior_anchors: usize,
    /// Diagnostic: 10-bin histogram of the winding number at each tet centroid
    /// (bin `i` = `w` in `[i/10, (i+1)/10)`), clamped to `0..=9`.
    pub(crate) winding_hist: [usize; 10],
}

pub(crate) fn label_tets(
    bvh: &MeshBvh,
    centroids: &[[f64; 3]],
    out_tet: &[bool],
    parallel: bool,
) -> Labels {
    let one = |ti: usize| -> (f64, u8, usize) {
        let c = centroids[ti];
        let u = bvh.unsigned_distance_at(c);
        let w = bvh.winding_at(c);
        // Negative inside the solid.
        let s = if w > 0.5 { -u } else { u };
        let label = if !out_tet[ti] && s < 0.0 { 0 } else { 1 };
        let bin = ((w * 10.0).floor() as i64).clamp(0, 9) as usize;
        (s, label, bin)
    };

    let pairs: Vec<(f64, u8, usize)> = if parallel {
        centroids
            .par_iter()
            .enumerate()
            .map(|(ti, _)| one(ti))
            .collect()
    } else {
        (0..centroids.len()).map(one).collect()
    };

    let mut phi = Vec::with_capacity(pairs.len());
    let mut init_label = Vec::with_capacity(pairs.len());
    let mut winding_hist = [0usize; 10];
    for (s, l, bin) in pairs {
        phi.push(s);
        init_label.push(l);
        winding_hist[bin] += 1;
    }
    let interior_anchors = init_label.iter().filter(|&&l| l == 0).count();
    Labels {
        phi,
        init_label,
        interior_anchors,
        winding_hist,
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
    fn parallel_labels_match_serial() {
        let (v, t) = cube();
        let bvh = MeshBvh::from_arrays(&v, &t);
        let coords = vec![
            [0.0, 0.0, 0.0],
            [0.1, 0.1, 0.1],
            [0.2, -0.1, 0.0],
            [0.0, 0.2, -0.1],
            [2.0, 2.0, 2.0],
        ];
        let tets = vec![[0, 1, 2, 3], [1, 2, 3, 4]];
        let out = vec![false, false];
        let s = label_tets(&bvh, &coords[..2], &out, false);
        let p = label_tets(&bvh, &coords[..2], &out, true);
        assert_eq!(
            s.phi.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
            p.phi.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
        );
        assert_eq!(s.init_label, p.init_label);
        assert_eq!(s.interior_anchors, p.interior_anchors);
    }
}
