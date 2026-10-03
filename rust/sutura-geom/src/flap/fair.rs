//! Discrete biharmonic (thin-plate) fairing of the patch interior with a C1
//! ghost boundary, solved as a sparse `LᵀL` normal-equation system with faer.

use super::V3;
use faer::linalg::solvers::Solve;
use faer::sparse::linalg::solvers::{Lu, SymbolicLu};
use faer::sparse::{SparseColMat, Triplet};
use faer::Mat;

/// Fair the interior vertices of `p`; the first `n_boundary` are fixed hole
/// rim vertices and `ghost_pos` are the fixed C1 ghost positions (one per rim
/// vertex) connected one-to-one to them.  Returns the rest of the fairing
/// system's diagnostics unused (mutates `p` in place).
pub fn fair_patch(p: &mut Vec<V3>, faces: &[[u32; 3]], n_boundary: usize, ghost_pos: &[V3]) {
    let n = p.len();
    if n <= n_boundary {
        return;
    }
    let ng = n_boundary;
    let ntot = n + ng;

    // Binary adjacency (the reference binarises W by forcing every stored
    // value to 1.0), plus the ghost 1-to-1 ring.
    let mut nb: Vec<Vec<u32>> = vec![Vec::new(); ntot];
    for f in faces {
        for (u, w) in [(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
            nb[u as usize].push(w);
            nb[w as usize].push(u);
        }
    }
    for i in 0..ng {
        nb[i].push((n + i) as u32);
        nb[n + i].push(i as u32);
    }
    for v in nb.iter_mut() {
        v.sort_unstable();
        v.dedup();
    }

    let n_unknown = n - n_boundary;
    let mut rhs = vec![[0.0f64; 3]; n_unknown];
    // Assemble the lower triangle of A = M[unknown, unknown] + 1e-9 I, keyed by
    // the ordered (max, min) pair so an unordered pair is stored exactly once
    // (the matrix is symmetric, so the triplet list must not repeat entries).
    let mut lower: std::collections::HashMap<(usize, usize), f64> =
        std::collections::HashMap::new();

    // M row r = sum_a L[r][a] * L[a]  (a in {r} ∪ nb[r]); only unknown rows.
    let mut mrow: Vec<(u32, f64)> = Vec::new();
    for r in n_boundary..n {
        mrow.clear();
        // entries of L row r: (r, deg[r]) and (c, -1) for c in nb[r]
        for a in std::iter::once(r as u32).chain(nb[r].iter().copied()) {
            let va = if a as usize == r {
                nb[r].len() as f64
            } else {
                -1.0
            };
            if va == 0.0 {
                continue;
            }
            // L row a: diagonal deg[a], neighbours -1
            mrow.push((a, va * nb[a as usize].len() as f64));
            for &b in &nb[a as usize] {
                mrow.push((b, -va));
            }
        }
        mrow.sort_by_key(|&(c, _)| c);
        // merge duplicate columns
        let mut merged: Vec<(u32, f64)> = Vec::with_capacity(mrow.len());
        for &(c, val) in mrow.iter() {
            if let Some(last) = merged.last_mut() {
                if last.0 == c {
                    last.1 += val;
                    continue;
                }
            }
            merged.push((c, val));
        }
        let row_local = r - n_boundary;
        for &(c, val) in &merged {
            let ci = c as usize;
            if ci >= n_boundary && ci < n {
                // unknown column
                let col_local = ci - n_boundary;
                // Only the row that is the larger index contributes the pair,
                // so each symmetric entry is stored exactly once (M is
                // symmetric: M[r][c] == M[c][r]).
                if row_local >= col_local {
                    let v = if ci == r { val + 1e-9 } else { val };
                    *lower.entry((row_local, col_local)).or_insert(0.0) += v;
                }
            } else {
                // fixed column (rim or ghost)
                let x = if ci < n { p[ci] } else { ghost_pos[ci - n] };
                rhs[row_local][0] -= val * x[0];
                rhs[row_local][1] -= val * x[1];
                rhs[row_local][2] -= val * x[2];
            }
        }
    }

    if n_unknown == 0 || lower.is_empty() {
        return;
    }
    // Mirror the lower triangle into the full symmetric triplet list.
    let mut trips: Vec<Triplet<usize, usize, f64>> = Vec::with_capacity(lower.len() * 2);
    for (&(hi, lo), &v) in &lower {
        trips.push(Triplet::new(hi, lo, v));
        if hi != lo {
            trips.push(Triplet::new(lo, hi, v));
        }
    }
    trips.sort_by(|a, b| a.row.cmp(&b.row).then(a.col.cmp(&b.col)));

    let a = match SparseColMat::<usize, f64>::try_new_from_triplets(n_unknown, n_unknown, &trips) {
        Ok(m) => m,
        Err(_) => return,
    };
    let sym = match SymbolicLu::try_new(a.symbolic()) {
        Ok(s) => s,
        Err(_) => return,
    };
    let lu = match Lu::try_new_with_symbolic(sym, a.as_ref()) {
        Ok(l) => l,
        Err(_) => return,
    };
    let mut b = Mat::<f64>::zeros(n_unknown, 3);
    for i in 0..n_unknown {
        b[(i, 0)] = rhs[i][0];
        b[(i, 1)] = rhs[i][1];
        b[(i, 2)] = rhs[i][2];
    }
    lu.solve_in_place(&mut b);
    for i in 0..n_unknown {
        p[n_boundary + i] = [b[(i, 0)], b[(i, 1)], b[(i, 2)]];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rim ring of `m` vertices around one interior centre vertex: a fan of
    /// `m` triangles.
    fn wheel(m: usize, centre_z: f64) -> (Vec<V3>, Vec<[u32; 3]>, Vec<V3>) {
        let mut p: Vec<V3> = (0..m)
            .map(|i| {
                let a = std::f64::consts::TAU * i as f64 / m as f64;
                [a.cos(), a.sin(), 0.0]
            })
            .collect();
        p.push([0.0, 0.0, centre_z]);
        let c = m as u32;
        let faces: Vec<[u32; 3]> = (0..m).map(|i| [i as u32, ((i + 1) % m) as u32, c]).collect();
        let ghost: Vec<V3> = (0..m)
            .map(|i| {
                let a = std::f64::consts::TAU * i as f64 / m as f64;
                [a.cos(), a.sin(), 0.0]
            })
            .collect();
        (p, faces, ghost)
    }

    #[test]
    fn noop_without_interior() {
        let (mut p, faces, ghost) = wheel(6, 0.0);
        p.pop(); // remove the interior centre
        let before = p.clone();
        fair_patch(&mut p, &faces, 6, &ghost);
        assert_eq!(p, before);
    }

    #[test]
    fn planar_patch_stays_planar() {
        // Every fixed position (rim and ghost) lies in z = 0, so the zero-RHS
        // harmonic solve must leave the interior vertex in the plane.
        let (mut p, faces, ghost) = wheel(8, 0.0);
        fair_patch(&mut p, &faces, 8, &ghost);
        assert!(p[8][2].abs() < 1e-12, "z = {}", p[8][2]);
    }

    #[test]
    fn deterministic() {
        let (mut p1, faces, ghost) = wheel(8, 0.5);
        let mut p2 = p1.clone();
        fair_patch(&mut p1, &faces, 8, &ghost);
        fair_patch(&mut p2, &faces, 8, &ghost);
        for c in 0..3 {
            assert!((p1[8][c] - p2[8][c]).abs() < 1e-14);
        }
    }
}
