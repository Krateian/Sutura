//! Unconditionally-robust generalized winding number (GWN) for a triangle soup.
//!
//! Independent Rust reimplementation of the robust "antipodal" mesh GWN of
//! Martens, Trettner & Bessmeltsev, ACM TOG 45(4), 2026, art. 43
//! (doi 10.1145/3811323; reference implementation
//! <https://github.com/MartensCedric/antipodal>, MIT).  Reimplemented from the
//! published algorithm; no C++ or Embree code was copied.  See ATTRIBUTION.md.
//!
//! The GWN is the sum of two terms:
//!
//! * the **integer term** — the signed ray/surface crossing count along a fixed
//!   `-x` ray, with each crossing signed by `sign(dir . n_face)`;
//! * the **fractional term** — the signed Van Oosterom-Strackee spherical
//!   triangle areas of the mesh's open boundary, in turns.
//!
//! Both terms are decided by the SAME exact 2-D determinant of the projected
//! edge (`atan2_num == -t_real`), so their discontinuities cancel on grazing
//! configurations and the field cannot jump by ~+-1 near the surface — the
//! failure mode of a classical floating-point GWN.
//!
//! Only the reference's `exact_float` precision policy is implemented:
//! coordinates stay `f64` and each determinant's sign is exact (an error-bound
//! filter plus a Shewchuk-style expansion fallback).  Grid policies
//! (`int64_grid`/`int128_grid`) are not ported; queries are expected to stay
//! within a few orders of magnitude of the mesh, so no clamping is needed.

use rayon::prelude::*;
use std::collections::BTreeMap;
use std::f64::consts::PI;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::winding::MeshBvh;

#[inline]
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn norm(a: [f64; 3]) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

#[inline]
fn dot_yz(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[1] * b[1] + a[2] * b[2]
}

/// `x + y == a + b` exactly (Knuth's two-sum; needs IEEE round-to-nearest and
/// no fast-math reassociation).
#[inline]
fn two_sum(a: f64, b: f64) -> (f64, f64) {
    let x = a + b;
    let bv = x - a;
    let av = x - bv;
    (x, (a - av) + (b - bv))
}

/// Push `v_in` onto the nonoverlapping expansion `e` (`n` components so far),
/// keeping the components ordered by increasing magnitude.
#[inline]
fn grow(v_in: f64, e: &mut [f64; 12], n: &mut usize) {
    let mut v = v_in;
    for i in 0..*n {
        let (sum, err) = two_sum(v, e[i]);
        e[i] = err;
        v = sum;
    }
    e[*n] = v;
    *n += 1;
}

/// Exact sign of `sum_{i<6} a[i] * b[i]`.  Each product is split exactly into
/// `p + e` with an FMA, and the twelve parts are summed as a nonoverlapping
/// expansion whose sign is that of its last nonzero component.
fn exact_sign_of_products(a: [f64; 6], b: [f64; 6]) -> i32 {
    let mut e = [0.0f64; 12];
    let mut n = 0usize;
    for i in 0..6 {
        let p = a[i] * b[i];
        grow(a[i].mul_add(b[i], -p), &mut e, &mut n);
        grow(p, &mut e, &mut n);
    }
    for i in (0..n).rev() {
        if e[i] != 0.0 {
            return if e[i] > 0.0 { 1 } else { -1 };
        }
    }
    0
}

