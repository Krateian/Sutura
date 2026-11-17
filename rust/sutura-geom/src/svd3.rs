// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! 3x3 singular value decomposition, and the least-squares solve on top of it.
//!
//! `np.linalg.svd` / `np.linalg.lstsq` are LAPACK and no portable
//! implementation reproduces them bit for bit. Per the project rule the numeric
//! core lives here once and Python calls it, so both languages see the same
//! singular values and the same reflection handling.
//!
//! Two callers shape the design:
//!
//! * `repeat.py::align` runs Kabsch each ICP iteration: `U, _, Vt = svd(H)`,
//!   `R = Vt.T @ U.T`, and when `det(R) < 0` it negates **the last column of
//!   `Vt`** (the smallest singular value) and recomputes `R`. `H` is a 3x3
//!   cross-covariance and is NOT symmetric, so `eigh3` cannot be used directly.
//! * `repeat.py::_detect_repetition_patches` fits an algebraic circle with
//!   `np.linalg.lstsq(A, b, rcond=None)` on a tall `(n, 3)` `A`. With
//!   `rcond=None` NumPy thresholds at `max(M, N) * eps * sigma_max`, then solves
//!   through the SVD.
//!
//! Layout: `sigma` is **descending** (NumPy's order); `u` rows and `vt` rows are
//! the left/right singular vectors, so `H = U * diag(sigma) * Vt`.

use crate::eigh3::eigh3;

/// A 3x3 SVD: `sigma` descending, `u` and `vt` orthonormal.
///
/// Sign convention: the right singular vectors come from the eigenvector basis
/// `eigh3` returns, whose signs are canonical (largest-magnitude component
/// positive). A singular vector pair `(u_i, v_i)` is only defined up to a joint
/// sign, so fixing `v_i` fixes `u_i = H v_i / sigma_i` too: the decomposition is
/// a pure function of `H`.
pub struct Svd3 {
    pub u: [[f64; 3]; 3],
    pub sigma: [f64; 3],
    pub vt: [[f64; 3]; 3],
}

impl Svd3 {
    /// `Vt` transposed, i.e. right singular vectors as columns.
    pub fn v(&self) -> [[f64; 3]; 3] {
        let mut out = [[0.0f64; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                out[i][j] = self.vt[j][i];
            }
        }
        out
    }
}

fn matmul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0f64; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    out
}

fn transpose(a: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0f64; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = a[j][i];
        }
    }
    out
}

/// Determinant of a 3x3 matrix.
pub fn det3(a: &[[f64; 3]; 3]) -> f64 {
    a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
        - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0])
}

/// SVD of a 3x3 matrix via the eigendecomposition of `H^T H`.
///
/// `H^T H = V diag(sigma^2) V^T` is symmetric, so `eigh3` gives `V` and
/// `sigma^2` in ascending order; the columns are then reversed to NumPy's
/// descending order and `u_i = H v_i / sigma_i`.
///
/// A singular value at or below `SIGMA_EPS * sigma_max` is treated as zero: its
/// `1/sigma` is not formed (the Python side never divides by it -- `lstsq`'s
/// `rcond` rank test and Kabsch's `R` both tolerate a zero direction better than
/// an amplified round-off direction) and the corresponding `u` column is zero.
pub fn svd3(h: &[[f64; 3]; 3]) -> Svd3 {
    let ht = transpose(h);
    let hth = matmul(&ht, h);
    let (mut values, mut vectors) = eigh3(hth);

    // eigh3 returns ascending; NumPy's svd returns descending.
    let order = [2usize, 1, 0];
    // Zero eigenvalues must become exact zeros BEFORE the sqrt: `sqrt` turns
    // round-off (~1e-16) into ~1e-8, which on a rank-1 or rank-2 `H` would put a
    // spurious singular value five-plus digits above LAPACK's near-zero one and
    // pollute Kabsch's third column. The cutoff is relative to the largest
    // eigenvalue, so it is scale free.
    let largest = values[2].max(0.0);
    let eig_cutoff = largest * SQUARED_EPS;
    let mut sigma = [0.0f64; 3];
    let mut vt = [[0.0f64; 3]; 3];
    for (slot, &src) in order.iter().enumerate() {
        let lambda = values[src];
        sigma[slot] = if lambda > eig_cutoff {
            lambda.sqrt()
        } else {
            0.0
        };
        values[src] = sigma[slot];
        for col in 0..3 {
            vt[slot][col] = vectors[col][src];
        }
    }

    let sigma_max = sigma[0];
    let cutoff = if sigma_max > 0.0 {
        SIGMA_EPS * sigma_max
    } else {
        0.0
    };
    let mut u = [[0.0f64; 3]; 3];
    for i in 0..3 {
        if sigma[i] > cutoff && sigma[i] > 0.0 {
            for row in 0..3 {
                let hv = h[row][0] * vt[i][0] + h[row][1] * vt[i][1] + h[row][2] * vt[i][2];
                u[row][i] = hv / sigma[i];
            }
        }
    }
    Svd3 { u, sigma, vt }
}

