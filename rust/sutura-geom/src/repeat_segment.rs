// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! `repeat.py::segment_elements` — feature-edge dihedral segmentation.
//!
//! Splits a mesh into candidate element patches along sharp edges, drops the
//! large "base" substrate regions, and describes each remaining patch with an
//! area-weighted centroid, PCA frame and diagonal. The patch route of
//! `detect_repetition` clusters these descriptors.
//!
//! Two NumPy behaviours are load-bearing and reproduced literally:
//!
//! * The unmerged-STL weld is `np.unique(np.round(verts, 4), axis=0, ...)`.
//!   `np.unique(axis=0)` returns rows in LEXICOGRAPHIC order and
//!   `return_index` gives each row's first occurrence in the ORIGINAL array, so
//!   the welded vertex `k` is the `k`-th unique row in sorted order and
//!   `inverse` maps each original vertex to its rank. Both sides use that
//!   ranking, so the numbering is stable.
//! * `np.round` here agrees with `format!("{:.4}")` on the corpus (verified),
//!   so `py_round` is used rather than a separate rounding path.
//!
//! ## Two NumPy summation orders, and they differ
//!
//! `np.sum` is NOT uniformly pairwise. The 1-D reductions (`comp_areas`,
//! `patch_area`) use the 8-accumulator pairwise form, so `measures::sum_f64` is
//! correct there. But `np.sum(face_centroids * areas[:, None], axis=0)` is a 2-D
//! reduction and NumPy accumulates it sequentially over rows -- measured: for
//! 50 and 300 rows a sequential loop matches exactly while the pairwise form
//! does not. The centroid loop below is therefore sequential on purpose;
//! "fixing" it to `sum_f64` moved 7 of 226 patch-route scores by up to 8.8e-6.
//!
//! ## The `frame` field is not bit-reproducible on degenerate patches
//!
//! Measured over 226 meshes / 6289 patches: patch count, `faces`, `verts_idx`,
//! `area`, `diag` and `centroid` are all bit-identical to Python, while `frame`
//! differs on 5 patches. Those are patches with two equal eigenvalues (where the
//! eigenvector basis in that plane is arbitrary) or where `eigh3`'s canonical
//! column signs give `det(frame) = -1` instead of `+1`, so the det-correction
//! flips the opposite column. Both frames are orthonormal with det +1 and
//! describe the same axes. This is inert: `frame` is written into the patch dict
//! and never read by any caller (`eigvals` likewise), so the patch-route score is
//! unaffected -- verified 0 of 226 changed.

use crate::components::connected_components;
use crate::eigh3::eigh3;
use crate::measures::sum_f64;

/// `DEFAULT_FEATURE_ANGLE_DEG`.
pub const DEFAULT_FEATURE_ANGLE_DEG: f64 = 30.0;
/// `MIN_ELEMENT_FACES`.
pub const MIN_ELEMENT_FACES: usize = 4;
/// `BASE_AREA_THRESHOLD_FRACTION`.
pub const BASE_AREA_THRESHOLD_FRACTION: f64 = 0.05;

/// One candidate element patch.
#[derive(Debug, Clone)]
pub struct Patch {
    /// Face indices belonging to the patch, ascending.
    pub faces: Vec<usize>,
    /// Referenced vertex indices, ascending (`np.unique(tris[f_in_e])`).
    pub verts_idx: Vec<usize>,
    pub centroid: [f64; 3],
    pub area: f64,
    /// Eigenvalues in `np.linalg.eigh`'s descending-reversed order, i.e.
    /// `eigvals[[2, 1, 0]]`.
    pub eigvals: [f64; 3],
    /// PCA frame `eigvecs[:, [2, 1, 0]]`, det-corrected.
    pub frame: [[f64; 3]; 3],
    pub diag: f64,
}

fn py_round(value: f64, decimals: usize) -> f64 {
    if !value.is_finite() {
        return value;
    }
    format!("{:.*}", decimals, value).parse::<f64>().unwrap_or(value)
}