/// The 2x2 determinant of `(q - q0)` and `(q1 - q0)` in the `(y, z)` plane.
///
/// Its sign says which side of the directed projected edge `q0 -> q1` the query
/// lies on; exactly `0` means on the edge's line.  The returned magnitude is the
/// double estimate, lifted to `f64::MIN_POSITIVE` when the exact value is not
/// zero, so `!= 0.0` and `> 0.0` read the exact sign.
fn edge_det(q: [f64; 3], q0: [f64; 3], q1: [f64; 3]) -> f64 {
    let p1 = (q[1] - q0[1]) * (q0[2] - q1[2]);
    let p2 = (q[2] - q0[2]) * (q1[1] - q0[1]);
    let det = p1 + p2;

    // Shewchuk's orient2d error bound for this form.
    let eps = f64::EPSILON / 2.0;
    let err_bound = (3.0 + 16.0 * eps) * eps;
    let magnitude = p1.abs() + p2.abs();
    if det.abs() > err_bound * magnitude {
        return det;
    }
    // Both products are exactly zero only if a difference is (or underflow, out
    // of the stated range); a degenerate edge lands here on every call, so it
    // must not reach the expansion.
    if magnitude == 0.0 {
        return 0.0;
    }
    // det = qy*q0z - qy*q1z + q0y*q1z + qz*q1y - qz*q0y - q0z*q1y,
    // expanded into raw coordinates so no rounded difference is involved.
    let sign = exact_sign_of_products(
        [q[1], -q[1], q0[1], q[2], -q[2], -q0[2]],
        [q0[2], q1[2], q1[2], q1[1], q0[1], q1[1]],
    );
    if sign == 0 {
        return 0.0;
    }
    det.abs().max(f64::MIN_POSITIVE).copysign(sign as f64)
}

/// Signed side of the directed projected edge `q0 -> q1` that `q` lies on
/// (`+1`/`-1`), from the exact real determinant.
///
/// On the exact-zero (grazing) case a lexicographic perturbation (`eps2` on `y`
/// dominating `eps3` on `z`) decides.  Returns `0` only for a degenerate edge
/// that projects to a single point (cannot be hit / contributes nothing).
fn edge_sign(q: [f64; 3], q0: [f64; 3], q1: [f64; 3]) -> i32 {
    let t = edge_det(q, q0, q1);
    if t != 0.0 {
        return if t > 0.0 { 1 } else { -1 };
    }
    // eps2 coefficient (perturb q.y): (q0.z - q1.z)
    if q0[2] != q1[2] {
        return if q0[2] > q1[2] { 1 } else { -1 };
    }
    // eps3 coefficient (perturb q.z): (q1.y - q0.y)
    if q1[1] != q0[1] {
        return if q1[1] > q0[1] { 1 } else { -1 };
    }
    0
}

/// Numerator of the spherical-area `atan2` for edge `q0 -> q1` seen from `q`.
///
/// Equals `-edge_det`, with an exact sign.  The magnitude is computed from the
/// query-relative vectors, not from `edge_det`: as the query approaches the ray
/// through a vertex that vertex's `(y, z)` offset gets small, and only
/// differences taken from the query keep it to full relative precision.
fn atan2_num(q: [f64; 3], q0: [f64; 3], q1: [f64; 3]) -> f64 {
    let v0 = sub(q0, q);
    let v1 = sub(q1, q);
    let t = edge_det(q, q0, q1);
    if t == 0.0 {
        return 0.0;
    }
    let num = v0[2] * v1[1] - v0[1] * v1[2];
    let sign = if t > 0.0 { -1.0 } else { 1.0 };
    num.abs().max(f64::MIN_POSITIVE).copysign(sign)
}

/// The perturbed sign of `atan2_num` when it is exactly zero.  Because
/// `num == -t_real`, that sign is exactly the negation of `edge_sign` — the one
/// fact that ties the atan2 quadrant choice to the ray/triangle predicate.
fn sign_num_perturbed(q: [f64; 3], q0: [f64; 3], q1: [f64; 3]) -> i32 {
    -edge_sign(q, q0, q1)
}

struct TriProjection {
    sign: i32,
    w0: f64,
    w1: f64,
    w2: f64,
}

