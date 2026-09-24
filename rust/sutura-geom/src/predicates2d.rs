//! Exact 2D geometric predicates on explicit and indirect points.
//!
//! Points live in a local 2D parameter space.  An explicit point is `(x,y)`;
//! an implicit point is a 2D line-line intersection represented by the
//! homogeneous triple `(A,B,d)` from Attene 2020 (§4.1), so that the
//! Cartesian coordinates are `(A/d, B/d)`.

use crate::point::f64_to_rat;
use crate::profile_count;
use num_rational::BigRational;
use num_traits::{One, Signed, Zero};

use robust::{incircle as robust_incircle, orient2d as robust_orient2d, Coord as RobustCoord2D};

/// A 2D point in parameter space.
#[derive(Clone, Debug, PartialEq)]
pub enum Point2 {
    Explicit([f64; 2]),
    /// Line-line intersection: intersection of line through `a,b` with line
    /// through `c,d`.
    Lli {
        a: [f64; 2],
        b: [f64; 2],
        c: [f64; 2],
        d: [f64; 2],
    },
}

impl Point2 {
    /// Homogeneous representation `(A, B, d)` such that the point is
    /// `(A/d, B/d)`.  For an explicit point, `(A,B) = (x,y)` and `d = 1`.
    pub fn homogeneous(&self) -> Option<(BigRational, BigRational, BigRational)> {
        match *self {
            Point2::Explicit([x, y]) => Some((f64_to_rat(x), f64_to_rat(y), BigRational::one())),
            Point2::Lli { a, b, c, d } => {
                let ax = f64_to_rat(a[0]);
                let ay = f64_to_rat(a[1]);
                let bx = f64_to_rat(b[0]);
                let by = f64_to_rat(b[1]);
                let cx = f64_to_rat(c[0]);
                let cy = f64_to_rat(c[1]);
                let dx = f64_to_rat(d[0]);
                let dy = f64_to_rat(d[1]);

                let term_a = &ax * &by - &bx * &ay;
                let term_b = &cx * &dy - &dx * &cy;

                let den = (&ax - &bx) * (&cy - &dy) - (&ay - &by) * (&cx - &dx);
                if den.is_zero() {
                    return None;
                }

                let a_num = &term_a * (&cx - &dx) - &term_b * (&ax - &bx);
                let b_num = &term_a * (&cy - &dy) - &term_b * (&ay - &by);

                Some((a_num, b_num, den))
            }
        }
    }

    /// Approximate `f64` coordinate, for filters only.
    pub fn to_f64(&self) -> Option<[f64; 2]> {
        match *self {
            Point2::Explicit(p) => Some(p),
            Point2::Lli { a, b, c, d } => {
                let denom = (a[0] - b[0]) * (c[1] - d[1]) - (a[1] - b[1]) * (c[0] - d[0]);
                if denom == 0.0 {
                    return None;
                }
                let term1 = a[0] * b[1] - b[0] * a[1];
                let term2 = c[0] * d[1] - d[0] * c[1];
                let px = (term1 * (c[0] - d[0]) - term2 * (a[0] - b[0])) / denom;
                let py = (term1 * (c[1] - d[1]) - term2 * (a[1] - b[1])) / denom;
                Some([px, py])
            }
        }
    }
}

fn hom_pair(p: &Point2) -> Option<(BigRational, BigRational, BigRational)> {
    p.homogeneous()
}

