//! roi — surface-preserving local repair primitives.
//!
//! `triangulate_annulus` stitches two nested, near-planar boundary loops (a cut
//! rim in the input and the matching rim of a locally generated patch) with a
//! triangle band.  It provides the geometric seam for the local-ROI Dressing
//! path (Bischoff & Kobbelt 2005): the patch and the untouched input share no
//! vertices, and the input of a broken mesh cannot be imported as a Manifold
//! solid, so the two rims are bridged explicitly instead of by CSG.
//!
//! The band is built by projecting both loops into their best-fit plane,
//! normalizing both to counter-clockwise, and solving a minimum-area monotone
//! strip between them (a small dynamic program over the outer/inner step
//! counts): each step advances one loop by one vertex, so the strip covers the
//! annulus exactly when it does not fold.  The function returns `None` on any
//! doubt — not near-planar, a loop not simple, loops not nested, or a folded
//! band whose summed 2D area does not match the annulus area — and the caller
//! falls back to the whole-surface result.  Everything is deterministic: fixed
//! projection plane, fixed anchor pair, fixed traversal, no clock.

use crate::hole::{loop_plane, plane_basis, polygon_simple, seg_proper_intersect, signed_area_2d};

pub(crate) type V3 = [f64; 3];

#[inline]
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn dist2d(a: [f64; 2], b: [f64; 2]) -> f64 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    dx * dx + dy * dy
}

/// Relative out-of-plane tolerance (max deviation / loop extent).  Beyond this
/// the seam cannot be judged from a single plane and the caller must fall back.
const ANNULUS_PLANAR_TOL: f64 = 0.1;

/// Relative area tolerance for the folded-band check.
const ANNULUS_AREA_TOL: f64 = 0.01;

/// Upper bound on `p * q` for the annular-strip DP (states are `(p+1)*(q+1)`).
const ANNULUS_MAX_PQ: usize = 250_000;

fn project(p: V3, c: V3, u: V3, v: V3) -> [f64; 2] {
    let d = sub(p, c);
    [dot(d, u), dot(d, v)]
}

