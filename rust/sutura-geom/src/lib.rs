//! sutura_geom — foundation crate for Sutura's future indirect-predicates geometry core.
//!
//! Phase A scope: thin PyO3 wrapper exposing the explicit-point robust predicates
//! `orient3d` and `insphere` from the `robust` crate (Shewchuk adaptive-precision
//! arithmetic) to Python.
//!
//! Phase B scope (in progress): indirect point representations (LPI/PPI),
//! exact predicates on explicit and implicit points (`orient3d`, `orient2d`,
//! `incircle`), and the predicate core for the future arrangement-lite engine.

use numpy::{AllowTypeChange, PyArrayLike1, PyArrayLike2, PyArrayLike3};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};
use robust::{insphere as robust_insphere, orient3d as robust_orient3d, Coord3D};

pub mod antipodal;
pub mod arrangement;
pub mod cdt2d;
pub mod dressing;
pub mod dual_contour;
pub mod flap;
pub mod interval;
pub mod lava;
pub mod melt;
pub mod manifold;
pub mod mincut;
pub mod morph;
pub mod octree;
pub mod point;
pub mod pointcloud;
pub mod predicates2d;
pub mod predicates3d;
pub mod profile;
pub mod rbf;
pub mod raycast;
pub mod shell;
pub mod tamponade;
pub mod topology;
pub mod triangle_intersection;
pub mod winding;

use numpy::ndarray::{Array3, ShapeBuilder};
use numpy::{PyArray1, PyArray2, PyArray3};
use pyo3::types::PyList;
use rayon::prelude::*;

/// Maximum grid size for the morphology API (`512^3` voxel budget).  Larger
/// requested grids are automatically coarsened (the voxel size is scaled up)
/// until they fit.
pub const MAX_VOXELS: u64 = 512 * 512 * 512;

/// Face count above which `self_intersecting_faces` refuses to run the exact
/// classifier (the broad phase plus exact predicates are super-linear); the
/// caller sees a `skipped` report instead.
pub const SI_MAX_FACES: usize = 2_000_000;

/// Parse a point argument into `[f64; 3]`, accepting a tuple, list, or numpy
/// 1-D array of length 3.
fn parse_point(arr: &PyArrayLike1<'_, f64, AllowTypeChange>) -> PyResult<[f64; 3]> {
    let view = arr.as_array();
    if view.len() != 3 {
        return Err(PyValueError::new_err(
            "point must have exactly 3 coordinates",
        ));
    }
    Ok([view[0], view[1], view[2]])
}

/// Pure-Rust `orient3d` helper: sign of the volume of the tetrahedron
/// `(pa, pb, pc, pd)`. Positive when `pd` lies below the plane through
/// `pa, pb, pc` (with `pa, pb, pc` counterclockwise viewed from above),
/// zero when coplanar, negative otherwise. Exact via adaptive precision.
fn orient3d_coords(pa: [f64; 3], pb: [f64; 3], pc: [f64; 3], pd: [f64; 3]) -> f64 {
    let pa = Coord3D {
        x: pa[0],
        y: pa[1],
        z: pa[2],
    };
    let pb = Coord3D {
        x: pb[0],
        y: pb[1],
        z: pb[2],
    };
    let pc = Coord3D {
        x: pc[0],
        y: pc[1],
        z: pc[2],
    };
    let pd = Coord3D {
        x: pd[0],
        y: pd[1],
        z: pd[2],
    };
    robust_orient3d(pa, pb, pc, pd)
}

/// Sign of the oriented volume of the tetrahedron `(pa, pb, pc, pd)`.
///
/// Returns a positive value when `pd` lies below the plane through `pa, pb, pc`
/// (with `pa, pb, pc` counterclockwise when viewed from above the plane), a
/// negative value when `pd` lies above it, and exactly `0` when the four points
/// are coplanar.  Each point is a length-3 sequence (tuple, list, or numpy
/// 1-D array).
#[pyfunction]
#[pyo3(signature = (pa, pb, pc, pd))]
fn orient3d<'py>(
    pa: PyArrayLike1<'py, f64, AllowTypeChange>,
    pb: PyArrayLike1<'py, f64, AllowTypeChange>,
    pc: PyArrayLike1<'py, f64, AllowTypeChange>,
    pd: PyArrayLike1<'py, f64, AllowTypeChange>,
) -> PyResult<f64> {
    let pa = parse_point(&pa)?;
    let pb = parse_point(&pb)?;
    let pc = parse_point(&pc)?;
    let pd = parse_point(&pd)?;
    Ok(orient3d_coords(pa, pb, pc, pd))
}

/// Pure-Rust `insphere` helper: sign of whether `pe` lies inside the sphere
/// through `pa, pb, pc, pd`. Requires `(pa, pb, pc, pd)` positively oriented.
/// Zero when cospherical, positive when `pe` is inside, negative when outside.
fn insphere_coords(pa: [f64; 3], pb: [f64; 3], pc: [f64; 3], pd: [f64; 3], pe: [f64; 3]) -> f64 {
    let pa = Coord3D {
        x: pa[0],
        y: pa[1],
        z: pa[2],
    };
    let pb = Coord3D {
        x: pb[0],
        y: pb[1],
        z: pb[2],
    };
    let pc = Coord3D {
        x: pc[0],
        y: pc[1],
        z: pc[2],
    };
    let pd = Coord3D {
        x: pd[0],
        y: pd[1],
        z: pd[2],
    };
    let pe = Coord3D {
        x: pe[0],
        y: pe[1],
        z: pe[2],
    };
    robust_insphere(pa, pb, pc, pd, pe)
}

/// Sign of whether `pe` lies inside the sphere through `pa, pb, pc, pd`.
///
/// Returns a positive value when `pe` is inside the sphere, a negative value
/// when it is outside, and exactly `0` when the five points are cospherical.
/// The first four points must be positively oriented (see `orient3d`). Each
/// point is a length-3 sequence (tuple, list, or numpy 1-D array).
#[pyfunction]
#[pyo3(signature = (pa, pb, pc, pd, pe))]
#[allow(clippy::too_many_arguments)]
fn insphere<'py>(
    pa: PyArrayLike1<'py, f64, AllowTypeChange>,
    pb: PyArrayLike1<'py, f64, AllowTypeChange>,
    pc: PyArrayLike1<'py, f64, AllowTypeChange>,
    pd: PyArrayLike1<'py, f64, AllowTypeChange>,
    pe: PyArrayLike1<'py, f64, AllowTypeChange>,
) -> PyResult<f64> {
    let pa = parse_point(&pa)?;
    let pb = parse_point(&pb)?;
    let pc = parse_point(&pc)?;
    let pd = parse_point(&pd)?;
    let pe = parse_point(&pe)?;
    Ok(insphere_coords(pa, pb, pc, pd, pe))
}

/// Split a self-intersecting triangle soup into one where no two triangles
/// properly intersect.  Returns `(verts_out, tris_out, report)` where
/// `verts_out` is `(N, 3)` float64, `tris_out` is `(M, 3)` int32, and
/// `report` is a dict with `input_faces`, `output_faces`,
/// `si_pairs_detected`, `degenerate_cases`, and `converged`.
#[pyfunction]
#[pyo3(signature = (verts, tris))]
fn arrangement_lite<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
) -> PyResult<(
    Bound<'py, numpy::PyArray2<f64>>,
    Bound<'py, numpy::PyArray2<i32>>,
    Bound<'py, PyDict>,
)> {
    let vv = verts.as_array();
    let tt = tris.as_array();
    let n = vv.nrows();
    let m = tt.nrows();
    if vv.ncols() != 3 || tt.ncols() != 3 {
        return Err(PyValueError::new_err(
            "verts must be Nx3 and tris must be Mx3",
        ));
    }

    let mut rust_verts: Vec<[f64; 3]> = Vec::with_capacity(n);
    for i in 0..n {
        rust_verts.push([vv[[i, 0]], vv[[i, 1]], vv[[i, 2]]]);
    }

    let mut rust_tris: Vec<[usize; 3]> = Vec::with_capacity(m);
    for i in 0..m {
        let a = tt[[i, 0]];
        let b = tt[[i, 1]];
        let c = tt[[i, 2]];
        if a < 0 || b < 0 || c < 0 || a as usize >= n || b as usize >= n || c as usize >= n {
            return Err(PyValueError::new_err("triangle index out of range"));
        }
        rust_tris.push([a as usize, b as usize, c as usize]);
    }

    let (pool, out_tris, report) = arrangement::arrangement_lite_core(&rust_verts, &rust_tris)
        .map_err(PyValueError::new_err)?;

    let (verts_np, tris_np, report_dict) = crate::profile_time!(PYO3_MARSHAL, {
        let out_verts: Vec<Vec<f64>> = pool
            .iter()
            .map(|p| arrangement::rat3_to_f64(p).to_vec())
            .collect();
        let out_tris_i32: Vec<Vec<i32>> = out_tris
            .iter()
            .map(|t| vec![t[0] as i32, t[1] as i32, t[2] as i32])
            .collect();

        let verts_np = numpy::PyArray2::from_vec2(py, &out_verts)?;
        let tris_np = numpy::PyArray2::from_vec2(py, &out_tris_i32)?;

        let report_dict = PyDict::new(py);
        (verts_np, tris_np, report_dict)
    });
    report_dict.set_item("input_faces", report.input_faces as i64)?;
    report_dict.set_item("output_faces", report.output_faces as i64)?;
    report_dict.set_item("si_pairs_detected", report.si_pairs_detected as i64)?;
    report_dict.set_item("converged", report.converged)?;
    let degen = PyDict::new(py);
    for (k, v) in report.degenerate_cases {
        degen.set_item(k, v as i64)?;
    }
    report_dict.set_item("degenerate_cases", degen)?;

    Ok((verts_np, tris_np, report_dict))
}

