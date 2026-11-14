//! Thin PyO3 bridge over `sutura-core`.
//!
//! No repair logic lives here: the module only converts numpy arrays and
//! dispatches to the pure-Rust core. It exists so the Python pipeline can
//! exercise the same I/O implementation while the CLI and core stay pyo3-free.

use std::path::Path;

use numpy::ndarray::Array2;
use numpy::{AllowTypeChange, PyArray2, PyArrayLike2};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};

use sutura_core::{
    aggregate_print_reports_value, build_health_report_json, build_print_report_json,
    build_record_json, p_weld_final, should_run_stage2, split_pinched_vertices, reload_strict_holes_nm,
    reload_topology, weld_reload_equivalent, check_units, classify_json, compute_fidelity_json, enforce_reload_verdict, compute_scores_json,
    detect_defects as core_detect_defects, estimate_confidence_json, human_line_json,
    load_mesh as core_load_mesh, load_stl, parse_3mf_mesh_blocks,
    possible_debris as core_possible_debris, read_3mf_units, read_mesh as core_read_mesh, read_obj,
    read_ply, read_stl, removed_small_parts as core_removed_small_parts, repair_confidence_json,
    rewrite_3mf_meshes, save_mesh as core_save_mesh, scan_bad_coordinates as core_scan,
    summary_args_json, summary_text_json, write_obj, write_ply, write_stl_binary, Mesh,
};

fn mesh_from_arrays<'py>(
    verts: PyArrayLike2<'py, f32, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
) -> PyResult<Mesh> {
    let v = verts.as_array();
    let t = tris.as_array();
    if v.ncols() != 3 || t.ncols() != 3 {
        return Err(PyValueError::new_err(
            "vertex and triangle arrays must be Nx3",
        ));
    }
    let verts = v
        .rows()
        .into_iter()
        .map(|r| [r[0], r[1], r[2]])
        .collect::<Vec<[f32; 3]>>();
    let tris = t
        .rows()
        .into_iter()
        .map(|r| [r[0], r[1], r[2]])
        .collect::<Vec<[i32; 3]>>();
    Ok(Mesh::new(verts, tris))
}

fn arrays_from_mesh<'py>(
    py: Python<'py>,
    mesh: &Mesh,
) -> (Bound<'py, PyArray2<f32>>, Bound<'py, PyArray2<i32>>) {
    let mut v = Array2::<f32>::zeros((mesh.verts.len(), 3));
    for (i, row) in mesh.verts.iter().enumerate() {
        v[[i, 0]] = row[0];
        v[[i, 1]] = row[1];
        v[[i, 2]] = row[2];
    }
    let mut t = Array2::<i32>::zeros((mesh.tris.len(), 3));
    for (i, row) in mesh.tris.iter().enumerate() {
        t[[i, 0]] = row[0];
        t[[i, 1]] = row[1];
        t[[i, 2]] = row[2];
    }
    (
        PyArray2::from_owned_array(py, v),
        PyArray2::from_owned_array(py, t),
    )
}

fn mesh_pair<'py>(
    py: Python<'py>,
    mesh: &Mesh,
) -> (Bound<'py, PyArray2<f32>>, Bound<'py, PyArray2<i32>>) {
    arrays_from_mesh(py, mesh)
}

/// Load one mesh (STL is welded like `mesh_io.load_mesh`).
#[pyfunction]
fn load_mesh<'py>(
    py: Python<'py>,
    path: &str,
) -> PyResult<(Bound<'py, PyArray2<f32>>, Bound<'py, PyArray2<i32>>)> {
    let mesh = core_load_mesh(path).map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(mesh_pair(py, &mesh))
}

/// Load every object: `[(name, verts, tris), ...]`.
#[pyfunction]
fn load_meshes<'py>(py: Python<'py>, path: &str) -> PyResult<Py<PyList>> {
    let mut out = Vec::new();
    if sutura_core::SourceFormat::from_extension(path)
        .map(|f| f == sutura_core::SourceFormat::ThreeMf)
        .unwrap_or(false)
    {
        let blocks =
            parse_3mf_mesh_blocks(path).map_err(|e| PyValueError::new_err(e.to_string()))?;
        for b in blocks {
            let (v, t) = mesh_pair(py, &b.mesh);
            out.push((Some(b.model_name), v, t));
        }
    } else {
        let mesh = core_load_mesh(path).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let (v, t) = mesh_pair(py, &mesh);
        out.push((None, v, t));
    }
    Ok(PyList::new(py, out)?.unbind())
}

/// Read STL/OBJ/PLY without the STL weld.
#[pyfunction]
fn read_mesh<'py>(
    py: Python<'py>,
    path: &str,
) -> PyResult<(Bound<'py, PyArray2<f32>>, Bound<'py, PyArray2<i32>>)> {
    let mesh = core_read_mesh(path).map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(mesh_pair(py, &mesh))
}

#[pyfunction(name = "read_stl")]
fn read_stl_py<'py>(
    py: Python<'py>,
    path: &str,
) -> PyResult<(Bound<'py, PyArray2<f32>>, Bound<'py, PyArray2<i32>>)> {
    let mesh = read_stl(path).map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(mesh_pair(py, &mesh))
}

#[pyfunction(name = "load_stl")]
fn load_stl_py<'py>(
    py: Python<'py>,
    path: &str,
) -> PyResult<(Bound<'py, PyArray2<f32>>, Bound<'py, PyArray2<i32>>)> {
    let mesh = load_stl(path).map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(mesh_pair(py, &mesh))
}

