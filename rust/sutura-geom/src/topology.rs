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
use pyo3::types::PyDict;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub(crate) struct DisjointSet {
    parent: Vec<usize>,
}

impl DisjointSet {
    pub(crate) fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    pub(crate) fn find(&mut self, mut x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        while self.parent[x] != root {
            let nxt = self.parent[x];
            self.parent[x] = root;
            x = nxt;
        }
        root
    }

    pub(crate) fn union(&mut self, x: usize, y: usize) {
        let rx = self.find(x);
        let ry = self.find(y);
        if rx != ry {
            self.parent[rx] = ry;
        }
    }
}

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

fn unique_rows_i64_last(rows: &[[i64; 3]]) -> Vec<usize> {
    let n = rows.len();
    if n <= 1 {
        return (0..n).collect();
    }
    let mut order: Vec<usize> = (0..n).rev().collect();
    order.sort_by(|&x, &y| rows[x].cmp(&rows[y]));
    let mut kept: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i < n {
        kept.push(order[i]);
        let k = rows[order[i]];
        let mut j = i + 1;
        while j < n && rows[order[j]] == k {
            j += 1;
        }
        i = j;
    }
    kept.sort_unstable();
    kept
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

/// Remove duplicate faces (matching VCGLib / PyMeshLab meshing_remove_duplicate_faces).
#[pyfunction]
#[pyo3(signature = (tris))]
pub fn remove_duplicate_faces<'py>(
    py: Python<'py>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
) -> PyResult<Bound<'py, PyArray2<i64>>> {
    let trows = read_i64_rows(&tris)?;
    let nf = trows.len();
    if nf <= 1 {
        return Ok(rows_to_array2_i64(py, &trows));
    }
    let keys: Vec<[i64; 3]> = trows
        .iter()
        .map(|r| {
            let mut k = *r;
            k.sort_unstable();
            k
        })
        .collect();
    let kept_indices = unique_rows_i64_last(&keys);
    if kept_indices.len() == nf {
        return Ok(rows_to_array2_i64(py, &trows));
    }
    let out: Vec<[i64; 3]> = kept_indices.iter().map(|&i| trows[i]).collect();
    Ok(rows_to_array2_i64(py, &out))
}

/// Remove null / degenerate faces (matching VCGLib / PyMeshLab meshing_remove_null_faces).
#[pyfunction]
#[pyo3(signature = (verts, tris))]
pub fn remove_null_faces<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f32, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
) -> PyResult<Bound<'py, PyArray2<i64>>> {
    let vrows = read_f32_rows(&verts)?;
    let trows = read_i64_rows(&tris)?;
    let nv = vrows.len();
    let mut out: Vec<[i64; 3]> = Vec::with_capacity(trows.len());
    for r in trows {
        if r[0] == r[1] || r[1] == r[2] || r[0] == r[2] {
            continue;
        }
        let (i0, i1, i2) = (r[0] as usize, r[1] as usize, r[2] as usize);
        if i0 >= nv || i1 >= nv || i2 >= nv {
            continue;
        }
        let p0 = vrows[i0];
        let p1 = vrows[i1];
        let p2 = vrows[i2];
        let d1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
        let d2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
        let cx = d1[1] * d2[2] - d1[2] * d2[1];
        let cy = d1[2] * d2[0] - d1[0] * d2[2];
        let cz = d1[0] * d2[1] - d1[1] * d2[0];
        let norm2 = cx * cx + cy * cy + cz * cz;
        if norm2 == 0.0 || !norm2.is_finite() {
            continue;
        }
        out.push(r);
    }
    Ok(rows_to_array2_i64(py, &out))
}

#[inline]
fn f32_canonical_bits(x: f32) -> u32 {
    let canonical = if x == 0.0 { 0.0 } else { x };
    canonical.to_bits()
}

