//! Indirect point representations for exact geometric predicates.
//!
//! An indirect point is stored as the tuple of primitives that define it,
//! together with the construction recipe (line-plane intersection or
//! plane-plane-plane intersection).  Coordinates are never stored as rounded
//! `f64` values inside the predicate core; instead each point has a
//! homogeneous representation `(λ, d)` such that `p = λ / d`, and predicates
//! are evaluated by clearing denominators.

use crate::{profile_count, profile_time};
use num_rational::BigRational;
use num_traits::{FromPrimitive, One, Zero};

/// A 3D point that is either an explicit input vertex or an implicit
/// construction over explicit vertices.
#[derive(Clone, Debug, PartialEq)]
pub enum Point3 {
    /// Explicit input vertex.
    Explicit([f64; 3]),
    /// Line-plane intersection: line through `q1,q2` intersected with the
    /// plane through `r,s,t`.
    Lpi {
        q1: [f64; 3],
        q2: [f64; 3],
        r: [f64; 3],
        s: [f64; 3],
        t: [f64; 3],
    },
    /// Plane-plane-plane intersection.
    Ppi {
        r1: [f64; 3],
        s1: [f64; 3],
        t1: [f64; 3],
        r2: [f64; 3],
        s2: [f64; 3],
        t2: [f64; 3],
        r3: [f64; 3],
        s3: [f64; 3],
        t3: [f64; 3],
    },
}

impl Point3 {
    /// Convert the point to an approximate `f64` coordinate.  This is the
    /// EPICK construction path and is used *only* for the fast filter, never
    /// for exact decisions.
    pub fn to_f64(&self) -> Option<[f64; 3]> {
        match *self {
            Point3::Explicit(p) => Some(p),
            Point3::Lpi { q1, q2, r, s, t } => {
                let d = det(sub(q1, q2), sub(s, r), sub(t, r));
                if d == 0.0 {
                    return None;
                }
                let nq = det(sub(q1, r), sub(s, r), sub(t, r));
                let lambda = add(scale(q1, d), scale(sub(q2, q1), nq));
                Some([lambda[0] / d, lambda[1] / d, lambda[2] / d])
            }
            Point3::Ppi {
                r1,
                s1,
                t1,
                r2,
                s2,
                t2,
                r3,
                s3,
                t3,
            } => {
                let n1 = cross(sub(s1, r1), sub(t1, r1));
                let n2 = cross(sub(s2, r2), sub(t2, r2));
                let n3 = cross(sub(s3, r3), sub(t3, r3));
                let d1 = dot(n1, r1);
                let d2 = dot(n2, r2);
                let d3 = dot(n3, r3);
                let n2xn3 = cross(n2, n3);
                let n3xn1 = cross(n3, n1);
                let n1xn2 = cross(n1, n2);
                let d = dot(n1, n2xn3);
                if d == 0.0 {
                    return None;
                }
                let n = add(add(scale(n2xn3, d1), scale(n3xn1, d2)), scale(n1xn2, d3));
                Some([n[0] / d, n[1] / d, n[2] / d])
            }
        }
    }