#[pyfunction(name = "read_obj")]
fn read_obj_py<'py>(
    py: Python<'py>,
    path: &str,
) -> PyResult<(Bound<'py, PyArray2<f32>>, Bound<'py, PyArray2<i32>>)> {
    let mesh = read_obj(path).map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(mesh_pair(py, &mesh))
}

#[pyfunction(name = "read_ply")]
fn read_ply_py<'py>(
    py: Python<'py>,
    path: &str,
) -> PyResult<(Bound<'py, PyArray2<f32>>, Bound<'py, PyArray2<i32>>)> {
    let mesh = read_ply(path).map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(mesh_pair(py, &mesh))
}

/// Save one mesh, dispatching by extension.
#[pyfunction]
fn save_mesh<'py>(
    path: &str,
    verts: PyArrayLike2<'py, f32, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
) -> PyResult<()> {
    let mesh = mesh_from_arrays(verts, tris)?;
    core_save_mesh(path, &mesh).map_err(|e| PyValueError::new_err(e.to_string()))
}

#[pyfunction(name = "write_stl_binary")]
fn write_stl_binary_py<'py>(
    path: &str,
    verts: PyArrayLike2<'py, f32, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
) -> PyResult<()> {
    let mesh = mesh_from_arrays(verts, tris)?;
    write_stl_binary(path, &mesh).map_err(|e| PyValueError::new_err(e.to_string()))
}

#[pyfunction(name = "write_obj")]
fn write_obj_py<'py>(
    path: &str,
    verts: PyArrayLike2<'py, f32, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
) -> PyResult<()> {
    let mesh = mesh_from_arrays(verts, tris)?;
    write_obj(path, &mesh).map_err(|e| PyValueError::new_err(e.to_string()))
}

#[pyfunction(name = "write_ply")]
fn write_ply_py<'py>(
    path: &str,
    verts: PyArrayLike2<'py, f32, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
) -> PyResult<()> {
    let mesh = mesh_from_arrays(verts, tris)?;
    write_ply(path, &mesh).map_err(|e| PyValueError::new_err(e.to_string()))
}

#[pyfunction]
fn scan_bad_coordinates(path: &str) -> PyResult<Option<String>> {
    core_scan(path).map_err(|e| PyValueError::new_err(e.to_string()))
}

#[pyfunction(name = "obj_has_material_refs")]
fn obj_has_material_refs_py(path: &str) -> bool {
    sutura_core::obj_has_material_refs(path)
}

/// Structural summary used by the Python double-path parity checks.
#[pyfunction]
fn probe<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    let bytes_in = std::fs::metadata(path)
        .map_err(|e| PyValueError::new_err(e.to_string()))?
        .len();
    let format = sutura_core::SourceFormat::from_extension(path)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    out.set_item(
        "format",
        match format {
            sutura_core::SourceFormat::Stl => "stl",
            sutura_core::SourceFormat::Obj => "obj",
            sutura_core::SourceFormat::Ply => "ply",
            sutura_core::SourceFormat::ThreeMf => "3mf",
        },
    )?;
    out.set_item("bytes_in", bytes_in)?;
    let meshes = PyList::empty(py);
    if format == sutura_core::SourceFormat::ThreeMf {
        let units = read_3mf_units(path);
        for b in parse_3mf_mesh_blocks(path).map_err(|e| PyValueError::new_err(e.to_string()))? {
            let d = PyDict::new(py);
            d.set_item("name", &b.model_name)?;
            d.set_item("index", b.index)?;
            d.set_item("verts", b.mesh.vertex_count())?;
            d.set_item("tris", b.mesh.triangle_count())?;
            d.set_item(
                "bbox",
                b.mesh
                    .bbox()
                    .map(|(lo, hi)| vec![lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]]),
            )?;
            meshes.append(d)?;
            let unit = units
                .iter()
                .find(|(n, _)| n == &b.model_name)
                .map(|(_, u)| u.as_str());
            if let Some(w) = check_units(&b.mesh, unit) {
                out.set_item("unit_hint", w.unit_hint)?;
            }
        }
    } else {
        let mesh = core_load_mesh(path).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let d = PyDict::new(py);
        d.set_item("name", Option::<String>::None)?;
        d.set_item("index", Option::<usize>::None)?;
        d.set_item("verts", mesh.vertex_count())?;
        d.set_item("tris", mesh.triangle_count())?;
        d.set_item(
            "bbox",
            mesh.bbox()
                .map(|(lo, hi)| vec![lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]]),
        )?;
        meshes.append(d)?;
        if let Some(w) = check_units(&mesh, None) {
            out.set_item("unit_hint", w.unit_hint)?;
        }
    }
    out.set_item("meshes", meshes)?;
    out.set_item("material_refs", obj_has_material_refs_py(path))?;
    out.set_item(
        "scan",
        core_scan(path).map_err(|e| PyValueError::new_err(e.to_string()))?,
    )?;
    Ok(out)
}

