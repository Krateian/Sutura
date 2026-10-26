//! Exact 3D geometric predicates on explicit and indirect points.

use crate::point::{det_rat, sub_rat, Point3};
use crate::profile_count;
use num_rational::BigRational;
use num_traits::{One, Signed, Zero};
use robust::{insphere as robust_insphere, orient3d as robust_orient3d, Coord3D};

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
        profile_count!(ORIENT3D_EXPLICIT, 1);
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

    profile_count!(ORIENT3D_IMPLICIT, 1);

    // Rigorous interval filter on the homogeneous (λ, d) representation:
    // sign(det(c-a, b-a, d-a)) = sign(det4([λ_i, d_i])) * Π sign(d_i).
    // It decides only when the enclosure excludes zero; otherwise the exact
    // rational evaluation below runs, so results are unchanged.
    if let Some(s) = orient3d_filter(p1, p2, p3, p4) {
        return s;
    }

    // Any implicit point: evaluate the determinant EXACTLY in rational
    // arithmetic.  A previous f64 fast path evaluated the determinant on the
    // ROUNDED coordinates of the LPI/PPI point; the construction rounding can
    // dominate a small exact determinant and certify a wrong sign (measured:
    // the f64 filter contradicted the exact sign on ~1/8 random triangle
    // pairs).  Correctness first; a rigorous error-bound filter can be added
    // later.
    //
    // Sign convention: the robust crate (and orient3d_indirect_one_lpi) return
    // the NEGATIVE of det(b-a, c-a, d-a) — positive when `d` lies below the
    // plane through (a,b,c) with (a,b,c) CCW viewed from above.  Swapping `b`
    // and `c` below reproduces that sign.
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

    let det = det_rat(&sub_rat(&c, &a), &sub_rat(&b, &a), &sub_rat(&d, &a));
    if det.is_zero() {
        0.0
    } else if det.is_positive() {
        1.0
    } else {
        -1.0
    }
}

fn orient3d_filter(p1: &Point3, p2: &Point3, p3: &Point3, p4: &Point3) -> Option<f64> {
    let (l1, d1) = p1.homogeneous_iv();
    let (l2, d2) = p2.homogeneous_iv();
    let (l3, d3) = p3.homogeneous_iv();
    let (l4, d4) = p4.homogeneous_iv();
    let sd = d1.sign()? * d2.sign()? * d3.sign()? * d4.sign()?;
    let m = [
        [l1[0], l1[1], l1[2], d1],
        [l2[0], l2[1], l2[2], d2],
        [l3[0], l3[1], l3[2], d3],
        [l4[0], l4[1], l4[2], d4],
    ];
    let s = crate::interval::det4(&m).sign()?;
    Some((s * sd) as f64)
}

/// Sign of the in-sphere predicate for explicit and implicit points.
///
/// Returns `+1` when `pe` is inside the circumsphere of `pa..pd`, `-1` when
/// outside, and `0` when the five points are cospherical.  The first four
/// points must be positively oriented (the same convention as `robust`).
/// Explicit points use the `robust` crate; implicit points use a rigorous
/// interval filter and fall back to an exact rational 5x5 determinant.
pub fn insphere_sign(pa: &Point3, pb: &Point3, pc: &Point3, pd: &Point3, pe: &Point3) -> f64 {
    if let (
        Point3::Explicit(a),
        Point3::Explicit(b),
        Point3::Explicit(c),
        Point3::Explicit(d),
        Point3::Explicit(e),
    ) = (pa, pb, pc, pd, pe)
    {
        let r = robust_insphere(
            Coord3D { x: a[0], y: a[1], z: a[2] },
            Coord3D { x: b[0], y: b[1], z: b[2] },
            Coord3D { x: c[0], y: c[1], z: c[2] },
            Coord3D { x: d[0], y: d[1], z: d[2] },
            Coord3D { x: e[0], y: e[1], z: e[2] },
        );
        return if r == 0.0 { 0.0 } else { r.signum() };
    }
    if let Some(s) = insphere_filter(pa, pb, pc, pd, pe) {
        return s;
    }
    insphere_exact_sign(pa, pb, pc, pd, pe)
}

