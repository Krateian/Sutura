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
    check_units, classify_json, compute_fidelity_json, compute_scores_json,
    detect_defects as core_detect_defects, estimate_confidence_json, load_mesh as core_load_mesh,
    load_stl, parse_3mf_mesh_blocks, possible_debris as core_possible_debris, read_3mf_units,
    read_mesh as core_read_mesh, read_obj, read_ply, read_stl,
    removed_small_parts as core_removed_small_parts, repair_confidence_json, rewrite_3mf_meshes,
    save_mesh as core_save_mesh, scan_bad_coordinates as core_scan, summary_args_json, write_obj,
    write_ply, write_stl_binary, Mesh,
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
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