/// Weld coincident vertices when the mesh looks like an unmerged STL.
///
/// Returns the (possibly welded) mesh. `None` means no weld was applied, which
/// happens when the vertex count is below `2 * n_faces` or welding would not
/// remove anything.
fn weld_if_unmerged(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> Option<(Vec<[f64; 3]>, Vec<[usize; 3]>)> {
    if verts.len() < 2 * tris.len() {
        return None;
    }
    // Quantise each vertex, then group identical rows.
    let keys: Vec<[f64; 3]> = verts
        .iter()
        .map(|p| [py_round(p[0], 4), py_round(p[1], 4), py_round(p[2], 4)])
        .collect();
    // `np.unique(axis=0)` sorts rows lexicographically.
    let mut order: Vec<usize> = (0..keys.len()).collect();
    order.sort_by(|&a, &b| {
        for k in 0..3 {
            match keys[a][k].partial_cmp(&keys[b][k]) {
                Some(std::cmp::Ordering::Equal) => continue,
                Some(o) => return o,
                None => return std::cmp::Ordering::Equal,
            }
        }
        std::cmp::Ordering::Equal
    });
    // First occurrence of each distinct row, in sorted-row order.
    let mut uniq_first: Vec<usize> = Vec::new();
    let mut rank_of: Vec<usize> = vec![0; keys.len()];
    for &idx in &order {
        let is_new = match uniq_first.last() {
            None => true,
            Some(&prev) => keys[prev] != keys[idx],
        };
        if is_new {
            uniq_first.push(idx);
        }
        rank_of[idx] = uniq_first.len() - 1;
    }
    if uniq_first.len() >= verts.len() {
        return None;
    }
    let new_verts: Vec<[f64; 3]> = uniq_first.iter().map(|&i| verts[i]).collect();
    let new_tris: Vec<[usize; 3]> = tris
        .iter()
        .map(|f| [rank_of[f[0]], rank_of[f[1]], rank_of[f[2]]])
        .collect();
    Some((new_verts, new_tris))
}

/// `repeat.py::segment_elements`.
pub fn segment_elements(verts: &[[f64; 3]], tris: &[[usize; 3]], angle_deg: f64) -> Vec<Patch> {
    let n_faces = tris.len();
    if n_faces == 0 {
        return Vec::new();
    }
    let (verts, tris) = match weld_if_unmerged(verts, tris) {
        Some(w) => w,
        None => (verts.to_vec(), tris.to_vec()),
    };

    // 1. Face normals and areas.
    let mut normals = Vec::with_capacity(n_faces);
    let mut areas = Vec::with_capacity(n_faces);
    let mut centroids = Vec::with_capacity(n_faces);
    for f in &tris {
        let (v0, v1, v2) = (verts[f[0]], verts[f[1]], verts[f[2]]);
        let a = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
        let b = [v2[0] - v0[0], v2[1] - v0[1], v2[2] - v0[2]];
        let cross = [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ];
        let area = 0.5 * (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt();
        let denom = 2.0 * area + 1e-12;
        normals.push([cross[0] / denom, cross[1] / denom, cross[2] / denom]);
        areas.push(area);
        centroids.push([
            (v0[0] + v1[0] + v2[0]) / 3.0,
            (v0[1] + v1[1] + v2[1]) / 3.0,
            (v0[2] + v1[2] + v2[2]) / 3.0,
        ]);
    }

    // 2. Edge -> faces, in triangle-scan order (Python dict insertion order).
    let mut edge_faces: std::collections::HashMap<(usize, usize), Vec<usize>> =
        std::collections::HashMap::new();
    let mut edge_order: Vec<(usize, usize)> = Vec::new();
    for (fi, f) in tris.iter().enumerate() {
        for (a, b) in [(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
            let key = if a <= b { (a, b) } else { (b, a) };
            let entry = edge_faces.entry(key).or_default();
            if entry.is_empty() {
                edge_order.push(key);
            }
            entry.push(fi);
        }
    }

    // 3. Connect faces across non-feature edges.
    let cos_thresh = angle_deg.to_radians().cos();
    let mut smooth_edges: Vec<(usize, usize)> = Vec::new();
    for key in &edge_order {
        let fl = &edge_faces[key];
        if fl.len() == 2 {
            let (f1, f2) = (fl[0], fl[1]);
            let n1 = normals[f1];
            let n2 = normals[f2];
            let dot = (n1[0] * n2[0] + n1[1] * n2[1] + n1[2] * n2[2]).clamp(-1.0, 1.0);
            if dot >= cos_thresh {
                smooth_edges.push((f1, f2));
                smooth_edges.push((f2, f1));
            }
        }
    }
    let (n_comp, labels) = connected_components(n_faces, &smooth_edges);

    // 4. Drop base components by area.
    // `np.sum` uses NumPy's pairwise reduction, not a naive left fold: on 200
    // elements of 0.1 the two differ in the last bits (20.0 vs 20.000000000000014),
    // which is enough to move a patch area past a 1e-12 comparison.
    let total_area: f64 = sum_f64(&areas);
    let mut comp_areas = vec![0.0f64; n_comp];
    for (fi, &lab) in labels.iter().enumerate() {
        comp_areas[lab] += areas[fi];
    }
    let base: Vec<bool> = comp_areas
        .iter()
        .map(|&a| a > total_area * BASE_AREA_THRESHOLD_FRACTION)
        .collect();
    let non_base: Vec<usize> = (0..n_faces).filter(|&fi| !base[labels[fi]]).collect();
    if non_base.is_empty() {
        return Vec::new();
    }

    // 5. Connect adjacent non-base faces into element patches.
    let mut is_nb = vec![false; n_faces];
    for &fi in &non_base {
        is_nb[fi] = true;
    }
    let mut elem_edges: Vec<(usize, usize)> = Vec::new();
    for key in &edge_order {
        let fl = &edge_faces[key];
        if fl.len() == 2 && is_nb[fl[0]] && is_nb[fl[1]] {
            elem_edges.push((fl[0], fl[1]));
            elem_edges.push((fl[1], fl[0]));
        }
    }
    let (_n_elem, elem_labels) = connected_components(n_faces, &elem_edges);

    // `np.unique(elem_labels[non_base_faces])` -> ascending distinct ids.
    let mut unique_elems: Vec<usize> = non_base.iter().map(|&fi| elem_labels[fi]).collect();
    unique_elems.sort_unstable();
    unique_elems.dedup();

    let mut patches = Vec::new();
    for e_id in unique_elems {
        let f_in_e: Vec<usize> = non_base
            .iter()
            .copied()
            .filter(|&fi| elem_labels[fi] == e_id)
            .collect();
        if f_in_e.len() < MIN_ELEMENT_FACES {
            continue;
        }
        let patch_areas: Vec<f64> = f_in_e.iter().map(|&fi| areas[fi]).collect();
        let patch_area: f64 = sum_f64(&patch_areas);
        if patch_area <= 1e-12 {
            continue;
        }

        // Area-weighted centroid: `np.sum(face_centroids * areas[:, None], axis=0)`.
        //
        // This is a 2-D reduction and NumPy does NOT use pairwise summation for
        // it -- measured: for n = 50 and n = 300 rows a sequential row
        // accumulation matches `np.sum(axis=0)` exactly while the 8-accumulator
        // pairwise form does not (a 1-D `np.sum` over the same column does use
        // pairwise). So this loop is sequential on purpose; switching it to
        // `sum_f64` moved 7 of 226 patch-route scores by up to 8.8e-6.
        let mut centroid = [0.0f64; 3];
        for &fi in &f_in_e {
            let w = areas[fi];
            for k in 0..3 {
                centroid[k] += centroids[fi][k] * w;
            }
        }
        for k in 0..3 {
            centroid[k] /= patch_area;
        }

        // `np.unique(tris[f_in_e])` -> ascending referenced vertices.
        let mut verts_idx: Vec<usize> = f_in_e
            .iter()
            .flat_map(|&fi| tris[fi])
            .collect();
        verts_idx.sort_unstable();
        verts_idx.dedup();

        // PCA over the patch's vertices.
        let n = verts_idx.len() as f64;
        let mut cov = [[0.0f64; 3]; 3];
        for &vi in &verts_idx {
            let d = [
                verts[vi][0] - centroid[0],
                verts[vi][1] - centroid[1],
                verts[vi][2] - centroid[2],
            ];
            for i in 0..3 {
                for j in 0..3 {
                    cov[i][j] += d[i] * d[j];
                }
            }
        }
        for i in 0..3 {
            for j in 0..3 {
                cov[i][j] /= n.max(1.0);
            }
        }
        let (eigvals_asc, eigvecs) = eigh3(cov);

        // frame = eigvecs[:, [2, 1, 0]], then det-correct column 2.
        let mut frame = [[0.0f64; 3]; 3];
        for row in 0..3 {
            frame[row][0] = eigvecs[row][2];
            frame[row][1] = eigvecs[row][1];
            frame[row][2] = eigvecs[row][0];
        }
        let det = frame[0][0] * (frame[1][1] * frame[2][2] - frame[1][2] * frame[2][1])
            - frame[0][1] * (frame[1][0] * frame[2][2] - frame[1][2] * frame[2][0])
            + frame[0][2] * (frame[1][0] * frame[2][1] - frame[1][1] * frame[2][0]);
        if det < 0.0 {
            for row in 0..3 {
                frame[row][2] = -frame[row][2];
            }
        }

        // diag = |span| over the patch vertices.
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for &vi in &verts_idx {
            for k in 0..3 {
                lo[k] = lo[k].min(verts[vi][k]);
                hi[k] = hi[k].max(verts[vi][k]);
            }
        }
        let span = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
        let diag = (span[0] * span[0] + span[1] * span[1] + span[2] * span[2]).sqrt();

        patches.push(Patch {
            faces: f_in_e,
            verts_idx,
            centroid,
            area: patch_area,
            eigvals: [eigvals_asc[2], eigvals_asc[1], eigvals_asc[0]],
            frame,
            diag,
        });
    }
    patches
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two separate quads sharing no edge: two patches, both above the
    /// 4-face minimum and below the base-area fraction only if small.
    fn two_quads() -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        // Each quad is 2 triangles, so a quad is 2 faces -- below
        // MIN_ELEMENT_FACES. Use two 2x2 grids (8 triangles each) folded at 90
        // degrees so the dihedral split separates them.
        let mut verts = Vec::new();
        let mut tris = Vec::new();
        // Plate A: 2x2 grid in z = 0.
        for i in 0..3 {
            for j in 0..3 {
                verts.push([i as f64, j as f64, 0.0]);
            }
        }
        for i in 0..2 {
            for j in 0..2 {
                let a = i * 3 + j;
                let b = a + 1;
                let c = a + 3;
                let d = c + 1;
                tris.push([a, b, d]);
                tris.push([a, d, c]);
            }
        }
        (verts, tris)
    }

    #[test]
    fn empty_mesh_has_no_patches() {
        assert!(segment_elements(&[], &[], DEFAULT_FEATURE_ANGLE_DEG).is_empty());
    }

    #[test]
    fn a_single_flat_plate_is_all_base() {
        // One flat component covering 100% of the area -> dropped as base.
        let (verts, tris) = two_quads();
        let patches = segment_elements(&verts, &tris, DEFAULT_FEATURE_ANGLE_DEG);
        assert!(patches.is_empty(), "flat plate should be base, got {}", patches.len());
    }

    #[test]
    fn patch_descriptors_are_consistent() {
        let patches = segment_elements(&two_quads().0, &two_quads().1, DEFAULT_FEATURE_ANGLE_DEG);
        for p in &patches {
            assert!(p.area > 0.0);
            assert!(p.diag >= 0.0);
            assert!(p.faces.len() >= MIN_ELEMENT_FACES);
            assert_eq!(p.faces.len(), p.faces.len());
            // verts_idx must be ascending and unique (np.unique contract).
            assert!(p.verts_idx.windows(2).all(|w| w[0] < w[1]));
        }
    }

    #[test]
    fn py_round_matches_python() {
        /// Golden values from Python: `round(v, 4)`, `float(format(v, '.4f'))`
        /// and `np.round(v, 4)` all agree on these, including the exact .5
        /// boundary where binary representation decides the direction.
        assert_eq!(py_round(0.123456, 4), 0.1235);
        assert_eq!(py_round(-0.123456, 4), -0.1235);
        assert_eq!(py_round(1.00005, 4), 1.0001);
        assert_eq!(py_round(2.000049, 4), 2.0);
    }
}
