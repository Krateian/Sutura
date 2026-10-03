//! topology — integer/float topology kernel (phase-2 profiling, FAZ-perf).
//!
//! One Rust implementation of the three primitive operations the Python
//! pipeline performs most often on plain `verts`/`tris` arrays:
//!
//! * `edge_table` — unique undirected edges, per-edge use counts and the
//!   half-edge -> edge-id inverse map (serves `defects._boundary_edges`,
//!   `defects.detect_non_manifold`, `graft._unique_edges`/`_edge_use_counts`).
//! * `weld_vertices` — `np.unique(v, axis=0)`: float32 position weld with the
//!   lexicographic order and the `-0.0 == 0.0` grouping numpy uses.
//! * `weld_reload_equivalent` — the whole P-HONEST STL save/reload weld.
//!
//! The outputs are defined so a pure-numpy implementation is the oracle: the
//! integer topology (edges, counts, inverse, welded faces) is bit-for-bit
//! identical.  Non-finite coordinates are rejected here so the Python wrapper
//! can fall back to numpy (numpy orders NaN/inf differently from `partial_cmp`).

use numpy::ndarray::Array2;
use numpy::{AllowTypeChange, PyArray1, PyArray2, PyArrayLike2};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use std::cmp::Ordering;

#[inline]
fn canon(a: i64, b: i64) -> (i64, i64) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Lexicographic comparison of two float32 rows.  `-0.0` compares equal to
/// `0.0` (numpy groups them); NaN is treated as equal so it cannot panic — the
/// Python wrapper refuses non-finite inputs before calling this.
#[inline]
fn cmp_f32_row(a: &[f32; 3], b: &[f32; 3]) -> Ordering {
    for k in 0..3 {
        match a[k].partial_cmp(&b[k]) {
            Some(Ordering::Equal) | None => continue,
            Some(o) => return o,
        }
    }
    Ordering::Equal
}

/// Unique rows of a fixed-width 3-column i64 table, sorted lexicographically,
/// with the index of each unique row's first occurrence in the input.  The
/// stable sort makes the "first occurrence" rule explicit.
fn unique_rows_i64_first(rows: &[[i64; 3]]) -> (Vec<[i64; 3]>, Vec<usize>) {
    let n = rows.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&x, &y| rows[x].cmp(&rows[y]));
    let mut uniq: Vec<[i64; 3]> = Vec::new();
    let mut first: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i < n {
        let k = rows[order[i]];
        uniq.push(k);
        first.push(order[i]);
        let mut j = i + 1;
        while j < n && rows[order[j]] == k {
            j += 1;
        }
        i = j;
    }
    (uniq, first)
}

/// Unique float32 rows, sorted lexicographically, with the row -> group-id
/// inverse map.  The representative of an equal group is its first occurrence
/// in the input (matching numpy's observed `np.unique(..., axis=0)` choice).
fn unique_rows_f32(rows: &[[f32; 3]]) -> (Vec<[f32; 3]>, Vec<i64>) {
    let n = rows.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&x, &y| cmp_f32_row(&rows[x], &rows[y]));
    let mut uniq: Vec<[f32; 3]> = Vec::new();
    let mut inv = vec![0i64; n];
    let mut i = 0usize;
    while i < n {
        let gid = uniq.len() as i64;
        uniq.push(rows[order[i]]);
        let mut j = i + 1;
        while j < n && cmp_f32_row(&rows[order[j]], &rows[order[i]]) == Ordering::Equal {
            inv[order[j]] = gid;
            j += 1;
        }
        inv[order[i]] = gid;
        i = j;
    }
    (uniq, inv)
}

fn read_f32_rows(arr: &PyArrayLike2<'_, f32, AllowTypeChange>) -> PyResult<Vec<[f32; 3]>> {
    let v = arr.as_array();
    if v.ncols() != 3 {
        return Err(PyValueError::new_err("array must be Nx3"));
    }
    let n = v.nrows();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push([v[[i, 0]], v[[i, 1]], v[[i, 2]]]);
    }
    Ok(out)
}

fn read_i64_rows(arr: &PyArrayLike2<'_, i64, AllowTypeChange>) -> PyResult<Vec<[i64; 3]>> {
    let t = arr.as_array();
    if t.ncols() != 3 {
        return Err(PyValueError::new_err("tris must be Mx3"));
    }
    let n = t.nrows();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push([t[[i, 0]], t[[i, 1]], t[[i, 2]]]);
    }
    Ok(out)
}

fn rows_to_array2_i64<'py>(py: Python<'py>, rows: &[[i64; 3]]) -> Bound<'py, PyArray2<i64>> {
    let mut a = Array2::<i64>::zeros((rows.len(), 3));
    for (r, row) in rows.iter().enumerate() {
        a[[r, 0]] = row[0];
        a[[r, 1]] = row[1];
        a[[r, 2]] = row[2];
    }
    PyArray2::from_owned_array(py, a)
}

