// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! The two Closing detectors, shared by the Python engine and the Rust CLI.
//!
//! `methods/closing.py` scores `single_side_score` (one dominant open boundary
//! loop facing a large spanned area) and `relief_score` (that loop's plane is
//! nearly flat and the surface sits mostly on one side of it). Both are plain
//! geometric statistics, so the port keeps the Python expressions verbatim and
//! the pyo3 bridge lets `closing.py` call this instead of duplicating them.
//!
//! `relief_score`'s plane fit is `np.linalg.svd(centered)[2]`, which is only
//! used for the smallest right singular vector. For an Nx3 `centered` that
//! vector is the smallest-eigenvalue eigenvector of the 3x3 covariance
//! `centered.T @ centered`, so no Nx3 SVD is needed: `eigh3` on the covariance
//! gives the same normal (measured: |dot| differs by 1e-15, plane-fit RMS by
//! 9e-14) and the sign is immaterial because the result is re-oriented from
//! the face-normal sum below.
//!
//! Per the project rule the numeric core lives here, once, and Python calls it.

use crate::eigh3::eigh3;
use crate::measures::sum_f64;

/// Score ramps, mirroring the constants in `methods/closing.py`.
const DOM_RAMP_LO: f64 = 0.40;
const DOM_RAMP_SPAN: f64 = 0.45;
const SPAN_RAMP_LO: f64 = 0.02;
const SPAN_RAMP_SPAN: f64 = 0.23;
const PLANAR_RAMP_HI: f64 = 0.06;
const PLANAR_RAMP_SPAN: f64 = 0.05;
const ONESIDED_RAMP_LO: f64 = 0.75;
const ONESIDED_RAMP_SPAN: f64 = 0.20;
const NORMAL_RAMP_LO: f64 = 0.60;
const NORMAL_RAMP_SPAN: f64 = 0.25;
/// `one_sided_ratio` counts vertices at or above `-0.02 * diag` relative to the
/// fitted plane.
const ONESIDED_SLACK: f64 = 0.02;

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

fn clamp01(x: f64) -> f64 {
    x.clamp(0.0, 1.0)
}

/// Python's `round(value, decimals)` used by both detectors.
fn py_round(value: f64, decimals: i32) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let text = format!("{:.*}", decimals as usize, value);
    text.parse::<f64>().unwrap_or(value)
}

/// `closing._surface_area`: `float(np.sum(0.5 * |cross|))` over f64 triangles.
///
/// `closing.py` widens the float32 mesh it receives from `diagnosis` to float64
/// first, so the areas and their NumPy pairwise sum are f64 here too.
pub fn surface_area_f64(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> f64 {
    if tris.is_empty() {
        return 0.0;
    }
    let mut areas = Vec::with_capacity(tris.len());
    for f in tris {
        if f[0] >= verts.len() || f[1] >= verts.len() || f[2] >= verts.len() {
            areas.push(0.0);
            continue;
        }
        let cr = cross(sub(verts[f[1]], verts[f[0]]), sub(verts[f[2]], verts[f[0]]));
        areas.push(0.5 * norm(cr));
    }
    sum_f64(&areas)
}

/// `closing._loop_stats`: perimeter, spanned area (3D vector area magnitude) and
/// centroid of one boundary loop.
pub fn loop_stats(verts: &[[f64; 3]], lp: &[usize]) -> (f64, f64, [f64; 3]) {
    if lp.is_empty() {
        return (0.0, 0.0, [0.0; 3]);
    }
    let pts: Vec<[f64; 3]> = lp.iter().map(|&i| verts[i]).collect();
    let mut perim = 0.0f64;
    for i in 0..pts.len() {
        let nxt = pts[(i + 1) % pts.len()];
        perim += norm(sub(nxt, pts[i]));
    }
    // A = 0.5 * sum(p_i x p_{i+1}), reduced in the loop order Python uses.
    let mut acc = [0.0f64; 3];
    for i in 0..pts.len() {
        let nxt = pts[(i + 1) % pts.len()];
        let c = cross(pts[i], nxt);
        acc[0] += c[0];
        acc[1] += c[1];
        acc[2] += c[2];
    }
    let vec_area = [0.5 * acc[0], 0.5 * acc[1], 0.5 * acc[2]];
    let mut centroid = [0.0f64; 3];
    for p in &pts {
        centroid[0] += p[0];
        centroid[1] += p[1];
        centroid[2] += p[2];
    }
    let n = pts.len() as f64;
    centroid = [centroid[0] / n, centroid[1] / n, centroid[2] / n];
    (perim, norm(vec_area), centroid)
}

/// Everything `closing.single_side_score` reports, already rounded the way the
/// Python original rounds it. `reason` is empty when the score was computed.
#[derive(Debug, Clone)]
pub struct SingleSide {
    pub score: f64,
    pub n_loops: usize,
    pub dominant_loop_len: usize,
    pub dominant_ratio: f64,
    pub spanned_area_ratio: f64,
    pub dominant_loop: Vec<usize>,
    pub reason: &'static str,
}

/// Everything `closing.relief_score` reports, already rounded.
#[derive(Debug, Clone)]
pub struct Relief {
    pub score: f64,
    pub plane_normal: [f64; 3],
    pub plane_center: [f64; 3],
    pub rms_rel: f64,
    pub one_sided_ratio: f64,
    pub normal_align_ratio: f64,
    pub dominant_loop: Vec<usize>,
    pub reason: &'static str,
}

/// `closing.single_side_score`: one dominant open boundary loop facing a large
/// spanned area.
///
/// `loops` are the boundary loops as vertex indices in
/// `closing.boundary_loops` order (the Python walk is deterministic and is the
/// single source of the loop order; this kernel does not walk edges again).
pub fn single_side(verts: &[[f64; 3]], tris: &[[usize; 3]], loops: &[Vec<usize>]) -> SingleSide {
    let empty = |reason: &'static str, n: usize| SingleSide {
        score: 0.0,
        n_loops: n,
        dominant_loop_len: 0,
        dominant_ratio: 0.0,
        spanned_area_ratio: 0.0,
        dominant_loop: Vec::new(),
        reason,
    };
    if loops.is_empty() {
        return empty("closed", 0);
    }
    let total_s = surface_area_f64(verts, tris);
    if total_s <= 1e-12 {
        return empty("zero_area", loops.len());
    }
    let mut perims = Vec::with_capacity(loops.len());
    let mut spanned = Vec::with_capacity(loops.len());
    for lp in loops {
        let (p, a, _) = loop_stats(verts, lp);
        perims.push(p);
        spanned.push(a);
    }
    let total_perim: f64 = perims.iter().sum();
    if total_perim <= 1e-12 {
        return empty("zero_perimeter", loops.len());
    }
    // `np.argmax` returns the FIRST maximum; keep that tie behaviour.
    let mut dominant = 0usize;
    for i in 1..perims.len() {
        if perims[i] > perims[dominant] {
            dominant = i;
        }
    }
    let dom_ratio = perims[dominant] / total_perim;
    let spanned_ratio = spanned[dominant] / total_s;
    let s_dom = clamp01((dom_ratio - DOM_RAMP_LO) / DOM_RAMP_SPAN);
    let s_span = clamp01((spanned_ratio - SPAN_RAMP_LO) / SPAN_RAMP_SPAN);
    SingleSide {
        score: py_round(s_dom * s_span, 4),
        n_loops: loops.len(),
        dominant_loop_len: loops[dominant].len(),
        dominant_ratio: py_round(dom_ratio, 4),
        spanned_area_ratio: py_round(spanned_ratio, 4),
        dominant_loop: loops[dominant].clone(),
        reason: "",
    }
}