/// Developer hook: set the experimental, output-changing CDT behaviours
/// (`cdt2d::experimental` bits: 1 = keep constraint flags on flip,
/// 2 = Sloan flips, 4 = propagate edge points) for the calling thread; 0 restores
/// the reference behaviour.  Returns the previous value.
#[pyfunction]
fn _set_cdt_experimental(bits: u8) -> u8 {
    let prev = cdt2d::experimental::options();
    cdt2d::experimental::set_options(bits);
    prev
}

/// Parse `(N, 3)` arrays into Rust point/index vectors.
fn parse_mesh(
    verts: &PyArrayLike2<'_, f64, AllowTypeChange>,
    tris: &PyArrayLike2<'_, i32, AllowTypeChange>,
) -> PyResult<(Vec<[f64; 3]>, Vec<[usize; 3]>)> {
    let vv = verts.as_array();
    let tt = tris.as_array();
    if vv.ncols() != 3 || tt.ncols() != 3 {
        return Err(PyValueError::new_err(
            "verts must be Nx3 and tris must be Mx3",
        ));
    }
    let n = vv.nrows();
    let mut rust_verts: Vec<[f64; 3]> = Vec::with_capacity(n);
    if let Some(s) = vv.as_slice() {
        rust_verts.extend(s.chunks_exact(3).map(|c| [c[0], c[1], c[2]]));
    } else {
        for i in 0..n {
            rust_verts.push([vv[[i, 0]], vv[[i, 1]], vv[[i, 2]]]);
        }
    }
    let m = tt.nrows();
    let mut rust_tris: Vec<[usize; 3]> = Vec::with_capacity(m);
    if let Some(s) = tt.as_slice() {
        for c in s.chunks_exact(3) {
            let (a, b, c) = (c[0], c[1], c[2]);
            if a < 0 || b < 0 || c < 0 {
                return Err(PyValueError::new_err("triangle index out of range"));
            }
            let (a, b, c) = (a as usize, b as usize, c as usize);
            if a >= n || b >= n || c >= n {
                return Err(PyValueError::new_err("triangle index out of range"));
            }
            rust_tris.push([a, b, c]);
        }
    } else {
        for i in 0..m {
            let (a, b, c) = (tt[[i, 0]], tt[[i, 1]], tt[[i, 2]]);
            if a < 0 || b < 0 || c < 0 {
                return Err(PyValueError::new_err("triangle index out of range"));
            }
            let (a, b, c) = (a as usize, b as usize, c as usize);
            if a >= n || b >= n || c >= n {
                return Err(PyValueError::new_err("triangle index out of range"));
            }
            rust_tris.push([a, b, c]);
        }
    }
    Ok((rust_verts, rust_tris))
}

fn bbox_of(verts: &[[f64; 3]]) -> winding::Aabb {
    let mut b = winding::Aabb::empty();
    for v in verts {
        b.expand_point(*v);
    }
    b
}

fn parse_box(arr: &PyArrayLike2<'_, f64, AllowTypeChange>) -> PyResult<([f64; 3], [f64; 3])> {
    let v = arr.as_array();
    if v.nrows() != 2 || v.ncols() != 3 {
        return Err(PyValueError::new_err("box must be a 2x3 array of (min, max)"));
    }
    Ok((
        [v[[0, 0]], v[[0, 1]], v[[0, 2]]],
        [v[[1, 0]], v[[1, 1]], v[[1, 2]]],
    ))
}

/// Resolve a grid covering `[bmin - pad*voxel, bmax + pad*voxel]` with an
/// optional explicit voxel and a default target cell count on the longest
/// axis.  Coarsens the voxel until the grid fits `MAX_VOXELS`.
fn plan_grid(
    bmin: [f64; 3],
    bmax: [f64; 3],
    voxel_opt: Option<f64>,
    target: f64,
    pad: usize,
) -> ([f64; 3], f64, [usize; 3], bool) {
    let mut ext = [bmax[0] - bmin[0], bmax[1] - bmin[1], bmax[2] - bmin[2]];
    for e in ext.iter_mut() {
        if !e.is_finite() || *e < 1e-12 {
            *e = 1e-12;
        }
    }
    let maxext = ext[0].max(ext[1]).max(ext[2]);
    let mut voxel = voxel_opt.filter(|v| *v > 0.0).unwrap_or(maxext / target);
    if !voxel.is_finite() || voxel <= 0.0 {
        voxel = maxext / target;
    }
    let mut coarsened = false;
    loop {
        let mut dims = [0usize; 3];
        for i in 0..3 {
            dims[i] = ((bmax[i] - bmin[i]) / voxel).ceil() as usize + 1 + 2 * pad;
        }
        if winding::voxel_count(dims) <= MAX_VOXELS {
            let origin = [
                bmin[0] - pad as f64 * voxel,
                bmin[1] - pad as f64 * voxel,
                bmin[2] - pad as f64 * voxel,
            ];
            return (origin, voxel, dims, coarsened);
        }
        voxel *= 1.25;
        coarsened = true;
    }
}

fn vec_to_pyarray3<'py>(
    py: Python<'py>,
    data: Vec<f32>,
    dims: [usize; 3],
) -> PyResult<Bound<'py, PyArray3<f32>>> {
    let arr = Array3::from_shape_vec((dims[0], dims[1], dims[2]).f(), data)
        .map_err(|e| PyValueError::new_err(format!("grid shape error: {e}")))?;
    Ok(PyArray3::from_owned_array(py, arr))
}

fn dims_to_list<'py>(py: Python<'py>, dims: [usize; 3]) -> PyResult<Bound<'py, PyList>> {
    PyList::new(py, [dims[0], dims[1], dims[2]])
}

fn vec_to_pyarray3_u8<'py>(
    py: Python<'py>,
    data: Vec<u8>,
    dims: [usize; 3],
) -> PyResult<Bound<'py, PyArray3<u8>>> {
    let arr = Array3::from_shape_vec((dims[0], dims[1], dims[2]).f(), data)
        .map_err(|e| PyValueError::new_err(format!("grid shape error: {e}")))?;
    Ok(PyArray3::from_owned_array(py, arr))
}

fn vec_to_pyarray3_i32<'py>(
    py: Python<'py>,
    data: Vec<i32>,
    dims: [usize; 3],
) -> PyResult<Bound<'py, PyArray3<i32>>> {
    let arr = Array3::from_shape_vec((dims[0], dims[1], dims[2]).f(), data)
        .map_err(|e| PyValueError::new_err(format!("grid shape error: {e}")))?;
    Ok(PyArray3::from_owned_array(py, arr))
}

/// Build a ray-stab direction set for `bvh` with the documented defaults.
fn build_stabber(
    bvh: &winding::MeshBvh,
    n_dirs: Option<usize>,
    seed: u64,
    parity: bool,
) -> raycast::Stabber {
    raycast::Stabber::new(
        &bvh.bounds(),
        n_dirs.unwrap_or(raycast::DEFAULT_DIRS),
        seed,
        parity,
        raycast::DEFAULT_ESCAPE_WEIGHT,
    )
}

fn raystab_info<'py>(
    py: Python<'py>,
    dirs: usize,
    seed: u64,
    parity: bool,
    stats: &raycast::RayStabStats,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("enabled", true)?;
    d.set_item("dirs", dirs)?;
    d.set_item("seed", seed)?;
    d.set_item("parity", parity)?;
    d.set_item("cells_ambiguous", stats.cells_ambiguous)?;
    d.set_item("cells_flipped", stats.cells_flipped)?;
    d.set_item("cells_unresolved", stats.cells_unresolved)?;
    d.set_item("rays_cast", stats.rays_cast)?;
    d.set_item("seconds", stats.seconds)?;
    Ok(d)
}

fn antipodal_info<'py>(
    py: Python<'py>,
    stats: &antipodal::AntipodalStats,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("enabled", true)?;
    d.set_item("boundary_segments", stats.boundary_segments)?;
    d.set_item("cells_ambiguous", stats.cells_ambiguous)?;
    d.set_item("cells_flipped", stats.cells_flipped)?;
    d.set_item("cells_exact_fallback", stats.cells_exact_fallback)?;
    d.set_item("seconds", stats.seconds)?;
    Ok(d)
}

