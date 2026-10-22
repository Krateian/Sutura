//! Manifold-backed mesh booleans for the Sutura core.
//!
//! `manifold-csg` is the same C++ Manifold kernel the Python `manifold3d`
//! package uses, called in-process from Rust. It replaces the Python
//! `manifold_bridge.py` / `csg_bridge.py` subprocesses and the
//! `trimesh.boolean(engine="manifold")` path in Graft.
//!
//! Precision follows the old call sites: Stage 2 and outer-shell extraction
//! import f32 (as `manifold3d.Mesh` did there), while the volumetric
//! transplant and the Graft local stitch use f64.

use std::collections::{BTreeMap, HashMap};

use manifold_csg::{CsgError, Manifold, OpType};
use numpy::{AllowTypeChange, PyArrayLike1, PyArrayLike2, PyArrayLike3};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};

const DEBRIS_VOLUME_EPS: f64 = 1e-3;
const DEBRIS_FLATNESS: f64 = 1e-4;
/// Below this component count `Manifold::decompose()` is cheap and its exact
/// part construction keeps the output bytes stable; above it the per-component
/// scan dominates (thousands of components) and the array walker is used.
/// The 40-mesh corpus tops out at 83 components (thingi10k_1038441), where
/// the rebuilt parts can change the boolean result through their per-part
/// epsilon; 256 keeps every corpus mesh on the exact path while the 1,206-part
/// Splint repro still takes the walker.
const FAST_COMPONENT_MIN: usize = 256;

fn status_string(r: Result<(), CsgError>) -> String {
    match r {
        Ok(()) => "Error.NoError".to_string(),
        Err(e) => format!("Error.{e:?}"),
    }
}

fn is_debris_part(p: &Manifold) -> bool {
    let vol = p.volume().abs();
    if vol <= 0.0 || vol <= DEBRIS_VOLUME_EPS {
        return true;
    }
    let area = p.surface_area();
    let Some(bb) = p.bounding_box() else {
        return true;
    };
    let (lo, hi) = (bb.min(), bb.max());
    let diag = ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2)).sqrt();
    if area <= 0.0 || diag <= 0.0 {
        return true;
    }
    (2.0 * vol / area) <= DEBRIS_FLATNESS * diag
}

fn f32_mesh(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> Result<Manifold, CsgError> {
    let v: Vec<f32> = verts
        .iter()
        .flat_map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    let t: Vec<u32> = tris
        .iter()
        .flat_map(|t| [t[0] as u32, t[1] as u32, t[2] as u32])
        .collect();
    Manifold::from_mesh_f32(&v, 3, &t)
}

fn f64_mesh(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> Result<Manifold, CsgError> {
    let v: Vec<f64> = verts.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let t: Vec<u64> = tris
        .iter()
        .flat_map(|t| [t[0] as u64, t[1] as u64, t[2] as u64])
        .collect();
    Manifold::from_mesh_f64(&v, 3, &t)
}

fn mesh_to_f32(m: &Manifold) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
    let (vp, stride, tri) = m.to_mesh_f32();
    let verts = vp
        .chunks_exact(stride)
        .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
        .collect();
    let tris = tri
        .chunks_exact(3)
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    (verts, tris)
}

fn mesh_to_f64(m: &Manifold) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
    let (vp, stride, tri) = m.to_mesh_f64();
    let verts = vp
        .chunks_exact(stride)
        .map(|p| [p[0], p[1], p[2]])
        .collect();
    let tris = tri
        .chunks_exact(3)
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    (verts, tris)
}

fn parse_verts2<'py>(a: &PyArrayLike2<'py, f64, AllowTypeChange>) -> PyResult<Vec<[f64; 3]>> {
    let v = a.as_array();
    if v.ncols() != 3 {
        return Err(PyValueError::new_err("verts must be Nx3"));
    }
    Ok((0..v.nrows())
        .map(|i| [v[[i, 0]], v[[i, 1]], v[[i, 2]]])
        .collect())
}

fn parse_tris2<'py>(
    a: &PyArrayLike2<'py, i32, AllowTypeChange>,
    n: usize,
) -> PyResult<Vec<[usize; 3]>> {
    let t = a.as_array();
    if t.ncols() != 3 {
        return Err(PyValueError::new_err("tris must be Mx3"));
    }
    let mut out = Vec::with_capacity(t.nrows());
    for i in 0..t.nrows() {
        let a = t[[i, 0]];
        let b = t[[i, 1]];
        let c = t[[i, 2]];
        if a < 0 || b < 0 || c < 0 || a as usize >= n || b as usize >= n || c as usize >= n {
            return Err(PyValueError::new_err("triangle index out of range"));
        }
        out.push([a as usize, b as usize, c as usize]);
    }
    Ok(out)
}