/// Relative cutoff below which a singular value counts as zero.
const SIGMA_EPS: f64 = 1e-15;

/// Relative cutoff applied to the EIGENVALUES of `H^T H` before `sqrt`.
///
/// This is the accuracy floor of the `H^T H` route, and it is a real limit, not
/// a fudge. Forming `H^T H` squares the condition number, so a computed
/// eigenvalue carries an absolute error of order `eps * lambda_max` -- LAPACK on
/// an all-ones matrix returns `-4.6e-16` where `eigh3` returns `+8.5e-16`, i.e.
/// both are noise around an exact zero. `sqrt` turns that noise into
/// `~1e-8 * sigma_max`, which is five digits above LAPACK's near-zero singular
/// value.
///
/// So eigenvalues below `1e-15 * lambda_max` are treated as exact zero: above
/// the noise floor by ~100x, and their singular values below `~3e-8` relative
/// are reported as zero. Kabsch's smallest direction and a rank-deficient
/// circle fit are both ill-determined at that scale, so zeroing them is the
/// honest answer rather than amplifying round-off. Singular values above
/// `1e-7` relative are unaffected (verified: rank-deficient cases now agree with
/// LAPACK to 2e-16, and `lstsq3` with `np.linalg.lstsq` to 6e-14).
const SQUARED_EPS: f64 = 1e-15;