/// Signed/unsigned/winding distance grids over the mesh (or a sub-box).
///
/// Returns `(winding, unsigned, signed, info)` where each grid is a 3-D f32
/// numpy array in `[i, j, k]` order.  Signed distance is
/// `sign(winding - 0.5) * unsigned`.  `info` carries `dims`, `voxel`,
/// `origin`, `voxels`, `caps_coarsened`.
///
/// `raystab=True` overrides the sign in the generalized-winding-ambiguous band
/// with the ray-stabbing vote (see `raycast`); `info['raystab']` then records
/// the pass.  With `raystab=False` (the default) the output is byte-identical
/// to the winding-only field.
///
/// `antipodal=Some(false)` opts out of the robust (antipodal) generalized
/// winding number (`antipodal`) and restores the classic winding path.  It is
/// ON by default; when both raystab and antipodal are enabled, antipodal runs
/// last and wins.  `SUTURA_ANTIPODAL=0` opts out in the environment.
#[pyfunction]
#[pyo3(signature = (verts, tris, voxel=None, r#box=None, raystab=false, raystab_dirs=None, raystab_seed=0, raystab_parity=false, antipodal=None))]
fn sdf_grid<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
    voxel: Option<f64>,
    r#box: Option<PyArrayLike2<'py, f64, AllowTypeChange>>,
    raystab: bool,
    raystab_dirs: Option<usize>,
    raystab_seed: u64,
    raystab_parity: bool,
    antipodal: Option<bool>,
) -> PyResult<(
    Bound<'py, PyArray3<f32>>,
    Bound<'py, PyArray3<f32>>,
    Bound<'py, PyArray3<f32>>,
    Bound<'py, PyDict>,
)> {
    let (rv, rt) = parse_mesh(&verts, &tris)?;
    if rv.is_empty() || rt.is_empty() {
        return Err(PyValueError::new_err("empty mesh"));
    }
    let mesh_box = bbox_of(&rv);
    let (bmin, bmax) = match &r#box {
        Some(b) => parse_box(b)?,
        None => (mesh_box.min, mesh_box.max),
    };
    let (origin, voxel, dims, coarsened) = plan_grid(bmin, bmax, voxel, 96.0, 2);
    let use_antipodal = antipodal.unwrap_or_else(antipodal::antipodal_enabled_by_default);
    let (gwn, bvh_owned) = if use_antipodal {
        let g = antipodal::AntipodalGwn::build(&rv, &rt);
        (Some(g), None)
    } else {
        (None, Some(winding::MeshBvh::from_arrays(&rv, &rt)))
    };
    let bvh = match (&bvh_owned, &gwn) {
        (Some(b), _) => b,
        (_, Some(g)) => g.bvh(),
        _ => unreachable!(),
    };
    let (w, u, mut s) = bvh.sdf_grid(origin, voxel, dims);
    let mut stab_stats = None;
    if raystab {
        let stabber = build_stabber(bvh, raystab_dirs, raystab_seed, raystab_parity);
        stab_stats = Some(raycast::disambiguate_sdf(
            bvh,
            &w,
            &u,
            &mut s,
            origin,
            voxel,
            dims,
            &stabber,
            raycast::AMBIG_LO,
            raycast::AMBIG_HI,
        ));
    }
    let mut antipodal_stats = None;
    if let Some(ref g) = gwn {
        antipodal_stats = Some(antipodal::disambiguate_sdf(
            g,
            &w,
            &u,
            &mut s,
            origin,
            voxel,
            dims,
            raycast::AMBIG_LO,
            raycast::AMBIG_HI,
        ));
    }

    let info = PyDict::new(py);
    info.set_item("dims", dims_to_list(py, dims)?)?;
    info.set_item("voxel", voxel)?;
    info.set_item("origin", PyList::new(py, origin)?)?;
    info.set_item("voxels", winding::voxel_count(dims))?;
    info.set_item("caps_coarsened", coarsened)?;
    info.set_item("max_voxels", MAX_VOXELS)?;
    if let Some(st) = stab_stats {
        info.set_item(
            "raystab",
            raystab_info(
                py,
                st.dirs,
                raystab_seed,
                st.parity,
                &st,
            )?,
        )?;
    }
    if let Some(st) = antipodal_stats {
        info.set_item("antipodal", antipodal_info(py, &st)?)?;
    }

    Ok((
        vec_to_pyarray3(py, w, dims)?,
        vec_to_pyarray3(py, u, dims)?,
        vec_to_pyarray3(py, s, dims)?,
        info,
    ))
}

/// Morphological closing of the solid by radius `r`.
///
/// Pipeline: signed-distance grid (winding-number sign) -> dilate by `r` ->
/// true EDT re-distance of the dilated solid -> erode by `r` -> dual contour.
/// Returns `(verts, tris, info)`.  `box` restricts the grid AABB (local
/// window mode); `voxel` overrides the automatic voxel size; `fill_cavities`
/// keeps only the outermost shell (enclosed cavities become solid).
///
/// `r == 0` selects the sign-field path: the raw signed field is fed straight
/// to dual contouring with no dilation/erosion, so genuine gaps stay open
/// (the caller gates the result).  `info['sign_field']` records the mode.
///
/// `raystab=True` disambiguates the generalized-winding sign with the
/// ray-stabbing vote in the ambiguous band before the closing; `info['raystab']`
/// records the pass.  With `raystab=False` (the default) the output is
/// byte-identical to the winding-only path.
///
/// `antipodal=Some(false)` opts out of the robust (antipodal) generalized
/// winding number instead and restores the classic winding path.  It is ON by
/// default; when both raystab and antipodal are enabled, antipodal runs last
/// and wins.  `SUTURA_ANTIPODAL=0` opts out in the environment.
#[pyfunction]
#[pyo3(signature = (verts, tris, r, voxel=None, r#box=None, fill_cavities=false, surface=None, raystab=false, raystab_dirs=None, raystab_seed=0, raystab_parity=false, antipodal=None))]
fn morph_close<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
    r: f64,
    voxel: Option<f64>,
    r#box: Option<PyArrayLike2<'py, f64, AllowTypeChange>>,
    fill_cavities: bool,
    surface: Option<String>,
    raystab: bool,
    raystab_dirs: Option<usize>,
    raystab_seed: u64,
    raystab_parity: bool,
    antipodal: Option<bool>,
) -> PyResult<(
    Bound<'py, PyArray2<f64>>,
    Bound<'py, PyArray2<i32>>,
    Bound<'py, PyDict>,
)> {
    if !(r >= 0.0) {
        return Err(PyValueError::new_err("r must be >= 0"));
    }
    let (rv, rt) = parse_mesh(&verts, &tris)?;
    if rv.is_empty() || rt.is_empty() {
        return Err(PyValueError::new_err("empty mesh"));
    }
    let mesh_box = bbox_of(&rv);
    let (bmin, bmax) = match &r#box {
        Some(b) => parse_box(b)?,
        None => (mesh_box.min, mesh_box.max),
    };

    // Resolve the voxel first so the padding (which must clear the dilation
    // radius plus a few cells for the EDT boundary) can be expressed in cells.
    let ext = [
        (bmax[0] - bmin[0]).max(1e-12),
        (bmax[1] - bmin[1]).max(1e-12),
        (bmax[2] - bmin[2]).max(1e-12),
    ];
    let mut v0 = voxel
        .filter(|v| *v > 0.0)
        .unwrap_or(ext[0].max(ext[1]).max(ext[2]) / 96.0);
    if !v0.is_finite() || v0 <= 0.0 {
        v0 = 1.0;
    }
    // Sign-field mode (r == 0) bypasses the closing, so no dilation radius needs
    // to be cleared by the padding; the default 3-cell boundary suffices.
    let pad = if r > 0.0 {
        (r / v0).ceil() as usize + 3
    } else {
        3
    };
    let (origin, voxel, dims, coarsened) = plan_grid(bmin, bmax, Some(v0), 96.0, pad);

    let use_antipodal = antipodal.unwrap_or_else(antipodal::antipodal_enabled_by_default);
    let (gwn, bvh_owned) = if use_antipodal {
        let g = antipodal::AntipodalGwn::build(&rv, &rt);
        (Some(g), None)
    } else {
        (None, Some(winding::MeshBvh::from_arrays(&rv, &rt)))
    };
    let bvh = match (&bvh_owned, &gwn) {
        (Some(b), _) => b,
        (_, Some(g)) => g.bvh(),
        _ => unreachable!(),
    };
    let (w, u, mut s) = bvh.sdf_grid(origin, voxel, dims);
    let mut stab_stats = None;
    if raystab {
        let stabber = build_stabber(bvh, raystab_dirs, raystab_seed, raystab_parity);
        stab_stats = Some(raycast::disambiguate_sdf(
            bvh,
            &w,
            &u,
            &mut s,
            origin,
            voxel,
            dims,
            &stabber,
            raycast::AMBIG_LO,
            raycast::AMBIG_HI,
        ));
    }
    let mut antipodal_stats = None;
    if let Some(ref g) = gwn {
        antipodal_stats = Some(antipodal::disambiguate_sdf(
            g,
            &w,
            &u,
            &mut s,
            origin,
            voxel,
            dims,
            raycast::AMBIG_LO,
            raycast::AMBIG_HI,
        ));
    }
    // r == 0 selects the sign-field path: the raw signed field (GWN sign,
    // optionally band-corrected) feeds dual contouring directly, with no
    // dilation/erosion.  This keeps real gaps open instead of bridging them.
    let field = if r > 0.0 {
        let closed = morph::close_sdf(&s, dims, r as f32, voxel as f32);
        if fill_cavities {
            morph::fill_cavities(&closed, dims, voxel as f32)
        } else {
            closed
        }
    } else if fill_cavities {
        morph::fill_cavities(&s, dims, voxel as f32)
    } else {
        s.clone()
    };
    let use_tets = matches!(
        surface.as_deref(),
        Some("tets") | Some("tet") | Some("marching_tets")
    );
    let dc = if use_tets {
        dual_contour::marching_tets_mesh(&field, dims, origin, voxel)
    } else {
        dual_contour::dual_contour(&field, dims, origin, voxel)
    };

    let out_verts: Vec<Vec<f64>> = dc.verts.iter().map(|p| p.to_vec()).collect();
    let out_tris: Vec<Vec<i32>> = dc
        .tris
        .iter()
        .map(|t| vec![t[0] as i32, t[1] as i32, t[2] as i32])
        .collect();
    let verts_np = PyArray2::from_vec2(py, &out_verts)?;
    let tris_np = PyArray2::from_vec2(py, &out_tris)?;

    let info = PyDict::new(py);
    info.set_item("dims", dims_to_list(py, dims)?)?;
    info.set_item("voxel", voxel)?;
    info.set_item("origin", PyList::new(py, origin)?)?;
    info.set_item("voxels", winding::voxel_count(dims))?;
    info.set_item("caps_coarsened", coarsened)?;
    info.set_item("max_voxels", MAX_VOXELS)?;
    info.set_item("radius", r)?;
    info.set_item("sign_field", !(r > 0.0))?;
    info.set_item("fallback", dc.fallback)?;
    info.set_item("manifold", dc.manifold)?;
    info.set_item("fill_cavities", fill_cavities)?;
    info.set_item("surface", if use_tets { "tets" } else { "dual" })?;
    if let Some(st) = stab_stats {
        info.set_item("raystab", raystab_info(py, st.dirs, raystab_seed, st.parity, &st)?)?;
    }
    if let Some(st) = antipodal_stats {
        info.set_item("antipodal", antipodal_info(py, &st)?)?;
    }
    Ok((verts_np, tris_np, info))
}

