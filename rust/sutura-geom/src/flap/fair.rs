//! Discrete fairing of the patch interior with a C1 ghost boundary:
//! `fair_patch` (biharmonic thin-plate, `GᵀG`, the historical solve) and
//! `fair_patch_energy`, which can also solve the membrane/umbrella energy
//! (order 1, the graph Laplacian `G`) and add a soft data term
//! `lambda * Σ‖x_i − data_i‖²` anchoring the interior to a reference surface.
//! Solved as a sparse normal-equation system with faer.

use super::V3;
use faer::linalg::solvers::Solve;
use faer::sparse::linalg::solvers::{Lu, SymbolicLu};
use faer::sparse::{SparseColMat, Triplet};
use faer::Mat;

/// Accumulate `val` into the lower-triangular system entry `(row_local, col)`
/// when `col` is an unknown interior column, or into the right-hand side when
/// `col` is a fixed (rim or ghost) column.
#[allow(clippy::too_many_arguments)]
fn add_entry(
    lower: &mut Vec<(usize, usize, f64)>,
    rhs: &mut [[f64; 3]],
    row_local: usize,
    col: u32,
    val: f64,
    n_boundary: usize,
    n: usize,
    p: &[V3],
    ghost_pos: &[V3],
) {
    let ci = col as usize;
    if ci >= n_boundary && ci < n {
        let col_local = ci - n_boundary;
        // M is symmetric: store each unordered pair once (row >= col).
        if row_local >= col_local {
            lower.push((row_local, col_local, val));
        }
    } else {
        let x = if ci < n { p[ci] } else { ghost_pos[ci - n] };
        rhs[row_local][0] -= val * x[0];
        rhs[row_local][1] -= val * x[1];
        rhs[row_local][2] -= val * x[2];
    }
}

