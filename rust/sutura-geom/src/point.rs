//! Indirect point representations for exact geometric predicates.
//!
//! An indirect point is stored as the tuple of primitives that define it,
//! together with the construction recipe (line-plane intersection or
//! plane-plane-plane intersection).  Coordinates are never stored as rounded
//! `f64` values inside the predicate core; instead each point has a
//! homogeneous representation `(λ, d)` such that `p = λ / d`, and predicates
//! are evaluated by clearing denominators.

use crate::{profile_count, profile_time};
use num_bigint::BigInt;
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
            // The implicit constructions run on integers: every input
            // coordinate is `X * 2^e` for one common exponent `e`, the
            // polynomial formulas are evaluated on the `X` without any
            // intermediate normalisation, and each coordinate is reduced
            // once at the end.  The value (and therefore the normalised
            // `BigRational`) is the same as evaluating the formulas in
            // rational arithmetic; `tests::integer_constructions_match_rational`
            // checks this against `to_rational_reference`.
            let result = match *self {
                Point3::Explicit(p) => Some(arr_rat(p)),
                Point3::Lpi { q1, q2, r, s, t } => {
                    let (x, e) = dyadic_ints(&[q1, q2, r, s, t]);
                    let (q1, q2, r, s, t) = (&x[0], &x[1], &x[2], &x[3], &x[4]);
                    let sr = sub_int(s, r);
                    let tr = sub_int(t, r);
                    // d scales with 2^(3e), λ = q1*d + (q2 - q1)*nq with
                    // 2^(4e), so p = λ / d carries 2^e.
                    let d = det_int(&sub_int(q1, q2), &sr, &tr);
                    if d.is_zero() {
                        None
                    } else {
                        let nq = det_int(&sub_int(q1, r), &sr, &tr);
                        let dq = sub_int(q2, q1);
                        Some(std::array::from_fn(|k| {
                            scaled_ratio(&q1[k] * &d + &dq[k] * &nq, &d, e)
                        }))
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
                    let (x, e) = dyadic_ints(&[r1, s1, t1, r2, s2, t2, r3, s3, t3]);
                    // Normals scale with 2^(2e), plane offsets with 2^(3e).
                    let (n1, d1) = plane_int(&x[0], &x[1], &x[2]);
                    let (n2, d2) = plane_int(&x[3], &x[4], &x[5]);
                    let (n3, d3) = plane_int(&x[6], &x[7], &x[8]);

                    let n2xn3 = cross_int(&n2, &n3);
                    let n3xn1 = cross_int(&n3, &n1);
                    let n1xn2 = cross_int(&n1, &n2);

                    // d scales with 2^(6e), the numerator with 2^(7e).
                    let d = dot_int(&n1, &n2xn3);
                    if d.is_zero() {
                        None
                    } else {
                        Some(std::array::from_fn(|k| {
                            let num = &n2xn3[k] * &d1 + &n3xn1[k] * &d2 + &n1xn2[k] * &d3;
                            scaled_ratio(num, &d, e)
                        }))
                    }
                }
            };
            result
        })
    }

    /// Textbook `BigRational` evaluation of the constructions, kept as the
    /// reference for the integer path in [`Point3::to_rational`].
    #[cfg(test)]
    pub(crate) fn to_rational_reference(&self) -> Option<[BigRational; 3]> {
        match *self {
            Point3::Explicit(p) => Some(arr_rat(p)),
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
        }
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

impl Point3 {
    /// Interval enclosure of the homogeneous representation `(λ, d)` with
    /// `p = λ / d`, built from the SAME polynomial formulas as
    /// [`Point3::homogeneous`].  Used only by the predicate filters.
    pub fn homogeneous_iv(&self) -> ([crate::interval::Iv; 3], crate::interval::Iv) {
        use crate::interval::*;
        match *self {
            Point3::Explicit(p) => (exact3(p), Iv::exact(1.0)),
            Point3::Lpi { q1, q2, r, s, t } => {
                let (q1, q2, r, s, t) = (exact3(q1), exact3(q2), exact3(r), exact3(s), exact3(t));
                let sr = sub3(&s, &r);
                let tr = sub3(&t, &r);
                let d = det3(&sub3(&q1, &q2), &sr, &tr);
                let nq = det3(&sub3(&q1, &r), &sr, &tr);
                let lambda = add3(&scale3(&q1, d), &scale3(&sub3(&q2, &q1), nq));
                (lambda, d)
            }
            Point3::Ppi { r1, s1, t1, r2, s2, t2, r3, s3, t3 } => {
                let plane = |r: [f64; 3], s: [f64; 3], t: [f64; 3]| {
                    let (r, s, t) = (exact3(r), exact3(s), exact3(t));
                    let n = cross3(&sub3(&s, &r), &sub3(&t, &r));
                    let d = dot3(&n, &r);
                    (n, d)
                };
                let (n1, d1) = plane(r1, s1, t1);
                let (n2, d2) = plane(r2, s2, t2);
                let (n3, d3) = plane(r3, s3, t3);
                let n2xn3 = cross3(&n2, &n3);
                let n3xn1 = cross3(&n3, &n1);
                let n1xn2 = cross3(&n1, &n2);
                let d = dot3(&n1, &n2xn3);
                let lambda = add3(
                    &add3(&scale3(&n2xn3, d1), &scale3(&n3xn1, d2)),
                    &scale3(&n1xn2, d3),
                );
                (lambda, d)
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

/// Hash-map key for a normalised `BigRational`.
///
/// `num-rational` hashes a ratio through its continued-fraction expansion (a
/// chain of `BigInt` floor divisions) and compares two ratios through `cmp`,
/// so that non-reduced ratios agree with `Eq`.  Every rational in this crate
/// is built with `Ratio::new`, `from_f64` or ratio arithmetic, all of which
/// keep the value reduced with a positive denominator; for such values
/// equality is exactly equality of `(numer, denom)`, which this key hashes
/// and compares directly.  Lookups therefore find the same entries as with
/// the plain `BigRational` key.
#[derive(Clone, Debug)]
pub(crate) struct RatKey(pub(crate) BigRational);

impl RatKey {
    #[inline]
    pub(crate) fn new(r: BigRational) -> Self {
        debug_assert!(
            {
                let reduced = BigRational::new(r.numer().clone(), r.denom().clone());
                reduced.numer() == r.numer() && reduced.denom() == r.denom()
            },
            "RatKey requires a normalised rational"
        );
        RatKey(r)
    }
}

impl PartialEq for RatKey {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.0.numer() == other.0.numer() && self.0.denom() == other.0.denom()
    }
}

impl Eq for RatKey {}

impl std::hash::Hash for RatKey {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.numer().hash(state);
        self.0.denom().hash(state);
    }
}

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

// ---------------------------------------------------------------------------
// Integer helpers for the dyadic construction path
// ---------------------------------------------------------------------------

/// Split a finite `f64` into `(m, e)` with `x = m * 2^e` and `m` odd (or
/// `(0, 0)` for zero).
fn decode_f64(x: f64) -> (i64, i64) {
    assert!(x.is_finite(), "coordinate must be finite");
    if x == 0.0 {
        return (0, 0);
    }
    let bits = x.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i64;
    let frac = bits & ((1u64 << 52) - 1);
    let (m, e) = if biased == 0 {
        (frac, -1074)
    } else {
        (frac | (1u64 << 52), biased - 1075)
    };
    let tz = m.trailing_zeros();
    let m = (m >> tz) as i64;
    (if x < 0.0 { -m } else { m }, e + tz as i64)
}

/// Integer coordinates `X` and one common exponent `e` such that every input
/// coordinate equals `X * 2^e` exactly.
fn dyadic_ints(pts: &[[f64; 3]]) -> (Vec<[BigInt; 3]>, i64) {
    let dec: Vec<[(i64, i64); 3]> = pts
        .iter()
        .map(|p| [decode_f64(p[0]), decode_f64(p[1]), decode_f64(p[2])])
        .collect();
    let e = dec
        .iter()
        .flatten()
        .filter(|(m, _)| *m != 0)
        .map(|&(_, e)| e)
        .min()
        .unwrap_or(0);
    let ints = dec
        .iter()
        .map(|c| {
            std::array::from_fn(|k| {
                let (m, ek) = c[k];
                if m == 0 {
                    BigInt::zero()
                } else {
                    BigInt::from(m) << ((ek - e) as usize)
                }
            })
        })
        .collect();
    (ints, e)
}

/// The normalised rational `num / den * 2^e` (`den` non-zero).
fn scaled_ratio(num: BigInt, den: &BigInt, e: i64) -> BigRational {
    if e >= 0 {
        BigRational::new(num << (e as usize), den.clone())
    } else {
        BigRational::new(num, den << ((-e) as usize))
    }
}

fn sub_int(a: &[BigInt; 3], b: &[BigInt; 3]) -> [BigInt; 3] {
    [&a[0] - &b[0], &a[1] - &b[1], &a[2] - &b[2]]
}

fn dot_int(a: &[BigInt; 3], b: &[BigInt; 3]) -> BigInt {
    &a[0] * &b[0] + &a[1] * &b[1] + &a[2] * &b[2]
}

fn cross_int(a: &[BigInt; 3], b: &[BigInt; 3]) -> [BigInt; 3] {
    [
        &a[1] * &b[2] - &a[2] * &b[1],
        &a[2] * &b[0] - &a[0] * &b[2],
        &a[0] * &b[1] - &a[1] * &b[0],
    ]
}

fn det_int(a: &[BigInt; 3], b: &[BigInt; 3], c: &[BigInt; 3]) -> BigInt {
    dot_int(a, &cross_int(b, c))
}

fn plane_int(r: &[BigInt; 3], s: &[BigInt; 3], t: &[BigInt; 3]) -> ([BigInt; 3], BigInt) {
    let n = cross_int(&sub_int(s, r), &sub_int(t, r));
    let d = dot_int(&n, r);
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

    /// The integer construction path must return exactly the rational
    /// formula value (same normalised `BigRational`), including mixed
    /// magnitudes, negative and subnormal coordinates and degenerate cases.
    #[test]
    fn integer_constructions_match_rational() {
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let coord = |next: &mut dyn FnMut() -> u64| -> f64 {
            let r = next();
            match r % 7 {
                0 => 0.0,
                1 => ((r >> 8) % 9) as f64 - 4.0,
                // Subnormal, or any exponent within roughly 2^-200..2^200.
                2 if r & 2 == 0 => f64::from_bits(r >> 12) * if r & 1 == 0 { 1.0 } else { -1.0 },
                2 => f64::from_bits(((823 + (r >> 56) % 400) << 52) | (r >> 12)),
                3 => ((r >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 1e-6,
                4 => ((r >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 1e6,
                _ => (r >> 11) as f64 / (1u64 << 53) as f64 - 0.5,
            }
        };
        let pt = |next: &mut dyn FnMut() -> u64| -> [f64; 3] {
            let mut p = [0.0; 3];
            for c in p.iter_mut() {
                *c = coord(next);
            }
            p
        };
        let mut checked = 0;
        for i in 0..4000 {
            let p = if i % 2 == 0 {
                let q1 = pt(&mut next);
                let q2 = if i % 10 == 0 { q1 } else { pt(&mut next) };
                Point3::Lpi { q1, q2, r: pt(&mut next), s: pt(&mut next), t: pt(&mut next) }
            } else {
                let r1 = pt(&mut next);
                let s1 = pt(&mut next);
                let t1 = pt(&mut next);
                let parallel = i % 11 == 1;
                Point3::Ppi {
                    r1,
                    s1,
                    t1,
                    r2: if parallel { r1 } else { pt(&mut next) },
                    s2: if parallel { s1 } else { pt(&mut next) },
                    t2: if parallel { t1 } else { pt(&mut next) },
                    r3: pt(&mut next),
                    s3: pt(&mut next),
                    t3: pt(&mut next),
                }
            };
            let fast = p.to_rational();
            let slow = p.to_rational_reference();
            assert_eq!(fast, slow, "construction mismatch for {:?}", p);
            if let Some(v) = fast {
                for c in &v {
                    let reduced = BigRational::new(c.numer().clone(), c.denom().clone());
                    assert!(reduced.numer() == c.numer() && reduced.denom() == c.denom());
                }
                checked += 1;
            }
        }
        assert!(checked > 3000);
    }

}