/// Variable-thickness coat ("Dressing", method #16).
///
/// Extracts the `F = s(x) - r(x) = 0` isosurface of a narrow-band signed field:
/// the generalized-winding sign and the unsigned distance are evaluated only
/// for cells within `band = r_max + 2*voxel` of the surface, and the sign
/// outside the band comes from a boundary flood fill.  `r(x)` grows from
/// `r_base` to `r_max` near the defect points (`defect_pts`, Nx3; empty means a
/// uniform `r_base`).  Returns `(verts, tris, info)` with a watertight,
/// two-manifold marching-tetrahedra coat.
///
/// `drain` is the healthy-region erode-back, in mm: the field adds
/// `drain * (1 - g)` (with `g` the defect influence) so the healthy isosurface
/// re-centres on the original boundary while the defect coverage is kept.
/// `0.0` (the default) leaves the plain outward coat.
#[pyfunction]
#[pyo3(signature = (verts, tris, defect_pts=None, voxel=None, r_base=None, r_max=None, sigma=None, band_voxels=2.0, margin_voxels=3.0, drain=0.0))]
fn dressing_coat<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
    defect_pts: Option<PyArrayLike2<'py, f64, AllowTypeChange>>,
    voxel: Option<f64>,
    r_base: Option<f64>,
    r_max: Option<f64>,
    sigma: Option<f64>,
    band_voxels: f64,
    margin_voxels: f64,
    drain: f64,
) -> PyResult<(
    Bound<'py, PyArray2<f64>>,
    Bound<'py, PyArray2<i32>>,
    Bound<'py, PyDict>,
)> {
    let (rv, rt) = parse_mesh(&verts, &tris)?;
    let mut dp: Vec<[f64; 3]> = Vec::new();
    if let Some(arr) = &defect_pts {
        let view = arr.as_array();
        if view.ncols() != 3 {
            return Err(PyValueError::new_err("defect_pts must be Nx3"));
        }
        for i in 0..view.nrows() {
            dp.push([view[[i, 0]], view[[i, 1]], view[[i, 2]]]);
        }
    }
    let params = dressing::DressingParams {
        voxel,
        r_base,
        r_max,
        sigma,
        band_voxels,
        margin_voxels,
        drain,
    };
    let m = dressing::dressing_coat(&rv, &rt, &dp, &params);

    let out_verts: Vec<Vec<f64>> = m.verts.iter().map(|p| p.to_vec()).collect();
    let out_tris: Vec<Vec<i32>> = m
        .tris
        .iter()
        .map(|t| vec![t[0] as i32, t[1] as i32, t[2] as i32])
        .collect();
    let verts_np = PyArray2::from_vec2(py, &out_verts)?;
    let tris_np = PyArray2::from_vec2(py, &out_tris)?;

    let info = PyDict::new(py);
    info.set_item("dims", dims_to_list(py, m.info.dims)?)?;
    info.set_item("voxel", m.info.voxel)?;
    info.set_item("origin", PyList::new(py, m.info.origin)?)?;
    info.set_item("voxels", m.info.voxels)?;
    info.set_item("r_base", m.info.r_base)?;
    info.set_item("r_max", m.info.r_max)?;
    info.set_item("sigma", m.info.sigma)?;
    info.set_item("drain", m.info.drain)?;
    info.set_item("band", m.info.band)?;
    info.set_item("band_cells", m.info.band_cells)?;
    info.set_item("sign_cells", m.info.sign_cells)?;
    info.set_item("gwn_cells", m.info.gwn_cells)?;
    info.set_item("gwn_exact_cells", m.info.gwn_exact_cells)?;
    info.set_item("coarsened", m.info.coarsened)?;
    info.set_item("fallback", m.info.fallback)?;
    info.set_item("manifold", m.info.manifold)?;
    info.set_item("seconds", m.info.seconds)?;
    info.set_item("band_seconds", m.info.band_seconds)?;
    info.set_item("field_seconds", m.info.field_seconds)?;
    info.set_item("extract_seconds", m.info.extract_seconds)?;
    Ok((verts_np, tris_np, info))
}

/// Tamponade (Method #17): seal open holes at their narrowest throat.
///
/// L2a: every input triangle is returned verbatim, plus one rim-only plug per
/// open boundary loop the min-cut accepts as a real hole. The plug is a
/// minimum-area triangulation of the loop's own vertices (no new vertex), so
/// it stays on the surface; a safety gate rejects a plug that would add a
/// boundary edge, a zero-area face or an inconsistent winding
/// (`info["plugs_rejected"]` counts those loops). Returns `(verts, tris,
/// info)`; a closed, empty or boundary-free input is returned byte-identical
/// with `info["reason"] == "no_holes"`.
#[pyfunction]
#[pyo3(name = "tamponade")]
#[pyo3(signature = (verts, tris, h_min=None, melt_si=true, max_nodes=None, time_budget_s=None))]
fn tamponade_py<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
    h_min: Option<f64>,
    melt_si: bool,
    max_nodes: Option<usize>,
    time_budget_s: Option<f64>,
) -> PyResult<(
    Bound<'py, PyArray2<f64>>,
    Bound<'py, PyArray2<i32>>,
    Bound<'py, PyDict>,
)> {
    let (rv, rt_usize) = parse_mesh(&verts, &tris)?;
    let rt: Vec<[u32; 3]> = rt_usize
        .iter()
        .map(|t| [t[0] as u32, t[1] as u32, t[2] as u32])
        .collect();
    let config = tamponade::TamponadeConfig {
        h_min,
        melt_si,
        max_nodes,
        time_budget_s,
        openings_gate: None,
        fwn: std::env::var("SUTURA_TAMP_FWN")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false),
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tamponade::tamponade_repair(&rv, &rt, &config)
    }));
    let (ov, ot, info) = match outcome {
        Ok(v) => v,
        Err(_) => {
            let mut info = tamponade::TamponadeInfo::default();
            info.healthy_faces_kept = rt.len();
            info.reason = "internal error".to_string();
            (rv.clone(), rt.clone(), info)
        }
    };

    let out_verts: Vec<Vec<f64>> = ov.iter().map(|p| p.to_vec()).collect();
    let out_tris: Vec<Vec<i32>> = ot
        .iter()
        .map(|t| vec![t[0] as i32, t[1] as i32, t[2] as i32])
        .collect();
    let verts_np = PyArray2::from_vec2(py, &out_verts)?;
    let tris_np = PyArray2::from_vec2(py, &out_tris)?;

    let d = PyDict::new(py);
    d.set_item("plugs", info.plugs)?;
    d.set_item("plugs_rejected", info.plugs_rejected)?;
    d.set_item("plugs_inconclusive", info.plugs_inconclusive)?;
    d.set_item("plugs_rejected_size", info.plugs_rejected_size)?;
    d.set_item("plugs_rejected_dev", info.plugs_rejected_dev)?;
    d.set_item("plug_faces", info.plug_faces)?;
    d.set_item("melt_layers_tried", info.melt_layers_tried)?;
    d.set_item("melt_layers_accepted", info.melt_layers_accepted)?;
    d.set_item("melt_layers_rolled_back", info.melt_layers_rolled_back)?;
    d.set_item("micro_welds_ok", info.micro_welds_ok)?;
    d.set_item("micro_welds_failed", info.micro_welds_failed)?;
    d.set_item("excised_faces", info.excised_faces)?;
    d.set_item("healthy_faces_kept", info.healthy_faces_kept)?;
    d.set_item("reference_used", info.reference_used)?;
    d.set_item("reference_fallback", info.reference_fallback)?;
    d.set_item("reference_rbf", info.reference_rbf)?;
    d.set_item("reference_skirt", info.reference_skirt)?;
    d.set_item("stent_used", info.stent_used)?;
    d.set_item("stent_rejected", info.stent_rejected)?;
    d.set_item("stent_subplugs", info.stent_subplugs)?;
    d.set_item("plug_metrics", info.plug_metrics_json)?;
    d.set_item("trimmed_vertices", info.trimmed_vertices)?;
    d.set_item("patch_vertices", info.patch_vertices)?;
    d.set_item("plugs_rolled_back", info.plugs_rolled_back)?;
    d.set_item("plugs_leaking", info.plugs_leaking)?;
    d.set_item("plugs_double_wall", info.plugs_double_wall)?;
    d.set_item("plugs_floating", info.plugs_floating)?;
    d.set_item("components_before", info.components_before)?;
    d.set_item("components_after", info.components_after)?;
    d.set_item("closed_shells_before", info.closed_shells_before)?;
    d.set_item("closed_shells_after", info.closed_shells_after)?;
    d.set_item("seconds", info.seconds)?;
    d.set_item("reason", info.reason.clone())?;
    d.set_item("loop_diag", info.loop_diag.clone())?;
    d.set_item("stent_diag", info.stent_diag.clone())?;
    Ok((verts_np, tris_np, d))
}