/// Fair the interior vertices of `p`; the first `n_boundary` are fixed hole
/// rim vertices and `ghost_pos` are the fixed C1 ghost positions (one per rim
/// vertex) connected one-to-one to them.
///
/// `order` 1 selects the membrane/umbrella energy (graph Laplacian `G`), 2 the
/// biharmonic thin-plate energy (`GᵀG = G²`, the historical behaviour).  When
/// `data` (one target per interior vertex) and `lambda > 0` are given, a soft
/// data term pulls each interior vertex toward its target.
pub fn fair_patch_energy(
    p: &mut Vec<V3>,
    faces: &[[u32; 3]],
    n_boundary: usize,
    ghost_pos: &[V3],
    order: u8,
    data: Option<&[V3]>,
    lambda: f64,
) {
    let n = p.len();
    if n <= n_boundary {
        return;
    }
    let ng = n_boundary;
    let ntot = n + ng;

    // Binary adjacency (the reference binarises W by forcing every stored value
    // to 1.0), plus the ghost 1-to-1 ring, in CSR form.  A single flat buffer
    // replaces one heap Vec per vertex; the row order after sorting and
    // deduplication is the same ascending order as before, so every
    // accumulation below keeps its exact floating-point order.
    let mut degree = vec![0u32; ntot];
    for f in faces {
        for (u, w) in [(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
            degree[u as usize] += 1;
            degree[w as usize] += 1;
        }
    }
    for i in 0..ng {
        degree[i] += 1;
        degree[n + i] += 1;
    }
    let mut offsets = vec![0usize; ntot + 1];
    for v in 0..ntot {
        offsets[v + 1] = offsets[v] + degree[v] as usize;
    }
    let mut adj = vec![0u32; offsets[ntot]];
    let mut cursor = offsets.clone();
    for f in faces {
        for (u, w) in [(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
            let u = u as usize;
            let w = w as usize;
            adj[cursor[u]] = w as u32;
            cursor[u] += 1;
            adj[cursor[w]] = u as u32;
            cursor[w] += 1;
        }
    }
    for i in 0..ng {
        adj[cursor[i]] = (n + i) as u32;
        cursor[i] += 1;
        adj[cursor[n + i]] = i as u32;
        cursor[n + i] += 1;
    }
    let mut new_offsets = vec![0usize; ntot + 1];
    let mut write = 0usize;
    for v in 0..ntot {
        let from = offsets[v];
        let to = offsets[v + 1];
        adj[from..to].sort_unstable();
        let mut last: Option<u32> = None;
        for k in from..to {
            let x = adj[k];
            if Some(x) != last {
                adj[write] = x;
                write += 1;
                last = Some(x);
            }
        }
        new_offsets[v + 1] = write;
    }
    adj.truncate(write);
    let offsets = new_offsets;
    let deg = |v: usize| offsets[v + 1] - offsets[v];
    let neighbours = |v: usize| &adj[offsets[v]..offsets[v + 1]];

    let n_unknown = n - n_boundary;
    let mut rhs = vec![[0.0f64; 3]; n_unknown];
    // Sparse lower-triangle triplets in a single growing buffer, keyed by the
    // ordered (max, min) pair so an unordered pair is stored exactly once.
    let mut lower: Vec<(usize, usize, f64)> = Vec::new();

    if order >= 2 {
        // M row r = Σ_a L[r][a] · L[a]  (a ∈ {r} ∪ nb[r]) = (G²)[r]; only
        // unknown rows.  G is symmetric, so M[r][c] == M[c][r].
        let mut mrow: Vec<(u32, f64)> = Vec::new();
        let mut merged: Vec<(u32, f64)> = Vec::new();
        for r in n_boundary..n {
            let row_local = r - n_boundary;
            mrow.clear();
            // entries of L row r: (r, deg[r]) and (c, −1) for c in nb[r]
            let row = neighbours(r);
            for a in std::iter::once(r as u32).chain(row.iter().copied()) {
                let a_idx = a as usize;
                let va = if a_idx == r {
                    row.len() as f64
                } else {
                    -1.0
                };
                if va == 0.0 {
                    continue;
                }
                // L row a: diagonal deg[a], neighbours −1
                mrow.push((a, va * deg(a_idx) as f64));
                for &b in neighbours(a_idx) {
                    mrow.push((b, -va));
                }
            }
            mrow.sort_by_key(|&(c, _)| c);
            merged.clear();
            for &(c, val) in mrow.iter() {
                if let Some(last) = merged.last_mut() {
                    if last.0 == c {
                        last.1 += val;
                        continue;
                    }
                }
                merged.push((c, val));
            }
            for &(c, val) in &merged {
                add_entry(
                    &mut lower,
                    &mut rhs,
                    row_local,
                    c,
                    val,
                    n_boundary,
                    n,
                    p,
                    ghost_pos,
                );
            }
            // Tikhonov ridge on the diagonal (matches the historical solve).
            lower.push((row_local, row_local, 1e-9));
        }
    } else {
        // G row r: diagonal deg[r], neighbours −1 (the umbrella operator).
        for r in n_boundary..n {
            let row_local = r - n_boundary;
            lower.push((row_local, row_local, deg(r) as f64));
            for &c in neighbours(r) {
                add_entry(
                    &mut lower,
                    &mut rhs,
                    row_local,
                    c,
                    -1.0,
                    n_boundary,
                    n,
                    p,
                    ghost_pos,
                );
            }
            lower.push((row_local, row_local, 1e-9));
        }
    }

    // Soft data term: lambda · ‖x_i − data_i‖² per interior vertex, pinning the
    // interior to the reference projection while the regulariser smooths it.
    if let Some(data) = data {
        if lambda > 0.0 && data.len() >= n_unknown {
            for i in 0..n_unknown {
                lower.push((i, i, lambda));
                rhs[i][0] += lambda * data[i][0];
                rhs[i][1] += lambda * data[i][1];
                rhs[i][2] += lambda * data[i][2];
            }
        }
    }

    if n_unknown == 0 || lower.is_empty() {
        return;
    }
    // Stable sort merges the diagonal ridge/data entries in their push order,
    // so the value is (row contribution + 1e-9) + lambda, exactly as the old
    // hash-map accumulation produced.  Then mirror the lower triangle.
    lower.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    // Compact duplicate keys in place, so the only other buffer is the final
    // symmetric triplet list.
    let mut write = 0usize;
    for read in 0..lower.len() {
        let (row, col, val) = lower[read];
        if write > 0 && lower[write - 1].0 == row && lower[write - 1].1 == col {
            lower[write - 1].2 += val;
        } else {
            lower[write] = (row, col, val);
            write += 1;
        }
    }
    lower.truncate(write);
    let mut trips: Vec<Triplet<usize, usize, f64>> = Vec::with_capacity(lower.len() * 2);
    for &(hi, lo, v) in &lower {
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

/// Historical biharmonic (thin-plate) fairing; see [`fair_patch_energy`].
pub fn fair_patch(p: &mut Vec<V3>, faces: &[[u32; 3]], n_boundary: usize, ghost_pos: &[V3]) {
    fair_patch_energy(p, faces, n_boundary, ghost_pos, 2, None, 0.0);
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
        fair_patch_energy(&mut p, &faces, 6, &ghost, 1, None, 0.0);
        assert_eq!(p, before);
    }

    #[test]
    fn planar_patch_stays_planar() {
        // Every fixed position (rim and ghost) lies in z = 0, so the zero-RHS
        // solve must leave the interior vertex in the plane, for both orders.
        for order in [1u8, 2] {
            let (mut p, faces, ghost) = wheel(8, 0.0);
            fair_patch_energy(&mut p, &faces, 8, &ghost, order, None, 0.0);
            assert!(p[8][2].abs() < 1e-12, "order {} z = {}", order, p[8][2]);
        }
    }

    #[test]
    fn membrane_planar_stays_planar() {
        let (mut p, faces, ghost) = wheel(8, 0.0);
        fair_patch_energy(&mut p, &faces, 8, &ghost, 1, None, 0.0);
        assert!(p[8][2].abs() < 1e-12, "z = {}", p[8][2]);
    }

    #[test]
    fn data_term_pulls_interior_to_target() {
        // With a strong data term the lone interior vertex is pulled to the
        // target height, for both energies.
        for order in [1u8, 2] {
            let (mut p, faces, ghost) = wheel(8, 0.0);
            let target = vec![[0.0, 0.0, 1.5]];
            fair_patch_energy(&mut p, &faces, 8, &ghost, order, Some(&target), 1e5);
            assert!(
                (p[8][2] - 1.5).abs() < 0.05,
                "order {} z = {} (want ~1.5)",
                order,
                p[8][2]
            );
        }
    }

    #[test]
    fn deterministic() {
        for order in [1u8, 2] {
            let (mut p1, faces, ghost) = wheel(8, 0.5);
            let mut p2 = p1.clone();
            fair_patch_energy(&mut p1, &faces, 8, &ghost, order, None, 0.0);
            fair_patch_energy(&mut p2, &faces, 8, &ghost, order, None, 0.0);
            for c in 0..3 {
                assert!((p1[8][c] - p2[8][c]).abs() < 1e-14);
            }
        }
    }
}