    /// Exact rational coordinate of the point.  The input `f64` coordinates
    /// are treated as exact dyadic rationals, and the construction is carried
    /// out in `BigRational` arithmetic.
    pub fn to_rational(&self) -> Option<[BigRational; 3]> {
        profile_time!(IMPLICIT_CONSTRUCTION, {
            profile_count!(IMPLICIT_CONSTRUCTION, 1);
            let result = match *self {
                Point3::Explicit(p) => {
                    Some([f64_to_rat(p[0]), f64_to_rat(p[1]), f64_to_rat(p[2])])
                }
                Point3::Lpi { q1, q2, r, s, t } => {
                    let q1 = arr_rat(q1);
                    let q2 = arr_rat(q2);
                    let r = arr_rat(r);
                    let s = arr_rat(s);
                    let t = arr_rat(t);

                    let sr = sub_rat(&s, &r);
                    let tr = sub_rat(&t, &r);
                    let q1q2 = sub_rat(&q1, &q2);
                    let d = det_rat(&q1q2, &sr, &tr);
                    if d.is_zero() {
                        None
                    } else {
                        let q1r = sub_rat(&q1, &r);
                        let nq = det_rat(&q1r, &sr, &tr);

                        let lambda =
                            add_rat(&scale_rat(&q1, &d), &scale_rat(&sub_rat(&q2, &q1), &nq));
                        Some([&lambda[0] / &d, &lambda[1] / &d, &lambda[2] / &d])
                    }
                }
                Point3::Ppi {
                    r1,
                    s1,
                    t1,
                    r2,
                    s2,
                    t2,
                    r3,
                    s3,
                    t3,
                } => {
                    let (n1, d1) = plane_rat(r1, s1, t1);
                    let (n2, d2) = plane_rat(r2, s2, t2);
                    let (n3, d3) = plane_rat(r3, s3, t3);

                    let n2xn3 = cross_rat(&n2, &n3);
                    let n3xn1 = cross_rat(&n3, &n1);
                    let n1xn2 = cross_rat(&n1, &n2);

                    let d = dot_rat(&n1, &n2xn3);
                    if d.is_zero() {
                        None
                    } else {
                        let num = add_rat(
                            &add_rat(&scale_rat(&n2xn3, &d1), &scale_rat(&n3xn1, &d2)),
                            &scale_rat(&n1xn2, &d3),
                        );
                        Some([&num[0] / &d, &num[1] / &d, &num[2] / &d])
                    }
                }
            };
            result
        })
    }