/// `closing.relief_score`: the dominant loop's plane is nearly flat and the
/// surface sits mostly on one side of it.
pub fn relief(verts: &[[f64; 3]], tris: &[[usize; 3]], loops: &[Vec<usize>]) -> Relief {
    let mut out = Relief {
        score: 0.0,
        plane_normal: [0.0; 3],
        plane_center: [0.0; 3],
        rms_rel: 0.0,
        one_sided_ratio: 0.0,
        normal_align_ratio: 0.0,
        dominant_loop: Vec::new(),
        reason: "closed",
    };
    if loops.is_empty() {
        return out;
    }
    let mut perims = Vec::with_capacity(loops.len());
    for lp in loops {
        perims.push(loop_stats(verts, lp).0);
    }
    let mut dominant = 0usize;
    for i in 1..perims.len() {
        if perims[i] > perims[dominant] {
            dominant = i;
        }
    }
    let pts: Vec<[f64; 3]> = loops[dominant].iter().map(|&i| verts[i]).collect();
    if pts.is_empty() {
        return out;
    }

    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in verts {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let diag = norm([hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]]);
    if diag <= 1e-12 {
        out.reason = "degenerate_bbox";
        return out;
    }

    // Plane fit: smallest-eigenvalue eigenvector of the loop's 3x3 covariance,
    // which is the smallest right singular vector of the centred point set.
    let n_pts = pts.len() as f64;
    let mut c = [0.0f64; 3];
    for p in &pts {
        c[0] += p[0];
        c[1] += p[1];
        c[2] += p[2];
    }
    c = [c[0] / n_pts, c[1] / n_pts, c[2] / n_pts];
    let mut cov = [[0.0f64; 3]; 3];
    for p in &pts {
        let d = sub(*p, c);
        for i in 0..3 {
            for j in 0..3 {
                cov[i][j] += d[i] * d[j];
            }
        }
    }
    let (_, vectors) = eigh3(cov);
    let mut n = [vectors[0][0], vectors[1][0], vectors[2][0]];

    let mut residuals = Vec::with_capacity(pts.len());
    for p in &pts {
        residuals.push(dot(sub(*p, c), n));
    }
    let rms = (residuals.iter().map(|r| r * r).sum::<f64>() / n_pts).sqrt();
    let rms_rel = rms / diag;

    // Orient the normal toward the surface (`np.dot(fn_sum, n) < 0` -> flip).
    let mut fn_sum = [0.0f64; 3];
    for f in tris {
        let a = verts[f[0]];
        let b = verts[f[1]];
        let cc = verts[f[2]];
        let nf = cross(sub(b, a), sub(cc, a));
        fn_sum[0] += nf[0];
        fn_sum[1] += nf[1];
        fn_sum[2] += nf[2];
    }
    if dot(fn_sum, n) < 0.0 {
        n = [-n[0], -n[1], -n[2]];
    }

    let slack = -ONESIDED_SLACK * diag;
    let above = verts
        .iter()
        .filter(|p| dot(sub(**p, c), n) >= slack)
        .count();
    let one_sided_ratio = above as f64 / verts.len() as f64;

    let mut aligned = 0usize;
    let mut valid = 0usize;
    for f in tris {
        let a = verts[f[0]];
        let b = verts[f[1]];
        let cc = verts[f[2]];
        let nf = cross(sub(b, a), sub(cc, a));
        let len = norm(nf);
        if len > 1e-12 {
            valid += 1;
            if dot([nf[0] / len, nf[1] / len, nf[2] / len], n) > 0.0 {
                aligned += 1;
            }
        }
    }
    let normal_align_ratio = if valid > 0 {
        aligned as f64 / valid as f64
    } else {
        0.0
    };

    let s_planar = clamp01((PLANAR_RAMP_HI - rms_rel) / PLANAR_RAMP_SPAN);
    let s_onesided = clamp01((one_sided_ratio - ONESIDED_RAMP_LO) / ONESIDED_RAMP_SPAN);
    let s_normal = clamp01((normal_align_ratio - NORMAL_RAMP_LO) / NORMAL_RAMP_SPAN);
    out.score = py_round(s_planar * s_onesided * s_normal, 4);
    out.plane_normal = n;
    out.plane_center = c;
    out.rms_rel = py_round(rms_rel, 5);
    out.one_sided_ratio = py_round(one_sided_ratio, 4);
    out.normal_align_ratio = py_round(normal_align_ratio, 4);
    out.dominant_loop = loops[dominant].clone();
    out.reason = "";
    out
}