/// Point-in-polygon by ray casting along +x (the polygon is closed implicitly).
fn point_in_polygon(p: [f64; 2], poly: &[[f64; 2]]) -> bool {
    let k = poly.len();
    let mut inside = false;
    let mut j = k - 1;
    for i in 0..k {
        let a = poly[i];
        let b = poly[j];
        if (a[1] > p[1]) != (b[1] > p[1]) {
            let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
            if p[0] < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Do any two edges, one from each loop, touch or cross?
fn loops_cross(pa: &[[f64; 2]], pb: &[[f64; 2]]) -> bool {
    let m = pa.len();
    let n = pb.len();
    for i in 0..m {
        let a1 = pa[i];
        let a2 = pa[(i + 1) % m];
        for j in 0..n {
            if seg_proper_intersect(a1, a2, pb[j], pb[(j + 1) % n]) {
                return true;
            }
        }
    }
    false
}

/// Index list of a loop in the requested winding, offset into the combined
/// `loop_a ++ loop_b` vertex space.  Reversing is a no-op on the cyclic order.
/// Both annulus boundaries are normalized counter-clockwise so the strip
/// advances the same way round each loop.
fn oriented_order(uv: &[[f64; 2]], offset: usize, want_ccw: bool) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..uv.len()).map(|i| offset + i).collect();
    if (signed_area_2d(uv) < 0.0) == want_ccw {
        idx.reverse();
    }
    idx
}

/// Stitch two nested, near-planar loops with a triangle band.
///
/// Returns the band as index triples into the concatenation `loop_a ++ loop_b`
/// (so `loop_a` keeps indices `0..m` and `loop_b` `m..m+n`), or `None` when the
/// seam cannot be built safely.
pub fn triangulate_annulus(loop_a: &[V3], loop_b: &[V3]) -> Option<Vec<[usize; 3]>> {
    let m = loop_a.len();
    let n = loop_b.len();
    if m < 3 || n < 3 {
        return None;
    }
    let (c, nrm, extent, _) = loop_plane(loop_a).or_else(|| loop_plane(loop_b))?;
    if !(extent > 0.0) || !extent.is_finite() {
        return None;
    }
    let mut maxd = 0.0f64;
    for p in loop_a.iter().chain(loop_b.iter()) {
        let d = dot(sub(*p, c), nrm).abs();
        if d > maxd {
            maxd = d;
        }
    }
    if maxd > ANNULUS_PLANAR_TOL * extent {
        return None;
    }
    let (u, v) = plane_basis(nrm);
    let pa: Vec<[f64; 2]> = loop_a.iter().map(|p| project(*p, c, u, v)).collect();
    let pb: Vec<[f64; 2]> = loop_b.iter().map(|p| project(*p, c, u, v)).collect();
    if !polygon_simple(&pa) || !polygon_simple(&pb) || loops_cross(&pa, &pb) {
        return None;
    }
    // Strict nesting: exactly one loop contains the other.
    let a_in_b = point_in_polygon(pa[0], &pb);
    let b_in_a = point_in_polygon(pb[0], &pa);
    if a_in_b == b_in_a {
        return None;
    }
    let all2d: Vec<[f64; 2]> = pa.iter().chain(pb.iter()).copied().collect();
    // Both boundaries counter-clockwise, so both advance the same way round.
    let (outer_idx, inner_idx) = if b_in_a {
        (oriented_order(&pa, 0, true), oriented_order(&pb, m, true))
    } else {
        (oriented_order(&pb, m, true), oriented_order(&pa, 0, true))
    };
    let p = outer_idx.len();
    let q = inner_idx.len();
    if p.saturating_mul(q) > ANNULUS_MAX_PQ {
        return None;
    }
    let outer2: Vec<[f64; 2]> = outer_idx.iter().map(|&i| all2d[i]).collect();
    let inner2: Vec<[f64; 2]> = inner_idx.iter().map(|&i| all2d[i]).collect();

    // Anchor: closest outer/inner vertex pair (ties to the lower (i, j)).
    let mut i0 = 0usize;
    let mut j0 = 0usize;
    let mut bd = f64::INFINITY;
    for i in 0..p {
        for j in 0..q {
            let d = dist2d(outer2[i], inner2[j]);
            if d < bd {
                bd = d;
                i0 = i;
                j0 = j;
            }
        }
    }

    // Minimum-area monotone strip from (0,0) to (p,q).  Each step advances the
    // outer loop (triangle O_i O_{i+1} I_j) or the inner loop (O_i I_{j+1} I_j).
    // A valid non-overlapping band covers exactly the annulus area, and no
    // overlapping strip can cover less, so the minimum is the valid band.
    let area_out = |ic: usize, jc: usize| -> f64 {
        let i = (i0 + ic) % p;
        let j = (j0 + jc) % q;
        tri_area2(outer2[i], outer2[(i + 1) % p], inner2[j])
    };
    let area_in = |ic: usize, jc: usize| -> f64 {
        let i = (i0 + ic) % p;
        let j = (j0 + jc) % q;
        tri_area2(outer2[i], inner2[(j + 1) % q], inner2[j])
    };
    let mut f = vec![vec![f64::INFINITY; q + 1]; p + 1];
    f[p][q] = 0.0;
    for ic in (0..=p).rev() {
        for jc in (0..=q).rev() {
            if ic == p && jc == q {
                continue;
            }
            f[ic][jc] = if ic == p {
                area_in(ic, jc) + f[ic][jc + 1]
            } else if jc == q {
                area_out(ic, jc) + f[ic + 1][jc]
            } else {
                let co = area_out(ic, jc) + f[ic + 1][jc];
                let ci = area_in(ic, jc) + f[ic][jc + 1];
                if ci <= co {
                    ci
                } else {
                    co
                }
            };
        }
    }

    let mut tris: Vec<[usize; 3]> = Vec::with_capacity(p + q);
    let (mut ic, mut jc) = (0usize, 0usize);
    while ic < p || jc < q {
        let i = (i0 + ic) % p;
        let j = (j0 + jc) % q;
        if ic == p {
            tris.push([outer_idx[i], inner_idx[(j + 1) % q], inner_idx[j]]);
            jc += 1;
        } else if jc == q {
            tris.push([outer_idx[i], outer_idx[(i + 1) % p], inner_idx[j]]);
            ic += 1;
        } else {
            let co = area_out(ic, jc) + f[ic + 1][jc];
            if f[ic][jc] == co {
                tris.push([outer_idx[i], outer_idx[(i + 1) % p], inner_idx[j]]);
                ic += 1;
            } else {
                tris.push([outer_idx[i], inner_idx[(j + 1) % q], inner_idx[j]]);
                jc += 1;
            }
        }
    }
    if tris.len() != p + q {
        return None;
    }

    // Coherent counter-clockwise winding.
    for t in tris.iter_mut() {
        let a = all2d[t[0]];
        let b = all2d[t[1]];
        let cc = all2d[t[2]];
        let ar = (b[0] - a[0]) * (cc[1] - a[1]) - (b[1] - a[1]) * (cc[0] - a[0]);
        if ar < 0.0 {
            t.swap(1, 2);
        }
    }

    // The band must cover exactly the annulus area (catches any mis-seam).
    let (a_area, b_area) = (signed_area_2d(&pa).abs(), signed_area_2d(&pb).abs());
    let expected = a_area.max(b_area) - a_area.min(b_area);
    if !(expected > 0.0) {
        return None;
    }
    let mut got = 0.0f64;
    for t in &tris {
        let a = all2d[t[0]];
        let b = all2d[t[1]];
        let cc = all2d[t[2]];
        got += 0.5 * ((b[0] - a[0]) * (cc[1] - a[1]) - (b[1] - a[1]) * (cc[0] - a[0])).abs();
    }
    if (got - expected).abs() > ANNULUS_AREA_TOL * expected {
        return None;
    }
    Some(tris)
}

#[inline]
fn tri_area2(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    0.5 * ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n_boundary_edges(tris: &[[usize; 3]]) -> usize {
        use std::collections::HashMap;
        let mut cnt: HashMap<(usize, usize), usize> = HashMap::new();
        for t in tris {
            for &(a, b) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                let k = if a < b { (a, b) } else { (b, a) };
                *cnt.entry(k).or_insert(0) += 1;
            }
        }
        cnt.values().filter(|&&c| c == 1).count()
    }

    #[test]
    fn annulus_between_concentric_triangles_is_manifold() {
        let outer = [[0.0, 0.0, 0.0], [4.0, 0.0, 0.0], [0.0, 4.0, 0.0]];
        let inner = [[1.0, 1.0, 0.0], [1.5, 1.0, 0.0], [1.0, 1.5, 0.0]];
        let tris = triangulate_annulus(&outer, &inner).expect("annulus");
        assert_eq!(tris.len(), 6);
        // boundary = the 3 outer + 3 inner edges, each used once
        assert_eq!(n_boundary_edges(&tris), 6);
        // every index in range of the concatenation
        assert!(tris.iter().flatten().all(|&x| x < 6));
    }

    #[test]
    fn annulus_between_concentric_squares_is_manifold() {
        let outer = [
            [0.0, 0.0, 0.0],
            [4.0, 0.0, 0.0],
            [4.0, 4.0, 0.0],
            [0.0, 4.0, 0.0],
        ];
        let inner = [
            [1.0, 1.0, 0.0],
            [2.0, 1.0, 0.0],
            [2.0, 2.0, 0.0],
            [1.0, 2.0, 0.0],
        ];
        let tris = triangulate_annulus(&outer, &inner).expect("annulus");
        assert_eq!(tris.len(), 8);
        assert_eq!(n_boundary_edges(&tris), 8);
    }

    #[test]
    fn annulus_accepts_swap_of_loop_order() {
        let outer = [[0.0, 0.0, 0.0], [4.0, 0.0, 0.0], [0.0, 4.0, 0.0]];
        let inner = [[1.0, 1.0, 0.0], [1.5, 1.0, 0.0], [1.0, 1.5, 0.0]];
        assert!(triangulate_annulus(&outer, &inner).is_some());
        assert!(triangulate_annulus(&inner, &outer).is_some());
    }

    #[test]
    fn annulus_rejects_non_planar_and_non_nested() {
        let outer = [[0.0, 0.0, 0.0], [4.0, 0.0, 0.0], [0.0, 4.0, 0.0]];
        let inner = [[1.0, 1.0, 0.0], [1.5, 1.0, 0.0], [1.0, 1.5, 0.0]];
        let mut bumpy = inner;
        bumpy[0][2] = 3.0;
        assert!(triangulate_annulus(&outer, &bumpy).is_none());
        // disjoint loops are not nested
        let far = [[10.0, 10.0, 0.0], [11.0, 10.0, 0.0], [10.0, 11.0, 0.0]];
        assert!(triangulate_annulus(&outer, &far).is_none());
    }
}