/// Remove duplicate vertices (matching VCGLib / PyMeshLab meshing_remove_duplicate_vertices).
/// Welds coincident vertices keeping first-occurrence order, drops degenerate faces
/// with repeated vertex indices, and remaps surviving faces.
#[pyfunction]
#[pyo3(signature = (verts, tris))]
pub fn remove_duplicate_vertices<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f32, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
) -> PyResult<(Bound<'py, PyArray2<f32>>, Bound<'py, PyArray2<i64>>)> {
    let vrows = read_f32_rows(&verts)?;
    let trows = read_i64_rows(&tris)?;
    let nv = vrows.len();
    if nv == 0 {
        return Ok((rows_to_array2_f32(py, &vrows), rows_to_array2_i64(py, &trows)));
    }

    let mut map: HashMap<[u32; 3], usize> = HashMap::with_capacity(nv);
    let mut new_verts: Vec<[f32; 3]> = Vec::with_capacity(nv);
    let mut old_to_new: Vec<usize> = Vec::with_capacity(nv);

    for p in &vrows {
        let key = [
            f32_canonical_bits(p[0]),
            f32_canonical_bits(p[1]),
            f32_canonical_bits(p[2]),
        ];
        match map.get(&key) {
            Some(&idx) => old_to_new.push(idx),
            None => {
                let idx = new_verts.len();
                map.insert(key, idx);
                new_verts.push(*p);
                old_to_new.push(idx);
            }
        }
    }

    let has_duplicates = new_verts.len() < nv;
    let mut has_degenerate = false;
    for r in &trows {
        let (i0, i1, i2) = (r[0] as usize, r[1] as usize, r[2] as usize);
        if i0 >= nv || i1 >= nv || i2 >= nv {
            has_degenerate = true;
            break;
        }
        let (n0, n1, n2) = (old_to_new[i0], old_to_new[i1], old_to_new[i2]);
        if n0 == n1 || n1 == n2 || n0 == n2 {
            has_degenerate = true;
            break;
        }
    }

    if !has_duplicates && !has_degenerate {
        return Ok((rows_to_array2_f32(py, &vrows), rows_to_array2_i64(py, &trows)));
    }

    let mut new_tris: Vec<[i64; 3]> = Vec::with_capacity(trows.len());
    for r in &trows {
        let (i0, i1, i2) = (r[0] as usize, r[1] as usize, r[2] as usize);
        if i0 >= nv || i1 >= nv || i2 >= nv {
            continue;
        }
        let (n0, n1, n2) = (old_to_new[i0] as i64, old_to_new[i1] as i64, old_to_new[i2] as i64);
        if n0 == n1 || n1 == n2 || n0 == n2 {
            continue;
        }
        new_tris.push([n0, n1, n2]);
    }

    Ok((rows_to_array2_f32(py, &new_verts), rows_to_array2_i64(py, &new_tris)))
}

pub(crate) fn remove_unreferenced_vertices_core(
    vrows: &[[f32; 3]],
    trows: &[[i64; 3]],
) -> (Vec<[f32; 3]>, Vec<[i64; 3]>) {
    let nv = vrows.len();
    if nv == 0 {
        return (vrows.to_vec(), trows.to_vec());
    }

    let mut used = vec![false; nv];
    for r in trows {
        let (i0, i1, i2) = (r[0] as usize, r[1] as usize, r[2] as usize);
        if i0 < nv { used[i0] = true; }
        if i1 < nv { used[i1] = true; }
        if i2 < nv { used[i2] = true; }
    }

    let mut remap = vec![i64::MAX; nv];
    let mut new_verts: Vec<[f32; 3]> = Vec::with_capacity(nv);
    for i in 0..nv {
        if used[i] {
            remap[i] = new_verts.len() as i64;
            new_verts.push(vrows[i]);
        }
    }

    if new_verts.len() == nv {
        return (vrows.to_vec(), trows.to_vec());
    }

    let mut new_tris: Vec<[i64; 3]> = Vec::with_capacity(trows.len());
    for r in trows {
        let (i0, i1, i2) = (r[0] as usize, r[1] as usize, r[2] as usize);
        if i0 >= nv || i1 >= nv || i2 >= nv {
            continue;
        }
        new_tris.push([remap[i0], remap[i1], remap[i2]]);
    }

    (new_verts, new_tris)
}

/// Remove unreferenced vertices (matching VCGLib / PyMeshLab meshing_remove_unreferenced_vertices).
/// Compacts vertex array preserving original relative order of referenced vertices.
#[pyfunction]
#[pyo3(signature = (verts, tris))]
pub fn remove_unreferenced_vertices<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f32, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
) -> PyResult<(Bound<'py, PyArray2<f32>>, Bound<'py, PyArray2<i64>>)> {
    let vrows = read_f32_rows(&verts)?;
    let trows = read_i64_rows(&tris)?;
    let (wv, wt) = remove_unreferenced_vertices_core(&vrows, &trows);
    Ok((rows_to_array2_f32(py, &wv), rows_to_array2_i64(py, &wt)))
}