/// Where the `-x` ray from `q` meets the projected triangle `(q0, q1, q2)`, if
/// it does.
///
/// `sign` is the common edge sign (`+1`/`-1`, the orientation of the projected
/// triangle) when `q` projects strictly or symbolically inside, and `0`
/// otherwise (outside, or a degenerate projection).  `w0, w1, w2` are then the
/// unnormalized barycentric weights of `q0, q1, q2` (each with the sign of
/// `sign` or `0`); their sum is twice the projected area, which is not zero.
fn project_into_tri(q: [f64; 3], q0: [f64; 3], q1: [f64; 3], q2: [f64; 3]) -> TriProjection {
    let e01 = edge_sign(q, q0, q1);
    let e12 = edge_sign(q, q1, q2);
    let e20 = edge_sign(q, q2, q0);
    if e01 == 0 || e01 != e12 || e01 != e20 {
        return TriProjection {
            sign: 0,
            w0: 0.0,
            w1: 0.0,
            w2: 0.0,
        };
    }
    // The weight of a vertex is the determinant of the edge opposite it.
    TriProjection {
        sign: e01,
        w0: edge_det(q, q1, q2),
        w1: edge_det(q, q2, q0),
        w2: edge_det(q, q0, q1),
    }
}

/// A weighted, oriented boundary segment (the net of the mesh's open boundary).
#[derive(Clone, Copy)]
struct QSeg {
    pos0: [f64; 3],
    pos1: [f64; 3],
    weight: f64,
}