fn f32_arrays<'py>(
    py: Python<'py>,
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
) -> PyResult<(
    Bound<'py, numpy::PyArray2<f32>>,
    Bound<'py, numpy::PyArray2<i32>>,
)> {
    let vv: Vec<Vec<f32>> = verts
        .iter()
        .map(|p| vec![p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    let tt: Vec<Vec<i32>> = tris
        .iter()
        .map(|t| vec![t[0] as i32, t[1] as i32, t[2] as i32])
        .collect();
    Ok((
        numpy::PyArray2::from_vec2(py, &vv)?,
        numpy::PyArray2::from_vec2(py, &tt)?,
    ))
}

fn f64_arrays<'py>(
    py: Python<'py>,
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
) -> PyResult<(
    Bound<'py, numpy::PyArray2<f64>>,
    Bound<'py, numpy::PyArray2<i32>>,
)> {
    let vv: Vec<Vec<f64>> = verts.iter().map(|p| vec![p[0], p[1], p[2]]).collect();
    let tt: Vec<Vec<i32>> = tris
        .iter()
        .map(|t| vec![t[0] as i32, t[1] as i32, t[2] as i32])
        .collect();
    Ok((
        numpy::PyArray2::from_vec2(py, &vv)?,
        numpy::PyArray2::from_vec2(py, &tt)?,
    ))
}

fn parse_point3<'py>(a: &PyArrayLike1<'py, f64, AllowTypeChange>) -> PyResult<[f64; 3]> {
    let v = a.as_array();
    if v.len() != 3 {
        return Err(PyValueError::new_err(
            "point must have exactly 3 coordinates",
        ));
    }
    Ok([v[0], v[1], v[2]])
}

fn parse_frame<'py>(a: &PyArrayLike2<'py, f64, AllowTypeChange>) -> PyResult<[[f64; 3]; 3]> {
    let v = a.as_array();
    if v.nrows() != 3 || v.ncols() != 3 {
        return Err(PyValueError::new_err("frame must be 3x3"));
    }
    Ok([
        [v[[0, 0]], v[[0, 1]], v[[0, 2]]],
        [v[[1, 0]], v[[1, 1]], v[[1, 2]]],
        [v[[2, 0]], v[[2, 1]], v[[2, 2]]],
    ])
}

fn mat3_mul(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    out
}

fn mat3_vec(a: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        a[0][0] * v[0] + a[0][1] * v[1] + a[0][2] * v[2],
        a[1][0] * v[0] + a[1][1] * v[1] + a[1][2] * v[2],
        a[2][0] * v[0] + a[2][1] * v[1] + a[2][2] * v[2],
    ]
}

fn mat3x4(r: [[f64; 3]; 3], t: [f64; 3]) -> [f64; 12] {
    [
        r[0][0], r[1][0], r[2][0], r[0][1], r[1][1], r[2][1], r[0][2], r[1][2], r[2][2], t[0],
        t[1], t[2],
    ]
}

