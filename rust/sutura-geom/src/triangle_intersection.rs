//! Exact triangle–triangle intersection classification.
//!
//! The classifier works from the six `orient3d` signs of the vertices of each
//! triangle against the plane of the other triangle.  Non-degenerate proper
//! intersections produce an implicit `LPI` segment.  Boundary contacts
//! (shared vertex/edge, vertex on plane, coplanar overlap) are classified
//! separately.

use crate::point::Point3;
use crate::predicates2d::orient2d_sign;
use crate::predicates3d::orient3d_sign;

/// Result of intersecting two triangles.
#[derive(Clone, Debug, PartialEq)]
pub enum TriangleIntersection {
    /// The triangles are separated.
    Disjoint,
    /// The interiors cross in a proper segment.  Endpoints are implicit LPI
    /// points (or explicit input vertices when the segment touches a vertex).
    ProperSegment { p: Point3, q: Point3 },
    /// The intersection degenerates to a single point (shared vertex or a
    /// vertex lying exactly on the other triangle).
    TouchPoint(Point3),
    /// The intersection is a non-zero segment lying on a triangle boundary
    /// (edge-through-plane or coplanar edge overlap).
    TouchSegment { p: Point3, q: Point3 },
    /// The two triangles are coplanar and their 2D projections overlap.
    /// The exact overlap geometry is intentionally not expanded here; the
    /// arrangement-lite layer handles coplanar faces as a later stage.
    CoplanarOverlap,
}

/// A triangle whose vertices are currently required to be explicit input
/// vertices.  The result of intersection is still expressed with implicit
/// points where appropriate.
#[derive(Clone, Debug)]
pub struct Triangle {
    pub a: Point3,
    pub b: Point3,
    pub c: Point3,
}

impl Triangle {
    pub fn explicit(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> Self {
        Self {
            a: Point3::Explicit(a),
            b: Point3::Explicit(b),
            c: Point3::Explicit(c),
        }
    }

    fn vertices(&self) -> [&Point3; 3] {
        [&self.a, &self.b, &self.c]
    }

    fn coords(&self) -> [[f64; 3]; 3] {
        [self.a.explicit(), self.b.explicit(), self.c.explicit()]
    }
}

impl Point3 {
    /// Expect this point to be explicit and return its coordinates.
    fn explicit(&self) -> [f64; 3] {
        match *self {
            Point3::Explicit(p) => p,
            _ => panic!("B2 classifier currently requires explicit triangle vertices"),
        }
    }