#[pyfunction]
#[pyo3(signature = (src, out, updates, attr_source=None))]
fn rewrite_3mf<'py>(
    py: Python<'py>,
    src: &str,
    out: &str,
    updates: &Bound<'py, PyList>,
    attr_source: Option<String>,
) -> PyResult<Py<PyList>> {
    let mut meshes = Vec::new();
    for item in updates.iter() {
        let tuple = item.cast::<PyTuple>()?;
        let verts: PyArrayLike2<'py, f32, AllowTypeChange> = tuple.get_item(0)?.extract()?;
        let tris: PyArrayLike2<'py, i32, AllowTypeChange> = tuple.get_item(1)?.extract()?;
        meshes.push(mesh_from_arrays(verts, tris)?);
    }
    let stats = rewrite_3mf_meshes(
        Path::new(src),
        Path::new(out),
        &meshes,
        attr_source.as_deref().map(Path::new),
    )
    .map_err(|e| PyValueError::new_err(e.to_string()))?;
    let list = PyList::empty(py);
    for st in stats {
        let d = PyDict::new(py);
        d.set_item("model", st.model)?;
        d.set_item("block", st.block)?;
        d.set_item("original_attributed", st.original_attributed)?;
        d.set_item("kept", st.kept)?;
        d.set_item("dropped", st.dropped)?;
        d.set_item("output_triangles", st.output_triangles)?;
        list.append(d)?;
    }
    Ok(list.unbind())
}

/// Parse every non-empty 3MF mesh block with its original spans.
#[pyfunction(name = "parse_3mf_mesh_blocks")]
fn parse_3mf_mesh_blocks_py<'py>(py: Python<'py>, path: &str) -> PyResult<Py<PyList>> {
    let blocks = parse_3mf_mesh_blocks(path).map_err(|e| PyValueError::new_err(e.to_string()))?;
    let out = PyList::empty(py);
    for b in blocks {
        let d = PyDict::new(py);
        d.set_item("model_name", &b.model_name)?;
        d.set_item("index", b.index)?;
        d.set_item("span", b.span)?;
        d.set_item("open_tag", &b.open_tag)?;
        d.set_item("close_tag", &b.close_tag)?;
        d.set_item("prefix", &b.prefix)?;
        let (v, t) = mesh_pair(py, &b.mesh);
        d.set_item("verts", v)?;
        d.set_item("tris", t)?;
        d.set_item("triangle_attrs", PyList::new(py, b.triangle_attrs)?)?;
        out.append(d)?;
    }
    Ok(out.unbind())
}

#[pyfunction(name = "read_3mf_units")]
fn read_3mf_units_py<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    for (name, unit) in read_3mf_units(path) {
        d.set_item(name, unit)?;
    }
    Ok(d)
}

#[pyfunction(name = "write_3mf_single")]
fn write_3mf_single_py<'py>(
    path: &str,
    verts: PyArrayLike2<'py, f32, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
) -> PyResult<()> {
    let mesh = mesh_from_arrays(verts, tris)?;
    sutura_core::write_3mf_single(path, &mesh).map_err(|e| PyValueError::new_err(e.to_string()))
}

#[pyfunction(name = "check_units")]
fn check_units_py<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f32, AllowTypeChange>,
    declared_unit: Option<&str>,
) -> PyResult<Option<Bound<'py, PyDict>>> {
    let v = verts.as_array();
    if v.ncols() != 3 {
        return Err(PyValueError::new_err("vertices must be Nx3"));
    }
    let mesh = Mesh::new(
        v.rows().into_iter().map(|r| [r[0], r[1], r[2]]).collect(),
        Vec::new(),
    );
    let Some(w) = check_units(&mesh, declared_unit) else {
        return Ok(None);
    };
    let d = PyDict::new(py);
    d.set_item("unit_warning", w.unit_warning)?;
    d.set_item("unit_hint", w.unit_hint)?;
    Ok(Some(d))
}

fn verts_f32(v: PyArrayLike2<'_, f32, AllowTypeChange>) -> PyResult<Vec<[f32; 3]>> {
    let a = v.as_array();
    if a.ncols() != 3 {
        return Err(PyValueError::new_err("vertices must be Nx3"));
    }
    Ok(a.rows().into_iter().map(|r| [r[0], r[1], r[2]]).collect())
}

fn verts_f64(v: PyArrayLike2<'_, f64, AllowTypeChange>) -> PyResult<Vec<[f64; 3]>> {
    let a = v.as_array();
    if a.ncols() != 3 {
        return Err(PyValueError::new_err("vertices must be Nx3"));
    }
    Ok(a.rows().into_iter().map(|r| [r[0], r[1], r[2]]).collect())
}

fn tris_i64(t: PyArrayLike2<'_, i64, AllowTypeChange>) -> PyResult<Vec<[i64; 3]>> {
    let a = t.as_array();
    if a.ncols() != 3 {
        return Err(PyValueError::new_err("triangles must be Nx3"));
    }
    Ok(a.rows().into_iter().map(|r| [r[0], r[1], r[2]]).collect())
}

fn tris_i32(t: PyArrayLike2<'_, i32, AllowTypeChange>) -> PyResult<Vec<[i32; 3]>> {
    let a = t.as_array();
    if a.ncols() != 3 {
        return Err(PyValueError::new_err("triangles must be Nx3"));
    }
    Ok(a.rows().into_iter().map(|r| [r[0], r[1], r[2]]).collect())
}