/// Remove connected components by face number (matching VCGLib / PyMeshLab
/// meshing_remove_connected_component_by_face_number).
/// Components of faces connected via shared undirected edges with fewer than
/// `min_faces` triangles are removed. If `remove_unreferenced` is true, dangling
/// vertices are pruned and faces reindexed.
#[pyfunction]
#[pyo3(signature = (verts, tris, min_faces, remove_unreferenced=true))]
pub fn remove_connected_components<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f32, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    min_faces: i64,
    remove_unreferenced: bool,
) -> PyResult<(Bound<'py, PyArray2<f32>>, Bound<'py, PyArray2<i64>>)> {
    let vrows = read_f32_rows(&verts)?;
    let trows = read_i64_rows(&tris)?;
    let nf = trows.len();
    if nf == 0 || min_faces <= 1 {
        return Ok((rows_to_array2_f32(py, &vrows), rows_to_array2_i64(py, &trows)));
    }

    let mut half_edges: Vec<((i64, i64), usize)> = Vec::with_capacity(3 * nf);
    for (f_idx, tri) in trows.iter().enumerate() {
        half_edges.push((canon(tri[0], tri[1]), f_idx));
        half_edges.push((canon(tri[1], tri[2]), f_idx));
        half_edges.push((canon(tri[2], tri[0]), f_idx));
    }
    half_edges.sort_unstable_by(|a, b| a.0.cmp(&b.0));

    let mut ds = DisjointSet::new(nf);
    let mut i = 0usize;
    let n_he = half_edges.len();
    while i < n_he {
        let edge = half_edges[i].0;
        let mut j = i + 1;
        while j < n_he && half_edges[j].0 == edge {
            ds.union(half_edges[i].1, half_edges[j].1);
            j += 1;
        }
        i = j;
    }

    let mut comp_sizes = vec![0usize; nf];
    for f in 0..nf {
        let root = ds.find(f);
        comp_sizes[root] += 1;
    }

    let min_sz = min_faces.max(0) as usize;
    let mut kept_tris: Vec<[i64; 3]> = Vec::with_capacity(nf);
    for f in 0..nf {
        let root = ds.find(f);
        if comp_sizes[root] >= min_sz {
            kept_tris.push(trows[f]);
        }
    }

    if kept_tris.len() == nf {
        return Ok((rows_to_array2_f32(py, &vrows), rows_to_array2_i64(py, &trows)));
    }

    if remove_unreferenced {
        let (wv, wt) = remove_unreferenced_vertices_core(&vrows, &kept_tris);
        Ok((rows_to_array2_f32(py, &wv), rows_to_array2_i64(py, &wt)))
    } else {
        Ok((rows_to_array2_f32(py, &vrows), rows_to_array2_i64(py, &kept_tris)))
    }
}

/// Invert face orientation (matching VCGLib / PyMeshLab meshing_invert_face_orientation).
/// Swaps the first two vertices of each triangle `[v0, v1, v2] -> [v1, v0, v2]`.
#[pyfunction]
#[pyo3(signature = (tris))]
pub fn invert_face_orientation<'py>(
    py: Python<'py>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
) -> PyResult<Bound<'py, PyArray2<i64>>> {
    let trows = read_i64_rows(&tris)?;
    let mut out: Vec<[i64; 3]> = Vec::with_capacity(trows.len());
    for r in trows {
        out.push([r[1], r[0], r[2]]);
    }
    Ok(rows_to_array2_i64(py, &out))
}

/// Re-orient faces coherently across manifold edges (matching VCGLib / PyMeshLab meshing_re_orient_faces_coherently).
///
/// Propagates orientation across adjacent faces sharing a 2-manifold edge via BFS on each
/// connected component. If any edge has >2 incident faces or a degenerate face exists, returns
/// an error matching VCGLib contract ("Mesh has some not 2-manifold faces, Orientability requires manifoldness").
#[derive(Debug, PartialEq, Eq)]
pub enum TopologyError {
    NonManifold,
}

#[pyfunction]
#[pyo3(signature = (tris))]
pub fn re_orient_faces_coherently<'py>(
    py: Python<'py>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
) -> PyResult<Bound<'py, PyArray2<i64>>> {
    let trows = read_i64_rows(&tris)?;
    let out = re_orient_faces_coherently_core(&trows).map_err(|e| match e {
        TopologyError::NonManifold => PyValueError::new_err(
            "Mesh has some not 2-manifold faces, Orientability requires manifoldness",
        ),
    })?;
    Ok(rows_to_array2_i64(py, &out))
}

