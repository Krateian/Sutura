//! decimate — native quadric mesh simplification (Faz 4 W3b of the
//! PyMeshLab-removal roadmap).
//!
//! The module wraps the `meshopt` crate (MIT OR Apache-2.0), a thin Rust
//! binding over the vendored meshoptimizer C++ `simplify` core (MIT), behind
//! the contract the PyMeshLab `meshing_decimation_quadric_edge_collapse` call
//! sites need: reduce a triangle mesh towards a target face count while the
//! surviving vertex positions stay bit-identical to the input.
//!
//! Contract:
//! * the input is welded on the f32 *bit pattern* of the position before
//!   simplification — meshopt needs a shared index buffer, and STL-style
//!   split vertices would otherwise read as topological borders.  All merged
//!   duplicates keep the first occurrence's position; no coordinate is
//!   modified.
//! * vertices that do not survive are dropped; the surviving f64 positions
//!   are copied from the input unchanged (there is no f32 round-trip in the
//!   returned array).
//! * degenerate faces created by the weld are removed, mirroring the
//!   `autoclean=True` the PyMeshLab call sites passed.
//! * `lock_border=true` maps to `SimplifyOptions::LockBorder` (the PyMeshLab
//!   `preserveboundary` flag).  Normal and topology preservation have no
//!   meshopt equivalent; the Python call sites keep their own validity
//!   guards (dressing `_decimate_coat` SI rollback, fTetWild `None`
//!   fallback, proxy `_check_validity`).
//! * `attributes` (optionally per-vertex normals) selects
//!   `simplify_with_attributes_and_locks` with a unit weight per attribute;
//!   values are averaged over the vertices a weld merged.
//!
//! `target_faces` is reached as closely as meshopt's error limit allows:
//! `target_error` (default `1.0`, i.e. 100 % of the mesh extent) is the
//! relaxation bound, so a large value lets the target be reached.  The
//! callers all guard on validity, so an inexact count is acceptable and
//! reported through `faces_out`/`result_error`.

use std::collections::HashMap;

use numpy::ndarray::Array2;
use numpy::{AllowTypeChange, PyArray2, PyArrayLike2};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use meshopt::{simplify, simplify_with_attributes_and_locks, typed_to_bytes, SimplifyOptions, VertexDataAdapter};

type V3 = [f64; 3];

/// Normalised f32 bit pattern of a position, so that `-0.0` and `0.0` weld
/// together and the key matches what meshopt itself sees after the f32 cast.
#[inline]
fn weld_key(p: V3) -> (u32, u32, u32) {
    let f = |x: f64| {
        let y = x as f32;
        let y = if y == 0.0 { 0.0f32 } else { y };
        y.to_bits()
    };
    (f(p[0]), f(p[1]), f(p[2]))
}

/// Result of a simplification run.
pub struct DecimateReport {
    pub verts: Vec<V3>,
    pub tris: Vec<[i64; 3]>,
    pub faces_in: usize,
    pub faces_out: usize,
    pub weld_merged: usize,
    pub welded_vertices: usize,
    pub degenerate_removed: usize,
    pub target_error_used: f64,
    pub result_error: f64,
    pub lock_border: bool,
    pub attributes_used: usize,
    pub no_op: bool,
}