#[pyfunction(name = "detect_defects")]
fn detect_defects_py<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f32, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
    with_indices: bool,
) -> PyResult<Bound<'py, PyDict>> {
    let v = verts_f32(verts)?;
    let t = tris_i32(tris)?;
    let r = core_detect_defects(&v, &t, with_indices);
    let holes = PyList::empty(py);
    for h in r.holes {
        let d = PyDict::new(py);
        d.set_item("centroid", h.centroid)?;
        d.set_item("diameter", h.diameter)?;
        d.set_item("vertices", h.vertices)?;
        if let Some(idx) = h.verts_idx {
            d.set_item("verts_idx", idx)?;
        }
        holes.append(d)?;
    }
    let nm = PyList::empty(py);
    for reg in r.non_manifold {
        let d = PyDict::new(py);
        d.set_item("centroid", reg.centroid)?;
        d.set_item("faces", reg.faces)?;
        if let Some(idx) = reg.verts_idx {
            d.set_item("verts_idx", idx)?;
        }
        if let Some(idx) = reg.faces_idx {
            d.set_item("faces_idx", idx)?;
        }
        nm.append(d)?;
    }
    let out = PyDict::new(py);
    out.set_item("holes", holes)?;
    out.set_item("non_manifold", nm)?;
    Ok(out)
}

#[pyfunction(name = "debris_removed_small_parts")]
fn debris_removed_small_parts_py<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
    min_faces: usize,
) -> PyResult<Option<Bound<'py, PyDict>>> {
    let v = verts_f64(verts)?;
    let t = tris_i32(tris)?;
    let Some(r) = core_removed_small_parts(&v, &t, min_faces) else {
        return Ok(None);
    };
    let d = PyDict::new(py);
    d.set_item("count", r.count)?;
    d.set_item("min_faces", r.min_faces)?;
    d.set_item("faces_total", r.faces_total)?;
    d.set_item("capped", r.capped)?;
    let parts = PyList::empty(py);
    for p in r.parts {
        let pd = PyDict::new(py);
        pd.set_item("faces", p.faces)?;
        pd.set_item("size_mm", p.size_mm)?;
        pd.set_item("size_pct", p.size_pct)?;
        pd.set_item("distance_mm", p.distance_mm)?;
        parts.append(pd)?;
    }
    d.set_item("parts", parts)?;
    Ok(Some(d))
}

#[pyfunction(name = "debris_possible_debris")]
fn debris_possible_debris_py<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
    min_faces: usize,
    tau: f64,
) -> PyResult<Option<Bound<'py, PyDict>>> {
    let v = verts_f64(verts)?;
    let t = tris_i32(tris)?;
    let Some(r) = core_possible_debris(&v, &t, min_faces, tau) else {
        return Ok(None);
    };
    let d = PyDict::new(py);
    d.set_item("count", r.count)?;
    d.set_item("capped", r.capped)?;
    let parts = PyList::empty(py);
    for p in r.parts {
        let pd = PyDict::new(py);
        pd.set_item("faces", p.faces)?;
        pd.set_item("volume_pct", p.volume_pct)?;
        pd.set_item("size_pct", p.size_pct)?;
        pd.set_item("distance_mm", p.distance_mm)?;
        parts.append(pd)?;
    }
    d.set_item("parts", parts)?;
    Ok(Some(d))
}

fn confidence_dict<'py>(
    py: Python<'py>,
    c: sutura_core::Confidence,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("score", c.score)?;
    d.set_item("label", c.label)?;
    let factors = PyDict::new(py);
    for (k, v) in c.factors {
        factors.set_item(k, v.as_i64().unwrap_or(0))?;
    }
    d.set_item("factors", factors)?;
    Ok(d)
}

#[pyfunction(name = "classify")]
fn classify_py(data_json: &str) -> PyResult<(String, Vec<String>, String)> {
    classify_json(data_json).map_err(PyValueError::new_err)
}

#[pyfunction(name = "repair_confidence")]
fn repair_confidence_py<'py>(py: Python<'py>, data_json: &str) -> PyResult<Bound<'py, PyDict>> {
    let c = repair_confidence_json(data_json).map_err(PyValueError::new_err)?;
    confidence_dict(py, c)
}

#[pyfunction(name = "estimate_confidence_pre_repair")]
fn estimate_confidence_py<'py>(
    py: Python<'py>,
    signals_json: &str,
    mech_gate: f64,
    org_gate: f64,
) -> PyResult<Bound<'py, PyDict>> {
    let c = estimate_confidence_json(signals_json, mech_gate, org_gate)
        .map_err(PyValueError::new_err)?;
    confidence_dict(py, c)
}

#[pyfunction(name = "summary_args")]
fn summary_args_py<'py>(py: Python<'py>, data_json: &str) -> PyResult<Bound<'py, PyTuple>> {
    let v = summary_args_json(data_json).map_err(PyValueError::new_err)?;
    Ok(PyTuple::new(py, v)?)
}

#[pyfunction(name = "compute_fidelity")]
fn compute_fidelity_py(
    vi: PyArrayLike2<'_, f64, AllowTypeChange>,
    ti: PyArrayLike2<'_, i32, AllowTypeChange>,
    vo: PyArrayLike2<'_, f64, AllowTypeChange>,
    to: PyArrayLike2<'_, i32, AllowTypeChange>,
    report_json: &str,
    config_json: &str,
) -> PyResult<String> {
    let vi = verts_f64(vi)?;
    let ti = tris_i32(ti)?;
    let vo = verts_f64(vo)?;
    let to = tris_i32(to)?;
    compute_fidelity_json(&vi, &ti, &vo, &to, report_json, config_json)
        .map_err(PyValueError::new_err)
}

#[pyfunction(name = "repair_scores")]
fn repair_scores_py(report_json: &str, config_json: &str) -> PyResult<String> {
    compute_scores_json(report_json, config_json).map_err(PyValueError::new_err)
}

