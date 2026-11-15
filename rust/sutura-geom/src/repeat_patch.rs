// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! `repeat.py::_detect_repetition_patches` and `detect_repetition`.
//!
//! The patch route clusters the segmented elements by invariant descriptors and
//! then looks for a rotational, helical or translational arrangement of their
//! centroids. `detect_repetition` runs that route and the segmentation-free one
//! and picks a winner by the same rules Python uses.
//!
//! Reproduction notes:
//!
//! * `np.mean(centroids, axis=0)` and `np.mean(diffs)` are 2-D vs 1-D
//!   respectively -- the first accumulates sequentially, the second pairwise.
//!   `mean_2d` and `sum_f64` below encode that split.
//! * `np.std` is `sqrt(mean((x - mean)^2))`, so the same rule applies.
//! * `connected_components` must label exactly as SciPy does, because the
//!   cluster id decides which patches become `members`.
//! * The algebraic circle fit reuses the shared `lstsq3`, so the plane basis and
//!   the reflection handling are the same in both languages.

use crate::components::connected_components;
use crate::eigh3::eigh3;
use crate::measures::sum_f64;
use crate::repeat_free::{detect_rotational_symmetry_free, RotationalMatch};
use crate::repeat_segment::segment_elements;
use crate::svd3::lstsq3;

/// `REPETITION_MIN_MEMBERS`.
pub const REPETITION_MIN_MEMBERS: usize = 3;
/// `REPETITION_SIMILARITY_TOL`.
pub const REPETITION_SIMILARITY_TOL: f64 = 0.30;
/// `DEFAULT_FEATURE_ANGLE_DEG`, used by `segment_elements`.
const FEATURE_ANGLE_DEG: f64 = 30.0;

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

/// `np.mean(a, axis=0)` for a list of 3-vectors: SEQUENTIAL over rows.
fn mean_2d(rows: &[[f64; 3]]) -> [f64; 3] {
    let mut acc = [0.0f64; 3];
    for r in rows {
        for k in 0..3 {
            acc[k] += r[k];
        }
    }
    let n = rows.len() as f64;
    if n > 0.0 {
        for k in 0..3 {
            acc[k] /= n;
        }
    }
    acc
}

/// `np.mean(x)` for a 1-D array: NumPy's pairwise reduction.
fn mean_1d(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return f64::NAN;
    }
    sum_f64(xs) / xs.len() as f64
}

/// `np.std(x)` = `sqrt(mean((x - mean)^2))`, pairwise like `np.mean`.
fn std_1d(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return f64::NAN;
    }
    let m = mean_1d(xs);
    let sq: Vec<f64> = xs.iter().map(|&x| (x - m) * (x - m)).collect();
    mean_1d(&sq).sqrt()
}

/// `_canonical_axis_sign`, shared with the free route.
fn canonical_axis_sign(ax: [f64; 3]) -> f64 {
    let mut dom = 0usize;
    for k in 1..3 {
        if ax[k].abs() > ax[dom].abs() {
            dom = k;
        }
    }
    if ax[dom] >= 0.0 {
        1.0
    } else {
        -1.0
    }
}

/// `np.round` (half to even on the binary value) — `format!` rounds correctly.
fn py_round(value: f64, decimals: i32) -> f64 {
    if !value.is_finite() || decimals < 0 {
        return value;
    }
    format!("{:.*}", decimals as usize, value)
        .parse::<f64>()
        .unwrap_or(value)
}

/// The patch route's result.
#[derive(Debug, Clone)]
pub struct PatchDetection {
    pub score: f64,
    pub pattern_type: String,
    /// Patch indices (into the full patch list) that formed the winning cluster.
    pub members: Vec<usize>,
    /// `order` for rotational patterns.
    pub order: Option<usize>,
    pub center: Option<[f64; 3]>,
    pub axis: Option<[f64; 3]>,
    pub u_axis: Option<[f64; 3]>,
    pub v_axis: Option<[f64; 3]>,
    /// Third grid axis, only for a 3-D lattice.
    pub w_axis: Option<[f64; 3]>,
    pub radius: Option<f64>,
    pub step_angle: Option<f64>,
    pub step_h: Option<f64>,
    pub k_indices: Option<Vec<i64>>,
    /// `int(np.min(k_fit))` / `int(np.max(k_fit))` for helical patterns.
    pub k_min: Option<i64>,
    pub k_max: Option<i64>,
    pub origin: Option<[f64; 3]>,
    pub step_u: Option<f64>,
    pub step_v: Option<f64>,
    pub step_w: Option<f64>,
    pub p_indices: Option<Vec<i64>>,
    pub q_indices: Option<Vec<i64>>,
    pub r_indices: Option<Vec<i64>>,
    /// Reason when the route returned early with score 0.
    pub reason: Option<String>,
}

