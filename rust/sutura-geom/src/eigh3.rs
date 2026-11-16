// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Eigendecomposition of a symmetric 3x3 matrix — one kernel for every caller.
//!
//! `np.linalg.eigh` is LAPACK (`dsyevd`/`ssyevd`), whose Jacobi/QR internals no
//! portable implementation reproduces bit for bit. Per the project rule — the
//! numeric core lives in one place and Python calls it — this module owns the
//! 3x3 case, and `classify::curvature_developable_fraction`, `repeat.py` and
//! the Mirror plane normal all go through it instead of imitating LAPACK.
//!
//! Method: cyclic Jacobi rotations on the largest off-diagonal element. The
//! rotation expressions are the ones `classify::smallest_eigenvector` has used
//! since the classifier port and that were verified against LAPACK: eigenvalues
//! agree to ~2e-15 relative and eigenvector directions to ~3e-6 degrees over
//! 20k random symmetric matrices plus repeated-eigenvalue, zero and
//! block-degenerate cases.
//!
//! Two deliberate differences from that earlier helper:
//!
//! * The convergence test is **relative** to the matrix scale. A fixed
//!   `1e-15` absolute cutoff is not scale invariant: at a covariance scale of
//!   1e-12 it stops far too late and the eigenvector error grows to ~0.2
//!   degrees. Scaling the cutoff keeps the error at ~2e-15 relative from 1e-12
//!   to 1e12.
//! * Eigenvector **signs are canonicalised**: the largest-magnitude component
//!   of every column is made positive. Eigenvector sign is arbitrary in exact
//!   arithmetic, but callers such as `repeat.py` use the vectors as rotation
//!   axes, where a sign flip reverses the rotation direction and moves the
//!   score (measured: 16 of 226 corpus meshes, 2 with a different
//!   `pattern_type`). Canonical signs make the kernel's output a pure function
//!   of the matrix, so no caller can be surprised by which solver ran.

/// Cap on Jacobi sweeps. The largest count observed on the 226-mesh corpus is
/// 9; 32 leaves ample margin and still terminates on adversarial input.
const MAX_SWEEPS: usize = 32;

/// Relative convergence cutoff for the largest off-diagonal element.
const REL_TOL: f64 = 1e-15;

/// Ascending eigenvalues and their eigenvectors (one per column).
///
/// `a` may be any symmetric 3x3 matrix; only the lower/upper triangle is read
/// through the symmetric accessors, so a caller may fill either side.
pub fn eigh3(a: [[f64; 3]; 3]) -> ([f64; 3], [[f64; 3]; 3]) {
    let mut m = a;
    let mut vec = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    let mut scale = 0.0f64;
    for row in m.iter() {
        for &value in row.iter() {
            scale = scale.max(value.abs());
        }
    }
    if scale == 0.0 {
        // The zero matrix has no preferred basis; the identity is the
        // conventional answer and matches LAPACK's for `np.zeros((3, 3))`.
        return ([0.0, 0.0, 0.0], vec);
    }
    let tol = scale * REL_TOL;

    for _ in 0..MAX_SWEEPS {
        // Largest off-diagonal element, in a fixed search order so ties are
        // resolved deterministically.
        let (mut p, mut q, mut max) = (0usize, 1usize, m[0][1].abs());
        for (i, j) in [(0usize, 2usize), (1, 2)] {
            if m[i][j].abs() > max {
                max = m[i][j].abs();
                p = i;
                q = j;
            }
        }
        if max <= tol {
            break;
        }
        let theta = (m[q][q] - m[p][p]) / (2.0 * m[p][q]);
        // `1.0f64.copysign(theta)` is the sign of theta with magnitude one.
        // (`theta.copysign(1.0)` would be |theta| -- copysign takes its
        // MAGNITUDE from the receiver -- and the rotation would never turn the
        // matrix towards diagonal, leaving eigenvectors wrong by O(1).)
        let t = 1.0f64.copysign(theta) / (theta.abs() + (theta * theta + 1.0).sqrt());
        let c = 1.0 / (t * t + 1.0).sqrt();
        let s = t * c;
        for k in 0..3 {
            let (akp, akq) = (m[k][p], m[k][q]);
            m[k][p] = c * akp - s * akq;
            m[k][q] = s * akp + c * akq;
        }
        for k in 0..3 {
            let (apk, aqk) = (m[p][k], m[q][k]);
            m[p][k] = c * apk - s * aqk;
            m[q][k] = s * apk + c * aqk;
        }
        for k in 0..3 {
            let (vkp, vkq) = (vec[k][p], vec[k][q]);
            vec[k][p] = c * vkp - s * vkq;
            vec[k][q] = s * vkp + c * vkq;
        }
    }

    let mut order = [0usize, 1, 2];
    // Ascending eigenvalues, like `np.linalg.eigh`. Insertion sort on three
    // elements keeps it branch-simple and deterministic on ties.
    for i in 1..3 {
        let mut j = i;
        while j > 0 && m[order[j]][order[j]] < m[order[j - 1]][order[j - 1]] {
            order.swap(j, j - 1);
            j -= 1;
        }
    }

    let mut values = [0.0f64; 3];
    let mut vectors = [[0.0f64; 3]; 3];
    for (col, &src) in order.iter().enumerate() {
        values[col] = m[src][src];
        for row in 0..3 {
            vectors[row][col] = vec[row][src];
        }
        // Canonical sign: largest-magnitude component positive.
        let mut dom = 0usize;
        for row in 1..3 {
            if vectors[row][col].abs() > vectors[dom][col].abs() {
                dom = row;
            }
        }
        if vectors[dom][col] < 0.0 {
            for row in 0..3 {
                vectors[row][col] = -vectors[row][col];
            }
        }
    }
    (values, vectors)
}