#[pyfunction(name = "history_build_record")]
fn history_build_record_py(
    report_json: &str,
    fingerprint: &str,
    version: &str,
    elapsed_ms: i64,
    fmt: Option<&str>,
    ts: &str,
) -> PyResult<String> {
    build_record_json(report_json, fingerprint, version, elapsed_ms, fmt, ts)
        .map_err(PyValueError::new_err)
}

#[pyfunction(name = "history_summary_text")]
fn history_summary_text_py(records_json: &str) -> PyResult<String> {
    summary_text_json(records_json).map_err(PyValueError::new_err)
}

#[pyfunction(name = "health_build_report")]
fn health_build_report_py(
    availability_json: &str,
    errors_json: &str,
    skips_json: &str,
) -> PyResult<String> {
    build_health_report_json(availability_json, errors_json, skips_json)
        .map_err(PyValueError::new_err)
}

#[pyfunction(name = "health_human_line")]
fn health_human_line_py(report_json: &str) -> PyResult<String> {
    human_line_json(report_json).map_err(PyValueError::new_err)
}

#[pyfunction(name = "build_print_report")]
fn build_print_report_py(
    report_json: &str,
    iv: Option<PyArrayLike2<'_, f64, AllowTypeChange>>,
    it: Option<PyArrayLike2<'_, i32, AllowTypeChange>>,
    ov: Option<PyArrayLike2<'_, f64, AllowTypeChange>>,
    ot: Option<PyArrayLike2<'_, i32, AllowTypeChange>>,
) -> PyResult<Option<String>> {
    let iv = match iv {
        Some(v) => Some(verts_f64(v)?),
        None => None,
    };
    let it = match it {
        Some(v) => Some(tris_i32(v)?),
        None => None,
    };
    let ov = match ov {
        Some(v) => Some(verts_f64(v)?),
        None => None,
    };
    let ot = match ot {
        Some(v) => Some(tris_i32(v)?),
        None => None,
    };
    build_print_report_json(
        report_json,
        iv.as_deref(),
        it.as_deref(),
        ov.as_deref(),
        ot.as_deref(),
    )
    .map_err(PyValueError::new_err)
}

#[pyfunction(name = "aggregate_print_reports")]
fn aggregate_print_reports_py(prs_json: &str) -> PyResult<Option<String>> {
    let v: serde_json::Value =
        serde_json::from_str(prs_json).map_err(|e| PyValueError::new_err(e.to_string()))?;
    let arr = v
        .as_array()
        .ok_or_else(|| PyValueError::new_err("expected an array"))?;
    Ok(aggregate_print_reports_value(arr).map(|x| x.to_string()))
}

#[pyfunction(name = "p_weld_reload_topology")]
fn p_weld_reload_topology_py(
    verts: PyArrayLike2<'_, f64, AllowTypeChange>,
    tris: PyArrayLike2<'_, i64, AllowTypeChange>,
) -> PyResult<(usize, usize, usize)> {
    let v = verts_f64(verts)?;
    let t = tris_i64(tris)?;
    Ok(reload_topology(&v, &t))
}

#[pyfunction(name = "p_weld_reload_strict")]
fn p_weld_reload_strict_py(
    verts: PyArrayLike2<'_, f64, AllowTypeChange>,
    tris: PyArrayLike2<'_, i64, AllowTypeChange>,
) -> PyResult<(usize, usize)> {
    let v = verts_f64(verts)?;
    let t = tris_i64(tris)?;
    Ok(reload_strict_holes_nm(&v, &t))
}

#[pyfunction(name = "p_weld_reload_equivalent")]
fn p_weld_reload_equivalent_py<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
) -> PyResult<(Bound<'py, PyArray2<f32>>, Bound<'py, PyArray2<i64>>)> {
    let v = verts_f64(verts)?;
    let t = tris_i64(tris)?;
    let (ov, ot) = weld_reload_equivalent(&v, &t).map_err(PyValueError::new_err)?;
    let mut av = numpy::ndarray::Array2::<f32>::zeros((ov.len(), 3));
    for (i, p) in ov.iter().enumerate() { av[[i,0]]=p[0]; av[[i,1]]=p[1]; av[[i,2]]=p[2]; }
    let mut at = numpy::ndarray::Array2::<i64>::zeros((ot.len(), 3));
    for (i, p) in ot.iter().enumerate() { at[[i,0]]=p[0]; at[[i,1]]=p[1]; at[[i,2]]=p[2]; }
    Ok((PyArray2::from_owned_array(py, av), PyArray2::from_owned_array(py, at)))
}

#[pyfunction(name = "p_weld_final")]
fn p_weld_final_py<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
) -> PyResult<(Bound<'py, PyArray2<f32>>, Bound<'py, PyArray2<i32>>, Option<String>)> {
    let v = verts_f64(verts)?;
    let t = tris_i64(tris)?;
    let r = p_weld_final(&v, &t);
    let mut av = numpy::ndarray::Array2::<f32>::zeros((r.verts.len(), 3));
    for (i, p) in r.verts.iter().enumerate() { av[[i,0]]=p[0]; av[[i,1]]=p[1]; av[[i,2]]=p[2]; }
    let mut at = numpy::ndarray::Array2::<i32>::zeros((r.tris.len(), 3));
    for (i, p) in r.tris.iter().enumerate() { at[[i,0]]=p[0]; at[[i,1]]=p[1]; at[[i,2]]=p[2]; }
    Ok((PyArray2::from_owned_array(py, av), PyArray2::from_owned_array(py, at),
        r.report.map(|x| x.to_string())))
}

