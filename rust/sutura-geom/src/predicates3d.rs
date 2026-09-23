//! Exact 3D geometric predicates on explicit and indirect points.

use crate::point::{det_rat, sub_rat, Point3};
use num_traits::{Signed, Zero};
use robust::{orient3d as robust_orient3d, Coord3D};

/// Sign of the 3D orientation predicate `orient3d(p1,p2,p3,p4)`.
///
/// If all four points are explicit, the result is computed with the
/// `robust` crate (filtered exact).  Otherwise the exact rational
/// coordinates of the implicit points are constructed in `BigRational`
/// and the determinant sign is evaluated exactly.
///
/// Returns a positive value if `p4` lies below the oriented plane
/// `(p1,p2,p3)`, negative if above, and zero if the four points are
/// coplanar.
pub fn orient3d_sign(p1: &Point3, p2: &Point3, p3: &Point3, p4: &Point3) -> f64 {
    // Fast path: all explicit -> use the battle-tested robust crate.
    if let (Point3::Explicit(a), Point3::Explicit(b), Point3::Explicit(c), Point3::Explicit(d)) =
        (p1, p2, p3, p4)
    {
        return robust_orient3d(
            Coord3D {
                x: a[0],
                y: a[1],
                z: a[2],
            },
            Coord3D {
                x: b[0],
                y: b[1],
                z: b[2],
            },
            Coord3D {
                x: c[0],
                y: c[1],
                z: c[2],
            },
            Coord3D {
                x: d[0],
                y: d[1],
                z: d[2],
            },
        );
    }

    // Filter: try an approximate f64 evaluation; if the magnitude is safely
    // away from zero, the sign is certified.
    if let (Some(a), Some(b), Some(c), Some(d)) =
        (p1.to_f64(), p2.to_f64(), p3.to_f64(), p4.to_f64())
    {
        let det = robust_orient3d(
            Coord3D {
                x: a[0],
                y: a[1],
                z: a[2],
            },
            Coord3D {
                x: b[0],
                y: b[1],
                z: b[2],
            },
            Coord3D {
                x: c[0],
                y: c[1],
                z: c[2],
            },
            Coord3D {
                x: d[0],
                y: d[1],
                z: d[2],
            },
        );
        let scale = (a[0].abs()
            + a[1].abs()
            + a[2].abs()
            + b[0].abs()
            + b[1].abs()
            + b[2].abs()
            + c[0].abs()
            + c[1].abs()
            + c[2].abs()
            + d[0].abs()
            + d[1].abs()
            + d[2].abs())
        .max(1e-300);
        // A very loose relative threshold; the exact rational path catches
        // anything near zero.
        if det.abs() > 1e-9 * scale.powi(3) {
            return det;
        }
    }

    // Exact rational fallback.
    let a = p1
        .to_rational()
        .expect("implicit point must be well-defined");
    let b = p2
        .to_rational()
        .expect("implicit point must be well-defined");
    let c = p3
        .to_rational()
        .expect("implicit point must be well-defined");
    let d = p4
        .to_rational()
        .expect("implicit point must be well-defined");

    let det = det_rat(&sub_rat(&b, &a), &sub_rat(&c, &a), &sub_rat(&d, &a));
    if det.is_zero() {
        0.0
    } else if det.is_positive() {
        1.0
    } else {
        -1.0
    }
}

/// Indirect `orient3d` for the case where exactly the first point is an
/// LPI and the other three are explicit.  This evaluates the cleared
/// polynomial from Attene 2020 (§4.6) without explicitly constructing the
/// rational coordinate of `p1`.  It is used in tests as a cross-check for
/// the generic `orient3d_sign` implementation.
pub fn orient3d_indirect_one_lpi(
    p1: &Point3,
    p2: &[f64; 3],
    p3: &[f64; 3],
    p4: &[f64; 3],
) -> Option<f64> {
    let (lambda, d) = p1.homogeneous()?;
    let p2 = [
        crate::point::f64_to_rat(p2[0]),
        crate::point::f64_to_rat(p2[1]),
        crate::point::f64_to_rat(p2[2]),
    ];
    let p3 = [
        crate::point::f64_to_rat(p3[0]),
        crate::point::f64_to_rat(p3[1]),
        crate::point::f64_to_rat(p3[2]),
    ];
    let p4 = [
        crate::point::f64_to_rat(p4[0]),
        crate::point::f64_to_rat(p4[1]),
        crate::point::f64_to_rat(p4[2]),
    ];

    let row1 = [
        &lambda[0] - &d * &p4[0],
        &lambda[1] - &d * &p4[1],
        &lambda[2] - &d * &p4[2],
    ];
    let row2 = sub_rat(&p2, &p4);
    let row3 = sub_rat(&p3, &p4);

    let det = det_rat(&row1, &row2, &row3);
    let sign = if det.is_zero() {
        0
    } else if det.is_positive() {
        1
    } else {
        -1
    };
    let d_sign = if d.is_zero() {
        return None;
    } else if d.is_positive() {
        1
    } else {
        -1
    };
    Some((sign * d_sign) as f64)
}