    /// Homogeneous representation `(λ, d)` such that `p = λ / d`.  For an
    /// explicit point `λ` is the coordinate vector and `d = 1`.  This is the
    /// form used by the indirect predicate formulas.
    pub fn homogeneous(&self) -> Option<([BigRational; 3], BigRational)> {
        match *self {
            Point3::Explicit(p) => Some((
                [f64_to_rat(p[0]), f64_to_rat(p[1]), f64_to_rat(p[2])],
                BigRational::one(),
            )),
            Point3::Lpi { q1, q2, r, s, t } => {
                let q1 = arr_rat(q1);
                let q2 = arr_rat(q2);
                let r = arr_rat(r);
                let s = arr_rat(s);
                let t = arr_rat(t);

                let sr = sub_rat(&s, &r);
                let tr = sub_rat(&t, &r);
                let q1q2 = sub_rat(&q1, &q2);
                let d = det_rat(&q1q2, &sr, &tr);
                if d.is_zero() {
                    return None;
                }

                let q1r = sub_rat(&q1, &r);
                let nq = det_rat(&q1r, &sr, &tr);

                let lambda = add_rat(&scale_rat(&q1, &d), &scale_rat(&sub_rat(&q2, &q1), &nq));
                Some((lambda, d))
            }
            Point3::Ppi {
                r1,
                s1,
                t1,
                r2,
                s2,
                t2,
                r3,
                s3,
                t3,
            } => {
                let (n1, d1) = plane_rat(r1, s1, t1);
                let (n2, d2) = plane_rat(r2, s2, t2);
                let (n3, d3) = plane_rat(r3, s3, t3);

                let n2xn3 = cross_rat(&n2, &n3);
                let n3xn1 = cross_rat(&n3, &n1);
                let n1xn2 = cross_rat(&n1, &n2);

                let d = dot_rat(&n1, &n2xn3);
                if d.is_zero() {
                    return None;
                }

                let lambda = add_rat(
                    &add_rat(&scale_rat(&n2xn3, &d1), &scale_rat(&n3xn1, &d2)),
                    &scale_rat(&n1xn2, &d3),
                );
                Some((lambda, d))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// f64 vector helpers (filter path only)
// ---------------------------------------------------------------------------

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn det(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> f64 {
    dot(a, cross(b, c))
}

// ---------------------------------------------------------------------------
// BigRational vector helpers (exact path)
// ---------------------------------------------------------------------------

pub(crate) fn f64_to_rat(x: f64) -> BigRational {
    BigRational::from_f64(x).expect("coordinate must be finite")
}

fn arr_rat(p: [f64; 3]) -> [BigRational; 3] {
    [f64_to_rat(p[0]), f64_to_rat(p[1]), f64_to_rat(p[2])]
}

pub(crate) fn sub_rat(a: &[BigRational; 3], b: &[BigRational; 3]) -> [BigRational; 3] {
    [
        a[0].clone() - &b[0],
        a[1].clone() - &b[1],
        a[2].clone() - &b[2],
    ]
}

pub(crate) fn add_rat(a: &[BigRational; 3], b: &[BigRational; 3]) -> [BigRational; 3] {
    [
        a[0].clone() + &b[0],
        a[1].clone() + &b[1],
        a[2].clone() + &b[2],
    ]
}

pub(crate) fn scale_rat(a: &[BigRational; 3], s: &BigRational) -> [BigRational; 3] {
    [a[0].clone() * s, a[1].clone() * s, a[2].clone() * s]
}

pub(crate) fn dot_rat(a: &[BigRational; 3], b: &[BigRational; 3]) -> BigRational {
    a[0].clone() * &b[0] + a[1].clone() * &b[1] + a[2].clone() * &b[2]
}

pub(crate) fn cross_rat(a: &[BigRational; 3], b: &[BigRational; 3]) -> [BigRational; 3] {
    [
        a[1].clone() * &b[2] - a[2].clone() * &b[1],
        a[2].clone() * &b[0] - a[0].clone() * &b[2],
        a[0].clone() * &b[1] - a[1].clone() * &b[0],
    ]
}

pub(crate) fn det_rat(
    a: &[BigRational; 3],
    b: &[BigRational; 3],
    c: &[BigRational; 3],
) -> BigRational {
    dot_rat(a, &cross_rat(b, c))
}

fn plane_rat(r: [f64; 3], s: [f64; 3], t: [f64; 3]) -> ([BigRational; 3], BigRational) {
    let r = arr_rat(r);
    let s = arr_rat(s);
    let t = arr_rat(t);
    let n = cross_rat(&sub_rat(&s, &r), &sub_rat(&t, &r));
    let d = dot_rat(&n, &r);
    (n, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: [f64; 3] = [0.0, 0.0, 0.0];
    const X: [f64; 3] = [1.0, 0.0, 0.0];
    const Y: [f64; 3] = [0.0, 1.0, 0.0];

    #[test]
    fn explicit_to_rational_round_trips() {
        let p = Point3::Explicit([1.25, -3.5, 0.0]);
        let r = p.to_rational().unwrap();
        assert_eq!(r[0], f64_to_rat(1.25));
        assert_eq!(r[1], f64_to_rat(-3.5));
        assert_eq!(r[2], f64_to_rat(0.0));
    }

    #[test]
    fn lpi_lies_on_line_and_plane() {
        // Line along Z through (0.5,0.5,0); plane XY (z=0).
        let p = Point3::Lpi {
            q1: [0.5, 0.5, -1.0],
            q2: [0.5, 0.5, 1.0],
            r: ORIGIN,
            s: X,
            t: Y,
        };
        let c = p.to_rational().unwrap();
        assert_eq!(c[0], f64_to_rat(0.5));
        assert_eq!(c[1], f64_to_rat(0.5));
        assert!(c[2].is_zero());
    }

    #[test]
    fn ppi_is_intersection_of_three_planes() {
        // x=1, y=2, z=3 -> (1,2,3).
        let p = Point3::Ppi {
            r1: [1.0, 0.0, 0.0],
            s1: [1.0, 1.0, 0.0],
            t1: [1.0, 0.0, 1.0],
            r2: [0.0, 2.0, 0.0],
            s2: [1.0, 2.0, 0.0],
            t2: [0.0, 2.0, 1.0],
            r3: [0.0, 0.0, 3.0],
            s3: [1.0, 0.0, 3.0],
            t3: [0.0, 1.0, 3.0],
        };
        let c = p.to_rational().unwrap();
        assert_eq!(c[0], f64_to_rat(1.0));
        assert_eq!(c[1], f64_to_rat(2.0));
        assert_eq!(c[2], f64_to_rat(3.0));
    }

    #[test]
    fn parallel_lpi_returns_none() {
        let p = Point3::Lpi {
            q1: [0.0, 0.0, 1.0],
            q2: [1.0, 0.0, 1.0],
            r: ORIGIN,
            s: X,
            t: Y,
        };
        assert!(p.to_rational().is_none());
    }
}
