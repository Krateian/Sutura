//! resample — native voxel resampling (Faz 4 W3c of the PyMeshLab-removal
//! roadmap).
//!
//! Replaces the two `generate_resampled_uniform_mesh` call sites in the proxy
//! tier (strategy 1 signed, strategy 2 unsigned-offset).  The Python caller
//! already resolves the voxel size and clamps the grid, so this module only
//! builds the scalar field on the grid and extracts its zero level set with
//! dual contouring (manifold, unlike marching cubes).
//!
//! Field:
//! * `absdist == false` -> the generalized-winding signed distance
//!   (`sdf_grid`'s `s` output, `sign(w - 0.5) * unsigned`), shifted by `offset`;
//! * `absdist == true`  -> the unsigned nearest-surface distance, shifted by
//!   `offset` (the caller passes `offset == voxel`).
//!
//! Unlike VCG's `generate_resampled_uniform_mesh` the extraction is dual
//! contouring, so the output is already a closed, two-manifold surface; the
//! caller still validates it (`_check_validity`) and drops debris components.

use numpy::ndarray::Array2;
use numpy::{AllowTypeChange, PyArray2, PyArrayLike2};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::{dual_contour, plan_grid, winding};

type V3 = [f64; 3];

/// Result of one resampling pass.
pub struct ResampleReport {
    pub verts: Vec<V3>,
    pub tris: Vec<[i64; 3]>,
    pub voxel: f64,
    pub offset: f64,
    pub absdist: bool,
    pub dims: [usize; 3],
    pub coarsened: bool,
    pub manifold: bool,
}

