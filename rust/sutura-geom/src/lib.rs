//! sutura_geom — foundation crate for Sutura's future indirect-predicates geometry core.
//!
//! Phase A scope: thin PyO3 wrapper exposing the explicit-point robust predicates
//! `orient3d` and `insphere` from the `robust` crate (Shewchuk adaptive-precision
//! arithmetic) to Python. Indirect/implicit predicates, expansion arithmetic,
//! CDT and arrangement logic are explicitly out of scope and will be layered on
//! top of this crate in later phases.

use numpy::{AllowTypeChange, PyArrayLike1};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyModule;
use robust::{Coord3D, insphere as robust_insphere, orient3d as robust_orient3d};

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
    let pa = Coord3D { x: pa[0], y: pa[1], z: pa[2] };
    let pb = Coord3D { x: pb[0], y: pb[1], z: pb[2] };
    let pc = Coord3D { x: pc[0], y: pc[1], z: pc[2] };
    let pd = Coord3D { x: pd[0], y: pd[1], z: pd[2] };
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
fn insphere_coords(
    pa: [f64; 3],
    pb: [f64; 3],
    pc: [f64; 3],
    pd: [f64; 3],
    pe: [f64; 3],
) -> f64 {
    let pa = Coord3D { x: pa[0], y: pa[1], z: pa[2] };
    let pb = Coord3D { x: pb[0], y: pb[1], z: pb[2] };
    let pc = Coord3D { x: pc[0], y: pc[1], z: pc[2] };
    let pd = Coord3D { x: pd[0], y: pd[1], z: pd[2] };
    let pe = Coord3D { x: pe[0], y: pe[1], z: pe[2] };
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

#[pymodule]
fn sutura_geom(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(orient3d, m)?)?;
    m.add_function(wrap_pyfunction!(insphere, m)?)?;
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
}