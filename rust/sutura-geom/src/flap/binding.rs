//! PyO3 binding for the Flap hole-filling core.
//!
//! `sutura_geom.flap_fill(verts, tris, **kwargs) -> (verts, tris, report)`
//! mirrors the validated Python prototype's public entry point (same keyword
//! arguments, same report keys).  Triangles are accepted and returned as
//! `int64`.

use numpy::{AllowTypeChange, PyArray2, PyArrayLike2};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};

use super::*;

fn parse_tris_i64(
    tris: &PyArrayLike2<'_, i64, AllowTypeChange>,
    n: usize,
) -> PyResult<Vec<T3>> {
    let tt = tris.as_array();
    if tt.ncols() != 3 {
        return Err(PyValueError::new_err("tris must be Mx3"));
    }
    let m = tt.nrows();
    let mut out: Vec<T3> = Vec::with_capacity(m);
    for i in 0..m {
        let (a, b, c) = (tt[[i, 0]], tt[[i, 1]], tt[[i, 2]]);
        if a < 0 || b < 0 || c < 0 {
            return Err(PyValueError::new_err("triangle index out of range"));
        }
        let (a, b, c) = (a as usize, b as usize, c as usize);
        if a >= n || b >= n || c >= n {
            return Err(PyValueError::new_err("triangle index out of range"));
        }
        out.push([a as u32, b as u32, c as u32]);
    }
    Ok(out)
}

fn norm_info_dict<'py>(py: Python<'py>, info: &topology::NormInfo) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("iterations", info.iterations)?;
    d.set_item("splits", info.splits)?;
    d.set_item("welds", info.welds)?;
    d.set_item("unbalanced", info.unbalanced)?;
    d.set_item("nm_edges", info.nm_edges)?;
    let sites = PyList::empty(py);
    for s in &info.weld_sites {
        sites.append(PyTuple::new(
            py,
            [
                PyList::new(py, s.a)?.into_any(),
                PyList::new(py, s.b)?.into_any(),
                (s.gap).into_pyobject(py)?.into_any(),
                (s.is_orig).into_pyobject(py)?.to_owned().into_any(),
            ],
        )?)?;
    }
    d.set_item("weld_sites", sites)?;
    Ok(d)
}

fn quality_dict<'py>(py: Python<'py>, q: &QualityAgg) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("n", q.n)?;
    let opt = |v: f64| -> PyResult<Option<f64>> {
        if q.n == 0 {
            Ok(None)
        } else {
            Ok(Some(v))
        }
    };
    d.set_item("min_angle_p5", opt(q.min_angle_p5)?)?;
    d.set_item("min_angle_p50", opt(q.min_angle_p50)?)?;
    d.set_item("aspect_p50", opt(q.aspect_p50)?)?;
    d.set_item("aspect_p95", opt(q.aspect_p95)?)?;
    Ok(d)
}

