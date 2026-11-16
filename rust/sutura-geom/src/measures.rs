// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Mesh measures shared by the Python pipeline and the Rust CLI.
//!
//! `signed_volume` is the one reduction NumPy computes through `einsum`, i.e.
//! a BLAS-style inner loop that cannot be reproduced portably (the 3-element
//! dot differs from every plain association order and every FMA order). Per the
//! project rule -- one numeric core instead of imitating LAPACK -- the kernel
//! lives here and Python calls it, so both sides are identical by
//! construction.

/// NumPy's pairwise summation (8 accumulators, 128-element block).
pub fn sum_f64(values: &[f64]) -> f64 {
    pairwise(values)
}

/// `np.sum` over an `f32` slice.
pub fn sum_f32(values: &[f32]) -> f32 {
    pairwise(values)
}

const PW_BLOCKSIZE: usize = 128;

trait Accum: Copy {
    fn zero() -> Self;
    fn add(self, other: Self) -> Self;
}

impl Accum for f64 {
    fn zero() -> Self {
        0.0
    }
    fn add(self, other: Self) -> Self {
        self + other
    }
}

impl Accum for f32 {
    fn zero() -> Self {
        0.0
    }
    fn add(self, other: Self) -> Self {
        self + other
    }
}

fn pairwise<T: Accum>(values: &[T]) -> T {
    let n = values.len();
    if n < 8 {
        let mut total = T::zero();
        for v in values {
            total = total.add(*v);
        }
        return total;
    }
    if n <= PW_BLOCKSIZE {
        let mut acc = [
            values[0], values[1], values[2], values[3], values[4], values[5],
            values[6], values[7],
        ];
        let mut i = 8;
        while i + 8 <= n {
            for (k, slot) in acc.iter_mut().enumerate() {
                *slot = slot.add(values[i + k]);
            }
            i += 8;
        }
        // NumPy reduces the eight accumulators as ((r0+r1)+(r2+r3)) +
        // ((r4+r5)+(r6+r7)).
        let mut total = acc[0]
            .add(acc[1])
            .add(acc[2].add(acc[3]))
            .add(acc[4].add(acc[5]).add(acc[6].add(acc[7])));
        while i < n {
            total = total.add(values[i]);
            i += 1;
        }
        return total;
    }
    let mut n2 = n / 2;
    n2 -= n2 % 8;
    pairwise(&values[..n2]).add(pairwise(&values[n2..]))
}

/// Signed volume: sum of the origin-tetrahedra volumes, divided by six.
///
/// `verts` are the mesh's f64 coordinates (float32 values widened, exactly
/// like the Python path) and `tris` its indices.
pub fn signed_volume(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> f64 {
    if tris.is_empty() || verts.is_empty() {
        return 0.0;
    }
    let mut terms: Vec<f64> = Vec::with_capacity(tris.len());
    for f in tris {
        let (i, j, k) = (f[0], f[1], f[2]);
        if i < 0 || j < 0 || k < 0 {
            terms.push(0.0);
            continue;
        }
        let (i, j, k) = (i as usize, j as usize, k as usize);
        if i >= verts.len() || j >= verts.len() || k >= verts.len() {
            terms.push(0.0);
            continue;
        }
        let a = verts[i];
        let b = verts[j];
        let c = verts[k];
        let cross = [
            b[1] * c[2] - b[2] * c[1],
            b[2] * c[0] - b[0] * c[2],
            b[0] * c[1] - b[1] * c[0],
        ];
        terms.push(a[0] * cross[0] + a[1] * cross[1] + a[2] * cross[2]);
    }
    sum_f64(&terms) / 6.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_volume_of_a_unit_tetrahedron() {
        let verts = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
        let tris = [[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]];
        let v = signed_volume(&verts, &tris);
        assert!((v - 1.0 / 6.0).abs() < 1e-15, "{v}");
    }

    #[test]
    fn empty_mesh_is_zero() {
        assert_eq!(signed_volume(&[], &[]), 0.0);
    }
}