/// Resample `verts`/`tris` on a uniform voxel grid and extract the iso-surface.
pub fn resample_core(
    verts: &[V3],
    tris: &[[i64; 3]],
    voxel: f64,
    offset: f64,
    absdist: bool,
) -> Result<ResampleReport, String> {
    if !voxel.is_finite() || voxel <= 0.0 {
        return Err("voxel must be a positive finite number".to_string());
    }
    if !offset.is_finite() {
        return Err("offset must be finite".to_string());
    }
    if verts.len() < 3 || tris.is_empty() {
        return Err("empty or degenerate mesh".to_string());
    }
    for v in verts {
        if !v[0].is_finite() || !v[1].is_finite() || !v[2].is_finite() {
            return Err("verts must be finite".to_string());
        }
    }

    let nv = verts.len();
    let mut rt: Vec<[usize; 3]> = Vec::with_capacity(tris.len());
    for t in tris {
        if t[0] < 0
            || t[1] < 0
            || t[2] < 0
            || (t[0] as usize) >= nv
            || (t[1] as usize) >= nv
            || (t[2] as usize) >= nv
        {
            return Err("triangle index out of range".to_string());
        }
        rt.push([t[0] as usize, t[1] as usize, t[2] as usize]);
    }

    let mut bb = winding::Aabb::empty();
    for v in verts {
        bb.expand_point(*v);
    }
    let (bmin, bmax) = (bb.min, bb.max);

    // The padding must clear the offset band (the iso-surface can sit up to
    // `|offset|` outside the mesh bbox) plus a couple of boundary cells.
    let pad = ((offset.abs() / voxel).ceil() as usize) + 2;
    let (origin, voxel, dims, coarsened) = plan_grid(bmin, bmax, Some(voxel), 96.0, pad);

    let bvh = winding::MeshBvh::from_arrays(verts, &rt);
    let (_w, u, s) = bvh.sdf_grid(origin, voxel, dims);
    let off = offset as f32;
    let field: Vec<f32> = if absdist {
        u.iter().map(|&x| x - off).collect()
    } else {
        s.iter().map(|&x| x - off).collect()
    };
    let dc = dual_contour::dual_contour(&field, dims, origin, voxel);

    let out_tris: Vec<[i64; 3]> = dc
        .tris
        .iter()
        .map(|t| [t[0] as i64, t[1] as i64, t[2] as i64])
        .collect();
    Ok(ResampleReport {
        verts: dc.verts,
        tris: out_tris,
        voxel,
        offset,
        absdist,
        dims,
        coarsened,
        manifold: dc.manifold,
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

/// Native `generate_resampled_uniform_mesh` (Faz 4 W3c).
///
/// `voxel` is the absolute cell size in model units (the caller resolves and
/// clamps it).  `offset` shifts the extracted iso-surface; `absdist=True`
/// resamples the unsigned nearest-surface distance instead of the
/// generalized-winding signed distance.  Returns `(verts, tris, info)`.
#[pyfunction]
#[pyo3(signature = (verts, tris, voxel, offset=0.0, absdist=false))]
pub fn resample_uniform<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    voxel: f64,
    offset: f64,
    absdist: bool,
) -> PyResult<(
    Bound<'py, PyArray2<f64>>,
    Bound<'py, PyArray2<i64>>,
    Bound<'py, PyDict>,
)> {
    let vrows = read_f64_rows(&verts)?;
    let trows = read_i64_rows(&tris)?;
    let rep = resample_core(&vrows, &trows, voxel, offset, absdist)
        .map_err(PyValueError::new_err)?;
    let info = PyDict::new(py);
    info.set_item("voxel", rep.voxel)?;
    info.set_item("offset", rep.offset)?;
    info.set_item("absdist", rep.absdist)?;
    info.set_item("mode", if rep.absdist { "unsigned" } else { "signed" })?;
    info.set_item("dims", [rep.dims[0], rep.dims[1], rep.dims[2]])?;
    info.set_item("faces", rep.tris.len())?;
    info.set_item("coarsened", rep.coarsened)?;
    info.set_item("manifold", rep.manifold)?;
    Ok((
        rows_to_array2_f64(py, &rep.verts),
        rows_to_array2_i64(py, &rep.tris),
        info,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Axis-aligned unit cube centred at the origin (12 triangles).
    fn cube(size: f64) -> (Vec<V3>, Vec<[i64; 3]>) {
        let h = size / 2.0;
        let v = vec![
            [-h, -h, -h],
            [h, -h, -h],
            [h, h, -h],
            [-h, h, -h],
            [-h, -h, h],
            [h, -h, h],
            [h, h, h],
            [-h, h, h],
        ];
        let t = vec![
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

    #[test]
    fn signed_cube_is_watertight_manifold() {
        let (v, t) = cube(1.0);
        let rep = resample_core(&v, &t, 0.1, 0.0, false).unwrap();
        assert!(rep.tris.len() > 0, "no faces extracted");
        assert!(rep.manifold, "dual contouring did not yield a manifold mesh");
    }

    #[test]
    fn unsigned_offset_wraps_outside_the_input() {
        let (v, t) = cube(1.0);
        let rep = resample_core(&v, &t, 0.1, 0.1, true).unwrap();
        assert!(rep.tris.len() > 0, "no faces extracted");
        assert!(rep.manifold, "unsigned resample not manifold");
        // The outer offset surface sits at least `offset` beyond the input.
        let max_x = rep
            .verts
            .iter()
            .map(|p| p[0])
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(max_x > 0.5, "output did not extend past the input bbox: {max_x}");
    }

    #[test]
    fn output_order_is_deterministic() {
        let (v, t) = cube(1.0);
        let a = resample_core(&v, &t, 0.1, 0.0, false).unwrap();
        for _ in 0..3 {
            let b = resample_core(&v, &t, 0.1, 0.0, false).unwrap();
            assert_eq!(a.tris, b.tris);
            assert_eq!(a.verts, b.verts);
        }
    }

    #[test]
    fn rejects_bad_input() {
        let (v, t) = cube(1.0);
        assert!(resample_core(&v, &t, 0.0, 0.0, false).is_err());
        assert!(resample_core(&v, &t, -1.0, 0.0, false).is_err());
        assert!(resample_core(&v, &t, f64::NAN, 0.0, false).is_err());
        assert!(resample_core(&[], &[], 0.1, 0.0, false).is_err());
    }

    #[test]
    fn rejects_out_of_range_index() {
        let (v, _) = cube(1.0);
        let t = vec![[0, 1, 99]];
        assert!(resample_core(&v, &t, 0.1, 0.0, false).is_err());
    }
}