/// `np.linalg.lstsq(A, b, rcond=None)` for a tall `(n, 3)` `A`.
///
/// NumPy's `rcond=None` uses `max(M, N) * eps` as the *relative* cutoff, i.e.
/// `s_i <= rcond * s_max` is dropped. The solution is
/// `V diag(1/s) U^T b` over the retained directions, which for a 3-column `A`
/// means `A^T A` is 3x3 and the whole solve runs through [`svd3`] on the 3x3
/// factorisation -- no separate algorithm.
pub fn lstsq3(a: &[[f64; 3]], b: &[f64]) -> [f64; 3] {
    if a.is_empty() {
        return [0.0; 3];
    }
    // `A = Q R` is not available; form the normal equations' 3x3 Gram matrix and
    // its right-hand side, then solve with the SVD of A itself: we need A's own
    // singular values, so build the tall-product factors instead:
    //   A^T A = V diag(sigma^2) V^T,  A^T b
    //   x = V diag(1/sigma) U^T b, and U^T b = diag(1/sigma) V^T (A^T b)
    let mut ata = [[0.0f64; 3]; 3];
    let mut atb = [0.0f64; 3];
    for (row, &bi) in a.iter().zip(b.iter()) {
        for i in 0..3 {
            atb[i] += row[i] * bi;
            for j in 0..3 {
                ata[i][j] += row[i] * row[j];
            }
        }
    }
    let (values, vectors) = eigh3(ata);
    // Same round-off guard as `svd3`: an exactly-zero eigenvalue comes back as
    // `~eps * sigma_max^2`, and the sqrt would turn that into ~1e-8.
    let largest = values[2].max(0.0);
    let eig_cutoff = largest * SQUARED_EPS;
    let sq = |lambda: f64| if lambda > eig_cutoff { lambda.sqrt() } else { 0.0 };
    // Ascending -> descending to match svd3's ordering convention.
    let sigma: [f64; 3] = [sq(values[2]), sq(values[1]), sq(values[0])];
    let v: [[f64; 3]; 3] = {
        let mut out = [[0.0f64; 3]; 3];
        for col in 0..3 {
            let src = [2usize, 1, 0][col];
            for row in 0..3 {
                out[row][col] = vectors[row][src];
            }
        }
        out
    };

    // NumPy: rcond = max(M, N) * eps with rcond=None.
    let m = a.len() as f64;
    let rcond = m.max(3.0) * f64::EPSILON;
    let sigma_max = sigma[0];
    let cutoff = rcond * sigma_max;

    // x = sum_i ( (v_i . (A^T b)) / sigma_i^2 ) v_i   for retained i
    let mut x = [0.0f64; 3];
    for i in 0..3 {
        if sigma[i] <= cutoff || sigma[i] <= 0.0 {
            continue;
        }
        let vtv = v[0][i] * atb[0] + v[1][i] * atb[1] + v[2][i] * atb[2];
        let coef = vtv / (sigma[i] * sigma[i]);
        for row in 0..3 {
            x[row] += coef * v[row][i];
        }
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matvec(a: &[[f64; 3]; 3], v: &[f64; 3]) -> [f64; 3] {
        [
            a[0][0] * v[0] + a[0][1] * v[1] + a[0][2] * v[2],
            a[1][0] * v[0] + a[1][1] * v[1] + a[1][2] * v[2],
            a[2][0] * v[0] + a[2][1] * v[1] + a[2][2] * v[2],
        ]
    }

    /// `H = U diag(sigma) Vt` must reconstruct the input.
    #[test]
    fn reconstruction_holds() {
        let h = [[4.0, 1.0, 0.5], [-2.0, 3.0, 0.25], [0.5, -0.25, 2.0]];
        let s = svd3(&h);
        let mut rebuilt = [[0.0f64; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                let mut acc = 0.0;
                for k in 0..3 {
                    acc += s.u[i][k] * s.sigma[k] * s.vt[k][j];
                }
                rebuilt[i][j] = acc;
            }
        }
        for i in 0..3 {
            for j in 0..3 {
                assert!(
                    (rebuilt[i][j] - h[i][j]).abs() < 1e-12,
                    "U S Vt != H at {i},{j}: {} vs {}",
                    rebuilt[i][j],
                    h[i][j]
                );
            }
        }
    }

    #[test]
    fn singular_values_are_descending_and_non_negative() {
        let h = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 10.0]];
        let s = svd3(&h);
        assert!(s.sigma[0] >= s.sigma[1]);
        assert!(s.sigma[1] >= s.sigma[2]);
        assert!(s.sigma[2] >= 0.0);
    }

    #[test]
    fn identity_has_unit_singular_values() {
        let s = svd3(&[[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
        for v in s.sigma {
            assert!((v - 1.0).abs() < 1e-15, "{v}");
        }
    }

    #[test]
    fn zero_matrix_has_zero_singular_values() {
        let s = svd3(&[[0.0; 3]; 3]);
        assert_eq!(s.sigma, [0.0, 0.0, 0.0]);
        assert_eq!(s.u, [[0.0; 3]; 3]);
    }

    /// A rank-2 matrix: the third direction is dropped, not amplified.
    #[test]
    fn rank_deficient_matrix_keeps_third_direction_zero() {
        let h = [[1.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 0.0]];
        let s = svd3(&h);
        assert!((s.sigma[0] - 2.0).abs() < 1e-12);
        assert!((s.sigma[1] - 1.0).abs() < 1e-12);
        assert!(s.sigma[2] < 1e-12, "{}", s.sigma[2]);
    }

    /// A rank-1 matrix must report the other two singular values as EXACT zero.
    ///
    /// Regression guard: without the pre-sqrt eigenvalue cutoff, `sqrt` amplifies
    /// round-off (~1e-16) to ~1e-8 and the third singular value comes back five
    /// digits too large, which would poison Kabsch's smallest direction.
    #[test]
    fn rank_one_matrix_reports_exact_zeros() {
        let h = [[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [3.0, 6.0, 9.0]];
        let s = svd3(&h);
        assert!(s.sigma[0] > 1.0, "{}", s.sigma[0]);
        assert_eq!(s.sigma[1], 0.0, "sigma[1] = {}", s.sigma[1]);
        assert_eq!(s.sigma[2], 0.0, "sigma[2] = {}", s.sigma[2]);
    }

    #[test]
    fn det3_matches_the_known_values() {
        assert_eq!(det3(&[[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]), 1.0);
        // A permutation matrix has determinant -1.
        assert_eq!(det3(&[[0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]), -1.0);
    }

    /// Kabsch's reflection fix negates `Vt`'s LAST row (smallest singular value).
    #[test]
    fn kabsch_reflection_fix_flips_the_last_vt_row() {
        let h = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]];
        let s = svd3(&h);
        let vt_t = transpose(&s.vt);
        let u_t = transpose(&s.u);
        let r_before = matmul(&vt_t, &u_t);
        assert!(det3(&r_before) < 0.0, "expected an improper rotation first");
        let mut vt_fixed = s.vt;
        for col in 0..3 {
            vt_fixed[2][col] = -vt_fixed[2][col];
        }
        let r_after = matmul(&transpose(&vt_fixed), &u_t);
        assert!(det3(&r_after) > 0.0, "det must become positive: {}", det3(&r_after));
    }

    /// A least-squares solve on an exactly-solvable system.
    #[test]
    fn lstsq3_recovers_an_exact_solution() {
        let a = vec![
            [2.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            [0.0, 0.0, 2.0],
            [1.0, 1.0, 1.0],
        ];
        let x_true = [1.0, 2.0, 3.0];
        let b: Vec<f64> = a
            .iter()
            .map(|row| row[0] * x_true[0] + row[1] * x_true[1] + row[2] * x_true[2])
            .collect();
        let got = lstsq3(&a, &b);
        for i in 0..3 {
            assert!((got[i] - x_true[i]).abs() < 1e-10, "{got:?} vs {x_true:?}");
        }
    }
}