/// Complete shell analysis & filter pre-pass for Tamponade.
/// Returns (candidate_verts, candidate_tris, info_dict).
#[pyfunction]
#[pyo3(name = "shell_analyze")]
#[pyo3(signature = (verts, tris, delta=0.1, keep_cavities=true, split_intersections=true))]
fn shell_analyze_py<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
    delta: f64,
    keep_cavities: bool,
    split_intersections: bool,
) -> PyResult<(
    Bound<'py, PyArray2<f64>>,
    Bound<'py, PyArray2<i32>>,
    Bound<'py, PyDict>,
)> {
    let (rv, rt_usize) = parse_mesh(&verts, &tris)?;
    let rt: Vec<[u32; 3]> = rt_usize
        .iter()
        .map(|t| [t[0] as u32, t[1] as u32, t[2] as u32])
        .collect();
    let config = shell::ShellConfig {
        delta,
        keep_cavities,
        split_intersections,
        ..Default::default()
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        shell::shell_analyze_and_filter(&rv, &rt, &config)
    }));
    let (ov, ot, res) = match outcome {
        Ok(v) => v,
        Err(_) => {
            let mut res = shell::ShellAnalysisResult::default();
            res.reason = "panic".to_string();
            res.has_internal_walls = true;
            (rv.clone(), rt.clone(), res)
        }
    };

    let out_verts: Vec<Vec<f64>> = ov.iter().map(|p| p.to_vec()).collect();
    let out_tris: Vec<Vec<i32>> = ot
        .iter()
        .map(|t| vec![t[0] as i32, t[1] as i32, t[2] as i32])
        .collect();
    let verts_np = PyArray2::from_vec2(py, &out_verts)?;
    let tris_np = PyArray2::from_vec2(py, &out_tris)?;

    let d = PyDict::new(py);
    d.set_item("outer_count", res.outer_count)?;
    d.set_item("buried_count", res.buried_count)?;
    d.set_item("cavity_count", res.cavity_count)?;
    d.set_item("coincident_opp_count", res.coincident_opp_count)?;
    d.set_item("coincident_same_count", res.coincident_same_count)?;
    d.set_item("internal_walls_count", res.internal_walls_count)?;
    d.set_item("unresolved_coincident_count", res.unresolved_coincident_count)?;
    d.set_item("has_internal_walls", res.has_internal_walls)?;
    d.set_item("is_disjoint", res.is_disjoint)?;
    d.set_item("reason", res.reason.clone())?;
    Ok((verts_np, tris_np, d))
}

fn parse_point_rows(arr: &PyArrayLike2<'_, f64, AllowTypeChange>) -> PyResult<Vec<[f64; 3]>> {
    let view = arr.as_array();
    if view.ncols() != 3 {
        return Err(PyValueError::new_err("array must be Nx3"));
    }
    if let Some(s) = view.as_slice() {
        Ok(s.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect())
    } else {
        Ok((0..view.nrows())
            .map(|i| [view[[i, 0]], view[[i, 1]], view[[i, 2]]])
            .collect())
    }
}

fn nearest_output<'py>(
    py: Python<'py>,
    dist: Vec<f64>,
    idx: Vec<i32>,
    take: usize,
    n_queries: usize,
) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    if take == 1 {
        return Ok((
            PyArray1::from_vec(py, dist).into_any(),
            PyArray1::from_vec(py, idx).into_any(),
        ));
    }
    if n_queries == 0 {
        let d = numpy::ndarray::Array2::<f64>::zeros((0, take));
        let i = numpy::ndarray::Array2::<i32>::zeros((0, take));
        return Ok((
            PyArray2::from_owned_array(py, d).into_any(),
            PyArray2::from_owned_array(py, i).into_any(),
        ));
    }
    let d_rows: Vec<Vec<f64>> = dist.chunks(take).map(|c| c.to_vec()).collect();
    let i_rows: Vec<Vec<i32>> = idx.chunks(take).map(|c| c.to_vec()).collect();
    Ok((
        PyArray2::from_vec2(py, &d_rows)?.into_any(),
        PyArray2::from_vec2(py, &i_rows)?.into_any(),
    ))
}

/// Nearest neighbours in a point cloud for every query point.
///
/// Returns `(distances, indices)`.  With `k == 1` both arrays are 1-D
/// `(M,)`; with `k > 1` they are 2-D `(M, k)` sorted by distance.  Distances
/// are exact f64 Euclidean distances.  Exact ties select the smallest source
/// index, so the result is deterministic.
#[pyfunction]
#[pyo3(signature = (points, queries, k = 1))]
fn nearest_points<'py>(
    py: Python<'py>,
    points: PyArrayLike2<'py, f64, AllowTypeChange>,
    queries: PyArrayLike2<'py, f64, AllowTypeChange>,
    k: usize,
) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    let pts = parse_point_rows(&points)?;
    let qs = parse_point_rows(&queries)?;
    if pts.is_empty() && !qs.is_empty() {
        return Err(PyValueError::new_err("points must be non-empty"));
    }
    if pts.is_empty() {
        return Ok((
            PyArray1::<f64>::from_vec(py, Vec::new()).into_any(),
            PyArray1::<i32>::from_vec(py, Vec::new()).into_any(),
        ));
    }
    let tree = pointcloud::build_tree(&pts)
        .ok_or_else(|| PyValueError::new_err("could not build the point tree"))?;
    let (dist, idx, take) = pointcloud::query_neighbours(&tree, pts.len(), &qs, k);
    nearest_output(py, dist, idx, take, qs.len())
}

/// Reusable nearest-neighbour tree over a fixed point cloud.
///
/// Build once and call `query(queries, k=1)` in a loop (for example an ICP
/// iteration) so the tree is not rebuilt for every batch.
#[pyclass(name = "KdTree")]
struct PyKdTree {
    inner: pointcloud::PointTree,
    point_count: usize,
}

#[pymethods]
impl PyKdTree {
    #[new]
    fn new(points: PyArrayLike2<'_, f64, AllowTypeChange>) -> PyResult<Self> {
        let pts = parse_point_rows(&points)?;
        if pts.is_empty() {
            return Err(PyValueError::new_err("points must be non-empty"));
        }
        let inner = pointcloud::build_tree(&pts)
            .ok_or_else(|| PyValueError::new_err("could not build the point tree"))?;
        Ok(Self {
            inner,
            point_count: pts.len(),
        })
    }

    #[pyo3(signature = (queries, k = 1))]
    fn query<'py>(
        &self,
        py: Python<'py>,
        queries: PyArrayLike2<'py, f64, AllowTypeChange>,
        k: usize,
    ) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
        let qs = parse_point_rows(&queries)?;
        let (dist, idx, take) = pointcloud::query_neighbours(&self.inner, self.point_count, &qs, k);
        nearest_output(py, dist, idx, take, qs.len())
    }

    fn __len__(&self) -> usize {
        self.point_count
    }
}

/// Exact Euclidean distance transform of a 3-D seed mask.
///
/// `seed_mask[i] != 0` marks a seed (distance 0).  Returns `(nx, ny, nz)`
/// float32 distances in cell units.  Replaces
/// `scipy.ndimage.distance_transform_edt(~mask)`.
#[pyfunction]
fn edt_grid<'py>(
    py: Python<'py>,
    seed_mask: PyArrayLike3<'py, u8, AllowTypeChange>,
) -> PyResult<Bound<'py, PyArray3<f32>>> {
    let view = seed_mask.as_array();
    let dims = [view.shape()[0], view.shape()[1], view.shape()[2]];
    // Rust grid kernels use i-fastest order (`i + nx*(j + ny*k)`); numpy is
    // C-order, so flatten explicitly instead of relying on `.iter()`.
    let mut seed = Vec::with_capacity(dims[0] * dims[1] * dims[2]);
    for k in 0..dims[2] {
        for j in 0..dims[1] {
            for i in 0..dims[0] {
                seed.push(u8::from(view[[i, j, k]] != 0));
            }
        }
    }
    let out = morph::edt_grid(&seed, dims);
    vec_to_pyarray3(py, out, dims)
}