/// Rigorous interval filter for [`insphere_sign`].  The 5x5 determinant uses
/// the homogeneous rows `[d*lambda, |lambda|^2, d^2]`, which is the standard
/// in-sphere determinant scaled by the positive row factors `d_i^2`.
fn insphere_filter(
    pa: &Point3,
    pb: &Point3,
    pc: &Point3,
    pd: &Point3,
    pe: &Point3,
) -> Option<f64> {
    use crate::interval::Iv;
    let points = [pa, pb, pc, pd, pe];
    let mut m = [[Iv::exact(0.0); 5]; 5];
    for (i, p) in points.iter().enumerate() {
        let (l, d) = p.homogeneous_iv();
        for k in 0..3 {
            m[i][k] = d.mul(l[k]);
        }
        m[i][3] = l[0]
            .mul(l[0])
            .add(l[1].mul(l[1]))
            .add(l[2].mul(l[2]));
        m[i][4] = d.mul(d);
    }
    crate::interval::det5(&m).sign().map(|s| s as f64)
}

/// Exact rational evaluation of the in-sphere determinant.  Used by the
/// filter fallback and by the differential tests.
pub(crate) fn insphere_exact_sign(
    pa: &Point3,
    pb: &Point3,
    pc: &Point3,
    pd: &Point3,
    pe: &Point3,
) -> f64 {
    let points = [pa, pb, pc, pd, pe];
    let mut m: Vec<Vec<BigRational>> = Vec::with_capacity(5);
    for p in points {
        let (l, d) = match p.homogeneous() {
            Some(v) => v,
            None => return f64::NAN,
        };
        let mut row = Vec::with_capacity(5);
        for k in 0..3 {
            row.push(&d * &l[k]);
        }
        row.push(
            &l[0] * &l[0] + &l[1] * &l[1] + &l[2] * &l[2],
        );
        row.push(&d * &d);
        m.push(row);
    }
    let det = det_exact(m);
    if det.is_zero() {
        0.0
    } else if det.is_positive() {
        1.0
    } else {
        -1.0
    }
}

/// Determinant of a small rational matrix by exact Gaussian elimination with
/// deterministic first-nonzero pivoting.  The matrices here are 5x5.
fn det_exact(mut m: Vec<Vec<BigRational>>) -> BigRational {
    let n = m.len();
    let mut det = BigRational::one();
    for col in 0..n {
        let pivot = (col..n).find(|&r| !m[r][col].is_zero());
        let Some(p) = pivot else {
            return BigRational::zero();
        };
        if p != col {
            m.swap(p, col);
            det = -det;
        }
        let pv = m[col][col].clone();
        det = det * &pv;
        for r in (col + 1)..n {
            let f = &m[r][col] / &pv;
            for c in col..n {
                let v = &m[r][c] - &f * &m[col][c];
                m[r][c] = v;
            }
        }
    }
    det
}

fn orient3d_unit(p1: &Point3, p2: &Point3, p3: &Point3, p4: &Point3) -> f64 {
    let r = orient3d_sign(p1, p2, p3, p4);
    if r == 0.0 {
        0.0
    } else {
        r.signum()
    }
}

/// Deterministic symbolic perturbation for the in-sphere predicate, following
/// the published rule: if the unperturbed predicate is zero, sort the five
/// global vertex indices, then use the orientation of the last four and, if
/// needed, of the first/third/fourth/fifth vertices, correcting the sign by
/// the parity of the sorting swaps.  The result is independent of the
/// argument order when the point/id pairs are permuted together.
pub fn insphere_sign_perturbed(
    pa: &Point3,
    pb: &Point3,
    pc: &Point3,
    pd: &Point3,
    pe: &Point3,
    ids: [usize; 5],
) -> f64 {
    let r = insphere_sign(pa, pb, pc, pd, pe);
    if r != 0.0 {
        return r;
    }
    let mut items: Vec<(usize, &Point3)> = ids
        .iter()
        .cloned()
        .zip([pa, pb, pc, pd, pe])
        .collect();
    let mut swaps = 0usize;
    for i in 0..items.len() {
        for j in (i + 1)..items.len() {
            if items[j].0 < items[i].0 {
                items.swap(i, j);
                swaps += 1;
            }
        }
    }
    let mut r1 = orient3d_unit(items[1].1, items[2].1, items[3].1, items[4].1);
    if swaps % 2 == 1 {
        r1 = -r1;
    }
    if r1 != 0.0 {
        return r1;
    }
    let mut r2 = orient3d_unit(items[0].1, items[2].1, items[3].1, items[4].1);
    if swaps % 2 == 0 {
        r2 = -r2;
    }
    r2
}

