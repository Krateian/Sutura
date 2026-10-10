//! Thin-plate radial-basis height field over a hole's local frame.
//!
//! Carr et al. 2001 ("Reconstruction and representation of 3D objects with
//! radial basis functions") fit an implicit surface as a sum of radial kernels
//! plus a low-order polynomial. This module implements the 2.5D special case the
//! Tamponade seal needs: a height field `F(u, v)` over the hole's local frame
//! `(u, v)`, whose samples are the intact-ring points (the same band the quadric
//! reference uses) plus the laser meeting points.
//!
//! Kernel `phi(r) = r^2 ln r` (thin plate). The solve is a dense `(M+3)` linear
//! system `[[K + lambda I, P], [P^T, 0]] [w; a] = [d; 0]` with a partial-pivot
//! Gaussian elimination. `M` is capped by `max_centres` and the sample set is
//! sub-sampled deterministically above that so a huge ring cannot blow the
//! `O(M^3)` solve. The single quadric stays the default for small holes; this is
//! the fallback for a large curved hole the quadric cannot represent.

/// Thin-plate spline kernel `phi(r) = r^2 ln r`, evaluated from `r2 = r^2`.
#[inline]
pub fn tps(r2: f64) -> f64 {
    if r2 <= 1e-12 {
        0.0
    } else {
        0.5 * r2 * r2.ln()
    }
}

/// A fitted height field `F(u, v) = a0 + a1 u + a2 v + sum_j w_j phi(|x - c_j|)`.
#[derive(Clone, Debug)]
pub struct TpsFit {
    centres: Vec<(f64, f64)>,
    weights: Vec<f64>,
    affine: [f64; 3],
}

impl TpsFit {
    /// Evaluate the height at `(u, v)`.
    pub fn eval(&self, u: f64, v: f64) -> f64 {
        let mut s = self.affine[0] + self.affine[1] * u + self.affine[2] * v;
        for (w, &(cu, cv)) in self.weights.iter().zip(self.centres.iter()) {
            let du = u - cu;
            let dv = v - cv;
            s += w * tps(du * du + dv * dv);
        }
        s
    }

    /// Number of RBF centres actually used (after sub-sampling).
    pub fn n_centres(&self) -> usize {
        self.centres.len()
    }

    /// Fit through `samples` `(u, v, d)`. `ridge` is a small Tikhonov term
    /// relative to the mean kernel magnitude (needed because `phi(0) = 0`, so
    /// the kernel matrix has a zero diagonal and can be singular on duplicate
    /// centres). Above `max_centres` samples the set is sub-sampled
    /// deterministically (fixed stride, last sample always kept).
    pub fn fit(samples: &[(f64, f64, f64)], ridge: f64, max_centres: usize) -> Option<TpsFit> {
        if samples.len() < 3 || max_centres < 3 {
            return None;
        }
        let pts = subsample(samples, max_centres);
        let m = pts.len();
        let n = m + 3;
        let mut a = vec![vec![0.0f64; n]; n];
        let mut b = vec![0.0f64; n];

        let mut k_abs = 0.0f64;
        let mut k_cnt = 0usize;
        for i in 0..m {
            let (ui, vi, di) = pts[i];
            for j in 0..m {
                let (uj, vj, _) = pts[j];
                let du = ui - uj;
                let dv = vi - vj;
                let k = tps(du * du + dv * dv);
                a[i][j] = k;
                if i != j {
                    k_abs += k.abs();
                    k_cnt += 1;
                }
            }
            a[i][m] = 1.0;
            a[i][m + 1] = ui;
            a[i][m + 2] = vi;
            a[m][i] = 1.0;
            a[m + 1][i] = ui;
            a[m + 2][i] = vi;
            b[i] = di;
        }
        let scale = if k_cnt > 0 { k_abs / k_cnt as f64 } else { 1.0 };
        let lam = ridge * scale + 1e-12;
        for i in 0..m {
            a[i][i] += lam;
        }

        let sol = solve_dense(a, b)?;
        if sol.iter().any(|x| !x.is_finite()) {
            return None;
        }
        let weights = sol[..m].to_vec();
        let affine = [sol[m], sol[m + 1], sol[m + 2]];
        Some(TpsFit {
            centres: pts.iter().map(|p| (p.0, p.1)).collect(),
            weights,
            affine,
        })
    }
}