/// Exact sign of `orient2d(a,b,c)`.
///
/// For all-explicit points the `robust` crate is used.  For mixed/implicit
/// points the cleared determinant from Attene 2020 (§4.3) is evaluated in
/// exact rational arithmetic.
pub fn orient2d_sign(a: &Point2, b: &Point2, c: &Point2) -> f64 {
    profile_count!(ORIENT2D_CALLS, 1);
    if let (Point2::Explicit(pa), Point2::Explicit(pb), Point2::Explicit(pc)) = (a, b, c) {
        return robust_orient2d(
            RobustCoord2D { x: pa[0], y: pa[1] },
            RobustCoord2D { x: pb[0], y: pb[1] },
            RobustCoord2D { x: pc[0], y: pc[1] },
        );
    }

    if let (Some(pa), Some(pb), Some(pc)) = (a.to_f64(), b.to_f64(), c.to_f64()) {
        let det = robust_orient2d(
            RobustCoord2D { x: pa[0], y: pa[1] },
            RobustCoord2D { x: pb[0], y: pb[1] },
            RobustCoord2D { x: pc[0], y: pc[1] },
        );
        let scale =
            (pa[0].abs() + pa[1].abs() + pb[0].abs() + pb[1].abs() + pc[0].abs() + pc[1].abs())
                .max(1e-300);
        if det.abs() > 1e-9 * scale.powi(2) {
            return det;
        }
    }

    let (a1, a2, da) = hom_pair(a).expect("point must be well-defined");
    let (b1, b2, db) = hom_pair(b).expect("point must be well-defined");
    let (c1, c2, dc) = hom_pair(c).expect("point must be well-defined");

    // Cleared determinant: Λ′ = (da*b1 - db*a1)*(da*c2 - dc*a2)
    //                          - (da*b2 - db*a2)*(da*c1 - dc*a1)
    // Denominator D′ = da² * db * dc (sign is product of denominator signs).
    let term1 = &da * &b1 - &db * &a1;
    let term2 = &da * &c2 - &dc * &a2;
    let term3 = &da * &b2 - &db * &a2;
    let term4 = &da * &c1 - &dc * &a1;
    let det = &term1 * &term2 - &term3 * &term4;

    let den_sign = da.signum() * db.signum() * dc.signum();
    if det.is_zero() {
        0.0
    } else if det.signum() == den_sign {
        1.0
    } else {
        -1.0
    }
}