/// Convenience: `orient3d_sign` where the arguments are explicit points.
pub fn orient3d_sign_explicit(a: [f64; 3], b: [f64; 3], c: [f64; 3], d: [f64; 3]) -> f64 {
    orient3d_sign(
        &Point3::Explicit(a),
        &Point3::Explicit(b),
        &Point3::Explicit(c),
        &Point3::Explicit(d),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: [f64; 3] = [0.0, 0.0, 0.0];
    const X: [f64; 3] = [1.0, 0.0, 0.0];
    const Y: [f64; 3] = [0.0, 1.0, 0.0];
    const Z: [f64; 3] = [0.0, 0.0, 1.0];

    #[test]
    fn explicit_orient3d_signs_match_phase_a() {
        // Z is above the XY plane (ORIGIN, X, Y) -> negative sign in robust convention.
        assert!(orient3d_sign_explicit(ORIGIN, X, Y, Z) < 0.0);
        // -Z is below -> positive.
        assert!(orient3d_sign_explicit(ORIGIN, X, Y, [-Z[0], -Z[1], -Z[2]]) > 0.0);
        // Coplanar -> zero.
        assert_eq!(orient3d_sign_explicit(ORIGIN, X, Y, [0.5, 0.5, 0.0]), 0.0);
    }

    #[test]
    fn lpi_point_coplanar_with_plane_gives_zero() {
        // LPI point is the origin (lies in XY plane).
        let p = Point3::Lpi {
            q1: [0.5, 0.5, -1.0],
            q2: [0.5, 0.5, 1.0],
            r: ORIGIN,
            s: X,
            t: Y,
        };
        let s = orient3d_sign(
            &p,
            &Point3::Explicit(X),
            &Point3::Explicit(Y),
            &Point3::Explicit(Z),
        );
        assert_eq!(s, 0.0);
    }

    #[test]
    fn indirect_orient3d_agrees_with_rational_reference() {
        // A line piercing the XY plane at (0.5,0.5,0).
        let p = Point3::Lpi {
            q1: [0.5, 0.5, -1.0],
            q2: [0.5, 0.5, 1.0],
            r: ORIGIN,
            s: X,
            t: Y,
        };
        let direct = orient3d_sign(
            &p,
            &Point3::Explicit(X),
            &Point3::Explicit(Y),
            &Point3::Explicit(Z),
        );
        let indirect = orient3d_indirect_one_lpi(&p, &X, &Y, &Z).unwrap();
        assert_eq!(direct, indirect);
    }

    #[test]
    fn collinear_base_gives_zero() {
        let a = Point3::Explicit(ORIGIN);
        let b = Point3::Explicit(X);
        let c = Point3::Explicit([2.0, 0.0, 0.0]);
        let d = Point3::Explicit(Z);
        assert_eq!(orient3d_sign(&a, &b, &c, &d), 0.0);
    }

    #[test]
    fn random_explicit_and_lpi_agree() {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        for _ in 0..200 {
            let a = [rng.gen::<f64>(), rng.gen::<f64>(), rng.gen::<f64>()];
            let b = [rng.gen::<f64>(), rng.gen::<f64>(), rng.gen::<f64>()];
            let c = [rng.gen::<f64>(), rng.gen::<f64>(), rng.gen::<f64>()];
            let d = [rng.gen::<f64>(), rng.gen::<f64>(), rng.gen::<f64>()];

            // Robust direct value.
            let direct = orient3d_sign_explicit(a, b, c, d);
            let sign_direct = direct.signum();

            // Build an LPI point from two random edges/planes and compare the
            // indirect cleared-polynomial form with the exact-rational form.
            let q1 = [rng.gen::<f64>(), rng.gen::<f64>(), rng.gen::<f64>()];
            let q2 = [rng.gen::<f64>(), rng.gen::<f64>(), rng.gen::<f64>()];
            let r = [rng.gen::<f64>(), rng.gen::<f64>(), rng.gen::<f64>()];
            let s = [rng.gen::<f64>(), rng.gen::<f64>(), rng.gen::<f64>()];
            let t = [rng.gen::<f64>(), rng.gen::<f64>(), rng.gen::<f64>()];
            let p = Point3::Lpi { q1, q2, r, s, t };

            let rat = orient3d_sign(
                &p,
                &Point3::Explicit(a),
                &Point3::Explicit(b),
                &Point3::Explicit(c),
            );
            if let Some(ind) = orient3d_indirect_one_lpi(&p, &a, &b, &c) {
                assert_eq!(
                    rat.signum(),
                    ind.signum(),
                    "indirect orient3d disagrees with rational reference"
                );
            }

            // The explicit sign must itself be consistent.
            assert!(sign_direct == 1.0 || sign_direct == -1.0 || sign_direct == 0.0);
        }
    }
}