/// Core simplification, free of the Python layer so the Rust tests can call it.
///
/// `attributes` is `Some((flat, stride))` with `flat.len() == verts.len() * stride`.
pub fn decimate_impl(
    verts_in: &[V3],
    tris_in: &[[i64; 3]],
    target_faces: usize,
    target_error: Option<f64>,
    lock_border: bool,
    attributes: Option<(&[f64], usize)>,
) -> PyResult<DecimateReport> {
    let nv_in = verts_in.len();
    let nf_in = tris_in.len();

    let attr_stride = attributes.map(|(_, s)| s).unwrap_or(0);
    if let Some((flat, stride)) = attributes {
        if stride == 0 || flat.len() != nv_in * stride {
            return Err(PyValueError::new_err(
                "attributes must hold one row of the same width per vertex",
            ));
        }
    }

    // --- validate the index buffer --------------------------------------- #
    for f in tris_in {
        for &v in f {
            if v < 0 || v as usize >= nv_in {
                return Err(PyValueError::new_err(format!(
                    "tris contains vertex index {v} outside 0..{nv_in}"
                )));
            }
        }
    }

    let no_op_report = |welded: usize, merged: usize| -> PyResult<DecimateReport> {
        Ok(DecimateReport {
            verts: verts_in.to_vec(),
            tris: tris_in.to_vec(),
            faces_in: nf_in,
            faces_out: nf_in,
            weld_merged: merged,
            welded_vertices: welded,
            degenerate_removed: 0,
            target_error_used: 0.0,
            result_error: 0.0,
            lock_border,
            attributes_used: attr_stride,
            no_op: true,
        })
    };

    if nf_in == 0 || nv_in == 0 {
        return no_op_report(nv_in, 0);
    }
    if target_faces == 0 || target_faces >= nf_in {
        // Nothing to gain (or an empty request): return the input unchanged.
        // The weld bookkeeping is still reported so the caller can see it.
        let (welded, merged) = weld_only(verts_in);
        return no_op_report(welded, merged);
    }

    // --- exact-position weld --------------------------------------------- #
    let mut map: HashMap<(u32, u32, u32), usize> = HashMap::with_capacity(nv_in);
    let mut welded_pos: Vec<V3> = Vec::with_capacity(nv_in);
    let mut weld_of: Vec<usize> = Vec::with_capacity(nv_in);
    let mut attr_sum: Vec<f64> = if attributes.is_some() {
        vec![0.0; nv_in * attr_stride]
    } else {
        Vec::new()
    };
    let mut attr_cnt: Vec<f64> = if attributes.is_some() {
        vec![0.0; nv_in]
    } else {
        Vec::new()
    };

    for (i, p) in verts_in.iter().enumerate() {
        let key = weld_key(*p);
        let id = match map.get(&key) {
            Some(&id) => id,
            None => {
                let id = welded_pos.len();
                welded_pos.push(*p);
                map.insert(key, id);
                id
            }
        };
        weld_of.push(id);
        if let Some((flat, stride)) = attributes {
            for j in 0..stride {
                attr_sum[id * stride + j] += flat[i * stride + j];
            }
            attr_cnt[id] += 1.0;
        }
    }
    let weld_merged = nv_in - welded_pos.len();

    // Welded attributes (average over merged duplicates).
    let attr_avg: Vec<f64> = if let Some((_, stride)) = attributes {
        let mut out = vec![0.0f64; welded_pos.len() * stride];
        for v in 0..welded_pos.len() {
            let c = attr_cnt[v];
            for j in 0..stride {
                out[v * stride + j] = if c > 0.0 { attr_sum[v * stride + j] / c } else { 0.0 };
            }
        }
        out
    } else {
        Vec::new()
    };

    // --- welded index buffer, degenerate faces dropped -------------------- #
    let mut welded_tris: Vec<[u32; 3]> = Vec::with_capacity(nf_in);
    let mut degenerate_removed = 0usize;
    for f in tris_in {
        let a = weld_of[f[0] as usize] as u32;
        let b = weld_of[f[1] as usize] as u32;
        let c = weld_of[f[2] as usize] as u32;
        if a == b || b == c || a == c {
            degenerate_removed += 1;
            continue;
        }
        welded_tris.push([a, b, c]);
    }
    if welded_tris.is_empty() {
        let mut r = no_op_report(welded_pos.len(), weld_merged)?;
        r.degenerate_removed = degenerate_removed;
        return Ok(r);
    }
    if target_faces * 3 >= welded_tris.len() * 3 {
        let mut r = no_op_report(welded_pos.len(), weld_merged)?;
        r.degenerate_removed = degenerate_removed;
        return Ok(r);
    }
    let positions_f32: Vec<[f32; 3]> = welded_pos
        .iter()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    let adapter = VertexDataAdapter::new(typed_to_bytes(&positions_f32), 12, 0)
        .map_err(|e| PyValueError::new_err(format!("meshopt vertex adapter: {e}")))?;

    let mut indices: Vec<u32> = Vec::with_capacity(welded_tris.len() * 3);
    for f in &welded_tris {
        indices.extend_from_slice(f);
    }

    let target_index_count = target_faces * 3;
    let err_in = target_error.unwrap_or(1.0) as f32;
    let options = if lock_border {
        SimplifyOptions::LockBorder
    } else {
        SimplifyOptions::None
    };
    let mut result_error = 0.0f32;

    let simplified: Vec<u32> = match attributes {
        None => simplify(
            &indices,
            &adapter,
            target_index_count,
            err_in,
            options,
            Some(&mut result_error),
        ),
        Some(_) => {
            let flat32: Vec<f32> = attr_avg.iter().map(|&v| v as f32).collect();
            let weights: Vec<f32> = vec![1.0; attr_stride];
            let locks: Vec<bool> = vec![false; welded_pos.len()];
            simplify_with_attributes_and_locks(
                &indices,
                &adapter,
                &flat32,
                &weights,
                attr_stride,
                &locks,
                target_index_count,
                err_in,
                options,
                Some(&mut result_error),
            )
        }
    };

    if simplified.is_empty() {
        let mut r = no_op_report(welded_pos.len(), weld_merged)?;
        r.degenerate_removed = degenerate_removed;
        return Ok(r);
    }

    // --- compact the surviving vertices, preserving the f64 positions ----- #
    let mut used: Vec<u32> = simplified.clone();
    used.sort_unstable();
    used.dedup();
    let mut remap: Vec<i64> = vec![-1; welded_pos.len()];
    for (new_id, &old) in used.iter().enumerate() {
        remap[old as usize] = new_id as i64;
    }
    let out_verts: Vec<V3> = used.iter().map(|&v| welded_pos[v as usize]).collect();
    let out_tris: Vec<[i64; 3]> = simplified
        .chunks_exact(3)
        .map(|t| [
            remap[t[0] as usize],
            remap[t[1] as usize],
            remap[t[2] as usize],
        ])
        .collect();

    Ok(DecimateReport {
        faces_in: nf_in,
        faces_out: out_tris.len(),
        verts: out_verts,
        tris: out_tris,
        weld_merged,
        welded_vertices: welded_pos.len(),
        degenerate_removed,
        target_error_used: err_in as f64,
        result_error: result_error as f64,
        lock_border,
        attributes_used: attr_stride,
        no_op: false,
    })
}