/// Exact sign of the incircle predicate.
///
/// Returns positive if `d` lies inside the oriented circumcircle of
/// `(a,b,c)`, negative if outside, zero if cospherical.
pub fn incircle_sign(a: &Point2, b: &Point2, c: &Point2, d: &Point2) -> f64 {
    profile_count!(INCIRCLE_CALLS, 1);
    if let (
        Point2::Explicit(pa),
        Point2::Explicit(pb),
        Point2::Explicit(pc),
        Point2::Explicit(pd),
    ) = (a, b, c, d)
    {
        return robust_incircle(
            RobustCoord2D { x: pa[0], y: pa[1] },
            RobustCoord2D { x: pb[0], y: pb[1] },
            RobustCoord2D { x: pc[0], y: pc[1] },
            RobustCoord2D { x: pd[0], y: pd[1] },
        );
    }

    if let (Some(pa), Some(pb), Some(pc), Some(pd)) =
        (a.to_f64(), b.to_f64(), c.to_f64(), d.to_f64())
    {
        let det = robust_incircle(
            RobustCoord2D { x: pa[0], y: pa[1] },
            RobustCoord2D { x: pb[0], y: pb[1] },
            RobustCoord2D { x: pc[0], y: pc[1] },
            RobustCoord2D { x: pd[0], y: pd[1] },
        );
        let scale = (pa[0].abs()
            + pa[1].abs()
            + pb[0].abs()
            + pb[1].abs()
            + pc[0].abs()
            + pc[1].abs()
            + pd[0].abs()
            + pd[1].abs())
        .max(1e-300);
        if det.abs() > 1e-9 * scale.powi(3) {
            return det;
        }
    }

    let (a1, a2, da) = hom_pair(a).expect("point must be well-defined");
    let (b1, b2, db) = hom_pair(b).expect("point must be well-defined");
    let (c1, c2, dc) = hom_pair(c).expect("point must be well-defined");
    let (d1, d2, dd) = hom_pair(d).expect("point must be well-defined");

    // Indirect incircle matrix entries from Attene 2020 (§4.4).
    let two = BigRational::one() + BigRational::one();

    let m11 = &da * &dd * &dd * &a1 - &da * &da * &dd * &d1;
    let m12 = &da * &dd * &dd * &a2 - &da * &da * &dd * &d2;
    let m13 = &dd * &dd * (&a1 * &a1 + &a2 * &a2) + &da * &da * (&d1 * &d1 + &d2 * &d2)
        - &two * &da * &dd * (&a1 * &d1 + &a2 * &d2);

    let m21 = &db * &dd * &dd * &b1 - &db * &db * &dd * &d1;
    let m22 = &db * &dd * &dd * &b2 - &db * &db * &dd * &d2;
    let m23 = &dd * &dd * (&b1 * &b1 + &b2 * &b2) + &db * &db * (&d1 * &d1 + &d2 * &d2)
        - &two * &db * &dd * (&b1 * &d1 + &b2 * &d2);

    let m31 = &dc * &dd * &dd * &c1 - &dc * &dc * &dd * &d1;
    let m32 = &dc * &dd * &dd * &c2 - &dc * &dc * &dd * &d2;
    let m33 = &dd * &dd * (&c1 * &c1 + &c2 * &c2) + &dc * &dc * (&d1 * &d1 + &d2 * &d2)
        - &two * &dc * &dd * (&c1 * &d1 + &c2 * &d2);

    let det = m11 * (m22.clone() * m33.clone() - m23.clone() * m32.clone())
        - m12 * (m21.clone() * m33.clone() - m23 * m31.clone())
        + m13 * (m21 * m32 - m22 * m31);

    let den_sign = da.signum() * db.signum() * dc.signum() * dd.signum();
    if det.is_zero() {
        0.0
    } else if det.signum() == den_sign {
        1.0
    } else {
        -1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_orient2d_and_incircle() {
        let a = Point2::Explicit([0.0, 0.0]);
        let b = Point2::Explicit([1.0, 0.0]);
        let c = Point2::Explicit([0.0, 1.0]);
        let d = Point2::Explicit([0.1, 0.1]); // inside triangle circumcircle
        assert!(orient2d_sign(&a, &b, &c) > 0.0);
        assert!(incircle_sign(&a, &b, &c, &d) > 0.0);
        let far = Point2::Explicit([10.0, 10.0]);
        assert!(incircle_sign(&a, &b, &c, &far) < 0.0);
    }

    #[test]
    fn lli_orient2d_agrees_with_coordinate_form() {
        // Two crossing lines: (0,0)-(1,1) and (0,1)-(1,0) intersect at (0.5,0.5).
        let p = Point2::Lli {
            a: [0.0, 0.0],
            b: [1.0, 1.0],
            c: [0.0, 1.0],
            d: [1.0, 0.0],
        };
        let a = Point2::Explicit([0.0, 0.0]);
        let b = Point2::Explicit([1.0, 0.0]);

        // (0.5,0.5) is to the left of the directed line a->b?
        let s = orient2d_sign(&a, &b, &p);
        assert!(s > 0.0, "intersection point should be left of a->b->c");

        let coord = p.to_f64().unwrap();
        assert!((coord[0] - 0.5).abs() < 1e-12);
        assert!((coord[1] - 0.5).abs() < 1e-12);
    }

    #[test]
    fn collinear_points_give_zero_orient2d() {
        let a = Point2::Explicit([0.0, 0.0]);
        let b = Point2::Explicit([1.0, 1.0]);
        let c = Point2::Explicit([2.0, 2.0]);
        assert_eq!(orient2d_sign(&a, &b, &c), 0.0);
    }

    #[test]
    fn degenerate_parallel_lines_give_none() {
        let p = Point2::Lli {
            a: [0.0, 0.0],
            b: [1.0, 0.0],
            c: [0.0, 1.0],
            d: [1.0, 1.0],
        };
        assert!(p.homogeneous().is_none());
    }

    #[test]
    fn random_mixed_orient2d_and_incircle_agree() {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        for _ in 0..200 {
            let ax = rng.gen::<f64>();
            let ay = rng.gen::<f64>();
            let bx = rng.gen::<f64>();
            let by = rng.gen::<f64>();
            let cx = rng.gen::<f64>();
            let cy = rng.gen::<f64>();

            let a = Point2::Explicit([ax, ay]);
            let b = Point2::Explicit([bx, by]);
            let c = Point2::Explicit([cx, cy]);

            // Build an LLI point from two random lines.
            let p1 = Point2::Lli {
                a: [rng.gen::<f64>(), rng.gen::<f64>()],
                b: [rng.gen::<f64>(), rng.gen::<f64>()],
                c: [rng.gen::<f64>(), rng.gen::<f64>()],
                d: [rng.gen::<f64>(), rng.gen::<f64>()],
            };
            if p1.homogeneous().is_none() {
                continue;
            }

            let s_indirect = orient2d_sign(&a, &b, &p1);
            let coord = p1.to_f64().unwrap();
            let p_explicit = Point2::Explicit(coord);
            let s_explicit = orient2d_sign(&a, &b, &p_explicit);
            assert_eq!(
                s_indirect.signum(),
                s_explicit.signum(),
                "indirect orient2d must agree with explicit-coordinate evaluation"
            );

            let ic_indirect = incircle_sign(&a, &b, &c, &p1);
            let ic_explicit = incircle_sign(&a, &b, &c, &p_explicit);
            assert_eq!(
                ic_indirect.signum(),
                ic_explicit.signum(),
                "indirect incircle must agree with explicit-coordinate evaluation"
            );
        }
    }
}