/// Split a triangle soup into edge-connected face components, matching the
/// pure-numpy `_split_face_components` used by outer-shell extraction.
fn split_components(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
) -> Vec<(Vec<[f64; 3]>, Vec<[usize; 3]>)> {
    let n = tris.len();
    if n == 0 {
        return Vec::new();
    }
    let mut edge_faces: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    for (fi, t) in tris.iter().enumerate() {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a < b { (a, b) } else { (b, a) };
            edge_faces.entry(key).or_default().push(fi);
        }
    }
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for faces in edge_faces.values() {
        if faces.len() == 2 {
            let ra = find(&mut parent, faces[0]);
            let rb = find(&mut parent, faces[1]);
            if ra != rb {
                parent[ra] = rb;
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut out = Vec::with_capacity(groups.len());
    for (_, faces) in groups {
        let mut used: Vec<usize> = faces.iter().flat_map(|&fi| tris[fi]).collect();
        used.sort_unstable();
        used.dedup();
        let mut remap: HashMap<usize, usize> = HashMap::with_capacity(used.len());
        for (new, &old) in used.iter().enumerate() {
            remap.insert(old, new);
        }
        let cv: Vec<[f64; 3]> = used.iter().map(|&i| verts[i]).collect();
        let ct: Vec<[usize; 3]> = faces
            .iter()
            .map(|&fi| {
                [
                    remap[&tris[fi][0]],
                    remap[&tris[fi][1]],
                    remap[&tris[fi][2]],
                ]
            })
            .collect();
        out.push((cv, ct));
    }
    out
}

/// One edge-connected face component of a Manifold mesh export.
///
/// `faces` and `verts` are ascending original indices. Components are emitted
/// in the order of their smallest vertex index, which matches the label order
/// of Manifold's `DisjointSets::connectedComponents` (a first-vertex scan) and
/// therefore the part order of `Manifold::Decompose`.
struct FaceComponent {
    faces: Vec<usize>,
    verts: Vec<usize>,
}

fn edge_face_components(tris: &[[usize; 3]]) -> Vec<FaceComponent> {
    let n = tris.len();
    if n == 0 {
        return Vec::new();
    }
    let mut edge_faces: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    for (fi, t) in tris.iter().enumerate() {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a < b { (a, b) } else { (b, a) };
            edge_faces.entry(key).or_default().push(fi);
        }
    }
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for faces in edge_faces.values() {
        if faces.len() == 2 {
            let ra = find(&mut parent, faces[0]);
            let rb = find(&mut parent, faces[1]);
            if ra != rb {
                parent[ra] = rb;
            }
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut comps: Vec<FaceComponent> = groups
        .into_values()
        .map(|faces| {
            let mut used: Vec<usize> = faces.iter().flat_map(|&fi| tris[fi]).collect();
            used.sort_unstable();
            used.dedup();
            FaceComponent { faces, verts: used }
        })
        .collect();
    comps.sort_by_key(|c| (c.verts[0], c.faces[0]));
    comps
}

/// Extract one component as a standalone mesh, keeping the ascending vertex
/// and face order used by Manifold's `Decompose`.
fn component_arrays(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    comp: &FaceComponent,
) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
    let mut remap: HashMap<usize, usize> = HashMap::with_capacity(comp.verts.len());
    for (new, &old) in comp.verts.iter().enumerate() {
        remap.insert(old, new);
    }
    let cv: Vec<[f64; 3]> = comp.verts.iter().map(|&i| verts[i]).collect();
    let ct: Vec<[usize; 3]> = comp
        .faces
        .iter()
        .map(|&fi| {
            [
                remap[&tris[fi][0]],
                remap[&tris[fi][1]],
                remap[&tris[fi][2]],
            ]
        })
        .collect();
    (cv, ct)
}

/// Split `cur` into its connected Solid parts.
///
/// Fast path: walk the manifold's mesh export once and rebuild each part from
/// its arrays, avoiding `Manifold::Decompose`'s per-component scan over every
/// vertex and face (quadratic on meshes with thousands of components). The
/// components are emitted in Manifold's label order and each part is built in
/// the same vertex/face order, so the downstream `batch_boolean`/`compose`
/// receive the same input sequence.
///
/// At or below `FAST_COMPONENT_MIN` the exact `Manifold::decompose()` path is
/// kept (its per-component scan is cheap there and its part construction keeps
/// the output bytes stable). Returns `(n, parts)`; `parts` is empty when
/// `n <= 1` (no union needed). When a rebuilt part is rejected, the exact
/// `Manifold::decompose()` fallback is used instead.
fn stage2_components(cur: &Manifold) -> (usize, Vec<Manifold>) {
    let (v, t) = mesh_to_f32(cur);
    let comps = edge_face_components(&t);
    if comps.len() <= 1 {
        return (comps.len(), Vec::new());
    }
    if comps.len() > FAST_COMPONENT_MIN {
        let mut parts = Vec::with_capacity(comps.len());
        let mut built = true;
        for comp in &comps {
            let (cv, ct) = component_arrays(&v, &t, comp);
            match f32_mesh(&cv, &ct) {
                Ok(m) if m.status().is_ok() && !m.is_empty() => parts.push(m),
                _ => {
                    built = false;
                    break;
                }
            }
        }
        if built {
            return (comps.len(), parts);
        }
    }
    let parts = cur.decompose();
    (parts.len(), parts)
}

fn area_weighted_centroid(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> [f64; 3] {
    let mut acc = [0.0; 3];
    let mut wsum = 0.0;
    for t in tris {
        let a = verts[t[0]];
        let b = verts[t[1]];
        let c = verts[t[2]];
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let cross = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        let area = 0.5 * (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt();
        let center = [
            (a[0] + b[0] + c[0]) / 3.0,
            (a[1] + b[1] + c[1]) / 3.0,
            (a[2] + b[2] + c[2]) / 3.0,
        ];
        for k in 0..3 {
            acc[k] += center[k] * area;
        }
        wsum += area;
    }
    if wsum > 0.0 {
        [acc[0] / wsum, acc[1] / wsum, acc[2] / wsum]
    } else if !verts.is_empty() {
        let mut m = [0.0; 3];
        for p in verts {
            for k in 0..3 {
                m[k] += p[k];
            }
        }
        [
            m[0] / verts.len() as f64,
            m[1] / verts.len() as f64,
            m[2] / verts.len() as f64,
        ]
    } else {
        [0.0; 3]
    }
}

fn extents(verts: &[[f64; 3]]) -> [f64; 3] {
    if verts.is_empty() {
        return [0.0; 3];
    }
    let mut lo = verts[0];
    let mut hi = verts[0];
    for p in verts {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]]
}

fn largest_component(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
    let comps = split_components(verts, tris);
    if comps.is_empty() {
        return (verts.to_vec(), tris.to_vec());
    }
    let mut best = 0usize;
    let mut best_key = f64::NEG_INFINITY;
    for (i, (v, _)) in comps.iter().enumerate() {
        let e = extents(v);
        let key = e[0] * e[1] * e[2];
        if key > best_key {
            best_key = key;
            best = i;
        }
    }
    comps.into_iter().nth(best).unwrap()
}

fn set_report_common(
    report: &Bound<'_, PyDict>,
    input_v: usize,
    input_f: usize,
    status: &str,
    watertight: bool,
) -> PyResult<()> {
    report.set_item("input_vertices", input_v)?;
    report.set_item("input_faces", input_f)?;
    report.set_item("input_watertight", watertight)?;
    report.set_item("construct_status", status)?;
    report.set_item("ok", false)?;
    Ok(())
}

/// Stage 2: rebuild a closed mesh as a valid Manifold solid, merging
/// overlapping shells and dropping flat/post-union debris. Same algorithm and
/// report fields as the retired `manifold_bridge.run_bridge`.
#[pyfunction]
#[pyo3(signature = (verts, tris))]
pub fn stage2_rebuild<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
) -> PyResult<(
    Bound<'py, numpy::PyArray2<f32>>,
    Bound<'py, numpy::PyArray2<i32>>,
    Bound<'py, PyDict>,
)> {
    let v = parse_verts2(&verts)?;
    let t = parse_tris2(&tris, v.len())?;
    let report = PyDict::new(py);
    set_report_common(&report, v.len(), t.len(), "", false)?;
    let man = match f32_mesh(&v, &t) {
        Ok(m) => m,
        Err(e) => {
            let st = format!("Error.{e:?}");
            set_report_common(&report, v.len(), t.len(), &st, false)?;
            report.set_item(
                "error",
                format!("manifold3d could not process the input ({st})"),
            )?;
            let (ev, et) = f32_arrays(py, &[], &[])?;
            return Ok((ev, et, report));
        }
    };
    let st = status_string(man.status());
    let watertight = man.status().is_ok() && !man.is_empty();
    set_report_common(&report, v.len(), t.len(), &st, watertight)?;
    if man.is_empty() {
        report.set_item(
            "error",
            format!("manifold3d could not process the input ({st})"),
        )?;
        let (ev, et) = f32_arrays(py, &[], &[])?;
        return Ok((ev, et, report));
    }
    report.set_item("volume_before", man.volume())?;

    let mut cur = man;
    let (n_pre, parts) = stage2_components(&cur);
    report.set_item("shells_found", n_pre)?;
    let mut shells = n_pre;
    if n_pre > 1 {
        let flags: Vec<bool> = parts.iter().map(is_debris_part).collect();
        let dropped = flags.iter().filter(|x| **x).count();
        if dropped > 0 {
            report.set_item("debris_parts_dropped", dropped)?;
        }
        let kept: Vec<Manifold> = if dropped > 0 && dropped < parts.len() {
            parts
                .into_iter()
                .zip(flags)
                .filter_map(|(p, d)| if d { None } else { Some(p) })
                .collect()
        } else {
            parts
        };
        report.set_item("shells_merged", kept.len())?;
        shells = kept.len();
        cur = if kept.len() == 1 {
            kept.into_iter().next().unwrap()
        } else {
            Manifold::batch_boolean(&kept, OpType::Add)
        };
        report.set_item("volume_after_union", cur.volume())?;
    }

    if n_pre > 1 && cur.status().is_ok() {
        let (n_post, post) = stage2_components(&cur);
        shells = n_post;
        if n_post > 1 {
            let vols: Vec<f64> = post.iter().map(|p| p.volume().abs()).collect();
            let largest = vols
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .map(|(i, _)| i)
                .unwrap();
            let kept: Vec<Manifold> = post
                .into_iter()
                .enumerate()
                .filter_map(|(i, p)| {
                    if i == largest || !is_debris_part(&p) {
                        Some(p)
                    } else {
                        None
                    }
                })
                .collect();
            if kept.len() < vols.len() {
                report.set_item("post_union_debris_dropped", vols.len() - kept.len())?;
                shells = kept.len();
                cur = if kept.len() == 1 {
                    kept.into_iter().next().unwrap()
                } else {
                    Manifold::compose(&kept)
                };
            }
        }
    }

    if cur.status().is_err() {
        let est = status_string(cur.status());
        report.set_item(
            "error",
            format!("manifold3d could not process the input ({est})"),
        )?;
        let (ev, et) = f32_arrays(py, &[], &[])?;
        return Ok((ev, et, report));
    }

    let (ov, ot) = mesh_to_f32(&cur);
    report.set_item("output_vertices", ov.len())?;
    report.set_item("output_faces", ot.len())?;
    report.set_item("volume_after", cur.volume())?;
    report.set_item("shells", shells)?;
    report.set_item("ok", true)?;
    let (pv, pt) = f32_arrays(py, &ov, &ot)?;
    Ok((pv, pt, report))
}

/// Independent watertight/manifold verdict (the old
/// `manifold_bridge.watertight_check`). Never raises for bad mesh input.
#[pyfunction]
#[pyo3(signature = (verts, tris))]
pub fn watertight_check(
    verts: PyArrayLike2<'_, f64, AllowTypeChange>,
    tris: PyArrayLike2<'_, i32, AllowTypeChange>,
) -> PyResult<(Option<bool>, String)> {
    let v = match parse_verts2(&verts) {
        Ok(v) => v,
        Err(e) => return Ok((Some(false), e.to_string())),
    };
    let t = match parse_tris2(&tris, v.len()) {
        Ok(t) => t,
        Err(e) => return Ok((Some(false), e.to_string())),
    };
    if v.is_empty() || t.is_empty() {
        return Ok((Some(false), "empty".to_string()));
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f32_mesh(&v, &t)));
    match result {
        Ok(Ok(m)) => {
            let st = status_string(m.status());
            Ok((Some(m.status().is_ok() && !m.is_empty()), st))
        }
        Ok(Err(e)) => Ok((Some(false), format!("Error.{e:?}"))),
        Err(_) => Ok((Some(false), "panic: manifold import failed".to_string())),
    }
}

/// Union the edge-connected components of a multi-component mesh after
/// dilating each around its area-weighted centroid, then keep the largest
/// outer component. Replaces `hull.extract_outer_shell`'s manifold3d union.
#[pyfunction]
#[pyo3(signature = (verts, tris, dilation=None))]
pub fn outer_shell_union<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
    dilation: Option<f64>,
) -> PyResult<(
    Bound<'py, numpy::PyArray2<f64>>,
    Bound<'py, numpy::PyArray2<i32>>,
)> {
    let v = parse_verts2(&verts)?;
    let t = parse_tris2(&tris, v.len())?;
    let comps = split_components(&v, &t);
    if comps.len() <= 1 {
        return f64_arrays(py, &v, &t);
    }
    let mut lo = v[0];
    let mut hi = v[0];
    for p in &v {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let diag = ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2)).sqrt();
    let Some((lv, lt)) = union_ladder(&comps, diag, dilation) else {
        return f64_arrays(py, &v, &t);
    };
    f64_arrays(py, &lv, &lt)
}

