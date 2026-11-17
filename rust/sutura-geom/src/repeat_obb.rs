// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! `repeat.py`'s OBB helpers and the one-sided Hausdorff guard.
//!
//! `_compute_obb` picks the PCA frame, expands the box by `margin` and clamps
//! the extents to a minimum; `_is_inside_obb` projects a point cloud into that
//! frame; `_compute_hausdorff_outside` samples the ORIGINAL surface, drops the
//! samples that fall inside any repaired box, and reports the worst distance to
//! the result -- the guard that stops a transplant from moving untouched
//! geometry.
//!
//! Reproduction notes: the covariance comes from `measures::covariance` (NumPy
//! builds it with BLAS `dgemm`), `np.mean` for the centre is a 2-D reduction so
//! it accumulates sequentially, and the Hausdorff sample uses the shared
//! `sample_surface` (bit-identical to NumPy).

use crate::eigh3::eigh3;
use crate::measures::covariance;
use crate::sample::sample_surface;
use crate::svd3::det3;
use crate::winding;
use rayon::prelude::*;

/// `OBB_EXPANSION_MARGIN`.
pub const OBB_EXPANSION_MARGIN: f64 = 0.10;
/// `OBB_MIN_EPSILON`.
pub const OBB_MIN_EPSILON: f64 = 1e-3;

/// An oriented bounding box: centre, side lengths, and the frame whose COLUMNS
/// are the box axes (`proj = (pts - center) @ frame`).
#[derive(Clone, Copy, Debug)]
pub struct Obb {
    pub center: [f64; 3],
    pub extents: [f64; 3],
    pub frame: [[f64; 3]; 3],
}

fn mean_points(pts: &[[f64; 3]]) -> [f64; 3] {
    let mut acc = [0.0f64; 3];
    for p in pts {
        for k in 0..3 {
            acc[k] += p[k];
        }
    }
    let n = pts.len() as f64;
    if n > 0.0 {
        for k in 0..3 {
            acc[k] /= n;
        }
    }
    acc
}

/// `(pts - center) @ frame` for every point.
fn project(pts: &[[f64; 3]], center: [f64; 3], frame: &[[f64; 3]; 3]) -> Vec<[f64; 3]> {
    pts.iter()
        .map(|p| {
            let d = [p[0] - center[0], p[1] - center[1], p[2] - center[2]];
            [
                d[0] * frame[0][0] + d[1] * frame[1][0] + d[2] * frame[2][0],
                d[0] * frame[0][1] + d[1] * frame[1][1] + d[2] * frame[2][1],
                d[0] * frame[0][2] + d[1] * frame[1][2] + d[2] * frame[2][2],
            ]
        })
        .collect()
}

