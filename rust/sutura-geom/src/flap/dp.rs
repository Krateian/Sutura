//! Minimum-area dynamic-programme triangulation of a simple 3-D polygon
//! (Liepa 2003), with a forbidden-chord set and a fan fallback above the cap.

use super::V3;
use std::collections::HashSet;

#[inline]
fn tri_area(p: &[V3], i: usize, k: usize, j: usize) -> f64 {
    let ax = p[k][0] - p[i][0];
    let ay = p[k][1] - p[i][1];
    let az = p[k][2] - p[i][2];
    let bx = p[j][0] - p[i][0];
    let by = p[j][1] - p[i][1];
    let bz = p[j][2] - p[i][2];
    let cx = ay * bz - az * by;
    let cy = az * bx - ax * bz;
    let cz = ax * by - ay * bx;
    0.5 * (cx * cx + cy * cy + cz * cz).sqrt()
}

/// Degeneracy test for the triangle `(i, k, j)`.
///
/// A triangle is rejected when its area is at or below `min_area` (absolute,
/// squared mesh units) or when its largest interior angle lies within
/// `collinear_margin_deg` of 180 degrees (a near-collinear sliver). Both
/// thresholds at, or below, zero disable the corresponding test, which is what
/// the historical [`min_area_triangulation`] uses, so its output is unchanged.
#[inline]
fn tri_rejected(
    p: &[V3],
    i: usize,
    k: usize,
    j: usize,
    min_area: f64,
    collinear_margin_deg: f64,
) -> bool {
    if min_area > 0.0 && tri_area(p, i, k, j) <= min_area {
        return true;
    }
    if collinear_margin_deg > 0.0 {
        let a = p[i];
        let b = p[k];
        let c = p[j];
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let bc = [c[0] - b[0], c[1] - b[1], c[2] - b[2]];
        let l2a = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
        let l2b = ac[0] * ac[0] + ac[1] * ac[1] + ac[2] * ac[2];
        let l2c = bc[0] * bc[0] + bc[1] * bc[1] + bc[2] * bc[2];
        let mut ls = [l2a, l2b, l2c];
        ls.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
        // The largest interior angle is opposite the longest side `ls[2]`;
        // its cosine follows from the law of cosines over the other two sides.
        let y2 = ls[0];
        let x2 = ls[1];
        let l2 = ls[2];
        let denom = 2.0 * (x2 * y2).sqrt();
        if denom <= 0.0 {
            return true;
        }
        let cos_max = (x2 + y2 - l2) / denom;
        if cos_max < -(collinear_margin_deg.to_radians().cos()) {
            return true;
        }
    }
    false
}

/// Triangulate the polygon `p` (indices local to `p`).  `forbidden` holds
/// sorted `(a, b)` chord pairs that must not be used.  Returns local triples,
/// or an empty vec when no valid triangulation exists.  Above `cap` a fan is
/// returned.
pub fn min_area_triangulation(p: &[V3], cap: usize, forbidden: &HashSet<(u32, u32)>) -> Vec<[u32; 3]> {
    min_area_triangulation_filtered(p, cap, forbidden, 0.0, 0.0)
}

