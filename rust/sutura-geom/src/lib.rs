//! sutura_geom — foundation crate for Sutura's future indirect-predicates geometry core.
//!
//! Phase A scope: thin PyO3 wrapper exposing the explicit-point robust predicates
//! `orient3d` and `insphere` from the `robust` crate (Shewchuk adaptive-precision
//! arithmetic) to Python.
//!
//! Phase B scope (in progress): indirect point representations (LPI/PPI),
//! exact predicates on explicit and implicit points (`orient3d`, `orient2d`,
//! `incircle`), and the predicate core for the future arrangement-lite engine.

use numpy::{AllowTypeChange, PyArrayLike1, PyArrayLike2};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};
use robust::{insphere as robust_insphere, orient3d as robust_orient3d, Coord3D};

pub mod arrangement;
pub mod cdt2d;
pub mod dressing;
pub mod dual_contour;
pub mod flap;
pub mod interval;
pub mod morph;
pub mod point;
pub mod predicates2d;
pub mod predicates3d;
pub mod profile;
pub mod raycast;
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
    for i in 0..n {
        rust_verts.push([vv[[i, 0]], vv[[i, 1]], vv[[i, 2]]]);
    }
    let m = tt.nrows();
    let mut rust_tris: Vec<[usize; 3]> = Vec::with_capacity(m);
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
#[pyfunction]
#[pyo3(signature = (verts, tris, voxel=None, r#box=None, raystab=false, raystab_dirs=None, raystab_seed=0, raystab_parity=false))]
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
    let bvh = winding::MeshBvh::from_arrays(&rv, &rt);
    let (w, u, mut s) = bvh.sdf_grid(origin, voxel, dims);
    let mut stab_stats = None;
    if raystab {
        let stabber = build_stabber(&bvh, raystab_dirs, raystab_seed, raystab_parity);
        stab_stats = Some(raycast::disambiguate_sdf(
            &bvh,
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
#[pyfunction]
#[pyo3(signature = (verts, tris, r, voxel=None, r#box=None, fill_cavities=false, surface=None, raystab=false, raystab_dirs=None, raystab_seed=0, raystab_parity=false))]
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

    let bvh = winding::MeshBvh::from_arrays(&rv, &rt);
    let (w, u, mut s) = bvh.sdf_grid(origin, voxel, dims);
    let mut stab_stats = None;
    if raystab {
        let stabber = build_stabber(&bvh, raystab_dirs, raystab_seed, raystab_parity);
        stab_stats = Some(raycast::disambiguate_sdf(
            &bvh,
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
    // r == 0 selects the sign-field path: the raw signed field (GWN sign,
    // optionally ray-stab-corrected) feeds dual contouring directly, with no
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
#[pyo3(signature = (verts, tris))]
fn self_intersecting_faces<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i32, AllowTypeChange>,
) -> PyResult<(Bound<'py, PyArray1<bool>>, i64)> {
    let (rv, rt) = parse_mesh(&verts, &tris)?;
    let (mask, count, _skipped) = self_intersections_core(&rv, &rt);
    Ok((PyArray1::from_vec(py, mask), count))
}

/// Core self-intersection scan (no Python): `(mask, count, skipped)`.
fn self_intersections_core(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> (Vec<bool>, i64, bool) {
    let f = tris.len();
    if f == 0 {
        return (Vec::new(), 0, false);
    }
    if f > SI_MAX_FACES {
        return (vec![false; f], -1, true);
    }
    let triangles: Vec<triangle_intersection::Triangle> = tris
        .iter()
        .map(|t| triangle_intersection::Triangle::explicit(verts[t[0]], verts[t[1]], verts[t[2]]))
        .collect();
    let pairs = arrangement::broad_phase_candidates(verts, tris);
    let mut mask = vec![false; f];
    for (i, j) in pairs {
        let (a, b) = (tris[i], tris[j]);
        let adjacent = a.iter().any(|x| b.contains(x));
        if adjacent {
            continue;
        }
        match triangle_intersection::intersect_triangles(&triangles[i], &triangles[j]) {
            triangle_intersection::TriangleIntersection::ProperSegment { .. } => {
                mask[i] = true;
                mask[j] = true;
            }
            _ => {}
        }
    }
    let count = mask.iter().filter(|&&b| b).count() as i64;
    (mask, count, false)
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
}

#[pymethods]
impl PyMeshBvh {
    #[new]
    fn new(
        verts: PyArrayLike2<'_, f64, AllowTypeChange>,
        tris: PyArrayLike2<'_, i32, AllowTypeChange>,
    ) -> PyResult<Self> {
        let (rv, rt) = parse_mesh(&verts, &tris)?;
        Ok(Self {
            inner: winding::MeshBvh::from_arrays(&rv, &rt),
        })
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
}

#[pymodule]
fn sutura_geom(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(orient3d, m)?)?;
    m.add_function(wrap_pyfunction!(insphere, m)?)?;
    m.add_function(wrap_pyfunction!(arrangement_lite, m)?)?;
    m.add_function(wrap_pyfunction!(_set_cdt_experimental, m)?)?;
    m.add_function(wrap_pyfunction!(sdf_grid, m)?)?;
    m.add_function(wrap_pyfunction!(morph_close, m)?)?;
    m.add_function(wrap_pyfunction!(dressing_coat, m)?)?;
    m.add_function(wrap_pyfunction!(closest_points, m)?)?;
    m.add_function(wrap_pyfunction!(self_intersecting_faces, m)?)?;
    m.add_function(wrap_pyfunction!(raystab_points, m)?)?;
    m.add_function(wrap_pyfunction!(raystab_grid, m)?)?;
    m.add_function(wrap_pyfunction!(topology::edge_table, m)?)?;
    m.add_function(wrap_pyfunction!(topology::weld_vertices, m)?)?;
    m.add_function(wrap_pyfunction!(topology::weld_reload_equivalent, m)?)?;
    m.add_function(wrap_pyfunction!(flap::flap_fill_py, m)?)?;
    m.add_class::<PyMeshBvh>()?;
    m.add("MAX_VOXELS", MAX_VOXELS)?;
    m.add("SI_MAX_FACES", SI_MAX_FACES)?;
    m.add("RAYSTAB_DEFAULT_DIRS", raycast::DEFAULT_DIRS)?;
    m.add("RAYSTAB_MAX_DIRS", raycast::MAX_DIRS)?;
    m.add("SIGN_FIELD_SUPPORTED", true)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{insphere_coords, orient3d_coords};

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
