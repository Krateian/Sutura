// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! `repeat.py::detect_rotational_symmetry_free` — the segmentation-free route.
//!
//! Samples the surface twice (dense cloud + query cloud), takes candidate axes
//! from the three coordinate axes and the dense cloud's PCA frame, and scores
//! each candidate order by how much of the rotated query cloud lands within
//! `tol` of the dense cloud. Candidates that pass the first gate get a
//! multi-step check before the winner is chosen.
//!
//! Reproduction notes, each of which changes numbers if done differently:
//!
//! * **Sample order.** The dense cloud is `sample_surface(..., n*2, seed=42)`
//!   and the query cloud `sample_surface(..., n, seed=123)`; both are
//!   bit-identical to NumPy through the shared sampler.
//! * **Both axis directions.** Rotating by `+theta` about `+ax` is a different
//!   point set than about `-ax`, and rotational symmetry belongs to the
//!   unoriented axis line. Every candidate is therefore evaluated in both
//!   directions with the canonical sign first, so ties break deterministically
//!   and the score cannot depend on which sign a solver produced.
//! * **`np.mean` for `c`.** The dense centroid and `move_d` use NumPy's pairwise
//!   reduction, so `sum_f64` is the right accumulator for those.
//! * **`rot = q0 @ R.T + c`.** NumPy computes this as a matrix product; the
//!   arithmetic below is the same sum of three products per component.

use crate::measures::sum_f64;
use crate::pointcloud::KdTree;
use crate::sample::sample_surface;
use crate::eigh3::eigh3;