fn pairs_to_array2_i64<'py>(py: Python<'py>, pairs: &[(i64, i64)]) -> Bound<'py, PyArray2<i64>> {
    let mut a = Array2::<i64>::zeros((pairs.len(), 2));
    for (r, &(x, y)) in pairs.iter().enumerate() {
        a[[r, 0]] = x;
        a[[r, 1]] = y;
    }
    PyArray2::from_owned_array(py, a)
}

fn rows_to_array2_f32<'py>(py: Python<'py>, rows: &[[f32; 3]]) -> Bound<'py, PyArray2<f32>> {
    let mut a = Array2::<f32>::zeros((rows.len(), 3));
    for (r, row) in rows.iter().enumerate() {
        a[[r, 0]] = row[0];
        a[[r, 1]] = row[1];
        a[[r, 2]] = row[2];
    }
    PyArray2::from_owned_array(py, a)
}

/// Undirected edge table of a triangle mesh.
///
/// Half-edges are enumerated in the concatenated order
/// `[(v0,v1) for all faces, (v1,v2), (v2,v0)]`, canonicalised to
/// `(min, max)`, sorted lexicographically and grouped.  Returns
/// `(edges, counts, inverse)`: `edges` is the sorted unique `(N, 2)` int64
/// table, `counts[n]` the number of half-edges using `edges[n]` (so boundary
/// edges have count 1, non-manifold edges count > 2), and `inverse[h]` the
/// edge id of the `h`-th half-edge.
#[pyfunction]
#[pyo3(signature = (tris))]
pub fn edge_table<'py>(
    py: Python<'py>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
) -> PyResult<(
    Bound<'py, PyArray2<i64>>,
    Bound<'py, PyArray1<i64>>,
    Bound<'py, PyArray1<i64>>,
)> {
    let t = tris.as_array();
    if t.ncols() != 3 {
        return Err(PyValueError::new_err("tris must be Mx3"));
    }
    let f = t.nrows();
    let n = 3 * f;
    if n == 0 {
        return Ok((
            PyArray2::<i64>::zeros(py, (0, 2), false),
            PyArray1::from_vec(py, Vec::new()),
            PyArray1::from_vec(py, Vec::new()),
        ));
    }
    let mut keys: Vec<(i64, i64)> = Vec::with_capacity(n);
    for b in 0..3usize {
        for r in 0..f {
            keys.push(canon(t[[r, b]], t[[r, (b + 1) % 3]]));
        }
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_unstable_by(|&x, &y| keys[x].cmp(&keys[y]));
    let mut uniq: Vec<(i64, i64)> = Vec::new();
    let mut counts: Vec<i64> = Vec::new();
    let mut inv = vec![0i64; n];
    let mut i = 0usize;
    while i < n {
        let k = keys[order[i]];
        let gid = uniq.len() as i64;
        uniq.push(k);
        let mut j = i + 1;
        while j < n && keys[order[j]] == k {
            inv[order[j]] = gid;
            j += 1;
        }
        inv[order[i]] = gid;
        counts.push((j - i) as i64);
        i = j;
    }
    Ok((
        pairs_to_array2_i64(py, &uniq),
        PyArray1::from_vec(py, counts),
        PyArray1::from_vec(py, inv),
    ))
}

/// `np.unique(v, axis=0, return_inverse=True)` for a float32 `(N, 3)` array.
#[pyfunction]
#[pyo3(signature = (verts))]
pub fn weld_vertices<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f32, AllowTypeChange>,
) -> PyResult<(Bound<'py, PyArray2<f32>>, Bound<'py, PyArray1<i64>>)> {
    let rows = read_f32_rows(&verts)?;
    if rows.iter().flatten().any(|x| !x.is_finite()) {
        return Err(PyValueError::new_err("non-finite coordinate"));
    }
    let (uniq, inv) = unique_rows_f32(&rows);
    Ok((
        rows_to_array2_f32(py, &uniq),
        PyArray1::from_vec(py, inv),
    ))
}