#[pyfunction(name = "p_weld_enforce_verdict")]
fn p_weld_enforce_verdict_py(
    report_json: &str,
    verts: PyArrayLike2<'_, f64, AllowTypeChange>,
    tris: PyArrayLike2<'_, i64, AllowTypeChange>,
    include_n2m: bool,
) -> PyResult<(bool, String)> {
    let mut r: serde_json::Value = serde_json::from_str(report_json).map_err(|e| PyValueError::new_err(e.to_string()))?;
    let v = verts_f64(verts)?;
    let t = tris_i64(tris)?;
    let changed = enforce_reload_verdict(&mut r, &v, &t, include_n2m);
    Ok((changed, r.to_string()))
}

#[pyfunction(name = "stage2_should_run")]
fn stage2_should_run_py(report_json: &str) -> PyResult<bool> {
    let v: serde_json::Value = serde_json::from_str(report_json).map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(should_run_stage2(&v))
}

#[pyfunction(name = "stage2_split_pinched")]
fn stage2_split_pinched_py<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
) -> PyResult<(Bound<'py, PyArray2<f32>>, Bound<'py, PyArray2<i32>>, Option<String>)> {
    let v = verts_f64(verts)?;
    let t = tris_i64(tris)?;
    let r = split_pinched_vertices(&v, &t);
    let mut av = numpy::ndarray::Array2::<f32>::zeros((r.verts.len(), 3));
    for (i, p) in r.verts.iter().enumerate() { av[[i,0]]=p[0]; av[[i,1]]=p[1]; av[[i,2]]=p[2]; }
    let mut at = numpy::ndarray::Array2::<i32>::zeros((r.tris.len(), 3));
    for (i, p) in r.tris.iter().enumerate() { at[[i,0]]=p[0]; at[[i,1]]=p[1]; at[[i,2]]=p[2]; }
    Ok((PyArray2::from_owned_array(py, av), PyArray2::from_owned_array(py, at),
        r.report.map(|x| x.to_string())))
}


fn parse_params(json: &str) -> PyResult<serde_json::Value> {
    serde_json::from_str(json).map_err(|e| PyValueError::new_err(e.to_string()))
}

fn arrays_f64<'py>(py: Python<'py>, v: &[[f64; 3]]) -> PyResult<Bound<'py, PyArray2<f64>>> {
    if v.is_empty() {
        return Ok(PyArray2::<f64>::zeros(py, [0, 3], false));
    }
    let rows: Vec<Vec<f64>> = v.iter().map(|p| p.to_vec()).collect();
    PyArray2::from_vec2(py, &rows).map_err(|e| PyValueError::new_err(e.to_string()))
}

fn arrays_i64<'py>(py: Python<'py>, t: &[[i64; 3]]) -> PyResult<Bound<'py, PyArray2<i64>>> {
    if t.is_empty() {
        return Ok(PyArray2::<i64>::zeros(py, [0, 3], false));
    }
    let rows: Vec<Vec<i64>> = t.iter().map(|r| r.to_vec()).collect();
    PyArray2::from_vec2(py, &rows).map_err(|e| PyValueError::new_err(e.to_string()))
}

#[pyfunction(name = "tier_closing_decide")]
fn tier_closing_decide_py<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    input_verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    input_tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    cand_verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    cand_tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    rep_json: &str,
    params_json: &str,
) -> PyResult<(Bound<'py, PyArray2<f64>>, Bound<'py, PyArray2<i64>>, String)> {
    let v = verts_f64(verts)?;
    let t = tris_i64(tris)?;
    let iv = verts_f64(input_verts)?;
    let it = tris_i64(input_tris)?;
    let cv = verts_f64(cand_verts)?;
    let ct = tris_i64(cand_tris)?;
    let rep = parse_params(rep_json)?;
    let params = parse_params(params_json)?;
    let (ov, ot, rec) =
        sutura_core::tiers::closing::decide(&v, &t, &iv, &it, &cv, &ct, &rep, &params);
    Ok((arrays_f64(py, &ov)?, arrays_i64(py, &ot)?, rec.to_string()))
}

#[pyfunction(name = "tier_splint")]
fn tier_splint_py<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    input_verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    input_tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    params_json: &str,
) -> PyResult<(Bound<'py, PyArray2<f64>>, Bound<'py, PyArray2<i64>>, String)> {
    let v = verts_f64(verts)?;
    let t = tris_i64(tris)?;
    let iv = verts_f64(input_verts)?;
    let it = tris_i64(input_tris)?;
    let params = parse_params(params_json)?;
    let (ov, ot, rec) = sutura_core::tiers::splint::run(&v, &t, &iv, &it, &params);
    Ok((arrays_f64(py, &ov)?, arrays_i64(py, &ot)?, rec.to_string()))
}

#[pyfunction(name = "tier_cdt")]
fn tier_cdt_py<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    input_verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    input_tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    params_json: &str,
) -> PyResult<(Bound<'py, PyArray2<f64>>, Bound<'py, PyArray2<i64>>, String)> {
    let v = verts_f64(verts)?;
    let t = tris_i64(tris)?;
    let iv = verts_f64(input_verts)?;
    let it = tris_i64(input_tris)?;
    let params = parse_params(params_json)?;
    let (ov, ot, rec) = sutura_core::tiers::cdt::run(&v, &t, &iv, &it, &params);
    Ok((arrays_f64(py, &ov)?, arrays_i64(py, &ot)?, rec.to_string()))
}