impl PatchDetection {
    fn early(reason: impl Into<String>) -> Self {
        Self {
            score: 0.0,
            pattern_type: String::new(),
            members: Vec::new(),
            order: None,
            center: None,
            axis: None,
            u_axis: None,
            v_axis: None,
            w_axis: None,
            radius: None,
            step_angle: None,
            step_h: None,
            k_indices: None,
            k_min: None,
            k_max: None,
            origin: None,
            step_u: None,
            step_v: None,
            step_w: None,
            p_indices: None,
            q_indices: None,
            r_indices: None,
            reason: Some(reason.into()),
        }
    }
}

/// `_find_step` — the dominant spacing along one coordinate.
fn find_step(coords: &[f64]) -> f64 {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for &c in coords {
        lo = lo.min(c);
        hi = hi.max(c);
    }
    let span = hi - lo;
    if span <= 1e-4 {
        return 0.0;
    }
    let mut sorted = coords.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let diffs: Vec<f64> = sorted.windows(2).map(|w| w[1] - w[0]).collect();
    let gaps: Vec<f64> = diffs.iter().copied().filter(|&d| d > 0.08 * span).collect();
    if !gaps.is_empty() {
        let mut g = gaps.clone();
        g.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        return median_sorted(&g);
    }
    let nz: Vec<f64> = diffs.iter().copied().filter(|&d| d > 1e-2).collect();
    if nz.is_empty() {
        0.0
    } else {
        nz.iter().copied().fold(f64::INFINITY, f64::min)
    }
}

/// `np.median` of an already-sorted slice.
fn median_sorted(sorted: &[f64]) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        0.5 * (sorted[n / 2 - 1] + sorted[n / 2])
    }
}

fn median(xs: &[f64]) -> f64 {
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    median_sorted(&v)
}

