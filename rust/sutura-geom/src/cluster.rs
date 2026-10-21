//! cluster — native vertex-clustering decimation for the render LODs
//! (Faz 4 W3a of the PyMeshLab-removal roadmap).
//!
//! Replaces the two `meshing_decimation_clustering` call sites: the
//! interactive-viewer LOD and the repeated-element picker LOD.  Both are
//! throwaway render loops, so manifoldness is not guaranteed and the output is
//! quality-compatible with VCG's clustering, not byte-identical.
//!
//! Contract:
//! * `threshold` is an absolute cell size in model units (the Python wrapper
//!   converts VCG's percentage threshold via `pct/100 * bbox_diagonal`);
//! * each occupied cell contributes ONE representative vertex, placed at the
//!   mean of its member positions;
//! * a face is kept only when its three vertices fall in three distinct cells
//!   (a face with a repeated cell is degenerate and dropped);
//! * unreferenced representatives are dropped; the output order is
//!   deterministic (first-appearance cell order, faces in input order).

use numpy::ndarray::Array2;
use numpy::{AllowTypeChange, PyArray2, PyArrayLike2};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use std::collections::HashMap;

type V3 = [f64; 3];

/// Result of one clustering pass.
pub struct ClusterReport {
    pub verts: Vec<V3>,
    pub tris: Vec<[i64; 3]>,
    pub faces_in: i64,
    pub faces_out: i64,
}

/// Cluster-decimate `verts`/`tris` to the given absolute `threshold`.
pub fn cluster_decimate_core(
    verts: &[V3],
    tris: &[[i64; 3]],
    threshold: f64,
) -> Result<ClusterReport, String> {
    if !threshold.is_finite() || threshold <= 0.0 {
        return Err("threshold must be a positive finite number".to_string());
    }
    let nv = verts.len();
    let faces_in = tris.len();
    if faces_in == 0 {
        return Ok(ClusterReport {
            verts: Vec::new(),
            tris: Vec::new(),
            faces_in: 0,
            faces_out: 0,
        });
    }
    for v in verts {
        if !v[0].is_finite() || !v[1].is_finite() || !v[2].is_finite() {
            return Err("verts must be finite".to_string());
        }
    }

    // Cell key per vertex; representative ids assigned in first-appearance
    // order so the output is independent of hash iteration order.
    let mut cell_to_rep: HashMap<(i64, i64, i64), usize> = HashMap::new();
    let mut rep_of_vertex: Vec<usize> = Vec::with_capacity(nv);
    let mut sums: Vec<V3> = Vec::new();
    let mut counts: Vec<f64> = Vec::new();
    for v in verts.iter() {
        let key = (
            (v[0] / threshold).floor() as i64,
            (v[1] / threshold).floor() as i64,
            (v[2] / threshold).floor() as i64,
        );
        let rep = *cell_to_rep.entry(key).or_insert_with(|| {
            let id = sums.len();
            sums.push([0.0, 0.0, 0.0]);
            counts.push(0.0);
            id
        });
        sums[rep][0] += v[0];
        sums[rep][1] += v[1];
        sums[rep][2] += v[2];
        counts[rep] += 1.0;
        rep_of_vertex.push(rep);
    }
    let n_rep = sums.len();

    let mut out_tris: Vec<[i64; 3]> = Vec::with_capacity(faces_in);
    for t in tris.iter() {
        let (a, b, c) = (t[0], t[1], t[2]);
        if a < 0
            || b < 0
            || c < 0
            || (a as usize) >= nv
            || (b as usize) >= nv
            || (c as usize) >= nv
        {
            return Err("triangle index out of range".to_string());
        }
        let ra = rep_of_vertex[a as usize];
        let rb = rep_of_vertex[b as usize];
        let rc = rep_of_vertex[c as usize];
        if ra == rb || rb == rc || ra == rc {
            continue;
        }
        out_tris.push([ra as i64, rb as i64, rc as i64]);
    }

    // Drop unreferenced representatives, compacting in first-appearance order.
    let mut used = vec![false; n_rep];
    for t in &out_tris {
        used[t[0] as usize] = true;
        used[t[1] as usize] = true;
        used[t[2] as usize] = true;
    }
    let mut remap = vec![i64::MAX; n_rep];
    let mut out_verts: Vec<V3> = Vec::new();
    for rep in 0..n_rep {
        if used[rep] {
            remap[rep] = out_verts.len() as i64;
            let inv = 1.0 / counts[rep];
            out_verts.push([sums[rep][0] * inv, sums[rep][1] * inv, sums[rep][2] * inv]);
        }
    }
    for t in out_tris.iter_mut() {
        t[0] = remap[t[0] as usize];
        t[1] = remap[t[1] as usize];
        t[2] = remap[t[2] as usize];
    }

    let faces_out = out_tris.len() as i64;
    Ok(ClusterReport {
        verts: out_verts,
        tris: out_tris,
        faces_in: faces_in as i64,
        faces_out,
    })
}