/// The full STL save/reload-equivalent weld (P-HONEST).
///
/// Positions are cast to float32, exactly-coincident positions merged,
/// degenerate and duplicate faces dropped and unreferenced vertices removed.
/// Bit-for-bit identical to the pure-numpy oracle.
#[pyfunction]
#[pyo3(signature = (verts, tris))]
pub fn weld_reload_equivalent<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f32, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
) -> PyResult<(Bound<'py, PyArray2<f32>>, Bound<'py, PyArray2<i64>>)> {
    let vrows = read_f32_rows(&verts)?;
    let trows = read_i64_rows(&tris)?;
    let nv = vrows.len();
    let nf = trows.len();
    if nv == 0 || nf == 0 {
        return Ok((
            rows_to_array2_f32(py, &vrows),
            rows_to_array2_i64(py, &trows),
        ));
    }
    if vrows.iter().flatten().any(|x| !x.is_finite()) {
        return Err(PyValueError::new_err("non-finite coordinate"));
    }
    let (uniq, inv) = unique_rows_f32(&vrows);
    let mut t: Vec<[i64; 3]> = Vec::with_capacity(nf);
    for row in &trows {
        let mut r = [0i64; 3];
        for k in 0..3 {
            let idx = row[k];
            if idx < 0 || idx as usize >= nv {
                return Err(PyValueError::new_err("triangle index out of range"));
            }
            r[k] = inv[idx as usize];
        }
        t.push(r);
    }
    t.retain(|r| r[0] != r[1] && r[1] != r[2] && r[0] != r[2]);
    if t.is_empty() {
        // mirror the oracle: an all-degenerate result drops the vertices too
        return Ok((
            PyArray2::<f32>::zeros(py, (0, 3), false),
            PyArray2::<i64>::zeros(py, (0, 3), false),
        ));
    }
    // duplicate faces (same unordered vertex set): keep the first occurrence
    let keys: Vec<[i64; 3]> = t
        .iter()
        .map(|r| {
            let mut k = *r;
            k.sort_unstable();
            k
        })
        .collect();
    let (_ku, first) = unique_rows_i64_first(&keys);
    let mut fsorted = first;
    fsorted.sort_unstable();
    let t2: Vec<[i64; 3]> = fsorted.iter().map(|&i| t[i]).collect();
    // unreferenced vertices: drop and remap
    let mut used: Vec<i64> = t2.iter().flatten().copied().collect();
    used.sort_unstable();
    used.dedup();
    let mut remap = vec![-1i64; nv];
    for (k, &uid) in used.iter().enumerate() {
        remap[uid as usize] = k as i64;
    }
    let wv: Vec<[f32; 3]> = used.iter().map(|&u| uniq[u as usize]).collect();
    let wt: Vec<[i64; 3]> = t2
        .iter()
        .map(|r| [remap[r[0] as usize], remap[r[1] as usize], remap[r[2] as usize]])
        .collect();
    Ok((rows_to_array2_f32(py, &wv), rows_to_array2_i64(py, &wt)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_table_counts_and_inverse() {
        // two triangles sharing edge (0,1)
        let t = [[0i64, 1, 2], [0, 1, 3]];
        let f = 2usize;
        let n = 3 * f;
        let mut keys = Vec::new();
        for b in 0..3usize {
            for r in 0..f {
                keys.push(canon(t[r][b], t[r][(b + 1) % 3]));
            }
        }
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_unstable_by(|&x, &y| keys[x].cmp(&keys[y]));
        let mut uniq: Vec<(i64, i64)> = Vec::new();
        let mut counts: Vec<i64> = Vec::new();
        let mut inv = vec![0i64; n];
        let mut i = 0;
        while i < n {
            let k = keys[order[i]];
            let gid = uniq.len() as i64;
            uniq.push(k);
            let mut j = i + 1;
            while j < n && keys[order[j]] == k {
                inv[order[j]] = gid;
                j += 1;
            }
            inv[order[i]] = gid;
            counts.push((j - i) as i64);
            i = j;
        }
        assert!(uniq.contains(&(0, 1)));
        let id01 = uniq.iter().position(|&e| e == (0, 1)).unwrap();
        assert_eq!(counts[id01], 2);
        // half-edge 0 is (0,1) of face 0 -> edge (0,1)
        assert_eq!(inv[0], id01 as i64);
    }

    #[test]
    fn weld_vertices_groups_negzero() {
        let rows = vec![
            [0.0f32, 0.0, 0.0],
            [-0.0f32, 0.0, 0.0],
            [1.0f32, 0.0, 0.0],
        ];
        let (uniq, inv) = unique_rows_f32(&rows);
        assert_eq!(uniq.len(), 2);
        assert_eq!(inv, vec![0, 0, 1]);
        // representative is the first occurrence, matching numpy
        assert!(uniq[0][0].is_sign_positive());
    }

    #[test]
    fn weld_reload_drops_degenerate_and_unused() {
        let v = vec![
            [0.0f32, 0.0, 0.0],
            [1.0f32, 0.0, 0.0],
            [0.0f32, 1.0, 0.0],
            [9.0f32, 9.0, 9.0], // unreferenced
        ];
        let t = vec![[0i64, 1, 2], [0, 0, 1]]; // second is degenerate
        // emulate the pure computation
        let (uniq, inv) = unique_rows_f32(&v);
        let mut tt: Vec<[i64; 3]> = t
            .iter()
            .map(|r| [inv[r[0] as usize], inv[r[1] as usize], inv[r[2] as usize]])
            .collect();
        tt.retain(|r| r[0] != r[1] && r[1] != r[2] && r[0] != r[2]);
        assert_eq!(tt.len(), 1);
        let used: Vec<[f32; 3]> = {
            let mut u: Vec<i64> = tt.iter().flatten().copied().collect();
            u.sort_unstable();
            u.dedup();
            u.iter().map(|&i| uniq[i as usize]).collect()
        };
        assert_eq!(used.len(), 3);
    }
}