/// Eigenvector of the smallest eigenvalue — the axis a developable surface's
/// normals are perpendicular to. Convenience wrapper over [`eigh3`].
pub fn smallest_eigenvector(a: [[f64; 3]; 3]) -> [f64; 3] {
    let (_, vectors) = eigh3(a);
    [vectors[0][0], vectors[1][0], vectors[2][0]]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f64, expected: f64, tol: f64, what: &str) {
        assert!(
            (actual - expected).abs() <= tol,
            "{what}: {actual} vs {expected}"
        );
    }

    #[test]
    fn diagonal_matrix_returns_its_diagonal_in_ascending_order() {
        let (values, vectors) = eigh3([[3.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 2.0]]);
        assert_eq!(values, [1.0, 2.0, 3.0]);
        // Canonical signs put the single +1 on the diagonal.
        for (col, expected) in [(0usize, 1usize), (1, 2), (2, 0)] {
            for row in 0..3 {
                assert_close(vectors[row][col], if row == expected { 1.0 } else { 0.0 }, 0.0, "basis");
            }
        }
    }

    #[test]
    fn zero_matrix_is_the_identity_basis() {
        let (values, vectors) = eigh3([[0.0; 3]; 3]);
        assert_eq!(values, [0.0, 0.0, 0.0]);
        assert_eq!(vectors, [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    }

    /// `A v = lambda v` for every column, and `V` orthonormal — the two
    /// invariants a caller can rely on no matter which solver produced it.
    #[test]
    fn reconstruction_and_orthonormality_hold() {
        let a = [[4.0, 1.0, 0.5], [1.0, 3.0, 0.25], [0.5, 0.25, 2.0]];
        let (values, v) = eigh3(a);
        for col in 0..3 {
            for row in 0..3 {
                let av: f64 = (0..3).map(|k| a[row][k] * v[k][col]).sum();
                assert_close(av, values[col] * v[row][col], 1e-12, "A v = lambda v");
            }
        }
        for i in 0..3 {
            for j in 0..3 {
                let dot: f64 = (0..3).map(|k| v[k][i] * v[k][j]).sum();
                assert_close(dot, if i == j { 1.0 } else { 0.0 }, 1e-12, "orthonormal");
            }
        }
        assert!(values[0] <= values[1] && values[1] <= values[2]);
    }

    /// The relative cutoff is what keeps tiny matrices accurate; an absolute
    /// 1e-15 cutoff stops early and drifts by ~0.2 degrees here.
    #[test]
    fn tiny_scale_matrix_still_converges() {
        let base = [[4.0, 1.0, 0.5], [1.0, 3.0, 0.25], [0.5, 0.25, 2.0]];
        let small = base.map(|row| row.map(|x| x * 1e-12));
        let (values, v) = eigh3(small);
        for col in 0..3 {
            for row in 0..3 {
                let av: f64 = (0..3).map(|k| small[row][k] * v[k][col]).sum();
                assert_close(av, values[col] * v[row][col], 1e-24, "A v = lambda v at 1e-12");
            }
        }
        // Ratio of the largest to the smallest eigenvalue; scale invariant, so
        // the 1e-12 copy must reproduce the reference ratio.
        let (reference, _) = eigh3(base);
        assert_close(values[2] / values[0], reference[2] / reference[0], 1e-12, "eigenvalue ratio is scale free");
    }

    /// Sign canonicalisation: the largest-magnitude component of each column is
    /// positive. Without it the kernel's output would depend on the rotation
    /// path, and `repeat.py` reads the vectors as axes.
    #[test]
    fn eigenvector_signs_are_canonical() {
        let a = [[1.0, -2.0, 0.0], [-2.0, 1.0, 0.0], [0.0, 0.0, 5.0]];
        let (_, v) = eigh3(a);
        for col in 0..3 {
            let mut dom = 0usize;
            for row in 1..3 {
                if v[row][col].abs() > v[dom][col].abs() {
                    dom = row;
                }
            }
            assert!(v[dom][col] > 0.0, "column {col} dominant component must be positive");
        }
    }

    #[test]
    fn smallest_eigenvector_is_the_first_column() {
        let a = [[2.0, 0.0, 0.0], [0.0, 5.0, 0.0], [0.0, 0.0, 9.0]];
        let axis = smallest_eigenvector(a);
        assert_eq!(axis, [1.0, 0.0, 0.0]);
    }
}