/// Fill boundary loops with minimum-area patches (Flap).
///
/// See the Python prototype `/tmp/v073/epoxy/flap.py` for the reference
/// semantics.  Returns `(verts, tris, report)`.
#[pyfunction]
#[pyo3(name = "flap_fill")]
#[pyo3(signature = (
    verts, tris,
    refine=true, fair=true, max_loop=1200, max_faces=400000,
    collect_quality=false, drop_lone_tris=true, weld_cracks=true,
    split_nm=true, separate_stl=true, bridge_open_chains=false,
    weld_max_frac=0.02, orient="reverse".to_string(), avoid_coplanar=true,
    coplanar_frac=0.05, coplanar_cos=0.99, sliver_cleanup=false
))]
#[allow(clippy::too_many_arguments)]
pub fn flap_fill_py<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    refine: bool,
    fair: bool,
    max_loop: usize,
    max_faces: usize,
    collect_quality: bool,
    drop_lone_tris: bool,
    weld_cracks: bool,
    split_nm: bool,
    separate_stl: bool,
    bridge_open_chains: bool,
    weld_max_frac: f64,
    orient: String,
    avoid_coplanar: bool,
    coplanar_frac: f64,
    coplanar_cos: f64,
    sliver_cleanup: bool,
) -> PyResult<(
    Bound<'py, PyArray2<f64>>,
    Bound<'py, PyArray2<i64>>,
    Bound<'py, PyDict>,
)> {
    let vv = verts.as_array();
    if vv.ncols() != 3 {
        return Err(PyValueError::new_err("verts must be Nx3"));
    }
    let n = vv.nrows();
    let mut rv: Vec<V3> = Vec::with_capacity(n);
    for i in 0..n {
        rv.push([vv[[i, 0]], vv[[i, 1]], vv[[i, 2]]]);
    }
    let rt = parse_tris_i64(&tris, n)?;

    let prm = FlapParams {
        refine,
        fair,
        max_loop,
        max_faces,
        collect_quality,
        drop_lone_tris,
        weld_cracks,
        split_nm,
        separate_stl,
        bridge_open_chains,
        weld_max_frac,
        orient,
        avoid_coplanar,
        coplanar_frac,
        coplanar_cos,
        sliver_cleanup,
    };

    let (out_v, out_t, report) = flap_fill(&rv, &rt, &prm);

    let verts_np = if out_v.is_empty() {
        PyArray2::<f64>::zeros(py, [0, 3], false)
    } else {
        let rows: Vec<Vec<f64>> = out_v.iter().map(|p| p.to_vec()).collect();
        PyArray2::from_vec2(py, &rows)?
    };
    let tris_np = if out_t.is_empty() {
        PyArray2::<i64>::zeros(py, [0, 3], false)
    } else {
        let rows: Vec<Vec<i64>> = out_t
            .iter()
            .map(|t| vec![t[0] as i64, t[1] as i64, t[2] as i64])
            .collect();
        PyArray2::from_vec2(py, &rows)?
    };

    let rep = PyDict::new(py);
    rep.set_item("loops_found", report.loops_found)?;
    rep.set_item("loops_filled", report.loops_filled)?;
    rep.set_item("loops_skipped", report.loops_skipped)?;
    rep.set_item("loops_skipped_nm", report.loops_skipped_nm)?;
    rep.set_item("loops_skipped_sheet", report.loops_skipped_sheet)?;
    rep.set_item("loops_fan_fallback", report.loops_fan_fallback)?;
    rep.set_item("loops_seeded", report.loops_seeded)?;
    rep.set_item("loops_refined", report.loops_refined)?;
    rep.set_item("patch_faces", report.patch_faces)?;
    rep.set_item("new_vertices", report.new_vertices)?;
    rep.set_item("large_loops_fan", report.large_loops_fan)?;
    rep.set_item("refine", report.refine)?;
    rep.set_item("fair", report.fair)?;
    rep.set_item("loops_skipped_coplanar", report.loops_skipped_coplanar)?;
    rep.set_item("loops_skipped_flat", report.loops_skipped_flat)?;
    rep.set_item("non_simple_reported", report.non_simple_reported)?;

    let split = PyDict::new(py);
    split.set_item("splits", report.split_splits)?;
    split.set_item("nm_edges_in", report.split_nm_edges_in)?;
    rep.set_item("split", split)?;

    let errors = PyList::empty(py);
    for e in &report.errors {
        errors.append(e)?;
    }
    rep.set_item("errors", errors)?;

    let logs = PyList::empty(py);
    for l in &report.loop_log {
        let d = PyDict::new(py);
        d.set_item("m", l.m)?;
        d.set_item("fan", l.fan)?;
        d.set_item("large", l.large)?;
        d.set_item("action", l.action.clone())?;
        logs.append(d)?;
    }
    rep.set_item("loop_log", logs)?;

    if let Some(info) = &report.boundary_normalize {
        rep.set_item("boundary_normalize", norm_info_dict(py, info)?)?;
    }
    if prm.weld_cracks {
        rep.set_item("welded_cracks", report.welded_cracks)?;
    }
    if let Some(d) = report.dropped_lone_triangles {
        rep.set_item("dropped_lone_triangles", d)?;
    }
    if let Some(c) = report.chains_bridged {
        rep.set_item("chains_bridged", c)?;
    }
    if report.loops_skipped_sliver > 0 {
        rep.set_item("loops_skipped_sliver", report.loops_skipped_sliver)?;
    }
    if report.orient_reversed > 0 {
        rep.set_item("orient_reversed", report.orient_reversed)?;
    }
    if !report.nm_collisions.is_empty() {
        let colls = PyList::empty(py);
        for c in &report.nm_collisions {
            let d = PyDict::new(py);
            d.set_item("m", c.m)?;
            d.set_item("fan", c.fan)?;
            let edges = PyList::empty(py);
            for e in &c.edges {
                edges.append([e.0, e.1])?;
            }
            d.set_item("edges", edges)?;
            colls.append(d)?;
        }
        rep.set_item("nm_collisions", colls)?;
    }
    if let Some(s) = report.separated_stl {
        rep.set_item("separated_stl", s)?;
    }
    if collect_quality {
        if let Some(q) = &report.quality_before {
            rep.set_item("quality_before", quality_dict(py, q)?)?;
        }
        if let Some(q) = &report.quality_after {
            rep.set_item("quality_after", quality_dict(py, q)?)?;
        }
    }

    Ok((verts_np, tris_np, rep))
}