/// Binary dilation of a 3-D occupancy field by an integer cell radius.
///
/// Uses the exact Euclidean ball structuring element.  Replaces
/// `scipy.ndimage.binary_dilation(occ, structure=ball(radius))`.
#[pyfunction]
#[pyo3(signature = (occupancy, radius = 1))]
fn dilate_occupancy<'py>(
    py: Python<'py>,
    occupancy: PyArrayLike3<'py, u8, AllowTypeChange>,
    radius: u32,
) -> PyResult<Bound<'py, PyArray3<u8>>> {
    let view = occupancy.as_array();
    let dims = [view.shape()[0], view.shape()[1], view.shape()[2]];
    let mut occ = Vec::with_capacity(dims[0] * dims[1] * dims[2]);
    for k in 0..dims[2] {
        for j in 0..dims[1] {
            for i in 0..dims[0] {
                occ.push(u8::from(view[[i, j, k]] != 0));
            }
        }
    }
    let out = morph::dilate_occupancy_ball(&occ, dims, radius);
    vec_to_pyarray3_u8(py, out, dims)
}

/// Closest point on the triangle soup for every query point.
///
/// Returns `(xyz, distance, face_index)`; each query gives the closest point
/// on the surface, its Euclidean distance and the index of the triangle
/// (or `-1` for an empty mesh).  Queries are evaluated in parallel.
#[pyfunction]
#[pyo3(signature = (verts, tris, points))]
fn closest_points<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
    points: PyArrayLike2<'py, f64, AllowTypeChange>,
) -> PyResult<(
    Bound<'py, PyArray2<f64>>,
    Bound<'py, PyArray1<f64>>,
    Bound<'py, PyArray1<i32>>,
)> {
    let (rv, rt) = parse_mesh(&verts, &tris)?;
    if rv.is_empty() || rt.is_empty() {
        return Err(PyValueError::new_err("empty mesh"));
    }
    let pp = points.as_array();
    if pp.ncols() != 3 {
        return Err(PyValueError::new_err("points must be Mx3"));
    }
    let m = pp.nrows();
    let bvh = winding::MeshBvh::from_arrays(&rv, &rt);
    let results: Vec<([f64; 3], f64, i32)> = (0..m)
        .into_par_iter()
        .map(|i| {
            let (q, d, f) = bvh.closest_point([pp[[i, 0]], pp[[i, 1]], pp[[i, 2]]]);
            (q, d, if f == usize::MAX { -1 } else { f as i32 })
        })
        .collect();
    let xyz: Vec<Vec<f64>> = results.iter().map(|r| r.0.to_vec()).collect();
    let dist: Vec<f64> = results.iter().map(|r| r.1).collect();
    let fid: Vec<i32> = results.iter().map(|r| r.2).collect();
    Ok((
        PyArray2::from_vec2(py, &xyz)?,
        PyArray1::from_vec(py, dist),
        PyArray1::from_vec(py, fid),
    ))
}

/// Faces involved in a self-intersection.
///
/// Runs a spatial-hash broad phase and the exact triangle-triangle classifier;
/// a face is flagged when it properly crosses a non-adjacent face (pairs
/// sharing a vertex index are ignored, as are coplanar/boundary contacts).
/// Returns `(mask, count)` where `count` is the number of flagged faces, or
/// `-1` when the mesh exceeds `SI_MAX_FACES` (the classifier is skipped).
#[pyfunction]
#[pyo3(signature = (verts, tris, bvh = None))]
fn self_intersecting_faces<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
    bvh: Option<&PyMeshBvh>,
) -> PyResult<(Bound<'py, PyArray1<bool>>, i64)> {
    if let Some(b) = bvh {
        return b.self_intersecting_faces(py);
    }
    let (rv, rt) = parse_mesh(&verts, &tris)?;
    let b = winding::MeshBvh::from_arrays(&rv, &rt);
    let (mask, count, _skipped) = b.self_intersections(&rv, &rt);
    Ok((PyArray1::from_vec(py, mask), count))
}

/// Core self-intersection scan (no Python): `(mask, count, skipped)`.
fn self_intersections_core(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> (Vec<bool>, i64, bool) {
    if tris.len() > SI_MAX_FACES {
        return (vec![false; tris.len()], -1, true);
    }
    let bvh = winding::MeshBvh::from_arrays(verts, tris);
    bvh.self_intersections(verts, tris)
}

/// Self-intersections counted only from a face SUBSET.
///
/// Returns the number of proper-crossing, non-adjacent face PAIRS `(i, j)` in
/// which at least one of `i`, `j` is flagged in `subset`.  Used by the
/// Tamponade prepass to compare only the faces it changed (plus whatever they
/// cross) instead of rescanning the whole mesh after every candidate; the
/// caller runs the same metric on the base mesh's changed faces and rejects
/// when the candidate's count rises.  `0` when the mesh exceeds
/// `SI_MAX_FACES` or the subset is empty.
#[pyfunction]
#[pyo3(signature = (verts, tris, subset))]
fn self_intersections_subset<'py>(
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
    subset: PyArrayLike1<'py, bool, AllowTypeChange>,
) -> PyResult<i64> {
    let (rv, rt) = parse_mesh(&verts, &tris)?;
    let s: Vec<bool> = subset.as_array().iter().copied().collect();
    Ok(self_intersections_subset_core(&rv, &rt, &s))
}

/// Core subset self-intersection count (no Python); see
/// `self_intersections_subset`.
pub(crate) fn self_intersections_subset_core(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    subset: &[bool],
) -> i64 {
    let f = tris.len();
    if f == 0 || f > SI_MAX_FACES {
        return 0;
    }
    if !subset.iter().take(f).any(|&b| b) {
        return 0;
    }
    let triangles: Vec<triangle_intersection::Triangle> = tris
        .iter()
        .map(|t| triangle_intersection::Triangle::explicit(verts[t[0]], verts[t[1]], verts[t[2]]))
        .collect();
    let pairs = arrangement::broad_phase_candidates(verts, tris);
    let mut count = 0i64;
    for (i, j) in pairs {
        let si = subset.get(i).copied().unwrap_or(false);
        let sj = subset.get(j).copied().unwrap_or(false);
        if !(si || sj) {
            continue;
        }
        let (a, b) = (tris[i], tris[j]);
        if a.iter().any(|x| b.contains(x)) {
            continue;
        }
        if let triangle_intersection::TriangleIntersection::ProperSegment { .. } =
            triangle_intersection::intersect_triangles(&triangles[i], &triangles[j])
        {
            count += 1;
        }
    }
    count
}

/// Ray-stabbing inside/outside for arbitrary query points.
///
/// Returns `(inside, inside_votes, outside_votes, escape_votes)` per point.
/// The vote is the oriented net crossing count with a parity fallback for
/// orientation-inconsistent rays; `parity=True` forces parity everywhere.  A ray
/// escaping without a hit is an outside vote weighted `escape_weight` in the
/// majority score (it is still reported once in `escape_votes`).
#[pyfunction]
#[pyo3(signature = (verts, tris, points, n_dirs=None, seed=0, parity=false, escape_weight=2.0))]
fn raystab_points<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
    points: PyArrayLike2<'py, f64, AllowTypeChange>,
    n_dirs: Option<usize>,
    seed: u64,
    parity: bool,
    escape_weight: f64,
) -> PyResult<(
    Bound<'py, PyArray1<bool>>,
    Bound<'py, PyArray1<i32>>,
    Bound<'py, PyArray1<i32>>,
    Bound<'py, PyArray1<i32>>,
)> {
    let (rv, rt) = parse_mesh(&verts, &tris)?;
    if rv.is_empty() || rt.is_empty() {
        return Err(PyValueError::new_err("empty mesh"));
    }
    let pp = points.as_array();
    if pp.ncols() != 3 {
        return Err(PyValueError::new_err("points must be Mx3"));
    }
    let m = pp.nrows();
    let bvh = winding::MeshBvh::from_arrays(&rv, &rt);
    let stabber = raycast::Stabber::new(
        &bvh.bounds(),
        n_dirs.unwrap_or(raycast::DEFAULT_DIRS),
        seed,
        parity,
        escape_weight,
    );
    let votes: Vec<raycast::StabVote> = (0..m)
        .into_par_iter()
        .map(|i| stabber.vote(&bvh, [pp[[i, 0]], pp[[i, 1]], pp[[i, 2]]]))
        .collect();
    let inside: Vec<bool> = votes.iter().map(|v| v.inside).collect();
    let iv: Vec<i32> = votes.iter().map(|v| v.inside_votes as i32).collect();
    let ov: Vec<i32> = votes.iter().map(|v| v.outside_votes as i32).collect();
    let ev: Vec<i32> = votes.iter().map(|v| v.escape_votes as i32).collect();
    Ok((
        PyArray1::from_vec(py, inside),
        PyArray1::from_vec(py, iv),
        PyArray1::from_vec(py, ov),
        PyArray1::from_vec(py, ev),
    ))
}