/// Weld bookkeeping only (used for the no-op paths).
fn weld_only(verts_in: &[V3]) -> (usize, usize) {
    let mut map: HashMap<(u32, u32, u32), usize> = HashMap::with_capacity(verts_in.len());
    for p in verts_in {
        let key = weld_key(*p);
        map.entry(key).or_insert(0);
    }
    (map.len(), verts_in.len() - map.len())
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

/// Native quadric simplification (Faz 4 W3b).
///
/// `verts`/`tris` are the input mesh, `target_faces` the requested face count.
/// `target_error` is the relative error limit (default `1.0`), `lock_border`
/// maps to meshopt's `LockBorder` (PyMeshLab `preserveboundary`), and
/// `attributes` is an optional `NxK` per-vertex attribute array (e.g. normals)
/// to weigh the collapse.  Returns `(verts, tris, report)` where `report` is a
/// dict (`faces_in`, `faces_out`, `target_faces`, `weld_merged`,
/// `welded_vertices`, `degenerate_removed`, `target_error_used`,
/// `result_error`, `lock_border`, `attributes_used`, `no_op`, `engine`).
#[pyfunction]
#[pyo3(signature = (verts, tris, target_faces, target_error=None, lock_border=false, attributes=None))]
pub fn decimate_qem<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    target_faces: usize,
    target_error: Option<f64>,
    lock_border: bool,
    attributes: Option<PyArrayLike2<'py, f64, AllowTypeChange>>,
) -> PyResult<(
    Bound<'py, PyArray2<f64>>,
    Bound<'py, PyArray2<i64>>,
    Bound<'py, PyDict>,
)> {
    let vrows = read_f64_rows(&verts)?;
    let trows = read_i64_rows(&tris)?;

    let attr_flat: Option<Vec<f64>> = match &attributes {
        None => None,
        Some(a) => {
            let arr = a.as_array();
            let k = arr.ncols();
            if k == 0 {
                return Err(PyValueError::new_err("attributes must have at least one column"));
            }
            if arr.nrows() != vrows.len() {
                return Err(PyValueError::new_err(
                    "attributes must have the same number of rows as verts",
                ));
            }
            let mut flat = Vec::with_capacity(vrows.len() * k);
            for i in 0..vrows.len() {
                for j in 0..k {
                    flat.push(arr[[i, j]]);
                }
            }
            Some(flat)
        }
    };
    let attr_stride = attr_flat
        .as_ref()
        .zip(attributes.as_ref())
        .map(|(_, a)| a.as_array().ncols())
        .unwrap_or(0);
    let att_ref = attr_flat.as_deref().map(|f| (f, attr_stride));

    let rep = decimate_impl(&vrows, &trows, target_faces, target_error, lock_border, att_ref)?;

    let dict = PyDict::new(py);
    dict.set_item("faces_in", rep.faces_in)?;
    dict.set_item("faces_out", rep.faces_out)?;
    dict.set_item("target_faces", target_faces)?;
    dict.set_item("weld_merged", rep.weld_merged)?;
    dict.set_item("welded_vertices", rep.welded_vertices)?;
    dict.set_item("degenerate_removed", rep.degenerate_removed)?;
    dict.set_item("target_error_used", rep.target_error_used)?;
    dict.set_item("result_error", rep.result_error)?;
    dict.set_item("lock_border", rep.lock_border)?;
    dict.set_item("attributes_used", rep.attributes_used)?;
    dict.set_item("no_op", rep.no_op)?;
    dict.set_item("engine", "meshopt")?;

    Ok((
        rows_to_array2_f64(py, &rep.verts),
        rows_to_array2_i64(py, &rep.tris),
        dict,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> (Vec<V3>, Vec<[i64; 3]>) {
        let v: Vec<V3> = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        let t: Vec<[i64; 3]> = vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ];
        (v, t)
    }

    /// A subdivided icosahedron (80 faces, closed, smooth) — a shape whose
    /// reduction to a target is reliable.
    fn sphere80() -> (Vec<V3>, Vec<[i64; 3]>) {
        let t = (1.0 + 5.0f64.sqrt()) / 2.0;
        let mut v: Vec<V3> = Vec::new();
        for &(a, b, c) in &[
            (-1.0, t, 0.0),
            (1.0, t, 0.0),
            (-1.0, -t, 0.0),
            (1.0, -t, 0.0),
            (0.0, -1.0, t),
            (0.0, 1.0, t),
            (0.0, -1.0, -t),
            (0.0, 1.0, -t),
            (t, 0.0, -1.0),
            (t, 0.0, 1.0),
            (-t, 0.0, -1.0),
            (-t, 0.0, 1.0),
        ] {
            let n = (a * a + b * b + c * c).sqrt();
            v.push([a / n, b / n, c / n]);
        }
        let faces: Vec<[i64; 3]> = vec![
            [0, 11, 5],
            [0, 5, 1],
            [0, 1, 7],
            [0, 7, 10],
            [0, 10, 11],
            [1, 5, 9],
            [5, 11, 4],
            [11, 10, 2],
            [10, 7, 6],
            [7, 1, 8],
            [3, 9, 4],
            [3, 4, 2],
            [3, 2, 6],
            [3, 6, 8],
            [3, 8, 9],
            [4, 9, 5],
            [2, 4, 11],
            [6, 2, 10],
            [8, 6, 7],
            [9, 8, 1],
        ];
        // Midpoint subdivision, one level: 20 -> 80 faces.
        let mut mid: HashMap<(i64, i64), i64> = HashMap::new();
        let mut out: Vec<[i64; 3]> = Vec::new();
        for f in &faces {
            let mut m = [0i64; 3];
            for k in 0..3 {
                let (a, b) = (f[k], f[(k + 1) % 3]);
                let key = if a < b { (a, b) } else { (b, a) };
                m[k] = *mid.entry(key).or_insert_with(|| {
                    let pa = v[a as usize];
                    let pb = v[b as usize];
                    let p = [
                        (pa[0] + pb[0]) / 2.0,
                        (pa[1] + pb[1]) / 2.0,
                        (pa[2] + pb[2]) / 2.0,
                    ];
                    let n = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
                    v.push([p[0] / n, p[1] / n, p[2] / n]);
                    (v.len() - 1) as i64
                });
            }
            out.push([f[0], m[0], m[2]]);
            out.push([f[1], m[1], m[0]]);
            out.push([f[2], m[2], m[1]]);
            out.push([m[0], m[1], m[2]]);
        }
        (v, out)
    }

    fn assert_valid(v: &[V3], t: &[[i64; 3]]) {
        for f in t {
            for &i in f {
                assert!(i >= 0 && (i as usize) < v.len(), "index out of range");
            }
        }
        for p in v {
            assert!(p.iter().all(|c| c.is_finite()), "non-finite vertex");
        }
    }

    #[test]
    fn sphere_reduces_towards_target() {
        let (v, t) = sphere80();
        assert_eq!(t.len(), 80);
        let r = decimate_impl(&v, &t, 20, None, false, None).unwrap();
        assert!(!r.no_op);
        assert!(r.faces_out > 0, "no faces survived");
        assert!(r.faces_out <= 20, "target overshot: {}", r.faces_out);
        assert_valid(&r.verts, &r.tris);
        assert_eq!(r.weld_merged, 0, "a clean sphere should not weld");
        assert!(r.result_error >= 0.0);
    }

    #[test]
    fn surviving_positions_are_bit_identical() {
        let (v, t) = sphere80();
        let r = decimate_impl(&v, &t, 20, None, false, None).unwrap();
        for p in &r.verts {
            assert!(
                v.iter().any(|q| q == p),
                "output vertex {p:?} is not an input position"
            );
        }
    }

    #[test]
    fn split_corners_weld_and_degenerates_are_dropped() {
        // The cube with every corner duplicated per face: 24 vertices, and the
        // first face's second triangle replaced by a degenerate one.
        let (v8, t) = cube();
        let mut v: Vec<V3> = Vec::new();
        let mut t24: Vec<[i64; 3]> = Vec::new();
        for f in &t {
            let base = v.len() as i64;
            for &i in f {
                v.push(v8[i as usize]);
            }
            t24.push([base, base + 1, base + 2]);
        }
        t24.push([0, 0, 1]); // degenerate after the weld

        let r = decimate_impl(&v, &t24, 6, None, false, None).unwrap();
        assert_eq!(r.welded_vertices, 8, "corners should weld to 8 vertices");
        assert_eq!(r.weld_merged, v.len() - 8);
        assert!(r.degenerate_removed >= 1);
        assert_valid(&r.verts, &r.tris);
    }

    #[test]
    fn target_at_or_above_input_is_a_no_op() {
        let (v, t) = cube();
        let r = decimate_impl(&v, &t, t.len(), None, false, None).unwrap();
        assert!(r.no_op);
        assert_eq!(r.verts, v);
        assert_eq!(r.tris, t);
        assert_eq!(r.faces_out, t.len());
    }

    #[test]
    fn empty_mesh_is_a_no_op() {
        let r = decimate_impl(&[], &[], 100, None, false, None).unwrap();
        assert!(r.no_op);
        assert_eq!(r.faces_out, 0);
    }

    #[test]
    fn lock_border_keeps_the_boundary() {
        // A 3x3 plane grid: 9 vertices, 8 triangles, all 8 outer vertices on
        // the border.
        let mut v: Vec<V3> = Vec::new();
        for j in 0..3 {
            for i in 0..3 {
                v.push([i as f64, j as f64, 0.0]);
            }
        }
        let t: Vec<[i64; 3]> = vec![
            [0, 1, 3],
            [1, 4, 3],
            [1, 2, 4],
            [2, 5, 4],
            [3, 4, 6],
            [4, 7, 6],
            [4, 5, 7],
            [5, 8, 7],
        ];
        let border: Vec<V3> = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [2.0, 1.0, 0.0],
            [0.0, 2.0, 0.0],
            [1.0, 2.0, 0.0],
            [2.0, 2.0, 0.0],
        ];
        let r = decimate_impl(&v, &t, 4, None, true, None).unwrap();
        assert!(r.lock_border);
        assert!(r.faces_out <= 8);
        for p in &border {
            assert!(
                r.verts.iter().any(|q| q == p),
                "locked border vertex {p:?} was dropped"
            );
        }
        assert_valid(&r.verts, &r.tris);
    }

    #[test]
    fn attribute_path_runs() {
        let (v, t) = sphere80();
        let normals: Vec<f64> = v.iter().flat_map(|p| p.iter().copied()).collect();
        let r = decimate_impl(&v, &t, 20, None, false, Some((&normals, 3))).unwrap();
        assert_eq!(r.attributes_used, 3);
        assert!(r.faces_out > 0 && r.faces_out <= 20);
        assert_valid(&r.verts, &r.tris);
    }

    #[test]
    fn out_of_range_index_is_rejected() {
        let (v, t) = cube();
        let mut bad = t.clone();
        bad[0][0] = 99;
        assert!(decimate_impl(&v, &bad, 6, None, false, None).is_err());
    }

    #[test]
    fn attribute_length_is_validated() {
        let (v, t) = sphere80();
        let short = vec![0.0f64; 6]; // not v.len() * 3
        assert!(decimate_impl(&v, &t, 20, None, false, Some((&short, 3))).is_err());
    }

    #[test]
    fn explicit_error_bound_is_honoured() {
        let (v, t) = sphere80();
        // A tiny error bound stops the collapse early: fewer faces are removed
        // than with the permissive default.
        let tight = decimate_impl(&v, &t, 20, Some(1e-6), false, None).unwrap();
        let loose = decimate_impl(&v, &t, 20, None, false, None).unwrap();
        assert!(tight.faces_out >= loose.faces_out);
        assert!(tight.result_error <= loose.result_error + 1e-12);
    }
}