#[pyfunction(name = "tier_coaptation")]
fn tier_coaptation_py<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    input_verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    input_tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    params_json: &str,
) -> PyResult<(Bound<'py, PyArray2<f64>>, Bound<'py, PyArray2<i64>>, String)> {
    let v = verts_f64(verts)?;
    let t = tris_i64(tris)?;
    let iv = verts_f64(input_verts)?;
    let it = tris_i64(input_tris)?;
    let params = parse_params(params_json)?;
    let (ov, ot, rec) =
        sutura_core::tiers::coaptation::run(&v, &t, &iv, &it, &params);
    Ok((arrays_f64(py, &ov)?, arrays_i64(py, &ot)?, rec.to_string()))
}

#[pyfunction(name = "tier_graft_decide")]
fn tier_graft_decide_py<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    input_verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    input_tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    cand_verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    cand_tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    grec_json: &str,
    params_json: &str,
) -> PyResult<(Bound<'py, PyArray2<f64>>, Bound<'py, PyArray2<i64>>, String)> {
    let v = verts_f64(verts)?;
    let t = tris_i64(tris)?;
    let iv = verts_f64(input_verts)?;
    let it = tris_i64(input_tris)?;
    let cv = verts_f64(cand_verts)?;
    let ct = tris_i64(cand_tris)?;
    let grec = parse_params(grec_json)?;
    let params = parse_params(params_json)?;
    let (ov, ot, rec) =
        sutura_core::tiers::graft::decide(&v, &t, &iv, &it, &cv, &ct, &grec, &params);
    Ok((arrays_f64(py, &ov)?, arrays_i64(py, &ot)?, rec.to_string()))
}

#[pyfunction(name = "tier_tamponade")]
fn tier_tamponade_py<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    params_json: &str,
) -> PyResult<(Bound<'py, PyArray2<f64>>, Bound<'py, PyArray2<i64>>, String)> {
    let v = verts_f64(verts)?;
    let t = tris_i64(tris)?;
    let params = parse_params(params_json)?;
    let (ov, ot, rec) = sutura_core::tiers::tamponade::run(&v, &t, &params);
    Ok((arrays_f64(py, &ov)?, arrays_i64(py, &ot)?, rec.to_string()))
}

#[pyfunction(name = "tier_flap")]
fn tier_flap_py<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    params_json: &str,
) -> PyResult<(Bound<'py, PyArray2<f64>>, Bound<'py, PyArray2<i64>>, String)> {
    let v = verts_f64(verts)?;
    let t = tris_i64(tris)?;
    let params = parse_params(params_json)?;
    let (ov, ot, rec) = sutura_core::tiers::flap::run(&v, &t, &params);
    Ok((arrays_f64(py, &ov)?, arrays_i64(py, &ot)?, rec.to_string()))
}

#[pyfunction(name = "tier_flap_debris")]
fn tier_flap_debris_py<'py>(
    py: Python<'py>,
    in_verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    in_tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    out_verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    out_tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    params_json: &str,
) -> PyResult<(Bound<'py, PyArray2<f64>>, Bound<'py, PyArray2<i64>>, Option<String>)> {
    let iv = verts_f64(in_verts)?;
    let it = tris_i64(in_tris)?;
    let ov = verts_f64(out_verts)?;
    let ot = tris_i64(out_tris)?;
    let params = parse_params(params_json)?;
    let (cv, ct, info) = sutura_core::tiers::flap::debris(&iv, &it, &ov, &ot, &params);
    Ok((arrays_f64(py, &cv)?, arrays_i64(py, &ct)?, info.map(|x| x.to_string())))
}

#[pyfunction(name = "tier_flap_final")]
fn tier_flap_final_py(
    in_v: PyArrayLike2<'_, f64, AllowTypeChange>,
    in_t: PyArrayLike2<'_, i64, AllowTypeChange>,
    out_v: PyArrayLike2<'_, f64, AllowTypeChange>,
    out_t: PyArrayLike2<'_, i64, AllowTypeChange>,
    measure_si: bool,
    si_flagged: bool,
) -> PyResult<(Option<String>, Option<String>)> {
    let iv = verts_f64(in_v)?;
    let it = tris_i64(in_t)?;
    let ov = verts_f64(out_v)?;
    let ot = tris_i64(out_t)?;
    let (reason, metrics) =
        sutura_core::tiers::flap::final_off(&iv, &it, &ov, &ot, measure_si, si_flagged);
    Ok((reason, metrics.map(|x| x.to_string())))
}

#[pyfunction(name = "tier_flap_off_preferred")]
fn tier_flap_off_preferred_py(on_reason: &str, on_json: &str, off_json: &str) -> PyResult<bool> {
    let on = parse_params(on_json)?;
    let off = parse_params(off_json)?;
    Ok(sutura_core::tiers::flap::off_preferred(on_reason, &on, &off))
}

#[pyfunction(name = "tier_flap_si_rose")]
fn tier_flap_si_rose_py(rec_json: &str) -> PyResult<bool> {
    let rec = parse_params(rec_json)?;
    Ok(sutura_core::tiers::flap::si_rose(&rec))
}

#[pyfunction(name = "tier_tamponade_final")]
fn tier_tamponade_final_py(
    in_v: PyArrayLike2<'_, f64, AllowTypeChange>,
    in_t: PyArrayLike2<'_, i64, AllowTypeChange>,
    out_v: PyArrayLike2<'_, f64, AllowTypeChange>,
    out_t: PyArrayLike2<'_, i64, AllowTypeChange>,
    force_metrics: bool,
    params_json: &str,
) -> PyResult<(Option<String>, Option<String>)> {
    let iv = verts_f64(in_v)?;
    let it = tris_i64(in_t)?;
    let ov = verts_f64(out_v)?;
    let ot = tris_i64(out_t)?;
    let params = parse_params(params_json)?;
    let (reason, metrics) =
        sutura_core::tiers::tamponade::final_off(&iv, &it, &ov, &ot, force_metrics, &params);
    Ok((reason, metrics.map(|x| x.to_string())))
}