pub fn re_orient_faces_coherently_core(trows: &[[i64; 3]]) -> Result<Vec<[i64; 3]>, TopologyError> {
    let nf = trows.len();
    if nf == 0 {
        return Ok(Vec::new());
    }

    let mut half_edges: Vec<((i64, i64), usize, i64, i64)> = Vec::with_capacity(3 * nf);
    for (f_idx, tri) in trows.iter().enumerate() {
        if tri[0] == tri[1] || tri[1] == tri[2] || tri[2] == tri[0] {
            return Err(TopologyError::NonManifold);
        }
        half_edges.push((canon(tri[0], tri[1]), f_idx, tri[0], tri[1]));
        half_edges.push((canon(tri[1], tri[2]), f_idx, tri[1], tri[2]));
        half_edges.push((canon(tri[2], tri[0]), f_idx, tri[2], tri[0]));
    }
    half_edges.sort_unstable_by(|a, b| a.0.cmp(&b.0));

    let mut adj: Vec<Vec<(usize, i64, i64)>> = vec![Vec::new(); nf];

    let mut i = 0;
    while i < half_edges.len() {
        let mut j = i + 1;
        while j < half_edges.len() && half_edges[j].0 == half_edges[i].0 {
            j += 1;
        }
        let count = j - i;
        if count > 2 {
            return Err(TopologyError::NonManifold);
        }
        if count == 2 {
            let f1 = half_edges[i].1;
            let f2 = half_edges[i + 1].1;
            if f1 != f2 {
                let (u, v) = half_edges[i].0;
                adj[f1].push((f2, u, v));
                adj[f2].push((f1, u, v));
            }
        }
        i = j;
    }

    let mut out: Vec<[i64; 3]> = trows.to_vec();
    let mut visited = vec![false; nf];
    let mut queue = std::collections::VecDeque::new();

    for start_f in 0..nf {
        if visited[start_f] {
            continue;
        }
        visited[start_f] = true;
        queue.push_back(start_f);

        while let Some(curr) = queue.pop_front() {
            let curr_tri = out[curr];
            // Explore in cyclic edge order of curr_tri
            for k in 0..3 {
                let a = curr_tri[k];
                let b = curr_tri[(k + 1) % 3];
                let e_canon = canon(a, b);
                for &(nxt, u, v) in &adj[curr] {
                    if (u, v) != e_canon {
                        continue;
                    }
                    if visited[nxt] {
                        continue;
                    }
                    // In curr_tri, directed edge is a -> b
                    let nxt_tri = out[nxt];
                    let mut pos_a = usize::MAX;
                    let mut pos_b = usize::MAX;
                    for idx in 0..3 {
                        if nxt_tri[idx] == a { pos_a = idx; }
                        if nxt_tri[idx] == b { pos_b = idx; }
                    }
                    if pos_a != usize::MAX && pos_b != usize::MAX {
                        if (pos_a + 1) % 3 == pos_b {
                            // nxt also has a -> b (same direction). Incoherent across edge!
                            // Swap pos_a and pos_b to invert nxt
                            out[nxt].swap(pos_a, pos_b);
                        }
                    }
                    visited[nxt] = true;
                    queue.push_back(nxt);
                }
            }
        }
    }

    Ok(out)
}

/// Repair non-manifold edges by pruning incident faces (matching VCGLib / PyMeshLab meshing_repair_non_manifold_edges).
///
/// Iteratively removes the minimum-area incident face from each non-manifold edge (degree > 2)
/// until all edges have at most 2 incident faces.
#[pyfunction]
#[pyo3(signature = (verts, tris))]
pub fn repair_non_manifold_edges<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f32, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
) -> PyResult<Bound<'py, PyArray2<i64>>> {
    let vrows = read_f32_rows(&verts)?;
    let trows = read_i64_rows(&tris)?;
    let out = repair_non_manifold_edges_core(&vrows, &trows);
    Ok(rows_to_array2_i64(py, &out))
}