/// Robust generalized winding number (antipodal method) for arbitrary query
/// points.
///
/// Returns `(inside, winding)`: `inside` is a `(M,)` bool array (`winding >
/// 0.5`) and `winding` is a `(M,)` float64 array, in turns.  The value is the
/// sum of an exact signed `-x` ray/surface crossing count and an exact boundary
/// term, coupled through one exact determinant so the field cannot jump by
/// ~+-1 near the surface (Martens, Trettner & Bessmeltsev, ACM TOG 45(4), 2026,
/// art. 43).  Deterministic and independent of triangle/vertex ordering.
#[pyfunction]
#[pyo3(signature = (verts, tris, points))]
fn antipodal_points<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
    points: PyArrayLike2<'py, f64, AllowTypeChange>,
) -> PyResult<(Bound<'py, PyArray1<bool>>, Bound<'py, PyArray1<f64>>)> {
    let (rv, rt) = parse_mesh(&verts, &tris)?;
    if rv.is_empty() || rt.is_empty() {
        return Err(PyValueError::new_err("empty mesh"));
    }
    let pp = points.as_array();
    if pp.ncols() != 3 {
        return Err(PyValueError::new_err("points must be Mx3"));
    }
    let m = pp.nrows();
    let evaluator = antipodal::AntipodalGwn::build(&rv, &rt);
    let pts: Vec<[f64; 3]> = (0..m)
        .map(|i| [pp[[i, 0]], pp[[i, 1]], pp[[i, 2]]])
        .collect();
    let winding = evaluator.winding_points_par(&pts);
    let inside: Vec<bool> = winding.iter().map(|w| *w > 0.5).collect();
    Ok((
        PyArray1::from_vec(py, inside),
        PyArray1::from_vec(py, winding),
    ))
}

/// Full-grid ray-stabbing inside mask (analysis / tests).
///
/// Returns `(inside, score, info)`: `inside` is a 3-D uint8 grid (1 = inside),
/// `score` is `inside_votes - (outside_votes + escape_votes)` per cell.  This
/// votes every cell and is therefore much more expensive than the
/// ambiguous-band pass used by `morph_close`.
#[pyfunction]
#[pyo3(signature = (verts, tris, voxel=None, r#box=None, n_dirs=None, seed=0, parity=false, escape_weight=2.0))]
fn raystab_grid<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
    voxel: Option<f64>,
    r#box: Option<PyArrayLike2<'py, f64, AllowTypeChange>>,
    n_dirs: Option<usize>,
    seed: u64,
    parity: bool,
    escape_weight: f64,
) -> PyResult<(
    Bound<'py, PyArray3<u8>>,
    Bound<'py, PyArray3<i32>>,
    Bound<'py, PyDict>,
)> {
    let (rv, rt) = parse_mesh(&verts, &tris)?;
    if rv.is_empty() || rt.is_empty() {
        return Err(PyValueError::new_err("empty mesh"));
    }
    let mesh_box = bbox_of(&rv);
    let (bmin, bmax) = match &r#box {
        Some(b) => parse_box(b)?,
        None => (mesh_box.min, mesh_box.max),
    };
    let (origin, voxel, dims, coarsened) = plan_grid(bmin, bmax, voxel, 96.0, 2);
    let bvh = winding::MeshBvh::from_arrays(&rv, &rt);
    let stabber = raycast::Stabber::new(
        &bvh.bounds(),
        n_dirs.unwrap_or(raycast::DEFAULT_DIRS),
        seed,
        parity,
        escape_weight,
    );
    let t0 = std::time::Instant::now();
    let grid = raycast::raystab_grid(&bvh, origin, voxel, dims, &stabber);
    let seconds = t0.elapsed().as_secs_f64();

    let info = PyDict::new(py);
    info.set_item("dims", dims_to_list(py, dims)?)?;
    info.set_item("voxel", voxel)?;
    info.set_item("origin", PyList::new(py, origin)?)?;
    info.set_item("voxels", winding::voxel_count(dims))?;
    info.set_item("caps_coarsened", coarsened)?;
    info.set_item("dirs", stabber.n_dirs())?;
    info.set_item("seed", seed)?;
    info.set_item("parity", parity)?;
    info.set_item("escape_weight", escape_weight)?;
    info.set_item("seconds", seconds)?;

    Ok((
        vec_to_pyarray3_u8(py, grid.inside, dims)?,
        vec_to_pyarray3_i32(py, grid.score, dims)?,
        info,
    ))
}

/// Python-exposed generalized-winding BVH over a triangle soup.
///
/// Build it once over the whole mesh and reuse it for many query points; the
/// structure is read-only and the query methods are parallelised with rayon.
#[pyclass]
pub struct PyMeshBvh {
    inner: winding::MeshBvh,
    verts: Vec<[f64; 3]>,
    tris: Vec<[usize; 3]>,
}

#[pymethods]
impl PyMeshBvh {
    #[new]
    fn new(
        verts: PyArrayLike2<'_, f64, AllowTypeChange>,
        tris: PyArrayLike2<'_, i32, AllowTypeChange>,
    ) -> PyResult<Self> {
        let (rv, rt) = parse_mesh(&verts, &tris)?;
        let inner = winding::MeshBvh::from_arrays(&rv, &rt);
        Ok(Self {
            inner,
            verts: rv,
            tris: rt,
        })
    }

    /// Self-intersecting face scan querying this BVH: `(mask, count)`.
    fn self_intersecting_faces<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<(Bound<'py, PyArray1<bool>>, i64)> {
        let (mask, count, _skipped) = self.inner.self_intersections(&self.verts, &self.tris);
        Ok((PyArray1::from_vec(py, mask), count))
    }

    /// Generalized winding number at a single `(x, y, z)` point.
    fn winding_at(&self, point: (f64, f64, f64)) -> f64 {
        self.inner.winding_at([point.0, point.1, point.2])
    }

    /// Generalized winding number for `(M, 3)` query points, parallel across
    /// all CPU cores.  Returns a `(M,)` float64 array.
    fn winding_points<'py>(
        &self,
        py: Python<'py>,
        points: PyArrayLike2<'py, f64, AllowTypeChange>,
    ) -> PyResult<Bound<'py, PyArray1<f64>>> {
        let pp = points.as_array();
        if pp.ncols() != 3 {
            return Err(PyValueError::new_err("points must be Mx3"));
        }
        let pts: Vec<[f64; 3]> = (0..pp.nrows())
            .map(|i| [pp[[i, 0]], pp[[i, 1]], pp[[i, 2]]])
            .collect();
        Ok(PyArray1::from_vec(py, self.inner.winding_points_par(&pts)))
    }

    /// Robust generalized winding number (antipodal method) for `(M, 3)` query
    /// points, parallel across all CPU cores.  Returns a `(M,)` float64 array.
    /// See `antipodal_points`.
    fn winding_robust_points<'py>(
        &self,
        py: Python<'py>,
        points: PyArrayLike2<'py, f64, AllowTypeChange>,
    ) -> PyResult<Bound<'py, PyArray1<f64>>> {
        let pp = points.as_array();
        if pp.ncols() != 3 {
            return Err(PyValueError::new_err("points must be Mx3"));
        }
        let pts: Vec<[f64; 3]> = (0..pp.nrows())
            .map(|i| [pp[[i, 0]], pp[[i, 1]], pp[[i, 2]]])
            .collect();
        let evaluator = antipodal::AntipodalGwn::build(&self.verts, &self.tris);
        Ok(PyArray1::from_vec(py, evaluator.winding_points_par(&pts)))
    }
}