#[pyfunction(name = "tier_tamponade_off_preferred")]
fn tier_tamponade_off_preferred_py(on_json: &str, off_json: &str) -> PyResult<bool> {
    let on = parse_params(on_json)?;
    let off = parse_params(off_json)?;
    Ok(sutura_core::tiers::tamponade::off_preferred_json(&on, &off))
}

#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[pymodule]
fn sutura_core_py(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(load_mesh, m)?)?;
    m.add_function(wrap_pyfunction!(load_meshes, m)?)?;
    m.add_function(wrap_pyfunction!(read_mesh, m)?)?;
    m.add_function(wrap_pyfunction!(read_stl_py, m)?)?;
    m.add_function(wrap_pyfunction!(load_stl_py, m)?)?;
    m.add_function(wrap_pyfunction!(read_obj_py, m)?)?;
    m.add_function(wrap_pyfunction!(read_ply_py, m)?)?;
    m.add_function(wrap_pyfunction!(save_mesh, m)?)?;
    m.add_function(wrap_pyfunction!(write_stl_binary_py, m)?)?;
    m.add_function(wrap_pyfunction!(write_obj_py, m)?)?;
    m.add_function(wrap_pyfunction!(write_ply_py, m)?)?;
    m.add_function(wrap_pyfunction!(scan_bad_coordinates, m)?)?;
    m.add_function(wrap_pyfunction!(obj_has_material_refs_py, m)?)?;
    m.add_function(wrap_pyfunction!(probe, m)?)?;
    m.add_function(wrap_pyfunction!(rewrite_3mf, m)?)?;
    m.add_function(wrap_pyfunction!(parse_3mf_mesh_blocks_py, m)?)?;
    m.add_function(wrap_pyfunction!(read_3mf_units_py, m)?)?;
    m.add_function(wrap_pyfunction!(write_3mf_single_py, m)?)?;
    m.add_function(wrap_pyfunction!(check_units_py, m)?)?;
    m.add_function(wrap_pyfunction!(detect_defects_py, m)?)?;
    m.add_function(wrap_pyfunction!(debris_removed_small_parts_py, m)?)?;
    m.add_function(wrap_pyfunction!(debris_possible_debris_py, m)?)?;
    m.add_function(wrap_pyfunction!(classify_py, m)?)?;
    m.add_function(wrap_pyfunction!(repair_confidence_py, m)?)?;
    m.add_function(wrap_pyfunction!(estimate_confidence_py, m)?)?;
    m.add_function(wrap_pyfunction!(summary_args_py, m)?)?;
    m.add_function(wrap_pyfunction!(repair_scores_py, m)?)?;
    m.add_function(wrap_pyfunction!(compute_fidelity_py, m)?)?;
    m.add_function(wrap_pyfunction!(p_weld_reload_topology_py, m)?)?;
    m.add_function(wrap_pyfunction!(p_weld_reload_strict_py, m)?)?;
    m.add_function(wrap_pyfunction!(p_weld_reload_equivalent_py, m)?)?;
    m.add_function(wrap_pyfunction!(p_weld_final_py, m)?)?;
    m.add_function(wrap_pyfunction!(p_weld_enforce_verdict_py, m)?)?;
    m.add_function(wrap_pyfunction!(stage2_should_run_py, m)?)?;
    m.add_function(wrap_pyfunction!(stage2_split_pinched_py, m)?)?;
    m.add_function(wrap_pyfunction!(tier_closing_decide_py, m)?)?;
    m.add_function(wrap_pyfunction!(tier_splint_py, m)?)?;
    m.add_function(wrap_pyfunction!(tier_cdt_py, m)?)?;
    m.add_function(wrap_pyfunction!(tier_coaptation_py, m)?)?;
    m.add_function(wrap_pyfunction!(tier_graft_decide_py, m)?)?;
    m.add_function(wrap_pyfunction!(tier_tamponade_py, m)?)?;
    m.add_function(wrap_pyfunction!(tier_flap_py, m)?)?;
    m.add_function(wrap_pyfunction!(tier_flap_debris_py, m)?)?;
    m.add_function(wrap_pyfunction!(tier_flap_final_py, m)?)?;
    m.add_function(wrap_pyfunction!(tier_flap_off_preferred_py, m)?)?;
    m.add_function(wrap_pyfunction!(tier_flap_si_rose_py, m)?)?;
    m.add_function(wrap_pyfunction!(tier_tamponade_final_py, m)?)?;
    m.add_function(wrap_pyfunction!(tier_tamponade_off_preferred_py, m)?)?;
    m.add_function(wrap_pyfunction!(history_build_record_py, m)?)?;
    m.add_function(wrap_pyfunction!(history_summary_text_py, m)?)?;
    m.add_function(wrap_pyfunction!(health_build_report_py, m)?)?;
    m.add_function(wrap_pyfunction!(health_human_line_py, m)?)?;
    m.add_function(wrap_pyfunction!(build_print_report_py, m)?)?;
    m.add_function(wrap_pyfunction!(aggregate_print_reports_py, m)?)?;
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