pub fn repair_non_manifold_edges_core(
    vrows: &[[f32; 3]],
    trows: &[[i64; 3]],
) -> Vec<[i64; 3]> {
    let nf = trows.len();
    let nv = vrows.len();
    if nf == 0 {
        return Vec::new();
    }

    let mut areas: Vec<f64> = Vec::with_capacity(nf);
    for tri in trows {
        let i0 = tri[0];
        let i1 = tri[1];
        let i2 = tri[2];
        if i0 < 0 || i1 < 0 || i2 < 0 || (i0 as usize) >= nv || (i1 as usize) >= nv || (i2 as usize) >= nv {
            areas.push(0.0);
            continue;
        }
        let p0 = vrows[i0 as usize];
        let p1 = vrows[i1 as usize];
        let p2 = vrows[i2 as usize];
        let e1 = [
            (p1[0] - p0[0]) as f64,
            (p1[1] - p0[1]) as f64,
            (p1[2] - p0[2]) as f64,
        ];
        let e2 = [
            (p2[0] - p0[0]) as f64,
            (p2[1] - p0[1]) as f64,
            (p2[2] - p0[2]) as f64,
        ];
        let cross = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        let norm = (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt();
        areas.push(0.5 * norm);
    }

    let mut half_edges: Vec<((i64, i64), usize)> = Vec::with_capacity(3 * nf);
    let mut face_edges: Vec<[(i64, i64); 3]> = Vec::with_capacity(nf);
    for (f_idx, tri) in trows.iter().enumerate() {
        let e0 = canon(tri[0], tri[1]);
        let e1 = canon(tri[1], tri[2]);
        let e2 = canon(tri[2], tri[0]);
        face_edges.push([e0, e1, e2]);
        half_edges.push((e0, f_idx));
        half_edges.push((e1, f_idx));
        half_edges.push((e2, f_idx));
    }
    half_edges.sort_unstable_by(|a, b| a.0.cmp(&b.0));

    let mut edge_faces: std::collections::HashMap<(i64, i64), Vec<usize>> =
        std::collections::HashMap::new();

    let mut i = 0;
    let mut active_nm: Vec<(i64, i64)> = Vec::new();
    while i < half_edges.len() {
        let mut j = i + 1;
        while j < half_edges.len() && half_edges[j].0 == half_edges[i].0 {
            j += 1;
        }
        let edge = half_edges[i].0;
        let mut flist: Vec<usize> = Vec::with_capacity(j - i);
        for k in i..j {
            flist.push(half_edges[k].1);
        }
        if flist.len() > 2 {
            active_nm.push(edge);
        }
        edge_faces.insert(edge, flist);
        i = j;
    }

    if active_nm.is_empty() {
        return trows.to_vec();
    }

    let mut removed = vec![false; nf];

    while let Some(edge) = active_nm.pop() {
        if let Some(flist) = edge_faces.get_mut(&edge) {
            flist.retain(|&f| !removed[f]);
            if flist.len() <= 2 {
                continue;
            }
            let mut best_idx = 0;
            let mut min_area = areas[flist[0]];
            let mut min_f = flist[0];
            for (idx, &f) in flist.iter().enumerate().skip(1) {
                let a = areas[f];
                if a < min_area || (a == min_area && f < min_f) {
                    min_area = a;
                    min_f = f;
                    best_idx = idx;
                }
            }
            let victim = flist.swap_remove(best_idx);
            removed[victim] = true;

            if flist.len() > 2 {
                active_nm.push(edge);
            }

            for &other_e in &face_edges[victim] {
                if other_e != edge {
                    if let Some(other_list) = edge_faces.get_mut(&other_e) {
                        other_list.retain(|&f| !removed[f]);
                        if other_list.len() > 2 {
                            active_nm.push(other_e);
                        }
                    }
                }
            }
        }
    }

    let mut out: Vec<[i64; 3]> = Vec::with_capacity(nf);
    for (f_idx, tri) in trows.iter().enumerate() {
        if !removed[f_idx] {
            out.push(*tri);
        }
    }
    out
}

/// Complete geometric measures of a triangle mesh (matching VCGLib / PyMeshLab get_geometric_measures).
#[pyfunction]
#[pyo3(signature = (verts, tris))]
pub fn mesh_geometric_measures<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f32, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
) -> PyResult<Bound<'py, PyDict>> {
    let vrows = read_f32_rows(&verts)?;
    let trows = read_i64_rows(&tris)?;
    let (vol_opt, area) = mesh_geometric_measures_core(&vrows, &trows);
    let dict = PyDict::new(py);
    dict.set_item("surface_area", area)?;
    if let Some(vol) = vol_opt {
        dict.set_item("mesh_volume", vol)?;
    }
    Ok(dict)
}