/// `_detect_repetition_patches`.
pub fn detect_repetition_patches(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> PatchDetection {
    let patches = segment_elements(verts, tris, FEATURE_ANGLE_DEG);
    if patches.len() < REPETITION_MIN_MEMBERS {
        return PatchDetection::early(format!(
            "Too few segmented patches ({})",
            patches.len()
        ));
    }
    let n_p = patches.len();

    // 1. Cluster patches by (area, diag) similarity, in i/j order.
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for i in 0..n_p {
        for j in (i + 1)..n_p {
            let a1 = patches[i].area;
            let a2 = patches[j].area;
            let area_diff = (a1 - a2).abs() / a1.max(a2).max(1e-12);
            let d1 = patches[i].diag;
            let d2 = patches[j].diag;
            let diag_diff = (d1 - d2).abs() / d1.max(d2).max(1e-12);
            if area_diff < REPETITION_SIMILARITY_TOL && diag_diff < REPETITION_SIMILARITY_TOL {
                pairs.push((i, j));
            }
        }
    }
    if pairs.is_empty() {
        return PatchDetection::early("No similar patch pairs found");
    }
    let (n_clusters, cluster_labels) = connected_components(n_p, &pairs);
    let mut counts = vec![0usize; n_clusters];
    for &lab in &cluster_labels {
        counts[lab] += 1;
    }
    // `np.argmax` -> first maximum.
    let mut best_c = 0usize;
    for i in 1..counts.len() {
        if counts[i] > counts[best_c] {
            best_c = i;
        }
    }
    if counts[best_c] < REPETITION_MIN_MEMBERS {
        return PatchDetection::early(format!(
            "Largest cluster has only {} members",
            counts[best_c]
        ));
    }
    let member_idx: Vec<usize> = (0..n_p).filter(|&i| cluster_labels[i] == best_c).collect();
    let centroids: Vec<[f64; 3]> = member_idx.iter().map(|&i| patches[i].centroid).collect();

    // 2. Rotational / helical arrangement about a cylinder.
    let c_rough = mean_2d(&centroids);
    let n_m = centroids.len() as f64;
    let mut cov_p = [[0.0f64; 3]; 3];
    for c in &centroids {
        let d = [c[0] - c_rough[0], c[1] - c_rough[1], c[2] - c_rough[2]];
        for i in 0..3 {
            for j in 0..3 {
                cov_p[i][j] += d[i] * d[j];
            }
        }
    }
    for i in 0..3 {
        for j in 0..3 {
            cov_p[i][j] /= n_m.max(1.0);
        }
    }
    let (_w_p, v_p) = eigh3(cov_p);

    // Patch normals: the summed (unnormalised) face crosses, normalised.
    let mut patch_normals: Vec<[f64; 3]> = Vec::with_capacity(member_idx.len());
    for &pi in &member_idx {
        let mut sum = [0.0f64; 3];
        for &fi in &patches[pi].faces {
            let f = tris[fi];
            let (v0, v1, v2) = (verts[f[0]], verts[f[1]], verts[f[2]]);
            let a = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
            let b = [v2[0] - v0[0], v2[1] - v0[1], v2[2] - v0[2]];
            let cr = cross(a, b);
            sum[0] += cr[0];
            sum[1] += cr[1];
            sum[2] += cr[2];
        }
        let denom = norm(sum) + 1e-12;
        patch_normals.push([sum[0] / denom, sum[1] / denom, sum[2] / denom]);
    }
    let mut cov_n = [[0.0f64; 3]; 3];
    for nv in &patch_normals {
        for i in 0..3 {
            for j in 0..3 {
                cov_n[i][j] += nv[i] * nv[j];
            }
        }
    }
    let (_w_n, v_n) = eigh3(cov_n);

    let column = |v: &[[f64; 3]; 3], c: usize| [v[0][c], v[1][c], v[2][c]];
    let cand_axes = [
        column(&v_n, 0),
        column(&v_p, 0),
        column(&v_p, 1),
        column(&v_p, 2),
    ];

    let mut best_axis: Option<[f64; 3]> = None;
    let mut best_rel_std = 1.0f64;
    let mut best_basis: Option<([f64; 3], [f64; 3])> = None;
    let mut best_circle: Option<(f64, f64, f64)> = None;

    for ax0 in cand_axes {
        let norm_ax = norm(ax0);
        if norm_ax < 1e-6 {
            continue;
        }
        let mut ax = [ax0[0] / norm_ax, ax0[1] / norm_ax, ax0[2] / norm_ax];
        // Canonical axis before the basis is built: u and v are cross products
        // of ax, so a negated ax reflects the 2-D frame and moves arctan2.
        let sgn = canonical_axis_sign(ax);
        ax = [ax[0] * sgn, ax[1] * sgn, ax[2] * sgn];
        let mut u = if ax[0].abs() < 0.9 {
            cross(ax, [1.0, 0.0, 0.0])
        } else {
            cross(ax, [0.0, 1.0, 0.0])
        };
        let un = norm(u);
        if un == 0.0 {
            continue;
        }
        u = [u[0] / un, u[1] / un, u[2] / un];
        let v_vec = cross(ax, u);

        let x2d: Vec<f64> = centroids
            .iter()
            .map(|c| dot([c[0] - c_rough[0], c[1] - c_rough[1], c[2] - c_rough[2]], u))
            .collect();
        let y2d: Vec<f64> = centroids
            .iter()
            .map(|c| dot([c[0] - c_rough[0], c[1] - c_rough[1], c[2] - c_rough[2]], v_vec))
            .collect();

        // Algebraic circle fit: A = [2x, 2y, 1], b = x^2 + y^2.
        let rows: Vec<[f64; 3]> = (0..x2d.len())
            .map(|i| [2.0 * x2d[i], 2.0 * y2d[i], 1.0])
            .collect();
        let rhs: Vec<f64> = (0..x2d.len()).map(|i| x2d[i] * x2d[i] + y2d[i] * y2d[i]).collect();
        let sol = lstsq3(&rows, &rhs);
        let (cx, cy) = (sol[0], sol[1]);
        let r_sq = sol[2] + cx * cx + cy * cy;
        if r_sq > 0.0 {
            let fit_r = r_sq.sqrt();
            let radial: Vec<f64> = (0..x2d.len())
                .map(|i| ((x2d[i] - cx).powi(2) + (y2d[i] - cy).powi(2)).sqrt())
                .collect();
            let rel_std = std_1d(&radial) / fit_r;
            if rel_std < best_rel_std {
                best_rel_std = rel_std;
                best_axis = Some(ax);
                best_basis = Some((u, v_vec));
                best_circle = Some((cx, cy, fit_r));
            }
        }
    }

    // centroids_span = |ptp(centroids, axis=0)|
    let (mut clo, mut chi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for c in &centroids {
        for k in 0..3 {
            clo[k] = clo[k].min(c[k]);
            chi[k] = chi[k].max(c[k]);
        }
    }
    let centroids_span = norm([chi[0] - clo[0], chi[1] - clo[1], chi[2] - clo[2]]);

    if let (Some(best_axis), Some(basis), Some(circle)) = (best_axis, best_basis, best_circle) {
        let (u, v_vec) = basis;
        let (cx, cy, fit_r) = circle;
        if fit_r > 1e-3
            && best_rel_std < 0.08
            && fit_r <= 2.5 * if centroids_span > 1e-4 { centroids_span } else { 1.0 }
        {
            let c_circle = [
                c_rough[0] + cx * u[0] + cy * v_vec[0],
                c_rough[1] + cx * u[1] + cy * v_vec[1],
                c_rough[2] + cx * u[2] + cy * v_vec[2],
            ];
            let z_coords: Vec<f64> = centroids
                .iter()
                .map(|c| dot([c[0] - c_circle[0], c[1] - c_circle[1], c[2] - c_circle[2]], best_axis))
                .collect();
            let x_rel: Vec<f64> = centroids
                .iter()
                .map(|c| dot([c[0] - c_circle[0], c[1] - c_circle[1], c[2] - c_circle[2]], u))
                .collect();
            let y_rel: Vec<f64> = centroids
                .iter()
                .map(|c| dot([c[0] - c_circle[0], c[1] - c_circle[1], c[2] - c_circle[2]], v_vec))
                .collect();
            let two_pi = 2.0 * std::f64::consts::PI;
            let theta_vals: Vec<f64> = (0..x_rel.len())
                .map(|i| y_rel[i].atan2(x_rel[i]).rem_euclid(two_pi))
                .collect();

            let z_max = z_coords.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let z_min = z_coords.iter().copied().fold(f64::INFINITY, f64::min);
            let z_span = z_max - z_min;
            let z_std = std_1d(&z_coords);

            // 2a. Pure rotational pattern (planar circular array).
            if z_span < 0.05 * fit_r || z_std < 0.02 * fit_r {
                let th0 = theta_vals[0];
                let mut best_n: Option<usize> = None;
                for n_cand in 3..=32usize {
                    let step = two_pi / n_cand as f64;
                    let mut worst = 0.0f64;
                    for &t in &theta_vals {
                        let d = (t - th0).rem_euclid(step);
                        let d = d.min(step - d);
                        worst = worst.max(d);
                    }
                    if worst / step < 0.05 {
                        best_n = Some(n_cand);
                        break;
                    }
                }
                if let Some(best_n) = best_n {
                    let step = two_pi / best_n as f64;
                    let diffs: Vec<f64> = theta_vals
                        .iter()
                        .map(|&t| {
                            let d = (t - th0).rem_euclid(step);
                            d.min(step - d)
                        })
                        .collect();
                    let score = (1.0 - mean_1d(&diffs) / step).clamp(0.0, 1.0);
                    return PatchDetection {
                        score,
                        pattern_type: "rotational".into(),
                        members: member_idx,
                        order: Some(best_n),
                        center: Some(c_circle),
                        axis: Some(best_axis),
                        u_axis: Some(u),
                        v_axis: Some(v_vec),
                        w_axis: None,
                        radius: Some(fit_r),
                        step_angle: Some(two_pi / best_n as f64),
                        step_h: None,
                        k_indices: None,
                        k_min: None,
                        k_max: None,
                        origin: None,
                        step_u: None,
                        step_v: None,
                        step_w: None,
                        p_indices: None,
                        q_indices: None,
                        r_indices: None,
                        reason: None,
                    };
                }
            }

            // 2b. Helical (screw) pattern.
            if z_span >= 0.05 * fit_r && z_std >= 0.02 * fit_r {
                let mut order_idx: Vec<usize> = (0..z_coords.len()).collect();
                order_idx.sort_by(|&a, &b| {
                    z_coords[a]
                        .partial_cmp(&z_coords[b])
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                let z_sorted: Vec<f64> = order_idx.iter().map(|&i| z_coords[i]).collect();
                let th_sorted: Vec<f64> = order_idx.iter().map(|&i| theta_vals[i]).collect();
                let dz: Vec<f64> = z_sorted.windows(2).map(|w| w[1] - w[0]).collect();
                let nz_dz: Vec<f64> = dz.iter().copied().filter(|&d| d > 1e-3 * fit_r).collect();
                let mut k_fit: Option<Vec<i64>> = None;
                let mut h_step = 0.0f64;
                if !nz_dz.is_empty() {
                    let h_min = nz_dz.iter().copied().fold(f64::INFINITY, f64::min);
                    let ratios: Vec<f64> = dz.iter().map(|&d| d / h_min).collect();
                    let rounded: Vec<f64> = ratios.iter().map(|r| py_round(*r, 0)).collect();
                    let all_close = ratios
                        .iter()
                        .zip(rounded.iter())
                        .all(|(r, rd)| (r - rd).abs() < 0.20);
                    let any_positive = rounded.iter().any(|&r| r > 0.0);
                    if all_close && any_positive {
                        // h_step = median(dz[nz_r] / round(ratios)[nz_r])
                        let terms: Vec<f64> = dz
                            .iter()
                            .zip(rounded.iter())
                            .filter(|(_, &rd)| rd > 0.0)
                            .map(|(&d, &rd)| d / rd)
                            .collect();
                        h_step = median(&terms);
                        if h_step > 1e-4 {
                            let z0 = z_sorted[0];
                            let ks: Vec<i64> = z_sorted
                                .iter()
                                .map(|&z| py_round((z - z0) / h_step, 0) as i64)
                                .collect();
                            k_fit = Some(ks);
                        }
                    }
                }
                if let Some(ks) = k_fit {
                    let z0 = z_sorted[0];
                    let err_z: Vec<f64> = z_sorted
                        .iter()
                        .zip(ks.iter())
                        .map(|(&z, &k)| ((z - z0) - k as f64 * h_step).abs() / h_step)
                        .collect();
                    let dth: Vec<f64> = th_sorted.windows(2).map(|w| w[1] - w[0]).collect();
                    let dth_wrap: Vec<f64> = dth
                        .iter()
                        .map(|&d| (d + std::f64::consts::PI).rem_euclid(two_pi) - std::f64::consts::PI)
                        .collect();
                    let dk: Vec<i64> = ks.windows(2).map(|w| w[1] - w[0]).collect();
                    let valid_terms: Vec<f64> = dth_wrap
                        .iter()
                        .zip(dk.iter())
                        .filter(|(_, &d)| d >= 1)
                        .map(|(&dthv, &dkv)| dthv / dkv as f64)
                        .collect();
                    if !valid_terms.is_empty() {
                        let th_step = median(&valid_terms);
                        let th_pred: Vec<f64> = ks.iter().map(|&k| th_sorted[0] + k as f64 * th_step).collect();
                        let err_th: Vec<f64> = th_sorted
                            .iter()
                            .zip(th_pred.iter())
                            .map(|(&t, &p)| ((t - p + std::f64::consts::PI).rem_euclid(two_pi) - std::f64::consts::PI).abs())
                            .collect();
                        let max_err_th = err_th.iter().copied().fold(0.0f64, f64::max);
                        let max_err_z = err_z.iter().copied().fold(0.0f64, f64::max);
                        if max_err_th < 0.15 && max_err_z < 0.15 {
                            let score = (1.0
                                - (mean_1d(&err_th) / 0.15 + mean_1d(&err_z) / 0.15) / 2.0)
                                .clamp(0.0, 1.0);
                            let mut k_members = vec![0i64; z_coords.len()];
                            for (slot, &src) in order_idx.iter().enumerate() {
                                k_members[src] = ks[slot];
                            }
                            let k_min = ks.iter().copied().min().unwrap_or(0);
                            let k_max = ks.iter().copied().max().unwrap_or(0);
                            return PatchDetection {
                                score,
                                pattern_type: "helical".into(),
                                members: member_idx,
                                order: None,
                                center: Some(c_circle),
                                axis: Some(best_axis),
                                u_axis: Some(u),
                                v_axis: Some(v_vec),
                                w_axis: None,
                                radius: Some(fit_r),
                                step_angle: Some(th_step),
                                step_h: Some(h_step),
                                k_indices: Some(k_members),
                        k_min: Some(k_min),
                        k_max: Some(k_max),
                                origin: None,
                                step_u: None,
                                step_v: None,
                                step_w: None,
                                p_indices: None,
                                q_indices: None,
                                r_indices: None,
                                reason: None,
                            };
                        }
                    }
                }
            }
        }
    }

    // 3. Translational / grid pattern.
    let ax1 = column(&v_p, 2);
    let ax2 = column(&v_p, 1);
    let ax3 = column(&v_p, 0);
    let proj = |ax: [f64; 3]| -> Vec<f64> {
        centroids
            .iter()
            .map(|c| dot([c[0] - c_rough[0], c[1] - c_rough[1], c[2] - c_rough[2]], ax))
            .collect()
    };
    let u_coords = proj(ax1);
    let v_coords = proj(ax2);
    let w_coords = proj(ax3);

    let step_u = find_step(&u_coords);
    let step_v = find_step(&v_coords);
    let step_w = find_step(&w_coords);

    if step_u > 1e-2 {
        let umin = u_coords.iter().copied().fold(f64::INFINITY, f64::min);
        let p_idx: Vec<i64> = u_coords
            .iter()
            .map(|&u| py_round((u - umin) / step_u, 0) as i64)
            .collect();
        let q_idx: Vec<i64> = if step_v > 1e-2 {
            let vmin = v_coords.iter().copied().fold(f64::INFINITY, f64::min);
            v_coords
                .iter()
                .map(|&v| py_round((v - vmin) / step_v, 0) as i64)
                .collect()
        } else {
            vec![0; p_idx.len()]
        };
        let p_max = p_idx.iter().copied().max().unwrap_or(0);
        let q_max = q_idx.iter().copied().max().unwrap_or(0);
        let min_planar_step = if step_v > 1e-2 {
            step_u.min(step_v)
        } else {
            step_u
        };

        // 3D lattice grid.
        if step_w >= 0.2 * min_planar_step {
            let wmin = w_coords.iter().copied().fold(f64::INFINITY, f64::min);
            let n_unique = {
                let mut v: Vec<i64> = w_coords
                    .iter()
                    .map(|&w| py_round((w - wmin) / step_w, 0) as i64)
                    .collect();
                v.sort_unstable();
                v.dedup();
                v.len()
            };
            if n_unique > 1 {
                let r_idx: Vec<i64> = w_coords
                    .iter()
                    .map(|&w| py_round((w - wmin) / step_w, 0) as i64)
                    .collect();
                let r_max = r_idx.iter().copied().max().unwrap_or(0);
                if (p_max + 1) * (q_max + 1) * (r_max + 1) <= 128 {
                    return PatchDetection {
                        score: 0.85,
                        pattern_type: "translational".into(),
                        members: member_idx,
                        order: None,
                        center: None,
                        axis: None,
                        u_axis: Some(ax1),
                        v_axis: Some(ax2),
                        w_axis: Some(ax3),
                        radius: None,
                        step_angle: None,
                        step_h: None,
                        k_indices: None,
                        k_min: None,
                        k_max: None,
                        origin: Some(c_rough),
                        step_u: Some(step_u),
                        step_v: Some(step_v),
                        step_w: Some(step_w),
                        p_indices: Some(p_idx),
                        q_indices: Some(q_idx),
                        r_indices: Some(r_idx),
                        reason: None,
                    };
                }
            }
        }

        if (p_max + 1) * (q_max + 1) <= 64 {
            return PatchDetection {
                score: 0.85,
                pattern_type: "translational".into(),
                members: member_idx,
                order: None,
                center: None,
                axis: None,
                u_axis: Some(ax1),
                v_axis: Some(ax2),
                w_axis: None,
                radius: None,
                step_angle: None,
                step_h: None,
                k_indices: None,
            k_min: None,
            k_max: None,
                origin: Some(c_rough),
                step_u: Some(step_u),
                step_v: Some(step_v),
                step_w: None,
                p_indices: Some(p_idx),
                q_indices: Some(q_idx),
                r_indices: None,
                reason: None,
            };
        }
    }

    PatchDetection {
        score: 0.3,
        pattern_type: "irregular".into(),
        members: member_idx,
        order: None,
        center: None,
        axis: None,
        u_axis: None,
        v_axis: None,
        w_axis: None,
        radius: None,
        step_angle: None,
        step_h: None,
        k_indices: None,
            k_min: None,
            k_max: None,
        origin: None,
        step_u: None,
        step_v: None,
        step_w: None,
        p_indices: None,
        q_indices: None,
        r_indices: None,
        reason: None,
    }
}

/// `detect_repetition`: run both routes and apply Python's selection rule.
///
/// Returns `(score, route)` where `route` names which side won, plus the info
/// for that side. The Python side rebuilds the info dict; only the numbers are
/// needed here.
pub fn detect_repetition(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
) -> (f64, &'static str, Option<RotationalMatch>, Option<PatchDetection>) {
    if verts.len() < 4 || tris.len() < 4 {
        return (0.0, "none", None, None);
    }
    let free = detect_rotational_symmetry_free(verts, tris, 3000, 64);
    let patch = detect_repetition_patches(verts, tris);
    let free_score = free.as_ref().map(|m| m.score).unwrap_or(0.0);
    let patch_score = patch.score;

    if patch_score >= 0.50 && patch_score >= free_score {
        return (patch_score, "patch", free, Some(patch));
    }
    if free_score >= 0.60 {
        return (free_score, "free", free, Some(patch));
    }
    if patch_score >= 0.50 {
        return (patch_score, "patch", free, Some(patch));
    }
    if free_score >= 0.50 {
        return (free_score, "free", free, Some(patch));
    }
    if free_score > patch_score && free_score > 0.0 {
        return (free_score, "free", free, Some(patch));
    }
    (patch_score, "patch", free, Some(patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_match_numpy_semantics() {
        // 2-D mean is sequential; a 3-element 1-D mean is the same either way.
        let rows = vec![[1.0, 2.0, 3.0], [3.0, 4.0, 5.0]];
        let m = mean_2d(&rows);
        assert!((m[0] - 2.0).abs() < 1e-15 && (m[1] - 3.0).abs() < 1e-15);
        assert!((mean_1d(&[1.0, 2.0, 3.0]) - 2.0).abs() < 1e-15);
    }

    #[test]
    fn std_matches_the_population_formula() {
        let xs = [1.0, 2.0, 3.0, 4.0];
        // population std of 1..4 is sqrt(1.25)
        assert!((std_1d(&xs) - 1.25f64.sqrt()).abs() < 1e-15);
    }

    #[test]
    fn find_step_returns_zero_for_a_flat_axis() {
        assert_eq!(find_step(&[2.0, 2.0, 2.0]), 0.0);
    }

    #[test]
    fn too_few_faces_is_none() {
        let verts = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let tris = vec![[0usize, 1, 2]];
        let (score, route, _, _) = detect_repetition(&verts, &tris);
        assert_eq!(score, 0.0);
        assert_eq!(route, "none");
    }
}