/// Dilation ladder for `outer_shell_union`. Tries ZERO dilation first: the
/// historical default `5e-4*diag` dilation pushed result vertices outside the
/// input (the measured "leak"). Zero dilation keeps the outward result vertices
/// on the input surface, and is preferred whenever it preserves at least as much
/// material (faces after `largest_component`) as the default; this removes the
/// leak at no topology cost. Only when the default keeps strictly more faces
/// (the parts need a bridge) is the default used. An explicit `dilation`
/// argument is honoured verbatim (single attempt, unchanged behaviour). Pure so
/// it can be unit-tested without the Python GIL; returns the final
/// (`largest_component`) f64 arrays.
fn union_ladder(
    comps: &[(Vec<[f64; 3]>, Vec<[usize; 3]>)],
    diag: f64,
    dilation: Option<f64>,
) -> Option<(Vec<[f64; 3]>, Vec<[usize; 3]>)> {
    let unite = |dil: f64| -> Option<Manifold> {
        let mut united: Option<Manifold> = None;
        for (cv, ct) in comps {
            let c = area_weighted_centroid(cv, ct);
            let ext = extents(cv);
            let scale = [
                (ext[0] + dil) / ext[0].max(1e-6),
                (ext[1] + dil) / ext[1].max(1e-6),
                (ext[2] + dil) / ext[2].max(1e-6),
            ];
            let scaled: Vec<[f64; 3]> = cv
                .iter()
                .map(|p| {
                    [
                        (p[0] - c[0]) * scale[0] + c[0],
                        (p[1] - c[1]) * scale[1] + c[1],
                        (p[2] - c[2]) * scale[2] + c[2],
                    ]
                })
                .collect();
            let Ok(m) = f32_mesh(&scaled, ct) else {
                continue;
            };
            if m.status().is_err() || m.is_empty() {
                continue;
            }
            united = Some(match united {
                Some(u) => u.union(&m),
                None => m,
            });
        }
        united
    };
    let attempt = |dil: f64| -> Option<(Vec<[f64; 3]>, Vec<[usize; 3]>)> {
        let u = unite(dil)?;
        if u.status().is_err() || u.is_empty() {
            return None;
        }
        let (ov, ot) = mesh_to_f32(&u);
        Some(largest_component(&ov, &ot))
    };

    if let Some(d) = dilation {
        return attempt(d);
    }

    let dil_default = 5e-4 * if diag > 1e-6 { diag } else { 100.0 };
    match (attempt(0.0), attempt(dil_default)) {
        (Some(zero), Some(def)) => Some(if zero.1.len() >= def.1.len() { zero } else { def }),
        (Some(zero), None) => Some(zero),
        (None, def) => def,
    }
}