pub fn mesh_geometric_measures_core(
    vrows: &[[f32; 3]],
    trows: &[[i64; 3]],
) -> (Option<f64>, f64) {
    let nf = trows.len();
    let nv = vrows.len();
    if nf == 0 {
        return (None, 0.0);
    }

    let mut area_sum = 0.0f64;
    let mut vol_sum = 0.0f64;

    for tri in trows {
        let i0 = tri[0];
        let i1 = tri[1];
        let i2 = tri[2];
        if i0 < 0 || i1 < 0 || i2 < 0 || (i0 as usize) >= nv || (i1 as usize) >= nv || (i2 as usize) >= nv {
            continue;
        }
        let p0 = vrows[i0 as usize];
        let p1 = vrows[i1 as usize];
        let p2 = vrows[i2 as usize];
        let p0_64 = [p0[0] as f64, p0[1] as f64, p0[2] as f64];
        let p1_64 = [p1[0] as f64, p1[1] as f64, p1[2] as f64];
        let p2_64 = [p2[0] as f64, p2[1] as f64, p2[2] as f64];

        let e1 = [p1_64[0] - p0_64[0], p1_64[1] - p0_64[1], p1_64[2] - p0_64[2]];
        let e2 = [p2_64[0] - p0_64[0], p2_64[1] - p0_64[1], p2_64[2] - p0_64[2]];
        let cross = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        let tri_area = 0.5 * (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt();
        area_sum += tri_area;

        let v_cross = [
            p1_64[1] * p2_64[2] - p1_64[2] * p2_64[1],
            p1_64[2] * p2_64[0] - p1_64[0] * p2_64[2],
            p1_64[0] * p2_64[1] - p1_64[1] * p2_64[0],
        ];
        vol_sum += p0_64[0] * v_cross[0] + p0_64[1] * v_cross[1] + p0_64[2] * v_cross[2];
    }

    let mut half_edges: Vec<(i64, i64)> = Vec::with_capacity(3 * nf);
    for tri in trows {
        half_edges.push(canon(tri[0], tri[1]));
        half_edges.push(canon(tri[1], tri[2]));
        half_edges.push(canon(tri[2], tri[0]));
    }
    half_edges.sort_unstable();

    let mut is_closed_manifold = true;
    let mut i = 0;
    while i < half_edges.len() {
        let mut j = i + 1;
        while j < half_edges.len() && half_edges[j] == half_edges[i] {
            j += 1;
        }
        if (j - i) != 2 {
            is_closed_manifold = false;
            break;
        }
        i = j;
    }

    let vol = if is_closed_manifold {
        Some(vol_sum / 6.0)
    } else {
        None
    };

    (vol, area_sum)
}

/// Complete topological measures of a triangle mesh (matching VCGLib / PyMeshLab get_topological_measures).
#[pyfunction]
#[pyo3(signature = (verts, tris))]
pub fn mesh_topology<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f32, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
) -> PyResult<Bound<'py, PyDict>> {
    let vrows = read_f32_rows(&verts)?;
    let trows = read_i64_rows(&tris)?;
    let nv = vrows.len();
    let nf = trows.len();
    let dict = PyDict::new(py);

    if nf == 0 {
        dict.set_item("boundary_edges", 0)?;
        dict.set_item("connected_components_number", 0)?;
        dict.set_item("edges_number", 0)?;
        dict.set_item("faces_number", 0)?;
        dict.set_item("genus", 0)?;
        dict.set_item("incident_faces_on_non_two_manifold_edges", 0)?;
        dict.set_item("incident_faces_on_non_two_manifold_vertices", 0)?;
        dict.set_item("is_mesh_two_manifold", true)?;
        dict.set_item("non_two_manifold_edges", 0)?;
        dict.set_item("non_two_manifold_vertices", 0)?;
        dict.set_item("number_holes", 0)?;
        dict.set_item("unreferenced_vertices", nv)?;
        dict.set_item("vertices_number", nv)?;
        return Ok(dict);
    }

    let mut half_edges: Vec<((i64, i64), usize, (i64, i64))> = Vec::with_capacity(3 * nf);
    for (f_idx, tri) in trows.iter().enumerate() {
        half_edges.push((canon(tri[0], tri[1]), f_idx, (tri[0], tri[1])));
        half_edges.push((canon(tri[1], tri[2]), f_idx, (tri[1], tri[2])));
        half_edges.push((canon(tri[2], tri[0]), f_idx, (tri[2], tri[0])));
    }
    half_edges.sort_unstable_by(|a, b| a.0.cmp(&b.0));

    let mut ds = DisjointSet::new(nf);
    let mut boundary_edges = 0usize;
    let mut non_two_manifold_edges = 0usize;
    let mut unique_edges_count = 0usize;
    let mut incident_on_nm_edges = vec![false; nf];
    let mut boundary_directed: HashMap<i64, i64> = HashMap::new();
    let mut nm_edge_verts: HashSet<i64> = HashSet::new();

    let mut i = 0usize;
    let n_he = half_edges.len();
    while i < n_he {
        let edge = half_edges[i].0;
        unique_edges_count += 1;
        let mut j = i + 1;
        while j < n_he && half_edges[j].0 == edge {
            ds.union(half_edges[i].1, half_edges[j].1);
            j += 1;
        }
        let deg = j - i;
        if deg == 1 {
            boundary_edges += 1;
            boundary_directed.insert(half_edges[i].2.0, half_edges[i].2.1);
        } else if deg > 2 {
            non_two_manifold_edges += 1;
            nm_edge_verts.insert(edge.0);
            nm_edge_verts.insert(edge.1);
            for k in i..j {
                incident_on_nm_edges[half_edges[k].1] = true;
            }
        }
        i = j;
    }
    let incident_faces_on_non_two_manifold_edges =
        incident_on_nm_edges.iter().filter(|&&b| b).count();

    let mut root_set: Vec<usize> = (0..nf).map(|f| ds.find(f)).collect();
    root_set.sort_unstable();
    root_set.dedup();
    let connected_components_number = root_set.len();

    let mut v_faces: Vec<Vec<usize>> = vec![Vec::new(); nv];
    for (f_idx, tri) in trows.iter().enumerate() {
        let mut uv: Vec<usize> = Vec::with_capacity(3);
        for &v in tri {
            if v >= 0 && (v as usize) < nv {
                let vu = v as usize;
                if !uv.contains(&vu) {
                    uv.push(vu);
                }
            }
        }
        for v in uv {
            v_faces[v].push(f_idx);
        }
    }

    let mut non_two_manifold_vertices = 0usize;
    let mut incident_on_nm_verts = vec![false; nf];
    let mut unreferenced_vertices = 0usize;

    for v in 0..nv {
        let inc = &v_faces[v];
        if inc.is_empty() {
            unreferenced_vertices += 1;
            continue;
        }
        if nm_edge_verts.contains(&(v as i64)) {
            continue;
        }
        if inc.len() <= 1 {
            continue;
        }
        let n_inc = inc.len();
        let mut local_ds = DisjointSet::new(n_inc);
        let mut edge_neighbors: HashMap<i64, Vec<usize>> = HashMap::new();
        for (local_idx, &f_idx) in inc.iter().enumerate() {
            let tri = trows[f_idx];
            let mut other_v: Vec<i64> = Vec::with_capacity(2);
            for &w in &tri {
                if w != v as i64 && !other_v.contains(&w) {
                    other_v.push(w);
                }
            }
            for w in other_v {
                edge_neighbors.entry(w).or_default().push(local_idx);
            }
        }
        for flist in edge_neighbors.values() {
            if flist.len() > 1 {
                for k in 1..flist.len() {
                    local_ds.union(flist[0], flist[k]);
                }
            }
        }
        let mut star_roots: Vec<usize> = (0..n_inc).map(|idx| local_ds.find(idx)).collect();
        star_roots.sort_unstable();
        star_roots.dedup();
        if star_roots.len() > 1 {
            non_two_manifold_vertices += 1;
            for &f_idx in inc {
                incident_on_nm_verts[f_idx] = true;
            }
        }
    }
    let incident_faces_on_non_two_manifold_vertices =
        incident_on_nm_verts.iter().filter(|&&b| b).count();

    let is_mesh_two_manifold =
        non_two_manifold_edges == 0 && non_two_manifold_vertices == 0;

    let (genus, number_holes) = if is_mesh_two_manifold {
        let mut visited: HashSet<i64> = HashSet::new();
        let mut holes = 0i64;
        for &start in boundary_directed.keys() {
            if visited.contains(&start) {
                continue;
            }
            let mut curr = start;
            while let Some(&nxt) = boundary_directed.get(&curr) {
                if !visited.insert(curr) {
                    break;
                }
                curr = nxt;
                if curr == start {
                    holes += 1;
                    break;
                }
            }
        }
        let v_ref = (nv - unreferenced_vertices) as i64;
        let e_count = unique_edges_count as i64;
        let f_count = nf as i64;
        let chi = v_ref - e_count + f_count;
        let g = (connected_components_number as i64) - (holes + chi) / 2;
        (g, holes)
    } else {
        (-1i64, -1i64)
    };

    dict.set_item("boundary_edges", boundary_edges)?;
    dict.set_item("connected_components_number", connected_components_number)?;
    dict.set_item("edges_number", unique_edges_count)?;
    dict.set_item("faces_number", nf)?;
    dict.set_item("genus", genus)?;
    dict.set_item("incident_faces_on_non_two_manifold_edges", incident_faces_on_non_two_manifold_edges)?;
    dict.set_item("incident_faces_on_non_two_manifold_vertices", incident_faces_on_non_two_manifold_vertices)?;
    dict.set_item("is_mesh_two_manifold", is_mesh_two_manifold)?;
    dict.set_item("non_two_manifold_edges", non_two_manifold_edges)?;
    dict.set_item("non_two_manifold_vertices", non_two_manifold_vertices)?;
    dict.set_item("number_holes", number_holes)?;
    dict.set_item("unreferenced_vertices", unreferenced_vertices)?;
    dict.set_item("vertices_number", nv)?;

    Ok(dict)
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

    #[test]
    fn test_remove_unreferenced_vertices_core() {
        let v = vec![
            [0.0f32, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [5.0, 5.0, 5.0], // unreferenced
        ];
        let t = vec![[0i64, 1, 2]];
        let (nv, nt) = remove_unreferenced_vertices_core(&v, &t);
        assert_eq!(nv.len(), 3);
        assert_eq!(nt.len(), 1);
        assert_eq!(nt[0], [0, 1, 2]);
    }

    #[test]
    fn test_invert_face_orientation_swaps_first_two() {
        let t = [[0i64, 1, 2], [3, 4, 5]];
        let mut out = Vec::new();
        for r in t {
            out.push([r[1], r[0], r[2]]);
        }
        assert_eq!(out, vec![[1, 0, 2], [4, 3, 5]]);
    }

    #[test]
    fn test_disjoint_set() {
        let mut ds = DisjointSet::new(5);
        ds.union(0, 1);
        ds.union(2, 3);
        ds.union(3, 4);
        assert_eq!(ds.find(0), ds.find(1));
        assert_eq!(ds.find(2), ds.find(4));
        assert_ne!(ds.find(0), ds.find(2));
    }

    #[test]
    fn test_re_orient_faces_coherently_two_triangles() {
        // Two triangles sharing edge (1, 2)
        // f0: [0, 1, 2], edge is 1 -> 2
        // f1: [3, 1, 2], edge is 1 -> 2 (incoherent)
        let t = vec![[0, 1, 2], [3, 1, 2]];
        let res = re_orient_faces_coherently_core(&t).unwrap();
        assert_eq!(res[0], [0, 1, 2]);
        assert_eq!(res[1], [3, 2, 1]); // swapped 1 and 2
    }

    #[test]
    fn test_re_orient_faces_coherently_non_manifold_error() {
        // 3 triangles sharing edge (0, 1)
        let t = vec![[0, 1, 2], [0, 1, 3], [0, 1, 4]];
        assert!(re_orient_faces_coherently_core(&t).is_err());
    }

    #[test]
    fn test_re_orient_faces_coherently_degenerate_error() {
        // Degenerate triangle [0, 0, 1]
        let t = vec![[0, 0, 1], [0, 1, 2]];
        assert!(re_orient_faces_coherently_core(&t).is_err());
    }

    #[test]
    fn test_repair_non_manifold_edges_prunes_smallest_area() {
        let v = vec![
            [0.0f32, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],  // t0 area 0.5
            [0.0, 2.0, 0.0],  // t1 area 1.0
            [0.0, 0.1, 0.0],  // t2 area 0.05
        ];
        let t = vec![[0i64, 1, 2], [0, 1, 3], [0, 1, 4]];
        let res = repair_non_manifold_edges_core(&v, &t);
        // t2 (smallest area) should be pruned, leaving t0 and t1
        assert_eq!(res.len(), 2);
        assert_eq!(res, vec![[0, 1, 2], [0, 1, 3]]);
    }

    #[test]
    fn test_mesh_geometric_measures_cube() {
        let v = vec![
            [0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [1.0, 1.0, 1.0], [0.0, 1.0, 1.0],
        ];
        let t = vec![
            [0i64, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
            [0, 1, 5], [0, 5, 4], [1, 2, 6], [1, 6, 5],
            [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7],
        ];
        let (vol_opt, area) = mesh_geometric_measures_core(&v, &t);
        assert!((area - 6.0).abs() < 1e-6);
        assert!(vol_opt.is_some());
        let vol = vol_opt.unwrap();
        assert!((vol.abs() - 1.0).abs() < 1e-6);
    }
}