/// A winning rotational symmetry.
#[derive(Debug, Clone)]
pub struct RotationalMatch {
    pub score: f64,
    pub order: usize,
    pub center: [f64; 3],
    pub axis: [f64; 3],
    pub u_axis: [f64; 3],
    pub v_axis: [f64; 3],
    pub step_angle: f64,
    pub match_fraction: f64,
    pub mean_distance: f64,
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

/// `_canonical_axis_sign`: +1 when the largest-magnitude component is positive.
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

/// `_rotation_matrix(angle, direction, point)[:3, :3]` — the Rodrigues part.
///
/// The Python helper normalises `direction` and returns a 4x4; the detector
/// only takes the rotation block, and the translation `p - R p` is applied
/// separately as `q0 @ R.T + c`, so this returns just `R`.
fn rotation_matrix(angle: f64, direction: [f64; 3]) -> [[f64; 3]; 3] {
    let n = norm(direction);
    if n < 1e-12 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    let x = direction[0] / n;
    let y = direction[1] / n;
    let z = direction[2] / n;
    let c = angle.cos();
    let s = angle.sin();
    let cc = 1.0 - c;
    [
        [c + x * x * cc, x * y * cc - z * s, x * z * cc + y * s],
        [y * x * cc + z * s, c + y * y * cc, y * z * cc - x * s],
        [z * x * cc - y * s, z * y * cc + x * s, c + z * z * cc],
    ]
}

/// `q0 @ R.T + c` for every query point.
///
/// The transpose is load-bearing: Python writes `q0 @ R.T`, so component `j` of
/// the result is `sum_i q0[i] * R[j][i]` -- that is `R[j][i]`, not `R[i][j]`.
/// Using `R[i][j]` computes `q0 @ R`, the inverse rotation about the same axis,
/// which scores differently and picked order 7 instead of 8 on a 1085-face
/// corpus mesh.
fn rotate_points(q0: &[[f64; 3]], r: &[[f64; 3]; 3], c: [f64; 3]) -> Vec<[f64; 3]> {
    q0.iter()
        .map(|p| {
            [
                p[0] * r[0][0] + p[1] * r[0][1] + p[2] * r[0][2] + c[0],
                p[0] * r[1][0] + p[1] * r[1][1] + p[2] * r[1][2] + c[1],
                p[0] * r[2][0] + p[1] * r[2][1] + p[2] * r[2][2] + c[2],
            ]
        })
        .collect()
}

struct Candidate {
    axis: [f64; 3],
    u: [f64; 3],
    v: [f64; 3],
    order: usize,
    theta: f64,
    rot: Vec<[f64; 3]>,
    move_d: f64,
}

/// `repeat.py::detect_rotational_symmetry_free`.
///
/// Returns `None` when no symmetry clears the 0.60 threshold (Python returns
/// `(0.0, {'pattern_type': 'none', ...})` in that case).
pub fn detect_rotational_symmetry_free(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    n_samples: usize,
    max_order: usize,
) -> Option<RotationalMatch> {
    if verts.len() < 4 || tris.len() < 4 {
        return None;
    }
    let (dense_pts, _) = sample_surface(verts, tris, n_samples * 2, 42);
    let (query_pts, _) = sample_surface(verts, tris, n_samples, 123);
    if dense_pts.is_empty() || query_pts.is_empty() {
        return None;
    }

    // `c = np.mean(dense_pts, axis=0)`.
    //
    // A 2-D reduction, and NumPy accumulates those SEQUENTIALLY over rows --
    // measured: `mean(axis=0)` equals a sequential row accumulation divided by
    // n, while the 8-accumulator pairwise form does not (n = 100 and 6000 both
    // checked). Using `sum_f64` here picked order 7 instead of 8 on a 1085-face
    // corpus mesh, because the centroid shifts every candidate distance.
    let mut c = [0.0f64; 3];
    for p in &dense_pts {
        for k in 0..3 {
            c[k] += p[k];
        }
    }
    for k in 0..3 {
        c[k] /= dense_pts.len() as f64;
    }

    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in &dense_pts {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let span = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let diag = norm(span);
    if diag <= 1e-12 {
        return None;
    }
    let tol = 0.015 * diag;

    // PCA of the dense cloud.
    let n_dense = dense_pts.len() as f64;
    let mut cov = [[0.0f64; 3]; 3];
    for p in &dense_pts {
        let d = [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
        for i in 0..3 {
            for j in 0..3 {
                cov[i][j] += d[i] * d[j];
            }
        }
    }
    for i in 0..3 {
        for j in 0..3 {
            cov[i][j] /= n_dense.max(1.0);
        }
    }
    let (_, eigvecs) = eigh3(cov);

    let tree = match KdTree::new(&dense_pts) {
        Some(t) => t,
        None => return None,
    };

    let mut candidate_orders: Vec<usize> = (2..=max_order).collect();
    candidate_orders.extend([72usize, 80, 90, 100, 120]);

    let mut candidate_axes: Vec<[f64; 3]> = vec![
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
    ];
    for i in 0..3 {
        candidate_axes.push([eigvecs[0][i], eigvecs[1][i], eigvecs[2][i]]);
    }

    let q0: Vec<[f64; 3]> = query_pts
        .iter()
        .map(|p| [p[0] - c[0], p[1] - c[1], p[2] - c[2]])
        .collect();

    let mut candidates: Vec<Candidate> = Vec::new();
    for ax in &candidate_axes {
        let norm_ax = norm(*ax);
        if norm_ax < 1e-6 {
            continue;
        }
        let ax = [ax[0] / norm_ax, ax[1] / norm_ax, ax[2] / norm_ax];

        let mut u = if ax[0].abs() < 0.9 {
            cross(ax, [1.0, 0.0, 0.0])
        } else {
            cross(ax, [0.0, 1.0, 0.0])
        };
        let un = norm(u) + 1e-12;
        u = [u[0] / un, u[1] / un, u[2] / un];
        let mut v = cross(ax, u);
        let vn = norm(v) + 1e-12;
        v = [v[0] / vn, v[1] / vn, v[2] / vn];

        // Canonical direction first so ties resolve in a fixed order.
        let sign = canonical_axis_sign(ax);
        for direction in [sign, -sign] {
            let ax_d = [ax[0] * direction, ax[1] * direction, ax[2] * direction];
            let u_d = [u[0] * direction, u[1] * direction, u[2] * direction];
            let v_d = [v[0] * direction, v[1] * direction, v[2] * direction];
            for &n in &candidate_orders {
                let theta = 2.0 * std::f64::consts::PI / n as f64;
                let r = rotation_matrix(theta, ax_d);
                let rot = rotate_points(&q0, &r, c);
                // move_d = mean(|rot - query_pts|) -- pairwise per the reduction
                // NumPy uses for `np.linalg.norm(..., axis=1)` then `.mean()`.
                let dists: Vec<f64> = rot
                    .iter()
                    .zip(query_pts.iter())
                    .map(|(a, b)| {
                        let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
                        norm(d)
                    })
                    .collect();
                let move_d = sum_f64(&dists) / dists.len() as f64;
                candidates.push(Candidate {
                    axis: ax_d,
                    u: u_d,
                    v: v_d,
                    order: n,
                    theta,
                    rot,
                    move_d,
                });
            }
        }
    }

    // One batched nearest-neighbour pass over every candidate's rotated cloud.
    let mut all_dists: Vec<Vec<f64>> = Vec::with_capacity(candidates.len());
    if !candidates.is_empty() {
        let mut flat: Vec<[f64; 3]> = Vec::with_capacity(candidates.len() * q0.len());
        for cand in &candidates {
            flat.extend_from_slice(&cand.rot);
        }
        let (dists, _idx, _k) = tree.query(&flat, 1);
        let per = q0.len();
        for i in 0..candidates.len() {
            all_dists.push(dists[i * per..(i + 1) * per].to_vec());
        }
    }

    let mut first: Vec<(f64, f64, f64)> = Vec::with_capacity(candidates.len());
    let mut step_jobs: Vec<(usize, Vec<Vec<[f64; 3]>>)> = Vec::new();
    for (i, cand) in candidates.iter().enumerate() {
        let dists = &all_dists[i];
        let below = dists.iter().filter(|&&d| d < tol).count();
        let frac = below as f64 / dists.len() as f64;
        let mean_d = sum_f64(dists) / dists.len() as f64;
        let rel_dev = mean_d / (cand.move_d + 1e-6);
        first.push((frac, mean_d, rel_dev));

        if frac >= 0.70 && (rel_dev < 0.30 || (cand.order >= 40 && rel_dev < 0.65)) {
            let mut steps = vec![2usize];
            if cand.order >= 4 {
                steps.push(cand.order / 2);
            }
            let mut rots = Vec::new();
            for &s_k in &steps {
                let r = rotation_matrix(s_k as f64 * cand.theta, cand.axis);
                rots.push(rotate_points(&q0, &r, c));
            }
            step_jobs.push((i, rots));
        }
    }

    // Second batched pass for the multi-step verification.
    let mut step_fracs: std::collections::HashMap<usize, Vec<f64>> =
        std::collections::HashMap::new();
    if !step_jobs.is_empty() {
        let mut flat: Vec<[f64; 3]> = Vec::new();
        for (_i, rots) in &step_jobs {
            for r in rots {
                flat.extend_from_slice(r);
            }
        }
        let (dists, _idx, _k) = tree.query(&flat, 1);
        let per = q0.len();
        let mut k = 0usize;
        for (i, rots) in &step_jobs {
            let mut fracs = Vec::with_capacity(rots.len());
            for j in 0..rots.len() {
                let slice = &dists[(k + j) * per..(k + j + 1) * per];
                let below = slice.iter().filter(|&&d| d < tol).count();
                fracs.push(below as f64 / slice.len() as f64);
            }
            k += rots.len();
            step_fracs.insert(*i, fracs);
        }
    }

    let mut best: Option<RotationalMatch> = None;
    let mut best_score = 0.0f64;
    for (i, cand) in candidates.iter().enumerate() {
        let fracs = match step_fracs.get(&i) {
            Some(f) => f,
            None => continue,
        };
        if !fracs.iter().all(|&sf| sf >= 0.55) {
            continue;
        }
        let (frac, mean_d, _rel_dev) = first[i];
        let sum_steps: f64 = fracs.iter().sum();
        let avg_frac = (frac + sum_steps) / (1.0 + fracs.len() as f64);
        let score = (avg_frac * (1.0 - (mean_d / tol).min(1.0) * 0.4)).clamp(0.0, 1.0);

        let mut is_better = score > best_score;
        if let Some(prev) = &best {
            if !is_better {
                let prev_n = prev.order;
                if cand.order > prev_n
                    && cand.order % prev_n == 0
                    && (cand.order / prev_n) <= 4
                    && score >= best_score * 0.90
                {
                    is_better = true;
                }
            }
        }
        if is_better {
            best_score = score;
            best = Some(RotationalMatch {
                score,
                order: cand.order,
                center: c,
                axis: cand.axis,
                u_axis: cand.u,
                v_axis: cand.v,
                step_angle: cand.theta,
                match_fraction: frac,
                mean_distance: mean_d,
            });
        }
    }

    if best_score >= 0.60 {
        best
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn too_few_faces_returns_none() {
        let verts = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let tris = vec![[0usize, 1, 2]];
        assert!(detect_rotational_symmetry_free(&verts, &tris, 100, 8).is_none());
    }

    #[test]
    fn rotation_matrix_is_orthonormal_with_unit_determinant() {
        let r = rotation_matrix(0.7, [0.0, 0.0, 1.0]);
        let det = r[0][0] * (r[1][1] * r[2][2] - r[1][2] * r[2][1])
            - r[0][1] * (r[1][0] * r[2][2] - r[1][2] * r[2][0])
            + r[0][2] * (r[1][0] * r[2][1] - r[1][1] * r[2][0]);
        assert!((det - 1.0).abs() < 1e-12, "det {det}");
        // A quarter turn about z maps x -> y.
        let r = rotation_matrix(std::f64::consts::FRAC_PI_2, [0.0, 0.0, 1.0]);
        assert!((r[0][0]).abs() < 1e-12);
        assert!((r[1][0] - 1.0).abs() < 1e-12, "x -> y: {}", r[1][0]);
    }

    #[test]
    fn canonical_axis_sign_uses_the_largest_component() {
        assert_eq!(canonical_axis_sign([0.1, -0.9, 0.2]), -1.0);
        assert_eq!(canonical_axis_sign([-0.1, 0.9, 0.2]), 1.0);
        assert_eq!(canonical_axis_sign([1.0, 0.0, 0.0]), 1.0);
    }

    /// `rotate_points` must implement `q0 @ R.T + c`, not `q0 @ R`.
    ///
    /// Regression guard: the transpose was missing, which applied the inverse
    /// rotation and picked order 7 instead of 8 on a 1085-face corpus mesh while
    /// every other mesh still matched by luck.
    #[test]
    fn rotate_points_applies_the_transpose() {
        // The two differ for a non-symmetric R, so a 90-degree turn about z
        // (whose transpose is the opposite turn) is a clean probe.
        let r = rotation_matrix(std::f64::consts::FRAC_PI_2, [0.0, 0.0, 1.0]);
        let c = [0.0, 0.0, 0.0];
        // R maps x -> y, so q0 @ R.T maps the x unit vector to -y.
        let out = rotate_points(&[[1.0, 0.0, 0.0]], &r, c);
        assert!(
            out[0][1].abs() > 0.9,
            "expected an in-plane turn, got {:?}",
            out[0]
        );
        // Explicitly: component j = sum_i q0[i] * R[j][i].
        let q0 = [0.3, -0.7, 0.2];
        let got = rotate_points(&[q0], &r, [1.0, 2.0, 3.0])[0];
        let want = [
            q0[0] * r[0][0] + q0[1] * r[0][1] + q0[2] * r[0][2] + 1.0,
            q0[0] * r[1][0] + q0[1] * r[1][1] + q0[2] * r[1][2] + 2.0,
            q0[0] * r[2][0] + q0[1] * r[2][1] + q0[2] * r[2][2] + 3.0,
        ];
        for k in 0..3 {
            assert!((got[k] - want[k]).abs() < 1e-15, "component {k}");
        }
        // And it must NOT equal q0 @ R.
        let wrong = [
            q0[0] * r[0][0] + q0[1] * r[1][0] + q0[2] * r[2][0] + 1.0,
            q0[0] * r[0][1] + q0[1] * r[1][1] + q0[2] * r[2][1] + 2.0,
            q0[0] * r[0][2] + q0[1] * r[1][2] + q0[2] * r[2][2] + 3.0,
        ];
        assert!(
            (got[0] - wrong[0]).abs() + (got[1] - wrong[1]).abs() > 1e-3,
            "rotate_points looks like q0 @ R"
        );
    }
}