/// Deterministic sub-sample: fixed stride, first and last kept.
fn subsample(samples: &[(f64, f64, f64)], max_centres: usize) -> Vec<(f64, f64, f64)> {
    if samples.len() <= max_centres {
        return samples.to_vec();
    }
    let stride = (samples.len() as f64 / max_centres as f64).ceil() as usize;
    let stride = stride.max(1);
    let mut out = Vec::with_capacity(max_centres + 1);
    let mut i = 0usize;
    while i < samples.len() {
        out.push(samples[i]);
        i += stride;
    }
    if let Some(last) = samples.last() {
        if out.last().map(|p| (p.0, p.1, p.2)) != Some((last.0, last.1, last.2)) {
            out.push(*last);
        }
    }
    out
}

/// Dense Gaussian elimination with partial pivoting. Returns `None` on a
/// (near-)singular pivot. `a` is consumed (row-reduced in place).
fn solve_dense(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    if n == 0 || a.len() != n {
        return None;
    }
    for col in 0..n {
        let mut piv = col;
        let mut best = a[col][col].abs();
        for r in (col + 1)..n {
            let v = a[r][col].abs();
            if v > best {
                best = v;
                piv = r;
            }
        }
        if best < 1e-14 {
            return None;
        }
        if piv != col {
            a.swap(col, piv);
            b.swap(col, piv);
        }
        let d = a[col][col];
        for c in col..n {
            a[col][c] /= d;
        }
        b[col] /= d;
        for r in 0..n {
            if r == col {
                continue;
            }
            let f = a[r][col];
            if f != 0.0 {
                for c in col..n {
                    a[r][c] -= f * a[col][c];
                }
                b[r] -= f * b[col];
            }
        }
    }
    Some(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tps_reproduces_a_polynomial() {
        // A plane z = 1 + 2u - 3v is in the affine space: the RBF weights must
        // stay near zero and the affine part must recover the plane.
        let mut s = Vec::new();
        for i in 0..12 {
            for j in 0..12 {
                let u = i as f64 * 0.25;
                let v = j as f64 * 0.25;
                s.push((u, v, 1.0 + 2.0 * u - 3.0 * v));
            }
        }
        let fit = TpsFit::fit(&s, 1e-6, 600).expect("fit");
        for &(u, v, d) in &s {
            assert!((fit.eval(u, v) - d).abs() < 1e-6, "plane not recovered");
        }
    }

    #[test]
    fn tps_interpolates_a_quadratic_bowl() {
        // z = a r^2 is smooth but not affine; the RBF must track it at the
        // centres and stay close between them.
        let a = 0.5;
        let mut s = Vec::new();
        let mut grid = Vec::new();
        for i in 0..10 {
            for j in 0..10 {
                let u = -1.0 + i as f64 * 2.0 / 9.0;
                let v = -1.0 + j as f64 * 2.0 / 9.0;
                let d = a * (u * u + v * v);
                s.push((u, v, d));
                grid.push((u, v, d));
            }
        }
        let fit = TpsFit::fit(&s, 1e-8, 600).expect("fit");
        let mut worst = 0.0f64;
        for &(u, v, d) in &grid {
            worst = worst.max((fit.eval(u, v) - d).abs());
        }
        assert!(worst < 1e-3, "bowl residual too large: {worst}");
    }

    #[test]
    fn subsample_keeps_first_and_last() {
        let s: Vec<(f64, f64, f64)> = (0..1000).map(|i| (i as f64, 0.0, 0.0)).collect();
        let out = subsample(&s, 100);
        assert_eq!(out.first().unwrap().0, 0.0);
        assert_eq!(out.last().unwrap().0, 999.0);
        assert!(out.len() <= 101);
    }
}