/// Extract the open boundary of a triangle soup as weighted oriented segments.
///
/// Each undirected edge `{min, max}` accumulates `+1` for every `min -> max`
/// traversal and `-1` for every `max -> min`; interior edges of an oriented
/// manifold cancel to zero and are dropped, while non-manifold edges keep their
/// net count as the weight.  Surviving segments are emitted with positive
/// weight.  Edges are emitted in sorted key order so the fractional-term sum is
/// deterministic (the reference's `unordered_map` iteration order is not).
fn build_boundary_segments(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> Vec<QSeg> {
    let mut counts: BTreeMap<(u32, u32), i64> = BTreeMap::new();
    for t in tris {
        let tri = [t[0] as u32, t[1] as u32, t[2] as u32];
        for e in 0..3 {
            let (u, v) = (tri[e], tri[(e + 1) % 3]);
            let key = (u.min(v), u.max(v));
            *counts.entry(key).or_insert(0) += if u < v { 1 } else { -1 };
        }
    }
    let mut out = Vec::new();
    for ((lo, hi), sum) in counts {
        if sum == 0 {
            continue;
        }
        let a = verts[lo as usize];
        let b = verts[hi as usize];
        let (pos0, pos1) = if sum > 0 { (a, b) } else { (b, a) };
        out.push(QSeg {
            pos0,
            pos1,
            weight: sum.unsigned_abs() as f64,
        });
    }
    out
}

/// Prepared robust GWN evaluator for a triangle soup.
///
/// `exact_float` policy: coordinates stay `f64` and every determinant sign is
/// exact.  Build once from the mesh arrays, then query concurrently (the batch
/// helper shares one evaluator across threads).
pub struct AntipodalGwn {
    bvh: MeshBvh,
    /// Boundary segments with their endpoints swapped (`pos1, pos0`): the exact
    /// `atan2_num` used by `fractional` expects the opposite orientation to the
    /// stored boundary, so the swap negates the fractional term to match the
    /// classical float kernels while keeping the atan2/grazing-sign pair
    /// internally consistent.
    boundary: Vec<QSeg>,
}

impl AntipodalGwn {
    pub fn build(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> Self {
        let bvh = MeshBvh::from_arrays(verts, tris);
        let boundary = build_boundary_segments(verts, tris)
            .into_iter()
            .map(|s| QSeg {
                pos0: s.pos1,
                pos1: s.pos0,
                weight: s.weight,
            })
            .collect();
        AntipodalGwn { bvh, boundary }
    }

    /// Integer term: the signed ray/surface crossing count along the fixed `-x`
    /// ray from `p`.  A crossing contributes `sign(dir . n) = -sign(n.x)` with
    /// `dir = -x`, so the inside of a closed, consistently outward-oriented
    /// shell gives `+1`.
    pub fn signed_intersection_count(&self, p: [f64; 3]) -> i32 {
        let mut sum: i32 = 0;
        self.bvh.for_each_x_ray_candidate(p, &mut |tri| {
            let hit = project_into_tri(p, tri.a, tri.b, tri.c);
            if hit.sign == 0 {
                return;
            }
            // The ray meets the triangle at the barycentric mix of its vertex
            // depths; that lies between them, so the front/back answer cannot
            // contradict the in-projection test.
            let hit_x =
                (hit.w0 * tri.a[0] + hit.w1 * tri.b[0] + hit.w2 * tri.c[0]) / (hit.w0 + hit.w1 + hit.w2);
            // Ray dir = -x; a tie is in front under the eps1 perturbation.
            if p[0] >= hit_x {
                sum += -hit.sign;
            }
        });
        sum
    }

    /// Fractional term: the signed Van Oosterom-Strackee boundary integral, in
    /// turns.  Exactly `0` for a closed mesh.
    pub fn fractional(&self, p: [f64; 3]) -> f64 {
        // Folds the factor 2 ("half area") and the 1 / (4 pi) normalization.
        const F_NORM: f64 = 2.0 / (4.0 * PI);
        let mut half_area = 0.0f64;
        for seg in &self.boundary {
            let (q0, q1) = (seg.pos0, seg.pos1);
            let v0 = sub(q0, p);
            let v1 = sub(q1, p);
            let l0 = norm(v0);
            let l1 = norm(v1);

            // atan2 numerator with an exact sign (== -t_real of the edge
            // predicate).
            let num = atan2_num(p, q0, q1);

            // denom = l0*l1 + v0.x*l1 + v1.x*l0 + dot(v0, v1)
            //       = (l0 + v0.x) * (l1 + v1.x) + dot_yz(v0, v1).
            // l + v.x cancels badly when v points almost along -x (an edge seen
            // along the ray), so there use l + v.x = (v.y^2 + v.z^2)/(l - v.x).
            let l_plus_x = |v: [f64; 3], l: f64| -> f64 {
                if v[0] >= 0.0 {
                    l + v[0]
                } else {
                    dot_yz(v, v) / (l - v[0])
                }
            };
            let denom = l_plus_x(v0, l0) * l_plus_x(v1, l1) + dot_yz(v0, v1);

            let contrib = if num != 0.0 {
                num.atan2(denom)
            } else if v0[1] == 0.0 && v0[2] == 0.0 && v0[0] < 0.0 && (v1[1] != 0.0 || v1[2] != 0.0) {
                // The ray passes through pos0, where num and denom both vanish:
                // take the limit for the perturbed query, as the edge predicate
                // does.
                if v1[2] != 0.0 {
                    v1[2].atan2(-v1[1])
                } else if v1[1] > 0.0 {
                    -PI
                } else {
                    0.0
                }
            } else if v1[1] == 0.0 && v1[2] == 0.0 && v1[0] < 0.0 && (v0[1] != 0.0 || v0[2] != 0.0) {
                if v0[2] != 0.0 {
                    (-v0[2]).atan2(-v0[1])
                } else if v0[1] > 0.0 {
                    PI
                } else {
                    0.0
                }
            } else {
                // Grazing: atan2 -> 0 (denom > 0) or +-pi (denom < 0); the
                // perturbed sign is locked to the ray/triangle predicate.
                let sgn = sign_num_perturbed(p, q0, q1) as f64;
                if denom < 0.0 {
                    sgn * PI
                } else {
                    0.0
                }
            };
            half_area += seg.weight * contrib;
        }
        half_area * F_NORM
    }

    /// The full robust GWN at `p` (fractional + integer), in turns.
    pub fn winding(&self, p: [f64; 3]) -> f64 {
        self.fractional(p) + self.signed_intersection_count(p) as f64
    }

    /// `winding(p) > 0.5`: inside for a closed shell.
    pub fn inside(&self, p: [f64; 3]) -> bool {
        self.winding(p) > 0.5
    }

    /// Robust GWN for many points, parallel across CPU cores.
    pub fn winding_points_par(&self, points: &[[f64; 3]]) -> Vec<f64> {
        points.par_iter().map(|p| self.winding(*p)).collect()
    }

    /// Number of boundary segments (0 for a closed, manifold soup).  Reported by
    /// the Python layer as a diagnostic.
    pub fn boundary_segment_count(&self) -> usize {
        self.boundary.len()
    }
}

/// Statistics from an antipodal grid disambiguation pass.
#[derive(Clone, Copy, Debug)]
pub struct AntipodalStats {
    pub boundary_segments: usize,
    pub cells_ambiguous: u64,
    pub cells_flipped: u64,
    pub seconds: f64,
}

/// Overwrite the sign of the signed grid for generalized-winding-ambiguous
/// cells (`lo < w < hi`) using the robust antipodal oracle.
///
/// The oracle always resolves a cell (it never returns "no opinion"), so unlike
/// the ray-stab pass there is no unresolved count.  Cells outside the band keep
/// their existing sign.  Parallel over z-slices, like the other grid passes.
#[allow(clippy::too_many_arguments)]
pub fn disambiguate_sdf(
    gwn: &AntipodalGwn,
    winding: &[f32],
    unsigned: &[f32],
    signed: &mut [f32],
    origin: [f64; 3],
    voxel: f64,
    dims: [usize; 3],
    lo: f32,
    hi: f32,
) -> AntipodalStats {
    let t_start = Instant::now();
    let slice = dims[0] * dims[1];
    let ambiguous = AtomicU64::new(0);
    let flipped = AtomicU64::new(0);
    signed
        .par_chunks_mut(slice)
        .enumerate()
        .for_each(|(k, sch)| {
            let z = origin[2] + voxel * k as f64;
            let base = k * slice;
            let wch = &winding[base..base + slice];
            let uch = &unsigned[base..base + slice];
            for j in 0..dims[1] {
                let y = origin[1] + voxel * j as f64;
                let row = j * dims[0];
                for i in 0..dims[0] {
                    let w = wch[row + i];
                    if !(w > lo && w < hi) {
                        continue;
                    }
                    ambiguous.fetch_add(1, Ordering::Relaxed);
                    let x = origin[0] + voxel * i as f64;
                    let inside = gwn.inside([x, y, z]);
                    let s = uch[row + i];
                    // `s` is an UNSIGNED distance (always >= 0); the previous
                    // inside/outside state comes from the SIGNED slice.
                    let was_inside = sch[row + i] < 0.0;
                    if was_inside != inside {
                        flipped.fetch_add(1, Ordering::Relaxed);
                    }
                    sch[row + i] = if inside { -s } else { s };
                }
            }
        });
    AntipodalStats {
        boundary_segments: gwn.boundary_segment_count(),
        cells_ambiguous: ambiguous.load(Ordering::Relaxed),
        cells_flipped: flipped.load(Ordering::Relaxed),
        seconds: t_start.elapsed().as_secs_f64(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube_verts() -> Vec<[f64; 3]> {
        vec![
            [0., 0., 0.],
            [1., 0., 0.],
            [1., 1., 0.],
            [0., 1., 0.],
            [0., 0., 1.],
            [1., 0., 1.],
            [1., 1., 1.],
            [0., 1., 1.],
        ]
    }

    /// Outward-oriented unit-cube faces (CCW seen from outside).
    fn cube_tris() -> Vec<[usize; 3]> {
        vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [3, 7, 6],
            [3, 6, 2],
            [0, 4, 7],
            [0, 7, 3],
            [1, 2, 6],
            [1, 6, 5],
        ]
    }

    #[test]
    fn closed_cube_winding_is_one_inside_zero_outside() {
        let gwn = AntipodalGwn::build(&cube_verts(), &cube_tris());
        assert_eq!(gwn.boundary_segment_count(), 0, "closed cube has no boundary");
        for inside in [[0.5, 0.5, 0.5], [0.25, 0.25, 0.25], [0.9, 0.1, 0.9]] {
            let w = gwn.winding(inside);
            assert!((w - 1.0).abs() < 1e-9, "inside {inside:?} -> {w}");
            assert!(gwn.inside(inside));
            assert_eq!(gwn.signed_intersection_count(inside), 1);
            assert_eq!(gwn.fractional(inside), 0.0);
        }
        for outside in [
            [2.0, 2.0, 2.0],
            [-1.0, 0.5, 0.5],
            [0.5, 0.5, -1.0],
            [0.5, 0.5, 2.0],
        ] {
            let w = gwn.winding(outside);
            assert!(w.abs() < 1e-9, "outside {outside:?} -> {w}");
            assert!(!gwn.inside(outside));
            assert_eq!(gwn.signed_intersection_count(outside), 0);
        }
    }

    #[test]
    fn open_cube_has_boundary_and_is_finite_on_grazing_queries() {
        // Drop the top face (indices 4,5,6,7 face: triangles [4,5,6],[4,6,7]).
        let mut tris = cube_tris();
        tris.retain(|t| *t != [4, 5, 6] && *t != [4, 6, 7]);
        let gwn = AntipodalGwn::build(&cube_verts(), &tris);
        assert_eq!(gwn.boundary_segment_count(), 4, "open cube boundary loop");
        assert_ne!(gwn.fractional([0.5, 0.5, 0.5]), 0.0);

        // Grazing queries: on a face plane / through a vertex's projection.
        for q in [
            [0.5, 0.5, 0.0],
            [0.5, 0.0, 0.0],
            [0.0, 0.0, 0.5],
            [0.5, 0.5, 0.5],
            [-0.25, 1.0, 1.0],
        ] {
            let w = gwn.winding(q);
            assert!(w.is_finite(), "grazing {q:?} -> {w}");
        }
    }

    #[test]
    fn inside_matches_threshold_and_is_deterministic() {
        let verts = cube_verts();
        let tris = cube_tris();
        let a = AntipodalGwn::build(&verts, &tris);
        let b = AntipodalGwn::build(&verts, &tris);
        // A deterministic pseudo-random set of query points.
        let mut seed = 0x9E3779B97F4A7C15u64;
        let mut pts = Vec::new();
        for _ in 0..500 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let u = ((seed >> 11) as f64) / ((1u64 << 53) as f64);
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let v = ((seed >> 11) as f64) / ((1u64 << 53) as f64);
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let s = ((seed >> 11) as f64) / ((1u64 << 53) as f64);
            pts.push([u * 2.0 - 0.5, v * 2.0 - 0.5, s * 2.0 - 0.5]);
        }
        let wa = a.winding_points_par(&pts);
        let wb = b.winding_points_par(&pts);
        for (i, p) in pts.iter().enumerate() {
            assert_eq!(wa[i].to_bits(), wb[i].to_bits(), "non-deterministic at {p:?}");
            assert_eq!(a.inside(*p), wa[i] > 0.5);
        }
    }

    #[test]
    fn closed_cube_agrees_with_parity_inside() {
        // For a closed consistent shell the integer count equals 1 inside and 0
        // outside, regardless of the query's exact position relative to the axis
        // ray — including rays that pass exactly through interior edges.
        let gwn = AntipodalGwn::build(&cube_verts(), &cube_tris());
        let diagonal = [[0.5, 0.5, 0.5], [0.5, 0.5, 0.25], [0.5, 0.25, 0.5], [0.25, 0.5, 0.5]];
        for p in diagonal {
            assert_eq!(gwn.signed_intersection_count(p), 1, "{p:?}");
        }
    }
}