fn read_f64_rows(arr: &PyArrayLike2<'_, f64, AllowTypeChange>) -> PyResult<Vec<V3>> {
    let v = arr.as_array();
    if v.ncols() != 3 {
        return Err(PyValueError::new_err("verts must be Nx3"));
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

fn rows_to_array2_f64<'py>(py: Python<'py>, rows: &[V3]) -> Bound<'py, PyArray2<f64>> {
    let mut a = Array2::<f64>::zeros((rows.len(), 3));
    for (r, row) in rows.iter().enumerate() {
        a[[r, 0]] = row[0];
        a[[r, 1]] = row[1];
        a[[r, 2]] = row[2];
    }
    PyArray2::from_owned_array(py, a)
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

/// Native `meshing_decimation_clustering` (Faz 4 W3a).
///
/// `threshold` is the absolute cell size in model units.  Returns
/// `(verts, tris, faces_in, faces_out)`.
#[pyfunction]
#[pyo3(signature = (verts, tris, threshold))]
pub fn cluster_decimate<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    threshold: f64,
) -> PyResult<(Bound<'py, PyArray2<f64>>, Bound<'py, PyArray2<i64>>, i64, i64)> {
    let vrows = read_f64_rows(&verts)?;
    let trows = read_i64_rows(&tris)?;
    let rep = cluster_decimate_core(&vrows, &trows, threshold).map_err(PyValueError::new_err)?;
    Ok((
        rows_to_array2_f64(py, &rep.verts),
        rows_to_array2_i64(py, &rep.tris),
        rep.faces_in,
        rep.faces_out,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat `n x n` grid of unit-spaced quads (two triangles each).
    fn grid(n: usize, step: f64) -> (Vec<V3>, Vec<[i64; 3]>) {
        let mut v = Vec::with_capacity(n * n);
        for j in 0..n {
            for i in 0..n {
                v.push([i as f64 * step, j as f64 * step, 0.0]);
            }
        }
        let idx = |i: usize, j: usize| (j * n + i) as i64;
        let mut t = Vec::new();
        for j in 0..n - 1 {
            for i in 0..n - 1 {
                t.push([idx(i, j), idx(i + 1, j), idx(i + 1, j + 1)]);
                t.push([idx(i, j), idx(i + 1, j + 1), idx(i, j + 1)]);
            }
        }
        (v, t)
    }

    #[test]
    fn fine_threshold_preserves_every_vertex() {
        let (v, t) = grid(10, 1.0);
        let rep = cluster_decimate_core(&v, &t, 0.5).unwrap();
        assert_eq!(rep.faces_in, t.len() as i64);
        assert_eq!(rep.faces_out, t.len() as i64);
        assert_eq!(rep.verts.len(), v.len());
    }

    #[test]
    fn coarse_threshold_collapses_everything() {
        let (v, t) = grid(10, 1.0);
        let rep = cluster_decimate_core(&v, &t, 100.0).unwrap();
        assert_eq!(rep.faces_in, t.len() as i64);
        assert_eq!(rep.faces_out, 0);
        assert!(rep.verts.is_empty());
    }

    #[test]
    fn larger_threshold_never_adds_faces() {
        let (v, t) = grid(12, 1.0);
        let a = cluster_decimate_core(&v, &t, 0.5).unwrap().faces_out;
        let b = cluster_decimate_core(&v, &t, 1.5).unwrap().faces_out;
        let c = cluster_decimate_core(&v, &t, 3.0).unwrap().faces_out;
        assert!(a >= b && b >= c, "{a} {b} {c}");
        assert!(c < a);
    }

    #[test]
    fn representative_is_member_mean() {
        // v0,v1 share cell (0,0,0) -> one representative at their mean.
        let verts = vec![
            [0.0, 0.0, 0.0],
            [0.4, 0.0, 0.0],
            [5.0, 0.0, 0.0],
            [5.0, 1.0, 0.0],
        ];
        let tris = vec![[0, 2, 3], [1, 3, 2]];
        let rep = cluster_decimate_core(&verts, &tris, 1.0).unwrap();
        assert_eq!(rep.verts.len(), 3);
        assert_eq!(rep.verts[0], [0.2, 0.0, 0.0]);
        assert_eq!(rep.faces_out, 2);
    }

    #[test]
    fn deterministic_output() {
        let (v, t) = grid(12, 1.0);
        let a = cluster_decimate_core(&v, &t, 1.5).unwrap();
        for _ in 0..5 {
            let b = cluster_decimate_core(&v, &t, 1.5).unwrap();
            assert_eq!(a.tris, b.tris);
            assert_eq!(a.verts, b.verts);
        }
    }

    #[test]
    fn rejects_non_positive_threshold() {
        let (v, t) = grid(4, 1.0);
        assert!(cluster_decimate_core(&v, &t, 0.0).is_err());
        assert!(cluster_decimate_core(&v, &t, -1.0).is_err());
        assert!(cluster_decimate_core(&v, &t, f64::NAN).is_err());
    }

    #[test]
    fn rejects_out_of_range_index() {
        let verts = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let tris = vec![[0, 1, 9]];
        assert!(cluster_decimate_core(&verts, &tris, 0.5).is_err());
    }
}