/// [`min_area_triangulation`] with an explicit degeneracy filter: a candidate
/// triangle is skipped when [`tri_rejected`] flags it. With zero thresholds the
/// output is byte-identical to [`min_area_triangulation`].
pub fn min_area_triangulation_filtered(
    p: &[V3],
    cap: usize,
    forbidden: &HashSet<(u32, u32)>,
    reject_min_area: f64,
    collinear_margin_deg: f64,
) -> Vec<[u32; 3]> {
    let m = p.len();
    let forb = |a: usize, b: usize| -> bool {
        let (x, y) = if a < b { (a, b) } else { (b, a) };
        forbidden.contains(&(x as u32, y as u32))
    };
    if m < 3 {
        return Vec::new();
    }
    if m == 3 {
        return if forb(0, 2) || tri_rejected(p, 0, 1, 2, reject_min_area, collinear_margin_deg) {
            Vec::new()
        } else {
            vec![[0, 1, 2]]
        };
    }
    if m > cap {
        return (1..m - 1).map(|i| [0, i as u32, (i + 1) as u32]).collect();
    }

    let inf = f64::INFINITY;
    let mut mm = vec![0.0f64; m * m];
    let mut kk = vec![-1i32; m * m];
    let idx = |i: usize, j: usize| i * m + j;
    for gap in 2..m {
        for i in 0..(m - gap) {
            let j = i + gap;
            if forb(i, j) {
                mm[idx(i, j)] = inf;
                continue;
            }
            if gap == 2 {
                if tri_rejected(p, i, i + 1, j, reject_min_area, collinear_margin_deg) {
                    mm[idx(i, j)] = inf;
                    continue;
                }
                mm[idx(i, j)] = tri_area(p, i, i + 1, j);
                kk[idx(i, j)] = (i + 1) as i32;
                continue;
            }
            let mut best = inf;
            let mut bk: i32 = -1;
            for k in (i + 1)..j {
                if mm[idx(i, k)] == inf || mm[idx(k, j)] == inf {
                    continue;
                }
                if tri_rejected(p, i, k, j, reject_min_area, collinear_margin_deg) {
                    continue;
                }
                let w = mm[idx(i, k)] + mm[idx(k, j)] + tri_area(p, i, k, j);
                if w < best {
                    best = w;
                    bk = k as i32;
                }
            }
            mm[idx(i, j)] = best;
            kk[idx(i, j)] = bk;
        }
    }

    let mut tris: Vec<[u32; 3]> = Vec::new();
    // iterative pre-order emission to match the reference recursion exactly
    let mut stack: Vec<(usize, usize)> = vec![(0, m - 1)];
    while let Some((i, j)) = stack.pop() {
        if j < i + 2 {
            continue;
        }
        let k = kk[idx(i, j)];
        if k < 0 {
            continue;
        }
        let k = k as usize;
        tris.push([i as u32, k as u32, j as u32]);
        // reference order: rec(i,k) first, then rec(k,j).  A LIFO stack must
        // push the second call first.
        stack.push((k, j));
        stack.push((i, k));
    }
    tris
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rectangle with an extra vertex on the middle of its bottom edge: the
    /// unfiltered min-area DP picks the zero-area ear `(0,1,2)`.
    fn degenerate_poly() -> Vec<V3> {
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 2.0, 0.0],
            [0.0, 2.0, 0.0],
        ]
    }

    #[inline]
    fn area_local(p: &[V3], t: [u32; 3]) -> f64 {
        tri_area(p, t[0] as usize, t[1] as usize, t[2] as usize)
    }

    #[test]
    fn default_thresholds_match_unfiltered() {
        let p = degenerate_poly();
        let f = HashSet::new();
        let a = min_area_triangulation(&p, 300, &f);
        let b = min_area_triangulation_filtered(&p, 300, &f, 0.0, 0.0);
        assert_eq!(a, b);
    }

    #[test]
    fn filtered_rejects_a_degenerate_single_triangle() {
        // m == 3 is the one place the reject is unconditional on the triangle
        // itself; the unfiltered path keeps the zero-area triangle.
        let p = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]];
        let f = HashSet::new();
        assert_eq!(min_area_triangulation(&p, 300, &f), vec![[0, 1, 2]]);
        assert!(min_area_triangulation_filtered(&p, 300, &f, 1e-12, 1.0).is_empty());
        assert_eq!(min_area_triangulation_filtered(&p, 300, &f, 0.0, 0.0), vec![[0, 1, 2]]);
    }

    #[test]
    fn filtered_triangulation_has_no_degenerate_faces() {
        let p = degenerate_poly();
        let f = HashSet::new();
        let filtered = min_area_triangulation_filtered(&p, 300, &f, 1e-12, 1.0);
        assert_eq!(filtered.len(), p.len() - 2);
        for t in &filtered {
            assert!(area_local(&p, *t) > 1e-12, "degenerate triangle {:?}", t);
        }
    }

    #[test]
    fn near_collinear_sliver_is_rejected() {
        let p = vec![[0.0, 0.0, 0.0], [1.0, 0.002, 0.0], [2.0, 0.0, 0.0]];
        assert!(tri_rejected(&p, 0, 1, 2, 0.0, 1.0));
        // The angle test is disabled at a zero margin.
        assert!(!tri_rejected(&p, 0, 1, 2, 0.0, 0.0));
        let q = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        assert!(!tri_rejected(&q, 0, 1, 2, 1e-12, 1.0));
    }

    #[test]
    fn above_cap_is_still_a_plain_fan() {
        let p: Vec<V3> = (0..5).map(|i| [i as f64, 0.0, 0.0]).collect();
        let f = HashSet::new();
        assert_eq!(
            min_area_triangulation(&p, 3, &f),
            min_area_triangulation_filtered(&p, 3, &f, 1.0, 5.0)
        );
    }
}