/// Union two f64 meshes (Graft local stitch).
#[pyfunction]
#[pyo3(signature = (av, at, bv, bt))]
pub fn union_pair<'py>(
    py: Python<'py>,
    av: PyArrayLike2<'py, f64, AllowTypeChange>,
    at: PyArrayLike2<'py, i32, AllowTypeChange>,
    bv: PyArrayLike2<'py, f64, AllowTypeChange>,
    bt: PyArrayLike2<'py, i32, AllowTypeChange>,
) -> PyResult<(
    Bound<'py, numpy::PyArray2<f64>>,
    Bound<'py, numpy::PyArray2<i32>>,
)> {
    let a_v = parse_verts2(&av)?;
    let a_t = parse_tris2(&at, a_v.len())?;
    let b_v = parse_verts2(&bv)?;
    let b_t = parse_tris2(&bt, b_v.len())?;
    let a = f64_mesh(&a_v, &a_t)
        .map_err(|e| PyValueError::new_err(format!("manifold import failed: {e:?}")))?;
    let b = f64_mesh(&b_v, &b_t)
        .map_err(|e| PyValueError::new_err(format!("manifold import failed: {e:?}")))?;
    let out = a.union(&b);
    if out.status().is_err() {
        return Err(PyValueError::new_err(format!(
            "manifold union failed: {:?}",
            out.status()
        )));
    }
    let (ov, ot) = mesh_to_f64(&out);
    f64_arrays(py, &ov, &ot)
}