#[pymodule]
fn sutura_geom(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(orient3d, m)?)?;
    m.add_function(wrap_pyfunction!(insphere, m)?)?;
    m.add_function(wrap_pyfunction!(arrangement_lite, m)?)?;
    m.add_function(wrap_pyfunction!(manifold::stage2_rebuild, m)?)?;
    m.add_function(wrap_pyfunction!(manifold::watertight_check, m)?)?;
    m.add_function(wrap_pyfunction!(manifold::outer_shell_union, m)?)?;
    m.add_function(wrap_pyfunction!(manifold::union_pair, m)?)?;
    m.add_function(wrap_pyfunction!(manifold::boolean_transplant, m)?)?;
    m.add_function(wrap_pyfunction!(_set_cdt_experimental, m)?)?;
    m.add_function(wrap_pyfunction!(sdf_grid, m)?)?;
    m.add_function(wrap_pyfunction!(morph_close, m)?)?;
    m.add_function(wrap_pyfunction!(dressing_coat, m)?)?;
    m.add_function(wrap_pyfunction!(nearest_points, m)?)?;
    m.add_class::<PyKdTree>()?;
    m.add_function(wrap_pyfunction!(edt_grid, m)?)?;
    m.add_function(wrap_pyfunction!(dilate_occupancy, m)?)?;
    m.add_function(wrap_pyfunction!(closest_points, m)?)?;
    m.add_function(wrap_pyfunction!(self_intersecting_faces, m)?)?;
    m.add_function(wrap_pyfunction!(self_intersections_subset, m)?)?;
    m.add_function(wrap_pyfunction!(raystab_points, m)?)?;
    m.add_function(wrap_pyfunction!(raystab_grid, m)?)?;
    m.add_function(wrap_pyfunction!(antipodal_points, m)?)?;
    m.add_function(wrap_pyfunction!(topology::edge_table, m)?)?;
    m.add_function(wrap_pyfunction!(topology::weld_vertices, m)?)?;
    m.add_function(wrap_pyfunction!(topology::weld_reload_equivalent, m)?)?;
    m.add_function(wrap_pyfunction!(topology::remove_duplicate_faces, m)?)?;
    m.add_function(wrap_pyfunction!(topology::remove_null_faces, m)?)?;
    m.add_function(wrap_pyfunction!(topology::remove_duplicate_vertices, m)?)?;
    m.add_function(wrap_pyfunction!(topology::remove_unreferenced_vertices, m)?)?;
    m.add_function(wrap_pyfunction!(topology::remove_connected_components, m)?)?;
    m.add_function(wrap_pyfunction!(topology::invert_face_orientation, m)?)?;
    m.add_function(wrap_pyfunction!(topology::re_orient_faces_coherently, m)?)?;
    m.add_function(wrap_pyfunction!(topology::repair_non_manifold_edges, m)?)?;
    m.add_function(wrap_pyfunction!(topology::mesh_topology, m)?)?;
    m.add_function(wrap_pyfunction!(topology::mesh_geometric_measures, m)?)?;
    m.add_function(wrap_pyfunction!(flap::flap_fill_py, m)?)?;
    m.add_function(wrap_pyfunction!(lava::lava_octree, m)?)?;
    m.add_function(wrap_pyfunction!(tamponade_py, m)?)?;
    m.add_function(wrap_pyfunction!(shell_analyze_py, m)?)?;
    m.add_class::<PyMeshBvh>()?;
    m.add("MAX_VOXELS", MAX_VOXELS)?;
    m.add("SI_MAX_FACES", SI_MAX_FACES)?;
    m.add("RAYSTAB_DEFAULT_DIRS", raycast::DEFAULT_DIRS)?;
    m.add("RAYSTAB_MAX_DIRS", raycast::MAX_DIRS)?;
    m.add("SIGN_FIELD_SUPPORTED", true)?;
    m.add("TAMPONADE_SUPPORTED", true)?;
    m.add("ANTIPODAL_SUPPORTED", true)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{insphere_coords, orient3d_coords, self_intersections_subset_core};

    const ORIGIN: [f64; 3] = [0.0, 0.0, 0.0];
    const X: [f64; 3] = [1.0, 0.0, 0.0];
    const Y: [f64; 3] = [0.0, 1.0, 0.0];
    const Z: [f64; 3] = [0.0, 0.0, 1.0];
    const NEG_Z: [f64; 3] = [0.0, 0.0, -1.0];

    #[test]
    fn orient3d_positive() {
        // pd below the (pa, pb, pc) plane -> positive (robust convention:
        // pa, pb, pc counterclockwise viewed from above, pd beneath them).
        assert!(orient3d_coords(ORIGIN, X, Y, NEG_Z) > 0.0);
    }

    #[test]
    fn orient3d_negative() {
        // pd above the (pa, pb, pc) plane -> negative.
        assert!(orient3d_coords(ORIGIN, X, Y, Z) < 0.0);
    }

    #[test]
    fn orient3d_coplanar_is_exactly_zero() {
        // pd on the (pa, pb, pc) plane -> exactly 0.
        assert_eq!(orient3d_coords(ORIGIN, X, Y, [0.0, 0.0, 0.0]), 0.0);
        assert_eq!(orient3d_coords(ORIGIN, X, Y, [0.25, 0.25, 0.0]), 0.0);
    }

    #[test]
    fn orient3d_collinear_input_is_exactly_zero() {
        // Degenerate base triangle: pa, pb, pc collinear -> exactly 0.
        assert_eq!(orient3d_coords(ORIGIN, X, [2.0, 0.0, 0.0], Z), 0.0);
    }

    #[test]
    fn insphere_center_inside_is_positive() {
        // Positively-oriented regular tetrahedron (side sqrt(2)) using
        // (ORIGIN, X, Y, NEG_Z): its circumcenter (0.25, 0.25, -0.25) lies
        // inside the circumsphere -> positive.
        let c = [0.25, 0.25, -0.25];
        assert!(insphere_coords(ORIGIN, X, Y, NEG_Z, c) > 0.0);
    }

    #[test]
    fn self_intersections_subset_counts_only_subset_pairs() {
        // Triangle 0 lies in z=0; triangle 1 pierces it at (0.25, 0.25, 0).
        let verts: Vec<[f64; 3]> = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.5, -0.5, -1.0],
            [0.5, 0.5, -1.0],
            [0.5, 0.5, 1.0],
        ];
        let tris: Vec<[usize; 3]> = vec![[0, 1, 2], [3, 4, 5]];
        assert_eq!(
            self_intersections_subset_core(&verts, &tris, &[true, false]),
            1
        );
        assert_eq!(
            self_intersections_subset_core(&verts, &tris, &[false, true]),
            1
        );
        assert_eq!(
            self_intersections_subset_core(&verts, &tris, &[false, false]),
            0
        );
    }

    #[test]
    fn insphere_outside_is_negative() {
        // A point far from the tetrahedron -> negative.
        let far = [10.0, 10.0, 10.0];
        assert!(insphere_coords(ORIGIN, X, Y, NEG_Z, far) < 0.0);
    }

    #[test]
    fn insphere_vertex_on_sphere_is_exactly_zero() {
        // pe equal to a vertex of the circumsphere -> exactly 0.
        assert_eq!(insphere_coords(ORIGIN, X, Y, NEG_Z, ORIGIN), 0.0);
        assert_eq!(insphere_coords(ORIGIN, X, Y, NEG_Z, X), 0.0);
        assert_eq!(insphere_coords(ORIGIN, X, Y, NEG_Z, NEG_Z), 0.0);
    }

    #[test]
    fn plan_grid_coarsens_to_the_voxel_budget() {
        // An absurdly small requested voxel must be coarsened until the grid
        // fits MAX_VOXELS, without ever allocating the oversized grid.
        let (_o, voxel, dims, coarsened) =
            super::plan_grid([0.0; 3], [1.0; 3], Some(1e-4), 96.0, 2);
        assert!(coarsened);
        assert!(voxel > 1e-4);
        assert!(super::winding::voxel_count(dims) <= super::MAX_VOXELS);
    }

    #[test]
    fn plan_grid_keeps_a_fitting_voxel() {
        let (_o, voxel, dims, coarsened) =
            super::plan_grid([0.0; 3], [1.0; 3], Some(0.1), 96.0, 1);
        assert!(!coarsened);
        assert!((voxel - 0.1).abs() < 1e-12);
        // ceil(1/0.1) + 1 + 2*1 = 13 per axis.
        assert_eq!(dims, [13, 13, 13]);
        assert!(super::winding::voxel_count(dims) <= super::MAX_VOXELS);
    }

    #[test]
    fn closest_point_on_cube() {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 2.0, 0.0],
            [0.0, 2.0, 0.0],
        ];
        let tris = vec![[0, 1, 2], [0, 2, 3]];
        let bvh = super::winding::MeshBvh::from_arrays(&verts, &tris);
        let (q, d, fid) = bvh.closest_point([1.0, 1.0, 3.0]);
        assert!((d - 3.0).abs() < 1e-9, "distance {d}");
        assert!((q[0] - 1.0).abs() < 1e-9 && (q[1] - 1.0).abs() < 1e-9 && q[2].abs() < 1e-9);
        assert!(fid < 2);
    }

    #[test]
    fn self_intersections_core_flags_crossing_faces() {
        // Triangle 0 lies in z=0; triangle 1 pierces its interior.
        let verts = vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            [0.5, 0.5, -1.0],
            [1.5, 0.5, 1.0],
            [0.5, 1.5, 1.0],
        ];
        let tris = vec![[0, 1, 2], [3, 4, 5]];
        // The exact classifier must see a proper crossing...
        use super::triangle_intersection::{
            intersect_triangles, Triangle as T, TriangleIntersection,
        };
        let t1 = T::explicit(verts[0], verts[1], verts[2]);
        let t2 = T::explicit(verts[3], verts[4], verts[5]);
        let r = intersect_triangles(&t1, &t2);
        assert!(
            matches!(r, TriangleIntersection::ProperSegment { .. }),
            "classifier returned {r:?}"
        );
        let (mask, count, skipped) = super::self_intersections_core(&verts, &tris);
        assert!(!skipped);
        assert_eq!(count, 2, "mask {mask:?}");
        assert!(mask[0] && mask[1]);
        // Two separated triangles -> no intersection.
        let verts2 = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [5.0, 5.0, 0.0],
            [6.0, 5.0, 0.0],
            [5.0, 6.0, 0.0],
        ];
        let tris2 = vec![[0, 1, 2], [3, 4, 5]];
        let (mask2, count2, _) = super::self_intersections_core(&verts2, &tris2);
        assert_eq!(count2, 0);
        assert!(!mask2[0] && !mask2[1]);
    }
}