/// Score-only views, kept for callers that do not need the info dict.
pub fn single_side_score(verts: &[[f64; 3]], tris: &[[usize; 3]], loops: &[Vec<usize>]) -> f64 {
    single_side(verts, tris, loops).score
}

pub fn relief_score(verts: &[[f64; 3]], tris: &[[usize; 3]], loops: &[Vec<usize>]) -> f64 {
    relief(verts, tris, loops).score
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An open flat sheet: one boundary loop, its spanned area equals the
    /// surface area, so both scores sit at their maximum.
    fn open_sheet() -> (Vec<[f64; 3]>, Vec<[usize; 3]>, Vec<Vec<usize>>) {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let tris = vec![[0usize, 1, 2], [0, 2, 3]];
        let loops = vec![vec![0usize, 1, 2, 3]];
        (verts, tris, loops)
    }

    #[test]
    fn closed_mesh_scores_zero() {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let tris = vec![[0usize, 1, 2]];
        assert_eq!(single_side_score(&verts, &tris, &[]), 0.0);
        assert_eq!(relief_score(&verts, &tris, &[]), 0.0);
    }

    #[test]
    fn open_sheet_hits_both_maxima() {
        let (verts, tris, loops) = open_sheet();
        assert_eq!(single_side_score(&verts, &tris, &loops), 1.0);
        assert_eq!(relief_score(&verts, &tris, &loops), 1.0);
    }

    /// The plane normal must come from the smallest eigenvector of the loop's
    /// covariance, i.e. the same vector `np.linalg.svd(centered)[2]` produces.
    /// A loop lying exactly in z = 0 must therefore yield a normal of +-z.
    #[test]
    fn plane_normal_is_the_smallest_eigenvector() {
        let (verts, _, loops) = open_sheet();
        let pts: Vec<[f64; 3]> = loops[0].iter().map(|&i| verts[i]).collect();
        let c = [0.5, 0.5, 0.0];
        let mut cov = [[0.0f64; 3]; 3];
        for p in &pts {
            let d = sub(*p, c);
            for i in 0..3 {
                for j in 0..3 {
                    cov[i][j] += d[i] * d[j];
                }
            }
        }
        let (_, vectors) = eigh3(cov);
        let n = [vectors[0][0], vectors[1][0], vectors[2][0]];
        assert!(
            n[2].abs() > 0.999,
            "expected the normal along z, got {n:?}"
        );
    }

    #[test]
    fn loop_stats_match_the_python_expressions() {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
            [3.0, 4.0, 0.0],
        ];
        let (perim, spanned, centroid) = loop_stats(&verts, &[0, 1, 2]);
        assert!((perim - 12.0).abs() < 1e-12, "perimeter {perim}");
        // 0.5 * |sum(p_i x p_{i+1})| = 0.5 * 12 for a right triangle.
        assert!((spanned - 6.0).abs() < 1e-12, "spanned {spanned}");
        assert!((centroid[0] - 2.0).abs() < 1e-12);
        assert!((centroid[1] - 4.0 / 3.0).abs() < 1e-12);
    }
}