/// `repeat._compute_obb`.
pub fn compute_obb(pts: &[[f64; 3]], margin: f64, epsilon: f64) -> Obb {
    let c = mean_points(pts);
    let cov = covariance(pts, c);
    let (_, v) = eigh3(cov);

    // frame = v[:, [2, 1, 0]], then det-correct column 2.
    let mut frame = [[0.0f64; 3]; 3];
    for row in 0..3 {
        frame[row][0] = v[row][2];
        frame[row][1] = v[row][1];
        frame[row][2] = v[row][0];
    }
    if det3(&frame) < 0.0 {
        for row in 0..3 {
            frame[row][2] = -frame[row][2];
        }
    }

    let proj = project(pts, c, &frame);
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in &proj {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    if proj.is_empty() {
        lo = [0.0; 3];
        hi = [0.0; 3];
    }
    let span = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let mid = [
        0.5 * (lo[0] + hi[0]),
        0.5 * (lo[1] + hi[1]),
        0.5 * (lo[2] + hi[2]),
    ];

    // center = c + frame @ p_mid
    let center = [
        c[0] + frame[0][0] * mid[0] + frame[0][1] * mid[1] + frame[0][2] * mid[2],
        c[1] + frame[1][0] * mid[0] + frame[1][1] * mid[1] + frame[1][2] * mid[2],
        c[2] + frame[2][0] * mid[0] + frame[2][1] * mid[1] + frame[2][2] * mid[2],
    ];
    let mut extents = [0.0f64; 3];
    for k in 0..3 {
        extents[k] = span[k] * (1.0 + 2.0 * margin) + 2.0 * epsilon;
        extents[k] = extents[k].max(epsilon * 2.0);
    }
    Obb {
        center,
        extents,
        frame,
    }
}

/// `repeat._is_inside_obb`: one boolean per point.
pub fn is_inside_obb(
    pts: &[[f64; 3]],
    center: [f64; 3],
    extents: [f64; 3],
    frame: &[[f64; 3]; 3],
    margin_factor: f64,
) -> Vec<bool> {
    let half = [
        extents[0] * margin_factor / 2.0,
        extents[1] * margin_factor / 2.0,
        extents[2] * margin_factor / 2.0,
    ];
    project(pts, center, frame)
        .iter()
        .map(|p| p[0].abs() <= half[0] && p[1].abs() <= half[1] && p[2].abs() <= half[2])
        .collect()
}

/// `repeat._compute_hausdorff_outside`.
///
/// Samples the ORIGINAL surface, discards samples inside any repaired box, and
/// returns `max(distance to the result surface) / bbox diagonal`.
pub fn compute_hausdorff_outside(
    orig_v: &[[f64; 3]],
    orig_t: &[[usize; 3]],
    result_v: &[[f64; 3]],
    result_t: &[[usize; 3]],
    dst_boxes: &[Obb],
    samples: usize,
) -> f64 {
    let (sample_pts, _) = sample_surface(orig_v, orig_t, samples, 0);
    if sample_pts.is_empty() {
        return 0.0;
    }
    let mut outside = vec![true; sample_pts.len()];
    for b in dst_boxes {
        let inside = is_inside_obb(&sample_pts, b.center, b.extents, &b.frame, 1.0);
        for (o, i) in outside.iter_mut().zip(inside.iter()) {
            if *i {
                *o = false;
            }
        }
    }
    let outside_pts: Vec<[f64; 3]> = sample_pts
        .iter()
        .zip(outside.iter())
        .filter(|(_, &o)| o)
        .map(|(p, _)| *p)
        .collect();
    if outside_pts.is_empty() {
        return 0.0;
    }
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in orig_v {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let d = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let diag = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if diag <= 1e-12 {
        return 0.0;
    }
    let bvh = winding::MeshBvh::from_arrays(result_v, result_t);
    let dists: Vec<f64> = outside_pts
        .par_iter()
        .map(|p| bvh.closest_point(*p).1)
        .collect();
    // `float(np.max(dists)) / diag` -- 1-D max needs no summation order.
    let worst = dists.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    worst / diag
}

/// `repeat._decimate_faces`: QEM decimation to a target face count.
///
/// Python calls `topology.decimate_qem(..., lock_border=False,
/// preserve_normal=False)`; here the geometry kernel is called directly, so the
/// light LOD the picker uses has one implementation.
#[cfg(feature = "decimate")]
pub fn decimate_faces(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    target: usize,
) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
    let t64: Vec<[i64; 3]> = tris
        .iter()
        .map(|f| [f[0] as i64, f[1] as i64, f[2] as i64])
        .collect();
    match crate::decimate::decimate_impl(verts, &t64, target, None, false, None) {
        Ok(rep) => (
            rep.verts,
            rep.tris
                .iter()
                .map(|f| [f[0] as usize, f[1] as usize, f[2] as usize])
                .collect(),
        ),
        // Python's `_decimate_faces` is wrapped in try/except at the call site
        // (the picker falls back to "decimation failed"), so a failure here
        // returns the input unchanged rather than panicking.
        Err(_) => (verts.to_vec(), tris.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> Vec<[f64; 3]> {
        let mut v = Vec::new();
        for &x in &[-1.0f64, 1.0] {
            for &y in &[-1.0f64, 1.0] {
                for &z in &[-1.0f64, 1.0] {
                    v.push([x, y, z]);
                }
            }
        }
        v
    }

    #[test]
    fn obb_of_a_cube_has_the_right_centre_and_span() {
        let obb = compute_obb(&cube(), OBB_EXPANSION_MARGIN, OBB_MIN_EPSILON);
        for k in 0..3 {
            assert!(obb.center[k].abs() < 1e-12, "centre {k}: {}", obb.center[k]);
        }
        // span 2, expanded by (1 + 2*margin) plus 2*epsilon.
        let want = 2.0 * (1.0 + 2.0 * OBB_EXPANSION_MARGIN) + 2.0 * OBB_MIN_EPSILON;
        for k in 0..3 {
            assert!((obb.extents[k] - want).abs() < 1e-12, "extent {k}: {}", obb.extents[k]);
        }
    }

    #[test]
    fn obb_frame_is_orthonormal_with_positive_determinant() {
        let obb = compute_obb(&cube(), OBB_EXPANSION_MARGIN, OBB_MIN_EPSILON);
        let f = obb.frame;
        for i in 0..3 {
            for j in 0..3 {
                let dot = f[0][i] * f[0][j] + f[1][i] * f[1][j] + f[2][i] * f[2][j];
                let want = if i == j { 1.0 } else { 0.0 };
                assert!((dot - want).abs() < 1e-12, "frame not orthonormal at {i},{j}");
            }
        }
        assert!(det3(&f) > 0.0);
    }

    #[test]
    fn inside_obb_holds_every_vertex_of_the_box() {
        let pts = cube();
        let obb = compute_obb(&pts, OBB_EXPANSION_MARGIN, OBB_MIN_EPSILON);
        let mask = is_inside_obb(&pts, obb.center, obb.extents, &obb.frame, 1.0);
        assert!(mask.iter().all(|&b| b), "all cube vertices must be inside");
    }

    #[test]
    fn inside_obb_rejects_a_far_point() {
        let pts = cube();
        let obb = compute_obb(&pts, OBB_EXPANSION_MARGIN, OBB_MIN_EPSILON);
        let mask = is_inside_obb(&[[100.0, 0.0, 0.0]], obb.center, obb.extents, &obb.frame, 1.0);
        assert_eq!(mask, vec![false]);
    }

    #[test]
    fn hausdorff_is_zero_when_the_result_contains_the_original() {
        // A quad; the "result" is the same quad plus a far-away triangle, so
        // every original sample is still on the result surface.
        let v = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let t = vec![[0usize, 1, 2], [0, 2, 3]];
        let d = compute_hausdorff_outside(&v, &t, &v, &t, &[], 64);
        assert!(d < 1e-9, "self distance must be ~0, got {d}");
    }

    #[test]
    fn hausdorff_grows_when_the_result_moves_away() {
        let v = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let t = vec![[0usize, 1, 2], [0, 2, 3]];
        let far: Vec<[f64; 3]> = v.iter().map(|p| [p[0], p[1], p[2] + 1.0]).collect();
        let moved = compute_hausdorff_outside(&v, &t, &far, &t, &[], 64);
        let same = compute_hausdorff_outside(&v, &t, &v, &t, &[], 64);
        assert!(moved > same, "moved {moved} should exceed same {same}");
    }
}