/// Volumetric boolean transplant used by the repeated-element repair.
///
/// The OBB is computed in Python (numpy eig) and passed in, so this function
/// performs only the Manifold CSG sequence. Returns vertices, triangles,
/// absolute volume change, destination boxes and the watertight verdict.
#[pyfunction]
#[pyo3(signature = (verts, tris, center, extents, frame, transforms))]
pub fn boolean_transplant<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
    center: PyArrayLike1<'py, f64, AllowTypeChange>,
    extents: PyArrayLike1<'py, f64, AllowTypeChange>,
    frame: PyArrayLike2<'py, f64, AllowTypeChange>,
    transforms: PyArrayLike3<'py, f64, AllowTypeChange>,
) -> PyResult<(
    Bound<'py, numpy::PyArray2<f64>>,
    Bound<'py, numpy::PyArray2<i32>>,
    f64,
    Bound<'py, PyList>,
    bool,
)> {
    let v = parse_verts2(&verts)?;
    let t = parse_tris2(&tris, v.len())?;
    let c_src = parse_point3(&center)?;
    let ext_src = parse_point3(&extents)?;
    let frame_src = parse_frame(&frame)?;
    let tm = transforms.as_array();
    if tm.shape().len() != 3 || tm.shape()[1] != 4 || tm.shape()[2] != 4 {
        return Err(PyValueError::new_err("transforms must be Nx4x4"));
    }
    let n = tm.shape()[0];
    let mut mats: Vec<([[f64; 3]; 3], [f64; 3])> = Vec::with_capacity(n);
    for i in 0..n {
        let r = [
            [tm[[i, 0, 0]], tm[[i, 0, 1]], tm[[i, 0, 2]]],
            [tm[[i, 1, 0]], tm[[i, 1, 1]], tm[[i, 1, 2]]],
            [tm[[i, 2, 0]], tm[[i, 2, 1]], tm[[i, 2, 2]]],
        ];
        mats.push((r, [tm[[i, 0, 3]], tm[[i, 1, 3]], tm[[i, 2, 3]]]));
    }

    let man = f64_mesh(&v, &t).map_err(|e| {
        PyValueError::new_err(format!("manifold3d error status on input mesh: {e:?}"))
    })?;
    if man.status().is_err() {
        return Err(PyValueError::new_err(format!(
            "manifold3d error status on input mesh: {:?}",
            man.status()
        )));
    }
    if man.is_empty() {
        return Err(PyValueError::new_err("manifold3d solid is empty"));
    }
    let initial_vol = man.volume();

    let box_src = Manifold::cube(ext_src[0], ext_src[1], ext_src[2], true)
        .transform(&mat3x4(frame_src, c_src));
    let element_solid = man.intersection(&box_src);
    let mut current = man;

    let eps = 1e-3;
    let ext_cut = [
        ext_src[0] * (1.0 - eps),
        ext_src[1] * (1.0 - eps),
        ext_src[2] * (1.0 - eps),
    ];
    let cube_cut = Manifold::cube(ext_cut[0], ext_cut[1], ext_cut[2], true);
    let box_src_cut = cube_cut.transform(&mat3x4(frame_src, c_src));

    let boxes = PyList::empty(py);
    for (r, tr) in mats {
        let c_dst = {
            let x = mat3_vec(r, c_src);
            [x[0] + tr[0], x[1] + tr[1], x[2] + tr[2]]
        };
        let frame_dst = mat3_mul(r, frame_src);
        let c_arr = numpy::PyArray1::from_vec(py, c_dst.to_vec());
        let e_arr = numpy::PyArray1::from_vec(py, ext_src.to_vec());
        let f_arr = numpy::PyArray2::from_vec2(
            py,
            &frame_dst.iter().map(|row| row.to_vec()).collect::<Vec<_>>(),
        )?;
        boxes.append(PyTuple::new(
            py,
            [c_arr.into_any(), e_arr.into_any(), f_arr.into_any()],
        )?)?;

        let box_dst_cut = box_src_cut.transform(&mat3x4(r, tr));
        let cavity = current.difference(&box_dst_cut);
        let element_prime = element_solid.transform(&mat3x4(r, tr));
        current = cavity.union(&element_prime);
    }

    let is_watertight = current.status().is_ok();
    let vol_change = (current.volume() - initial_vol).abs();
    let (ov, ot) = mesh_to_f64(&current);
    let (pv, pt) = f64_arrays(py, &ov, &ot)?;
    Ok((pv, pt, vol_change, boxes, is_watertight))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_components_follow_first_vertex_order() {
        // Component A uses vertices 2..6, component B uses 0,1,6,7.
        // B must come first because its smallest vertex index is 0, even
        // though A's faces were emitted first.
        let tris = vec![[2, 3, 4], [2, 4, 5], [0, 1, 6], [0, 6, 7]];
        let comps = edge_face_components(&tris);
        assert_eq!(comps.len(), 2);
        assert_eq!(comps[0].verts, vec![0, 1, 6, 7]);
        assert_eq!(comps[0].faces, vec![2, 3]);
        assert_eq!(comps[1].verts, vec![2, 3, 4, 5]);
        assert_eq!(comps[1].faces, vec![0, 1]);
    }

    #[test]
    fn edge_components_do_not_join_at_a_shared_vertex() {
        // Two triangles meeting only at vertex 0 are separate edge-connected
        // components. Manifold splits such a pinched vertex at import, so this
        // is the same partition as Decompose.
        let tris = vec![[0, 1, 2], [0, 3, 4]];
        let comps = edge_face_components(&tris);
        assert_eq!(comps.len(), 2);
        assert_eq!(comps[0].faces, vec![0]);
        assert_eq!(comps[1].faces, vec![1]);
    }

    #[test]
    fn component_arrays_keep_ascending_ids() {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
        ];
        let tris = vec![[3, 1, 2]];
        let comps = edge_face_components(&tris);
        assert_eq!(comps.len(), 1);
        let (cv, ct) = component_arrays(&verts, &tris, &comps[0]);
        assert_eq!(cv, vec![verts[1], verts[2], verts[3]]);
        assert_eq!(ct, vec![[2, 0, 1]]);
    }

    fn cube(cx: f64, cy: f64, cz: f64, side: f64) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let h = side / 2.0;
        let v = vec![
            [cx - h, cy - h, cz - h],
            [cx + h, cy - h, cz - h],
            [cx + h, cy + h, cz - h],
            [cx - h, cy + h, cz - h],
            [cx - h, cy - h, cz + h],
            [cx + h, cy - h, cz + h],
            [cx + h, cy + h, cz + h],
            [cx - h, cy + h, cz + h],
        ];
        let t = vec![
            [0, 2, 1], [0, 3, 2],
            [4, 5, 6], [4, 6, 7],
            [0, 1, 5], [0, 5, 4],
            [1, 2, 6], [1, 6, 5],
            [2, 3, 7], [2, 7, 6],
            [3, 0, 4], [3, 4, 7],
        ];
        (v, t)
    }

    fn bbox(v: &[[f64; 3]]) -> ([f64; 3], [f64; 3]) {
        let mut lo = v[0];
        let mut hi = v[0];
        for p in v {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        (lo, hi)
    }

    fn two_overlapping_cubes() -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let (av, at) = cube(0.0, 0.0, 0.0, 1.0);
        let (bv, bt) = cube(0.5, 0.0, 0.0, 1.0);
        let mut verts = av.clone();
        verts.extend_from_slice(&bv);
        let off = av.len();
        let mut tris = at;
        tris.extend(
            bt.iter()
                .map(|t| [t[0] + off, t[1] + off, t[2] + off]),
        );
        (verts, tris)
    }

    #[test]
    fn union_ladder_prefers_zero_dilation_when_components_fuse() {
        let (verts, tris) = two_overlapping_cubes();
        let comps = split_components(&verts, &tris);
        assert_eq!(comps.len(), 2);
        let (lo, hi) = bbox(&verts);
        let diag = ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2)).sqrt();
        let out = union_ladder(&comps, diag, None).expect("union of overlapping cubes");
        let (ov, _ot) = out;
        let (olo, ohi) = bbox(&ov);
        let eps = 1e-4;
        for k in 0..3 {
            assert!(
                olo[k] >= lo[k] - eps && ohi[k] <= hi[k] + eps,
                "auto ladder leaked outside the input bbox on axis {k}: {olo:?}..{ohi:?} vs {lo:?}..{hi:?}"
            );
        }
    }

    #[test]
    fn union_explicit_dilation_grows_beyond_input() {
        let (verts, tris) = two_overlapping_cubes();
        let comps = split_components(&verts, &tris);
        let (lo, hi) = bbox(&verts);
        let diag = ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2)).sqrt();
        let out = union_ladder(&comps, diag, Some(5e-4 * diag)).expect("union with explicit dilation");
        let (ov, _ot) = out;
        let (olo, ohi) = bbox(&ov);
        let eps = 1e-4;
        assert!(
            olo[0] < lo[0] - eps || ohi[0] > hi[0] + eps,
            "explicit dilation did not grow the result: {olo:?}..{ohi:?} vs {lo:?}..{hi:?}"
        );
    }
}