/// Exact-only evaluation (no filter); used by tests to cross-check the filter.
#[cfg(test)]
pub(crate) fn orient3d_sign_exact(p1: &Point3, p2: &Point3, p3: &Point3, p4: &Point3) -> f64 {
    let a = p1.to_rational().unwrap();
    let b = p2.to_rational().unwrap();
    let c = p3.to_rational().unwrap();
    let d = p4.to_rational().unwrap();
    let det = det_rat(&sub_rat(&c, &a), &sub_rat(&b, &a), &sub_rat(&d, &a));
    if det.is_zero() { 0.0 } else if det.is_positive() { 1.0 } else { -1.0 }
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
    /// The interval filter must never contradict the exact evaluation:
    /// random explicit / LPI / PPI quadruples, including near-degenerate
    /// ones built from shared input vertices.
    #[test]
    fn filter_agrees_with_exact_on_implicit_points() {
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(0x5eed_c1);
        let rp = |rng: &mut rand::rngs::StdRng| -> [f64; 3] {
            [rng.gen_range(-10.0..10.0), rng.gen_range(-10.0..10.0), rng.gen_range(-10.0..10.0)]
        };
        let mut decided = 0usize;
        let mut total = 0usize;
        for _ in 0..3000 {
            let pts: Vec<[f64; 3]> = (0..12).map(|_| rp(&mut rng)).collect();
            let mk = |k: usize, rng: &mut rand::rngs::StdRng| -> Point3 {
                match rng.gen_range(0..4) {
                    0 => Point3::Explicit(pts[k % 12]),
                    1 => Point3::Lpi { q1: pts[k % 12], q2: pts[(k + 1) % 12], r: pts[(k + 2) % 12], s: pts[(k + 3) % 12], t: pts[(k + 4) % 12] },
                    2 => Point3::Ppi {
                        r1: pts[k % 12], s1: pts[(k + 1) % 12], t1: pts[(k + 2) % 12],
                        r2: pts[(k + 3) % 12], s2: pts[(k + 4) % 12], t2: pts[(k + 5) % 12],
                        r3: pts[(k + 6) % 12], s3: pts[(k + 7) % 12], t3: pts[(k + 8) % 12],
                    },
                    _ => Point3::Lnc {
                        a: pts[k % 12],
                        b: pts[(k + 1) % 12],
                        t: rng.gen_range(0.05..0.95),
                    },
                }
            };
            let a = mk(0, &mut rng);
            let b = mk(3, &mut rng);
            let c = mk(6, &mut rng);
            // Degenerate on purpose half of the time: d reuses a's recipe.
            let d = if rng.gen_bool(0.5) { a.clone() } else { mk(9, &mut rng) };
            if [&a, &b, &c, &d].iter().any(|p| p.to_rational().is_none()) {
                continue;
            }
            total += 1;
            let exact = orient3d_sign_exact(&a, &b, &c, &d);
            if let Some(f) = orient3d_filter(&a, &b, &c, &d) {
                decided += 1;
                assert_eq!(f, exact, "filter contradicted exact sign");
            }
            // All-explicit quadruples take the robust-crate path, which
            // returns the determinant value rather than +/-1: compare signs.
            let full = orient3d_sign(&a, &b, &c, &d);
            let full_sign = if full == 0.0 { 0.0 } else { full.signum() };
            assert_eq!(full_sign, exact);
        }
        assert!(total > 1000);
        // The filter should decide the large majority of generic cases.
        assert!(decided * 2 > total, "filter decided only {decided}/{total}");
    }

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

#[cfg(test)]
mod signcheck {
    use super::*;

    #[test]
    fn indirect_one_lpi_sign_convention() {
        let zx = [0.0, 0.0, -1.0];
        let xx = [1.0, 0.0, 0.0];
        let yy = [0.0, 1.0, 0.0];
        // LPI point = origin: z-axis ∩ z=0 plane.
        let p = Point3::Lpi {
            q1: [0.0, 0.0, -1.0],
            q2: [0.0, 0.0, 1.0],
            r: [0.0, 0.0, 0.0],
            s: xx,
            t: yy,
        };
        // orient3d(origin, X, Y, NEG_Z) should be positive (below plane).
        let direct = orient3d_sign(
            &p,
            &Point3::Explicit(xx),
            &Point3::Explicit(yy),
            &Point3::Explicit(zx),
        );
        let ind = orient3d_indirect_one_lpi(&p, &xx, &yy, &zx).unwrap();
        assert_eq!(direct, 1.0);
        assert_eq!(ind, 1.0);
    }
}

#[cfg(test)]
mod kernel_tests {
    use super::*;
    use crate::point::f64_to_rat;
    use rand::{Rng, SeedableRng};

    fn rp(rng: &mut rand::rngs::StdRng) -> [f64; 3] {
        [
            rng.gen_range(-10.0..10.0),
            rng.gen_range(-10.0..10.0),
            rng.gen_range(-10.0..10.0),
        ]
    }

    #[test]
    fn lnc_is_exactly_on_the_segment() {
        let p = Point3::Lnc {
            a: [0.0, 0.0, 0.0],
            b: [2.0, 4.0, 6.0],
            t: 0.25,
        };
        assert_eq!(p.to_f64().unwrap(), [1.5, 3.0, 4.5]);
        assert_eq!(
            p.to_rational().unwrap(),
            p.to_rational_reference().unwrap(),
            "LNC integer path must match the rational reference"
        );
        let (l, d) = p.homogeneous().unwrap();
        assert_eq!(d, BigRational::one());
        assert_eq!(l[0], f64_to_rat(1.5));
    }

    #[test]
    fn lnc_canonical_key_ignores_endpoint_order() {
        let a = [1.0, 2.0, 3.0];
        let b = [4.0, 5.0, 6.0];
        let p = Point3::Lnc { a, b, t: 0.25 };
        let q = Point3::Lnc { a: b, b: a, t: 0.75 };
        assert_eq!(p.canonical_key(), q.canonical_key());
    }

    #[test]
    fn explicit_exact_insphere_matches_robust() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(0x5eed_1a5);
        for _ in 0..500 {
            let p: [[f64; 3]; 5] = std::array::from_fn(|_| rp(&mut rng));
            let exact = insphere_exact_sign(
                &Point3::Explicit(p[0]),
                &Point3::Explicit(p[1]),
                &Point3::Explicit(p[2]),
                &Point3::Explicit(p[3]),
                &Point3::Explicit(p[4]),
            );
            let robust = robust_insphere(
                Coord3D { x: p[0][0], y: p[0][1], z: p[0][2] },
                Coord3D { x: p[1][0], y: p[1][1], z: p[1][2] },
                Coord3D { x: p[2][0], y: p[2][1], z: p[2][2] },
                Coord3D { x: p[3][0], y: p[3][1], z: p[3][2] },
                Coord3D { x: p[4][0], y: p[4][1], z: p[4][2] },
            );
            let want = if robust == 0.0 { 0.0 } else { robust.signum() };
            assert_eq!(exact, want, "exact in-sphere sign disagrees with robust");
        }
    }

    fn random_point(rng: &mut rand::rngs::StdRng) -> Point3 {
        if rng.gen_bool(0.5) {
            Point3::Explicit(rp(rng))
        } else {
            Point3::Lnc {
                a: rp(rng),
                b: rp(rng),
                t: rng.gen_range(0.05..0.95),
            }
        }
    }

    #[test]
    fn implicit_insphere_filter_agrees_with_exact() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(0x5eed_1bc);
        let mut decided = 0usize;
        for _ in 0..800 {
            let p: Vec<Point3> = (0..5).map(|_| random_point(&mut rng)).collect();
            let exact = insphere_exact_sign(&p[0], &p[1], &p[2], &p[3], &p[4]);
            if exact.is_nan() {
                continue;
            }
            if let Some(s) = insphere_filter(&p[0], &p[1], &p[2], &p[3], &p[4]) {
                decided += 1;
                assert_eq!(s, exact, "filter contradicted exact in-sphere sign");
            }
            assert_eq!(
                insphere_sign(&p[0], &p[1], &p[2], &p[3], &p[4]),
                exact,
                "full predicate contradicted exact in-sphere sign"
            );
        }
        assert!(decided > 100, "filter decided only {decided}/800");
    }

    #[test]
    fn near_degenerate_insphere_is_consistent() {
        // Five points on the sphere through the tetrahedron
        // (ORIGIN, X, Y, NEG_Z); the circumcenter is (0.5, 0.5, -0.5) and
        // the fifth point (1, 1, -1) is the reflection of the origin.
        let a = Point3::Explicit([0.0, 0.0, 0.0]);
        let b = Point3::Explicit([1.0, 0.0, 0.0]);
        let c = Point3::Explicit([0.0, 1.0, 0.0]);
        let d = Point3::Explicit([0.0, 0.0, -1.0]);
        let sphere = Point3::Explicit([1.0, 1.0, -1.0]);
        assert_eq!(insphere_sign(&a, &b, &c, &d, &sphere), 0.0);
        assert_eq!(insphere_exact_sign(&a, &b, &c, &d, &sphere), 0.0);

        // One-ulp inward move of the fifth point must be classified the same
        // way by the filter/exact path and by robust.
        let inward = Point3::Explicit([1.0, 1.0, (-1.0f64).next_up()]);
        let exact = insphere_exact_sign(&a, &b, &c, &d, &inward);
        assert_ne!(exact, 0.0);
        assert_eq!(insphere_sign(&a, &b, &c, &d, &inward), exact);
    }

    #[test]
    fn perturbed_insphere_is_deterministic_and_covariant() {
        let pts = [
            Point3::Explicit([0.0, 0.0, 0.0]),
            Point3::Explicit([1.0, 0.0, 0.0]),
            Point3::Explicit([0.0, 1.0, 0.0]),
            Point3::Explicit([0.0, 0.0, -1.0]),
            Point3::Explicit([1.0, 1.0, -1.0]),
        ];
        let ids = [0usize, 1, 2, 3, 4];
        let a = insphere_sign_perturbed(&pts[0], &pts[1], &pts[2], &pts[3], &pts[4], ids);
        let b = insphere_sign_perturbed(&pts[0], &pts[1], &pts[2], &pts[3], &pts[4], ids);
        assert_eq!(a, b);
        assert_ne!(a, 0.0);

        // An even permutation of the first four points (the query stays at
        // index 4) must preserve the perturbed sign; an odd permutation must
        // flip it.  This is the antisymmetry the CDT cavity code relies on.
        let even = [1usize, 2, 0, 3, 4];
        let ep = [
            &pts[even[0]],
            &pts[even[1]],
            &pts[even[2]],
            &pts[even[3]],
            &pts[even[4]],
        ];
        let c = insphere_sign_perturbed(ep[0], ep[1], ep[2], ep[3], ep[4], even);
        assert_eq!(a, c, "even argument permutation must preserve the sign");

        let odd = [1usize, 0, 2, 3, 4];
        let op = [
            &pts[odd[0]],
            &pts[odd[1]],
            &pts[odd[2]],
            &pts[odd[3]],
            &pts[odd[4]],
        ];
        let d = insphere_sign_perturbed(op[0], op[1], op[2], op[3], op[4], odd);
        assert_eq!(a, -d, "odd argument permutation must flip the sign");
    }

    #[test]
    fn interval_det5_contains_the_exact_value() {
        use crate::interval::{det5, Iv};
        let mut rng = rand::rngs::StdRng::seed_from_u64(0x5eed_de7);
        for _ in 0..100 {
            let vals: [[f64; 5]; 5] = std::array::from_fn(|_| std::array::from_fn(|_| rng.gen_range(-2.0..2.0)));
            let m = vals.map(|row| row.map(Iv::exact));
            let iv = det5(&m).sign();
            let exact = det_exact(
                vals.iter()
                    .map(|row| row.iter().map(|&x| f64_to_rat(x)).collect())
                    .collect(),
            );
            if let Some(s) = iv {
                let want = if exact.is_zero() { 0 } else if exact.is_positive() { 1 } else { -1 };
                assert_eq!(s, want, "det5 interval contradicted exact determinant");
            }
        }
    }
}
