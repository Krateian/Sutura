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

/// Triangulate the polygon `p` (indices local to `p`).  `forbidden` holds
/// sorted `(a, b)` chord pairs that must not be used.  Returns local triples,
/// or an empty vec when no valid triangulation exists.  Above `cap` a fan is
/// returned.
pub fn min_area_triangulation(p: &[V3], cap: usize, forbidden: &HashSet<(u32, u32)>) -> Vec<[u32; 3]> {
    let m = p.len();
    let forb = |a: usize, b: usize| -> bool {
        let (x, y) = if a < b { (a, b) } else { (b, a) };
        forbidden.contains(&(x as u32, y as u32))
    };
    if m < 3 {
        return Vec::new();
    }
    if m == 3 {
        return if forb(0, 2) {
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