    /// A canonical key suitable for deduplicating implicit points that arise
    /// from the same geometric construction in different orderings.
    ///
    /// For an LPI point the line vertices and plane vertices are each sorted
    /// independently; for a PPI point each plane is sorted and the three
    /// planes are sorted.  Explicit points are keyed by their `f64` bit
    /// patterns.
    pub fn canonical_key(&self) -> Vec<u64> {
        fn key3(p: [f64; 3]) -> [u64; 3] {
            [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()]
        }
        fn sorted_plane(r: [f64; 3], s: [f64; 3], t: [f64; 3]) -> [u64; 3] {
            let mut k = [r, s, t].iter().flat_map(|p| key3(*p)).collect::<Vec<_>>();
            k.sort_unstable();
            let mut out = [0u64; 3];
            out.copy_from_slice(&k);
            out
        }

        match *self {
            Point3::Explicit(p) => {
                let mut k = key3(p).to_vec();
                k.push(0); // tag for explicit
                k
            }
            Point3::Lpi { q1, q2, r, s, t } => {
                let mut line = key3(q1).to_vec();
                line.extend_from_slice(&key3(q2));
                line.sort_unstable();
                let mut plane = [r, s, t].iter().flat_map(|p| key3(*p)).collect::<Vec<_>>();
                plane.sort_unstable();
                let mut out = vec![1]; // tag for LPI
                out.extend_from_slice(&line);
                out.extend_from_slice(&plane);
                out
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
                let mut planes = [
                    sorted_plane(r1, s1, t1),
                    sorted_plane(r2, s2, t2),
                    sorted_plane(r3, s3, t3),
                ];
                planes.sort_unstable();
                let mut out = vec![2]; // tag for PPI
                for p in &planes {
                    out.extend_from_slice(p);
                }
                out
            }
        }
    }
}

/// Classify the intersection of two triangles.
pub fn intersect_triangles(t1: &Triangle, t2: &Triangle) -> TriangleIntersection {
    let v1 = t1.vertices();
    let v2 = t2.vertices();

    // Signs of T2 vertices against plane of T1.
    let s = [
        orient3d_sign(v1[0], v1[1], v1[2], v2[0]),
        orient3d_sign(v1[0], v1[1], v1[2], v2[1]),
        orient3d_sign(v1[0], v1[1], v1[2], v2[2]),
    ];
    // Signs of T1 vertices against plane of T2.
    let t = [
        orient3d_sign(v2[0], v2[1], v2[2], v1[0]),
        orient3d_sign(v2[0], v2[1], v2[2], v1[1]),
        orient3d_sign(v2[0], v2[1], v2[2], v1[2]),
    ];

    intersect_from_signs(t1, t2, &s, &t)
}

fn intersect_from_signs(
    t1: &Triangle,
    t2: &Triangle,
    s: &[f64; 3],
    t: &[f64; 3],
) -> TriangleIntersection {
    // Fast disjoint tests.
    if all_same_nonzero(s) || all_same_nonzero(t) {
        return TriangleIntersection::Disjoint;
    }

    // Coplanar triangles: either all of T2 lies in plane T1, or all of T1
    // lies in plane T2.
    if s.iter().all(|&x| x == 0.0) || t.iter().all(|&x| x == 0.0) {
        return classify_coplanar(t1, t2);
    }

    let v1 = t1.vertices();
    let v2 = t2.vertices();

    // Collect candidate intersection points from edge-plane crossings.
    // `s` gives the signs of T2 vertices against plane T1, so it drives the
    // T2 edges; `t` gives the signs of T1 vertices against plane T2.
    let mut candidates: Vec<Point3> = Vec::with_capacity(8);
    collect_edge_candidates(&mut candidates, v1, v2, t);
    collect_edge_candidates(&mut candidates, v2, v1, s);

    // Deduplicate by exact rational geometry.  Two LPI constructions that
    // describe the same 3D point (e.g. an edge of T1 vs plane T2, and an
    // edge of T2 vs plane T1) must collapse to one vertex.
    // A candidate is a true intersection endpoint only if it lies inside
    // *both* triangles.  Edge-plane crossings that lie outside the other
    // triangle are discarded.
    let mut inside: Vec<Point3> = Vec::with_capacity(4);
    for p in candidates {
        if point_inside_triangle_3d(t1, &p) && point_inside_triangle_3d(t2, &p) {
            if !inside.iter().any(|q| points_are_equal(&p, q)) {
                inside.push(p);
            }
        }
    }

    match inside.len() {
        0 => TriangleIntersection::Disjoint,
        1 => TriangleIntersection::TouchPoint(inside.pop().unwrap()),
        2 => {
            let q = inside.pop().unwrap();
            let p = inside.pop().unwrap();
            let boundary = s.iter().any(|&x| x == 0.0) || t.iter().any(|&x| x == 0.0);
            if boundary {
                TriangleIntersection::TouchSegment { p, q }
            } else {
                TriangleIntersection::ProperSegment { p, q }
            }
        }
        _ => {
            // Degenerate edge overlap: keep the first two distinct points.
            let q = inside.pop().unwrap();
            let p = inside.pop().unwrap();
            TriangleIntersection::TouchSegment { p, q }
        }
    }
}

fn points_are_equal(p: &Point3, q: &Point3) -> bool {
    match (p.to_rational(), q.to_rational()) {
        (Some(a), Some(b)) => a == b,
        (None, None) => true,
        _ => false,
    }
}

fn all_same_nonzero(signs: &[f64; 3]) -> bool {
    if signs[0] == 0.0 {
        return false;
    }
    signs[1] != 0.0
        && signs[2] != 0.0
        && signs[0].signum() == signs[1].signum()
        && signs[0].signum() == signs[2].signum()
}

/// Given triangle `tri` and the signs of its vertices against `plane`,
/// append candidate intersection points.  `plane` is the *other* triangle.
fn collect_edge_candidates(
    out: &mut Vec<Point3>,
    tri: [&Point3; 3],
    plane: [&Point3; 3],
    signs: &[f64; 3],
) {
    let edges = [(0, 1), (1, 2), (2, 0)];
    for (i, j) in edges {
        let si = signs[i];
        let sj = signs[j];

        if si == 0.0 && sj == 0.0 {
            // Edge lies in the plane: add both endpoints.
            out.push(tri[i].clone());
            out.push(tri[j].clone());
        } else if si == 0.0 {
            out.push(tri[i].clone());
        } else if sj == 0.0 {
            out.push(tri[j].clone());
        } else if si * sj < 0.0 {
            // Proper interior crossing -> LPI.
            out.push(Point3::Lpi {
                q1: tri[i].explicit(),
                q2: tri[j].explicit(),
                r: plane[0].explicit(),
                s: plane[1].explicit(),
                t: plane[2].explicit(),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Coplanar 2D classification
// ---------------------------------------------------------------------------

fn classify_coplanar(t1: &Triangle, t2: &Triangle) -> TriangleIntersection {
    let c1 = t1.coords();
    let c2 = t2.coords();

    // Drop the coordinate along which the triangle normal has largest magnitude.
    let axis = dominant_axis(&c1);
    let p1 = project(c1, axis);
    let p2 = project(c2, axis);

    if triangles_overlap_2d(&p1, &p2) {
        TriangleIntersection::CoplanarOverlap
    } else {
        TriangleIntersection::Disjoint
    }
}

fn dominant_axis(tri: &[[f64; 3]; 3]) -> usize {
    let a = tri[0];
    let b = tri[1];
    let c = tri[2];
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let mut best = 0usize;
    for i in 1..3 {
        if n[i].abs() > n[best].abs() {
            best = i;
        }
    }
    best
}

fn project(tri: [[f64; 3]; 3], drop: usize) -> [[f64; 2]; 3] {
    let keep: [usize; 2] = match drop {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
    };
    [
        [tri[0][keep[0]], tri[0][keep[1]]],
        [tri[1][keep[0]], tri[1][keep[1]]],
        [tri[2][keep[0]], tri[2][keep[1]]],
    ]
}

fn triangles_overlap_2d(a: &[[f64; 2]; 3], b: &[[f64; 2]; 3]) -> bool {
    // Edge-edge intersection (including endpoints).
    let edges_a = [(0, 1), (1, 2), (2, 0)];
    let edges_b = [(0, 1), (1, 2), (2, 0)];
    for &(i1, j1) in &edges_a {
        for &(i2, j2) in &edges_b {
            if segments_intersect_2d(a[i1], a[j1], b[i2], b[j2]) {
                return true;
            }
        }
    }
    // No edge intersection: check containment.
    point_in_triangle_2d(a[0], a[1], a[2], b[0]) || point_in_triangle_2d(b[0], b[1], b[2], a[0])
}

fn segments_intersect_2d(p1: [f64; 2], p2: [f64; 2], q1: [f64; 2], q2: [f64; 2]) -> bool {
    let s1 = orient2d_sign(
        &crate::predicates2d::Point2::Explicit(p1),
        &crate::predicates2d::Point2::Explicit(p2),
        &crate::predicates2d::Point2::Explicit(q1),
    );
    let s2 = orient2d_sign(
        &crate::predicates2d::Point2::Explicit(p1),
        &crate::predicates2d::Point2::Explicit(p2),
        &crate::predicates2d::Point2::Explicit(q2),
    );
    let s3 = orient2d_sign(
        &crate::predicates2d::Point2::Explicit(q1),
        &crate::predicates2d::Point2::Explicit(q2),
        &crate::predicates2d::Point2::Explicit(p1),
    );
    let s4 = orient2d_sign(
        &crate::predicates2d::Point2::Explicit(q1),
        &crate::predicates2d::Point2::Explicit(q2),
        &crate::predicates2d::Point2::Explicit(p2),
    );

    let s1 = s1.signum() as i8;
    let s2 = s2.signum() as i8;
    let s3 = s3.signum() as i8;
    let s4 = s4.signum() as i8;

    if s1 == 0 && point_on_segment(q1, p1, p2) {
        return true;
    }
    if s2 == 0 && point_on_segment(q2, p1, p2) {
        return true;
    }
    if s3 == 0 && point_on_segment(p1, q1, q2) {
        return true;
    }
    if s4 == 0 && point_on_segment(p2, q1, q2) {
        return true;
    }

    s1 != s2 && s3 != s4
}

fn point_on_segment(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> bool {
    let minx = a[0].min(b[0]);
    let maxx = a[0].max(b[0]);
    let miny = a[1].min(b[1]);
    let maxy = a[1].max(b[1]);
    p[0] >= minx && p[0] <= maxx && p[1] >= miny && p[1] <= maxy
}

/// Test whether `p` (which is assumed to lie in the plane of `tri`) is
/// inside `tri` or on its boundary.  Uses three 3D orientation tests
/// against an out-of-plane point, which is equivalent to a 2D point-in-
/// triangle test in exact arithmetic.
fn point_inside_triangle_3d(tri: &Triangle, p: &Point3) -> bool {
    let c = tri.coords();
    let a = c[0];
    let b = c[1];
    let cc = c[2];

    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [cc[0] - a[0], cc[1] - a[1], cc[2] - a[2]];
    let mut n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let norm = n[0].abs() + n[1].abs() + n[2].abs();
    if norm < 1e-300 {
        return false;
    }
    // Scale the normal so the query point is well off the plane.
    let scale = 1000.0 / norm;
    n[0] *= scale;
    n[1] *= scale;
    n[2] *= scale;

    // Pick an out-of-plane query point.  In the extremely unlikely event
    // that it is coplanar, perturb it and try again.
    let mut offset = 0.0;
    loop {
        let q = [a[0] + n[0] + offset, a[1] + n[1], a[2] + n[2]];

        let ref_ab = orient3d_sign(
            &Point3::Explicit(a),
            &Point3::Explicit(b),
            &Point3::Explicit(cc),
            &Point3::Explicit(q),
        );
        let ref_bc = orient3d_sign(
            &Point3::Explicit(b),
            &Point3::Explicit(cc),
            &Point3::Explicit(a),
            &Point3::Explicit(q),
        );
        let ref_ca = orient3d_sign(
            &Point3::Explicit(cc),
            &Point3::Explicit(a),
            &Point3::Explicit(b),
            &Point3::Explicit(q),
        );

        if ref_ab == 0.0 || ref_bc == 0.0 || ref_ca == 0.0 {
            offset += 1.0;
            continue;
        }

        let s_ab = orient3d_sign(
            &Point3::Explicit(a),
            &Point3::Explicit(b),
            p,
            &Point3::Explicit(q),
        );
        let s_bc = orient3d_sign(
            &Point3::Explicit(b),
            &Point3::Explicit(cc),
            p,
            &Point3::Explicit(q),
        );
        let s_ca = orient3d_sign(
            &Point3::Explicit(cc),
            &Point3::Explicit(a),
            p,
            &Point3::Explicit(q),
        );

        return same_side_or_zero(s_ab, ref_ab)
            && same_side_or_zero(s_bc, ref_bc)
            && same_side_or_zero(s_ca, ref_ca);
    }
}

fn same_side_or_zero(test: f64, reference: f64) -> bool {
    if test == 0.0 {
        return true;
    }
    test.signum() == reference.signum()
}

fn point_in_triangle_2d(a: [f64; 2], b: [f64; 2], c: [f64; 2], p: [f64; 2]) -> bool {
    let s1 = orient2d_sign(
        &crate::predicates2d::Point2::Explicit(a),
        &crate::predicates2d::Point2::Explicit(b),
        &crate::predicates2d::Point2::Explicit(p),
    );
    let s2 = orient2d_sign(
        &crate::predicates2d::Point2::Explicit(b),
        &crate::predicates2d::Point2::Explicit(c),
        &crate::predicates2d::Point2::Explicit(p),
    );
    let s3 = orient2d_sign(
        &crate::predicates2d::Point2::Explicit(c),
        &crate::predicates2d::Point2::Explicit(a),
        &crate::predicates2d::Point2::Explicit(p),
    );
    let s1 = s1.signum() as i8;
    let s2 = s2.signum() as i8;
    let s3 = s3.signum() as i8;
    (s1 >= 0 && s2 >= 0 && s3 >= 0) || (s1 <= 0 && s2 <= 0 && s3 <= 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_traits::Zero;
    use rand::Rng;

    #[test]
    fn disjoint_triangles() {
        let t1 = Triangle::explicit([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let t2 = Triangle::explicit([0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [0.0, 1.0, 1.0]);
        assert_eq!(
            intersect_triangles(&t1, &t2),
            TriangleIntersection::Disjoint
        );
    }

    #[test]
    fn proper_intersection() {
        // T1 lies in the XY plane.  T2 is a vertical triangle at x = 0.5
        // that pierces T1.  No vertex lies in the opposite plane -> proper.
        // T1 lies in the XY plane.  T2 is a vertical triangle at x = 0.5
        // that pierces T1.  All coordinates are dyadic so the exact segment
        // endpoints are exact rationals.
        let t1 = Triangle::explicit([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let t2 = Triangle::explicit([0.5, -0.5, -1.0], [0.5, 0.5, -1.0], [0.5, 0.5, 1.0]);
        let r = intersect_triangles(&t1, &t2);
        match r {
            TriangleIntersection::ProperSegment { p, q } => {
                let pr = p.to_rational().unwrap();
                let qr = q.to_rational().unwrap();
                assert!(pr[2].is_zero());
                assert!(qr[2].is_zero());
                // Segment spans y in [0, 0.5].
                let y_vals = [pr[1].clone(), qr[1].clone()];
                let min_y = y_vals.iter().min().unwrap();
                let max_y = y_vals.iter().max().unwrap();
                assert!(min_y.is_zero());
                assert_eq!(max_y, &crate::point::f64_to_rat(0.5));
            }
            _ => panic!("expected proper segment, got {:?}", r),
        }
    }

    #[test]
    fn shared_vertex() {
        let t1 = Triangle::explicit([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let t2 = Triangle::explicit([0.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 1.0, 1.0]);
        let r = intersect_triangles(&t1, &t2);
        match r {
            TriangleIntersection::TouchPoint(_) | TriangleIntersection::TouchSegment { .. } => {}
            _ => panic!("expected touch, got {:?}", r),
        }
    }

    #[test]
    fn coplanar_overlap() {
        let t1 = Triangle::explicit([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let t2 = Triangle::explicit([0.2, 0.2, 0.0], [0.8, 0.2, 0.0], [0.2, 0.8, 0.0]);
        assert_eq!(
            intersect_triangles(&t1, &t2),
            TriangleIntersection::CoplanarOverlap
        );
    }

    #[test]
    fn coplanar_disjoint() {
        let t1 = Triangle::explicit([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let t2 = Triangle::explicit([2.0, 0.0, 0.0], [3.0, 0.0, 0.0], [2.0, 1.0, 0.0]);
        assert_eq!(
            intersect_triangles(&t1, &t2),
            TriangleIntersection::Disjoint
        );
    }

    #[test]
    fn random_explicit_triangles_agree_with_rational_reference() {
        let mut rng = rand::thread_rng();
        for _ in 0..200 {
            let t1 = random_triangle(&mut rng);
            let t2 = random_triangle(&mut rng);

            let main = intersect_triangles(&t1, &t2);
            let reference = reference_intersection(&t1, &t2);

            // Both should agree on the coarse classification (disjoint vs
            // intersecting).  For intersecting cases we additionally compare
            // the endpoints' rational coordinates.
            match (&main, &reference) {
                (TriangleIntersection::Disjoint, TriangleIntersection::Disjoint) => {}
                (TriangleIntersection::CoplanarOverlap, TriangleIntersection::CoplanarOverlap) => {}
                (TriangleIntersection::TouchPoint(mp), TriangleIntersection::TouchPoint(rp)) => {
                    assert!(
                        points_are_equal(mp, rp),
                        "main and reference touch points differ:\nmain: {:?}\nref:  {:?}",
                        main,
                        reference
                    );
                }
                (m, r) if is_segment(m) && is_segment(r) => {
                    let (mp, mq) = segment_endpoints(m);
                    let (rp, rq) = segment_endpoints(r);
                    // The two endpoints may be swapped; compare as unordered pairs.
                    let case1 = points_are_equal(mp, rp) && points_are_equal(mq, rq);
                    let case2 = points_are_equal(mp, rq) && points_are_equal(mq, rp);
                    assert!(
                        case1 || case2,
                        "main and reference segment endpoints differ:\nmain: {:?}\nref:  {:?}",
                        m,
                        r
                    );
                }
                _ => panic!("main {:?} disagrees with reference {:?}", main, reference),
            }
        }
    }

    fn random_triangle<R: Rng>(rng: &mut R) -> Triangle {
        let a = [rng.gen::<f64>(), rng.gen::<f64>(), rng.gen::<f64>()];
        let b = [rng.gen::<f64>(), rng.gen::<f64>(), rng.gen::<f64>()];
        let mut c = [rng.gen::<f64>(), rng.gen::<f64>(), rng.gen::<f64>()];
        // Avoid degenerate triangles by perturbing if nearly collinear.
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let cross = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        if cross[0].abs() + cross[1].abs() + cross[2].abs() < 1e-6 {
            c[0] += 0.5;
            c[1] += 0.3;
        }
        Triangle::explicit(a, b, c)
    }

    fn is_segment(r: &TriangleIntersection) -> bool {
        matches!(
            r,
            TriangleIntersection::ProperSegment { .. } | TriangleIntersection::TouchSegment { .. }
        )
    }

    fn segment_endpoints(r: &TriangleIntersection) -> (&Point3, &Point3) {
        match r {
            TriangleIntersection::ProperSegment { p, q } => (p, q),
            TriangleIntersection::TouchSegment { p, q } => (p, q),
            _ => panic!("not a segment result"),
        }
    }

    /// Reference classifier using exact rational coordinates directly.
    fn reference_intersection(t1: &Triangle, t2: &Triangle) -> TriangleIntersection {
        let v1 = t1.vertices();
        let v2 = t2.vertices();
        let s = [
            orient3d_sign(v1[0], v1[1], v1[2], v2[0]),
            orient3d_sign(v1[0], v1[1], v1[2], v2[1]),
            orient3d_sign(v1[0], v1[1], v1[2], v2[2]),
        ];
        let t = [
            orient3d_sign(v2[0], v2[1], v2[2], v1[0]),
            orient3d_sign(v2[0], v2[1], v2[2], v1[1]),
            orient3d_sign(v2[0], v2[1], v2[2], v1[2]),
        ];

        if all_same_nonzero(&s) || all_same_nonzero(&t) {
            return TriangleIntersection::Disjoint;
        }
        if s.iter().all(|&x| x == 0.0) || t.iter().all(|&x| x == 0.0) {
            return classify_coplanar(t1, t2);
        }

        // Brute-force edge-plane crossings using exact rational construction.
        // `t` are signs of T1 vertices against plane T2; `s` are signs of T2
        // vertices against plane T1.
        let mut pts: Vec<Point3> = Vec::new();
        for (i, j) in [(0, 1), (1, 2), (2, 0)] {
            if t[i] * t[j] < 0.0 {
                pts.push(reference_lpi(v1[i], v1[j], v2[0], v2[1], v2[2]));
            }
            if s[i] * s[j] < 0.0 {
                pts.push(reference_lpi(v2[i], v2[j], v1[0], v1[1], v1[2]));
            }
        }
        // Add zero-sign vertices that lie in the opposite plane.
        for (i, vi) in v2.iter().enumerate() {
            if s[i] == 0.0 {
                pts.push((*vi).clone());
            }
        }
        for (i, vi) in v1.iter().enumerate() {
            if t[i] == 0.0 {
                pts.push((*vi).clone());
            }
        }

        let mut uniq: Vec<Point3> = Vec::new();
        for p in pts {
            if point_inside_triangle_3d(t1, &p) && point_inside_triangle_3d(t2, &p) {
                if !uniq.iter().any(|q| points_are_equal(&p, q)) {
                    uniq.push(p);
                }
            }
        }

        match uniq.len() {
            0 => TriangleIntersection::Disjoint,
            1 => TriangleIntersection::TouchPoint(uniq.pop().unwrap()),
            2 => {
                let q = uniq.pop().unwrap();
                let p = uniq.pop().unwrap();
                let boundary = s.iter().any(|&x| x == 0.0) || t.iter().any(|&x| x == 0.0);
                if boundary {
                    TriangleIntersection::TouchSegment { p, q }
                } else {
                    TriangleIntersection::ProperSegment { p, q }
                }
            }
            _ => {
                let q = uniq.pop().unwrap();
                let p = uniq.pop().unwrap();
                TriangleIntersection::TouchSegment { p, q }
            }
        }
    }

    fn reference_lpi(q1: &Point3, q2: &Point3, r: &Point3, s: &Point3, t: &Point3) -> Point3 {
        Point3::Lpi {
            q1: q1.explicit(),
            q2: q2.explicit(),
            r: r.explicit(),
            s: s.explicit(),
            t: t.explicit(),
        }
    }
}
