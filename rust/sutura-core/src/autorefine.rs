// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Autorefine: snap-rounding self-intersection resolution.
//!
//! Bit-parity port of `sutura/autorefine.py` (Lazard & Valque 2025 plus the
//! CGAL 6.1 iterative snap-rounding second half).  The opt-in
//! `--experimental-autorefine` path calls [`autorefine`]; the default pipeline
//! never does.  It never deletes an input face: each triangle is kept or split
//! along its proper-intersection segments.

use std::collections::{HashMap, HashSet};

use sutura_geom::predicates3d::orient3d_adaptive_coords;

pub const MAX_ITERATIONS: usize = 5;
pub const DEFAULT_SNAP_BITS: i32 = 23;
pub const MAX_SNAP_ROUNDS: usize = 8;
pub const MAX_FACES_GROWTH: usize = 4;
const WELD_TOL_REL: f64 = 1e-5;
const INNER_WELD_TOL_REL: f64 = 2e-5;

type Pt = [f64; 3];
type Pt2 = [f64; 2];
type Tri = [usize; 3];

fn sub(a: Pt, b: Pt) -> Pt {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: Pt, b: Pt) -> Pt {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: Pt, k: f64) -> Pt {
    [a[0] * k, a[1] * k, a[2] * k]
}

fn dot(a: Pt, b: Pt) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: Pt, b: Pt) -> Pt {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn norm(a: Pt) -> f64 {
    dot(a, a).sqrt()
}

fn allclose(a: Pt, b: Pt, atol: f64) -> bool {
    (0..3).all(|k| (a[k] - b[k]).abs() <= atol + 1e-5 * b[k].abs())
}

fn allclose2(a: Pt2, b: Pt2, atol: f64) -> bool {
    (0..2).all(|k| (a[k] - b[k]).abs() <= atol + 1e-5 * b[k].abs())
}

/// Python's built-in `min(a, b, c)`: a NaN first operand stays NaN, but a
/// later NaN never wins a comparison.
fn min3(a: f64, b: f64, c: f64) -> f64 {
    let mut m = a;
    if b < m {
        m = b;
    }
    if c < m {
        m = c;
    }
    m
}

/// `np.round`: round-half-to-even, component-wise.
fn py_round3(a: Pt) -> Pt {
    [
        a[0].round_ties_even(),
        a[1].round_ties_even(),
        a[2].round_ties_even(),
    ]
}

fn aabbs(v: &[Pt], t: &[Tri]) -> (Vec<Pt>, Vec<Pt>) {
    let mut mins = Vec::with_capacity(t.len());
    let mut maxs = Vec::with_capacity(t.len());
    for tri in t {
        let p = v[tri[0]];
        let q = v[tri[1]];
        let r = v[tri[2]];
        mins.push([
            p[0].min(q[0]).min(r[0]),
            p[1].min(q[1]).min(r[1]),
            p[2].min(q[2]).min(r[2]),
        ]);
        maxs.push([
            p[0].max(q[0]).max(r[0]),
            p[1].max(q[1]).max(r[1]),
            p[2].max(q[2]).max(r[2]),
        ]);
    }
    (mins, maxs)
}

/// Mirror of `autorefine.broad_phase_candidates`.
fn broad_phase_candidates(v: &[Pt], t: &[Tri]) -> Vec<(usize, usize)> {
    if t.is_empty() {
        return Vec::new();
    }
    let (mins, maxs) = aabbs(v, t);
    let mut diags: Vec<f64> = mins
        .iter()
        .zip(maxs.iter())
        .map(|(lo, hi)| {
            let dx = hi[0] - lo[0];
            let dy = hi[1] - lo[1];
            let dz = hi[2] - lo[2];
            (dx * dx + dy * dy + dz * dz).sqrt()
        })
        .collect();
    diags.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let m = t.len();
    let median = if m % 2 == 0 {
        (diags[m / 2 - 1] + diags[m / 2]) / 2.0
    } else {
        diags[m / 2]
    };
    let mut cell = median * 1.5;
    if !(cell > 0.0) {
        cell = 1.0;
    }
    if cell < 1e-9 {
        cell = 1e-9;
    }

    let mut entries: Vec<(i64, usize)> = Vec::new();
    for (f, _) in t.iter().enumerate() {
        let c0 = [
            (mins[f][0] / cell).floor() as i64,
            (mins[f][1] / cell).floor() as i64,
            (mins[f][2] / cell).floor() as i64,
        ];
        let c1 = [
            (maxs[f][0] / cell).floor() as i64,
            (maxs[f][1] / cell).floor() as i64,
            (maxs[f][2] / cell).floor() as i64,
        ];
        let ncx = (c1[0] - c0[0] + 1).max(0);
        let ncy = (c1[1] - c0[1] + 1).max(0);
        let ncz = (c1[2] - c0[2] + 1).max(0);
        for x in 0..ncx {
            for y in 0..ncy {
                for z in 0..ncz {
                    let key = (c0[0] + x)
                        .wrapping_mul(1 << 21)
                        .wrapping_mul(1 << 21)
                        .wrapping_add((c0[1] + y).wrapping_mul(1 << 21))
                        .wrapping_add(c0[2] + z);
                    entries.push((key, f));
                }
            }
        }
    }
    entries.sort_by_key(|(key, _)| *key);

    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < entries.len() {
        let key = entries[start].0;
        let mut end = start + 1;
        while end < entries.len() && entries[end].0 == key {
            end += 1;
        }
        let grp: Vec<usize> = entries[start..end].iter().map(|e| e.1).collect();
        for x in 0..grp.len() {
            for y in (x + 1)..grp.len() {
                let (i, j) = if grp[x] < grp[y] {
                    (grp[x], grp[y])
                } else {
                    (grp[y], grp[x])
                };
                if !seen.insert((i, j)) {
                    continue;
                }
                let (lo_i, hi_i) = (&mins[i], &maxs[i]);
                let (lo_j, hi_j) = (&mins[j], &maxs[j]);
                if lo_i[0] <= hi_j[0]
                    && lo_j[0] <= hi_i[0]
                    && lo_i[1] <= hi_j[1]
                    && lo_j[1] <= hi_i[1]
                    && lo_i[2] <= hi_j[2]
                    && lo_j[2] <= hi_i[2]
                {
                    out.push((i, j));
                }
            }
        }
        start = end;
    }
    out
}

fn plane_line_point(n1: Pt, d1: f64, n2: Pt, d2: f64) -> Pt {
    let n11 = dot(n1, n1);
    let n12 = dot(n1, n2);
    let n22 = dot(n2, n2);
    let mut det = n11 * n22 - n12 * n12;
    if det.abs() < 1e-300 {
        det = 1.0;
    }
    let alpha = (d1 * n22 - d2 * n12) / det;
    let beta = (n11 * d2 - n12 * d1) / det;
    add(scale(n1, alpha), scale(n2, beta))
}

fn project_to_2d(tri: [Pt; 3], seg: (Pt, Pt)) -> ([Pt2; 3], (Pt2, Pt2)) {
    let a = tri[0];
    let b = tri[1];
    let c = tri[2];
    let n = cross(sub(b, a), sub(c, a));
    let abs = [n[0].abs(), n[1].abs(), n[2].abs()];
    let axis = if abs[0] >= abs[1] && abs[0] >= abs[2] {
        0
    } else if abs[1] >= abs[2] {
        1
    } else {
        2
    };
    let keep: Vec<usize> = (0..3).filter(|&k| k != axis).collect();
    let proj = |p: Pt| -> Pt2 { [p[keep[0]], p[keep[1]]] };
    (
        [proj(a), proj(b), proj(c)],
        (proj(seg.0), proj(seg.1)),
    )
}

fn clip_segment_2d(poly: &[Pt2], seg: (Pt2, Pt2)) -> Option<(Pt2, Pt2)> {
    let (p0, p1) = seg;
    let mut u0 = 0.0f64;
    let mut u1 = 1.0f64;
    let d = [p1[0] - p0[0], p1[1] - p0[1]];
    for k in 0..poly.len() {
        let a = poly[k];
        let b = poly[(k + 1) % poly.len()];
        let e = [b[0] - a[0], b[1] - a[1]];
        let f0 = e[0] * (p0[1] - a[1]) - e[1] * (p0[0] - a[0]);
        let f1 = e[0] * (p1[1] - a[1]) - e[1] * (p1[0] - a[0]);
        if f0 < 0.0 && f1 < 0.0 {
            return None;
        }
        if f0 < 0.0 {
            u0 = u0.max(f0 / (f0 - f1));
        } else if f1 < 0.0 {
            u1 = u1.min(f0 / (f0 - f1));
        }
    }
    if u1 < u0 {
        return None;
    }
    let q0 = [p0[0] + u0 * d[0], p0[1] + u0 * d[1]];
    let q1 = [p0[0] + u1 * d[0], p0[1] + u1 * d[1]];
    Some((q0, q1))
}

fn polygon_from_triangle(poly: &[Pt2]) -> Vec<Pt2> {
    let mut area = 0.0f64;
    for k in 0..poly.len() {
        let a = poly[k];
        let b = poly[(k + 1) % poly.len()];
        area += a[0] * b[1] - b[0] * a[1];
    }
    if area < 0.0 {
        poly.iter().rev().copied().collect()
    } else {
        poly.to_vec()
    }
}

fn insert_point_into_poly(pts: &[Pt2], pp: Pt2) -> Vec<Pt2> {
    let mut out: Vec<Pt2> = Vec::new();
    let mut inserted = false;
    let n = pts.len();
    for k in 0..n {
        let a = pts[k];
        let b = pts[(k + 1) % n];
        if !inserted {
            let ab = [b[0] - a[0], b[1] - a[1]];
            if ab[0] * ab[0] + ab[1] * ab[1] < 1e-24 {
                out.push(a);
                continue;
            }
            let t = ((pp[0] - a[0]) * ab[0] + (pp[1] - a[1]) * ab[1])
                / (ab[0] * ab[0] + ab[1] * ab[1]);
            let proj = [a[0] + t * ab[0], a[1] + t * ab[1]];
            let dist = ((pp[0] - proj[0]).powi(2) + (pp[1] - proj[1]).powi(2)).sqrt();
            if (-1e-9..=1.0 + 1e-9).contains(&t) && dist <= 1e-9 {
                if t > 1e-9 {
                    out.push(a);
                }
                out.push(proj);
                inserted = true;
                continue;
            }
        }
        out.push(a);
    }
    if !inserted {
        out.push(pp);
    }
    let mut res: Vec<Pt2> = Vec::new();
    for x in out {
        if res
            .last()
            .map(|last| !allclose2(*last, x, 1e-9))
            .unwrap_or(true)
        {
            res.push(x);
        }
    }
    if res.len() > 1 && allclose2(res[0], res[res.len() - 1], 1e-9) {
        res.pop();
    }
    res
}

fn split_polygon_by_chord(poly: &[Pt2], p: Pt2, q: Pt2) -> Vec<Vec<Pt2>> {
    let poly = insert_point_into_poly(poly, p);
    let poly = insert_point_into_poly(&poly, q);
    let index_of = |pts: &[Pt2], pp: Pt2| -> Option<usize> {
        pts.iter().position(|x| allclose2(*x, pp, 1e-9))
    };
    let kp = index_of(&poly, p);
    let kq = index_of(&poly, q);
    let (kp, kq) = match (kp, kq) {
        (Some(a), Some(b)) if a != b => (a, b),
        _ => return vec![poly],
    };
    let n = poly.len();
    let mut fwd = vec![poly[kp]];
    let mut k = (kp + 1) % n;
    while k != kq {
        fwd.push(poly[k]);
        k = (k + 1) % n;
    }
    fwd.push(poly[kq]);
    let mut bwd = vec![poly[kp]];
    let mut k = (kp + n - 1) % n;
    while k != kq {
        bwd.push(poly[k]);
        k = (k + n - 1) % n;
    }
    bwd.push(poly[kq]);
    vec![polygon_from_triangle(&fwd), polygon_from_triangle(&bwd)]
}

fn fan_triangulate(poly: &[Pt2]) -> Vec<[Pt2; 3]> {
    if poly.len() < 3 {
        return Vec::new();
    }
    (1..poly.len() - 1)
        .map(|k| [poly[0], poly[k], poly[k + 1]])
        .collect()
}

fn subdivide_triangle_2d(
    tri2: [Pt2; 3],
    chords: &[(Pt2, Pt2)],
    boundary_points: &[Pt2],
) -> Vec<[Pt2; 3]> {
    let mut polys = vec![polygon_from_triangle(&tri2)];
    for bp in boundary_points {
        let mut next = Vec::new();
        for poly in &polys {
            let inserted = insert_point_into_poly(poly, *bp);
            if inserted.len() != poly.len() {
                next.push(inserted);
            } else {
                next.push(poly.clone());
            }
        }
        polys = next;
    }
    for (p, q) in chords {
        let seg = (*p, *q);
        let mut next = Vec::new();
        for poly in &polys {
            let clipped = match clip_segment_2d(poly, seg) {
                Some(c) => c,
                None => {
                    next.push(poly.clone());
                    continue;
                }
            };
            if allclose2(clipped.0, clipped.1, 1e-12) {
                next.push(poly.clone());
                continue;
            }
            let split = split_polygon_by_chord(poly, clipped.0, clipped.1);
            if split.len() == 1 && split[0].as_slice() == poly.as_slice() {
                next.push(poly.clone());
            } else {
                next.extend(split);
            }
        }
        polys = next;
    }
    let mut out = Vec::new();
    for poly in &polys {
        out.extend(fan_triangulate(poly));
    }
    out
}

fn segment_intersection_3d(tri1: [Pt; 3], tri2: [Pt; 3], fast: bool) -> Option<(Pt, Pt)> {
    let (a, b, c) = (tri1[0], tri1[1], tri1[2]);
    let (d, e, f) = (tri2[0], tri2[1], tri2[2]);
    let n1 = cross(sub(b, a), sub(c, a));
    let n2 = cross(sub(e, d), sub(f, d));
    let nn = cross(n1, n2);
    if dot(nn, nn) < 1e-30 {
        return None;
    }
    let s = [
        orient3d_adaptive_coords(a, b, c, tri2[0]),
        orient3d_adaptive_coords(a, b, c, tri2[1]),
        orient3d_adaptive_coords(a, b, c, tri2[2]),
    ];
    let t = [
        orient3d_adaptive_coords(d, e, f, tri1[0]),
        orient3d_adaptive_coords(d, e, f, tri1[1]),
        orient3d_adaptive_coords(d, e, f, tri1[2]),
    ];
    if s.iter().all(|x| *x > 0.0) || s.iter().all(|x| *x < 0.0) {
        return None;
    }
    if t.iter().all(|x| *x > 0.0) || t.iter().all(|x| *x < 0.0) {
        return None;
    }
    let d1 = dot(n1, a);
    let d2 = dot(n2, d);
    let n11 = dot(n1, n1);
    let n12 = dot(n1, n2);
    let n22 = dot(n2, n2);
    let det = n11 * n22 - n12 * n12;
    if det.abs() < 1e-300 {
        return None;
    }
    let alpha = (d1 * n22 - d2 * n12) / det;
    let beta = (n11 * d2 - n12 * d1) / det;
    let o = add(scale(n1, alpha), scale(n2, beta));
    let u0 = cross(n1, n2);
    let un = norm(u0);
    let u = [u0[0] / un, u0[1] / un, u0[2] / un];

    let triangle_plane_intersection =
        |tri: [Pt; 3], plane: [Pt; 3]| -> Option<Vec<Pt>> {
            let mut pts: Vec<Pt> = Vec::new();
            for k in 0..3 {
                let p = tri[k];
                let q = tri[(k + 1) % 3];
                let sp = orient3d_adaptive_coords(plane[0], plane[1], plane[2], p);
                let sq = orient3d_adaptive_coords(plane[0], plane[1], plane[2], q);
                if sp == 0.0 {
                    pts.push(p);
                } else if sq == 0.0 {
                    pts.push(q);
                } else if (sp > 0.0) != (sq > 0.0) {
                    let w = -sp / (sq - sp);
                    pts.push(add(p, scale(sub(q, p), w)));
                }
            }
            let mut out: Vec<Pt> = Vec::new();
            for p in pts {
                if !out.iter().any(|q| allclose(p, *q, 1e-12)) {
                    out.push(p);
                }
            }
            if out.len() >= 2 {
                Some(out)
            } else {
                None
            }
        };

    let s1 = triangle_plane_intersection(tri1, tri2)?;
    let s2 = triangle_plane_intersection(tri2, tri1)?;
    let interval = |seg: &[Pt]| -> (f64, f64) {
        let ts: Vec<f64> = seg.iter().map(|p| dot(sub(*p, o), u)).collect();
        let lo = ts.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = ts.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        (lo, hi)
    };
    let (t1lo, t1hi) = interval(&s1);
    let (t2lo, t2hi) = interval(&s2);
    let lo = t1lo.max(t2lo);
    let hi = t1hi.min(t2hi);
    if hi - lo < 1e-12 {
        return None;
    }
    let p = add(o, scale(u, lo));
    let q = add(o, scale(u, hi));
    if fast {
        return Some((p, q));
    }
    let mid = scale(add(p, q), 0.5);
    for tri in [tri1, tri2] {
        let t2 = project_to_2d(tri, (mid, mid)).0;
        let m2 = (project_to_2d(tri, (mid, mid)).1).0;
        let v0 = [t2[1][0] - t2[0][0], t2[1][1] - t2[0][1]];
        let v1 = [t2[2][0] - t2[0][0], t2[2][1] - t2[0][1]];
        let v2 = [m2[0] - t2[0][0], m2[1] - t2[0][1]];
        let denom = v0[0] * v1[1] - v1[0] * v0[1];
        if denom.abs() < 1e-18 {
            return None;
        }
        let w0 = (v2[0] * v1[1] - v1[0] * v2[1]) / denom;
        let w1 = (v0[0] * v2[1] - v2[0] * v0[1]) / denom;
        let w2 = 1.0 - w0 - w1;
        if min3(w0, w1, w2) <= 1e-9 {
            return None;
        }
    }
    Some((p, q))
}

fn mid_inside(tris: &[[Pt; 3]], mids: &[Pt]) -> (Vec<bool>, Vec<bool>) {
    let mut inside = Vec::with_capacity(tris.len());
    let mut near = Vec::with_capacity(tris.len());
    for (tri, mid) in tris.iter().zip(mids.iter()) {
        let nrm = cross(sub(tri[1], tri[0]), sub(tri[2], tri[0]));
        let abs = [nrm[0].abs(), nrm[1].abs(), nrm[2].abs()];
        let axis = if abs[0] >= abs[1] && abs[0] >= abs[2] {
            0
        } else if abs[1] >= abs[2] {
            1
        } else {
            2
        };
        let keep: Vec<usize> = (0..3).filter(|&k| k != axis).collect();
        let proj = |p: Pt| -> Pt2 { [p[keep[0]], p[keep[1]]] };
        let a = proj(tri[0]);
        let b = proj(tri[1]);
        let c = proj(tri[2]);
        let m = proj(*mid);
        let v0 = [b[0] - a[0], b[1] - a[1]];
        let v1 = [c[0] - a[0], c[1] - a[1]];
        let v2 = [m[0] - a[0], m[1] - a[1]];
        let denom = v0[0] * v1[1] - v1[0] * v0[1];
        let w0 = (v2[0] * v1[1] - v1[0] * v2[1]) / denom;
        let w1 = (v0[0] * v2[1] - v2[0] * v0[1]) / denom;
        let w2 = 1.0 - w0 - w1;
        let min = if w0.is_nan() || w1.is_nan() || w2.is_nan() {
            f64::NAN
        } else {
            w0.min(w1).min(w2)
        };
        inside.push(min > 1e-9);
        near.push(min <= 1e-6);
    }
    (inside, near)
}

fn detect_pairs_with_segments(v: &[Pt], t: &[Tri]) -> Vec<(usize, usize, (Pt, Pt))> {
    let cand = broad_phase_candidates(v, t);
    if cand.is_empty() {
        return Vec::new();
    }
    let mut filtered: Vec<(usize, usize)> = Vec::with_capacity(cand.len());
    for (i, j) in cand {
        let ti = t[i];
        let tj = t[j];
        let share = ti.iter().any(|a| tj.iter().any(|b| a == b));
        if !share {
            filtered.push((i, j));
        }
    }
    if filtered.is_empty() {
        return Vec::new();
    }

    let mut keep: Vec<(usize, usize)> = Vec::with_capacity(filtered.len());
    for (i, j) in &filtered {
        let ta = [v[t[*i][0]], v[t[*i][1]], v[t[*i][2]]];
        let tb = [v[t[*j][0]], v[t[*j][1]], v[t[*j][2]]];
        let n1 = cross(sub(ta[1], ta[0]), sub(ta[2], ta[0]));
        let n2 = cross(sub(tb[1], tb[0]), sub(tb[2], tb[0]));
        let d1 = dot(n1, ta[0]);
        let d2 = dot(n2, tb[0]);
        let sd_b: Vec<f64> = tb.iter().map(|p| dot(n1, *p) - d1).collect();
        let sd_a: Vec<f64> = ta.iter().map(|p| dot(n2, *p) - d2).collect();
        let all_pos_b = sd_b.iter().all(|x| *x > 0.0);
        let all_neg_b = sd_b.iter().all(|x| *x < 0.0);
        let all_pos_a = sd_a.iter().all(|x| *x > 0.0);
        let all_neg_a = sd_a.iter().all(|x| *x < 0.0);
        if !(all_pos_b || all_neg_b || all_pos_a || all_neg_a) {
            keep.push((*i, *j));
        }
    }
    if keep.is_empty() {
        return Vec::new();
    }

    let mut aa: Vec<[Pt; 3]> = Vec::with_capacity(keep.len());
    let mut bb: Vec<[Pt; 3]> = Vec::with_capacity(keep.len());
    let mut sdb: Vec<[f64; 3]> = Vec::with_capacity(keep.len());
    let mut sda: Vec<[f64; 3]> = Vec::with_capacity(keep.len());
    let mut o: Vec<Pt> = Vec::with_capacity(keep.len());
    let mut uhat: Vec<Pt> = Vec::with_capacity(keep.len());
    let mut par: Vec<bool> = Vec::with_capacity(keep.len());
    for (i, j) in &keep {
        let ta = [v[t[*i][0]], v[t[*i][1]], v[t[*i][2]]];
        let tb = [v[t[*j][0]], v[t[*j][1]], v[t[*j][2]]];
        let n1 = cross(sub(ta[1], ta[0]), sub(ta[2], ta[0]));
        let n2 = cross(sub(tb[1], tb[0]), sub(tb[2], tb[0]));
        let d1 = dot(n1, ta[0]);
        let d2 = dot(n2, tb[0]);
        let sdb_i = [
            dot(n1, tb[0]) - d1,
            dot(n1, tb[1]) - d1,
            dot(n1, tb[2]) - d1,
        ];
        let sda_i = [
            dot(n2, ta[0]) - d2,
            dot(n2, ta[1]) - d2,
            dot(n2, ta[2]) - d2,
        ];
        let uu = cross(n1, n2);
        let len2 = dot(uu, uu);
        let is_par = len2 < 1e-24;
        let uh = if is_par {
            [1.0, 0.0, 0.0]
        } else {
            let l = len2.sqrt();
            [uu[0] / l, uu[1] / l, uu[2] / l]
        };
        aa.push(ta);
        bb.push(tb);
        sdb.push(sdb_i);
        sda.push(sda_i);
        o.push(plane_line_point(n1, d1, n2, d2));
        uhat.push(uh);
        par.push(is_par);
    }
    if par.iter().all(|p| *p) {
        return Vec::new();
    }

    let edge_cross_points = |tris: &[[Pt; 3]], sd: &[[f64; 3]]| -> (Vec<f64>, Vec<f64>, Vec<bool>) {
        let n = tris.len();
        let mut ts = vec![[f64::NAN; 3]; n];
        let mut valid = vec![false; n];
        for m in 0..3 {
            for k in 0..n {
                let s0 = sd[k][m];
                let s1 = sd[k][(m + 1) % 3];
                let crosses = s0 * s1 < 0.0;
                let denom = s1 - s0;
                let w = if denom.abs() > 1e-18 {
                    -s0 / denom
                } else {
                    0.0
                };
                if crosses {
                    valid[k] = true;
                    let p = add(tris[k][m], scale(sub(tris[k][(m + 1) % 3], tris[k][m]), w));
                    ts[k][m] = dot(sub(p, o[k]), uhat[k]);
                }
            }
            for k in 0..n {
                if sd[k][m].abs() < 1e-12 {
                    let p = tris[k][m];
                    valid[k] = true;
                    ts[k][m] = dot(sub(p, o[k]), uhat[k]);
                }
            }
        }
        let mut tmin = Vec::with_capacity(n);
        let mut tmax = Vec::with_capacity(n);
        for k in 0..n {
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for value in ts[k] {
                if !value.is_nan() {
                    lo = lo.min(value);
                    hi = hi.max(value);
                }
            }
            tmin.push(lo);
            tmax.push(hi);
            if lo.is_infinite() {
                valid[k] = false;
            }
        }
        (tmin, tmax, valid)
    };

    let (tmin_a, tmax_a, valid_a) = edge_cross_points(&aa, &sdb);
    let (tmin_b, tmax_b, valid_b) = edge_cross_points(&bb, &sda);
    let mut chosen: Vec<usize> = Vec::new();
    let mut mid: Vec<Pt> = Vec::new();
    let mut kk2: Vec<(usize, usize)> = Vec::new();
    for k in 0..keep.len() {
        let lo = tmin_a[k].max(tmin_b[k]);
        let hi = tmax_a[k].min(tmax_b[k]);
        if valid_a[k] && valid_b[k] && hi - lo > 1e-12 && !par[k] {
            let mid_t = (lo + hi) / 2.0;
            mid.push(add(o[k], scale(uhat[k], mid_t)));
            kk2.push(keep[k]);
            chosen.push(k);
        }
    }
    if kk2.is_empty() {
        return Vec::new();
    }

    let aa2: Vec<[Pt; 3]> = kk2
        .iter()
        .map(|(i, _)| [v[t[*i][0]], v[t[*i][1]], v[t[*i][2]]])
        .collect();
    let bb2: Vec<[Pt; 3]> = kk2
        .iter()
        .map(|(_, j)| [v[t[*j][0]], v[t[*j][1]], v[t[*j][2]]])
        .collect();
    let (in_a, near_a) = mid_inside(&aa2, &mid);
    let (in_b, near_b) = mid_inside(&bb2, &mid);

    let mut out = Vec::new();
    let certain: Vec<bool> = (0..kk2.len())
        .map(|k| in_a[k] && in_b[k] && !near_a[k] && !near_b[k])
        .collect();
    for (k, &is_certain) in certain.iter().enumerate() {
        if is_certain {
            let (i, j) = kk2[k];
            if let Some(seg) = segment_intersection_3d(aa2[k], bb2[k], true) {
                out.push((i, j, seg));
            }
        }
    }
    for (k, &is_certain) in certain.iter().enumerate() {
        if is_certain {
            continue;
        }
        let (i, j) = kk2[k];
        if let Some(seg) = segment_intersection_3d(aa2[k], bb2[k], false) {
            out.push((i, j, seg));
        }
    }
    out
}

fn grid_scale(v: &[Pt], bits: i32) -> f64 {
    let mut m = 0.0f64;
    for p in v {
        for c in p {
            m = m.max(c.abs());
        }
    }
    if m <= 0.0 {
        return 1.0;
    }
    let mut s = 1.0f64;
    while m * s < 2.0f64.powi(bits) {
        s *= 2.0;
    }
    while m * s >= 2.0f64.powi(bits + 1) {
        s /= 2.0;
    }
    s
}

fn snap_round(v: &[Pt], t: &[Tri], pairs: &[(usize, usize, (Pt, Pt))], bits: i32) -> Vec<Pt> {
    let mut out = v.to_vec();
    let s = grid_scale(v, bits);
    let cells: Vec<[i64; 3]> = v
        .iter()
        .map(|p| {
            [
                (p[0] * s).floor() as i64,
                (p[1] * s).floor() as i64,
                (p[2] * s).floor() as i64,
            ]
        })
        .collect();
    let mut to_snap: HashSet<usize> = HashSet::new();
    for (i, j, _) in pairs {
        for idx in t[*i] {
            to_snap.insert(idx);
        }
        for idx in t[*j] {
            to_snap.insert(idx);
        }
    }
    if to_snap.is_empty() {
        return out;
    }
    let snapped_cells: HashSet<[i64; 3]> = to_snap.iter().map(|&idx| cells[idx]).collect();
    for (idx, p) in out.iter_mut().enumerate() {
        if snapped_cells.contains(&cells[idx]) {
            let scaled = py_round3([p[0] * s, p[1] * s, p[2] * s]);
            *p = [scaled[0] / s, scaled[1] / s, scaled[2] / s];
        }
    }
    out
}

fn weld(verts: &[Pt], tris: &[Tri], tol: Option<f64>) -> (Vec<Pt>, Vec<Tri>) {
    if verts.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let tol = tol.unwrap_or_else(|| {
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for p in verts {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let span = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(hi[2] - lo[2]);
        WELD_TOL_REL * span.max(1e-300)
    });
    let cell = tol.max(1e-300);
    let mut lut: HashMap<(i64, i64, i64), Vec<usize>> = HashMap::new();
    let mut new_verts: Vec<Pt> = Vec::new();
    let mut remap: Vec<usize> = Vec::with_capacity(verts.len());
    for (i, p) in verts.iter().enumerate() {
        let k = [
            (p[0] / cell).floor() as i64,
            (p[1] / cell).floor() as i64,
            (p[2] / cell).floor() as i64,
        ];
        let mut found = None;
        'outer: for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let ck = (k[0] + dx, k[1] + dy, k[2] + dz);
                    if let Some(list) = lut.get(&ck) {
                        for &j in list {
                            if norm(sub(new_verts[j], *p)) <= tol {
                                found = Some(j);
                                break 'outer;
                            }
                        }
                    }
                }
            }
        }
        if let Some(j) = found {
            remap.push(j);
        } else {
            let idx = new_verts.len();
            new_verts.push(*p);
            remap.push(idx);
            lut.entry((k[0], k[1], k[2])).or_default().push(idx);
        }
        let _ = i;
    }
    let mut new_tris = Vec::new();
    for tri in tris {
        let nt = [remap[tri[0]], remap[tri[1]], remap[tri[2]]];
        if nt[0] != nt[1] && nt[1] != nt[2] && nt[0] != nt[2] {
            new_tris.push(nt);
        }
    }
    (new_verts, new_tris)
}

fn eliminate_degenerate(v: &[Pt], t: &[Tri], tol_rel: f64) -> (Vec<Pt>, Vec<Tri>) {
    if t.is_empty() {
        return (v.to_vec(), Vec::new());
    }
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in v {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let span = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(hi[2] - lo[2]);
    let (wv, wt) = weld(v, t, Some(tol_rel * span.max(1e-300)));
    if wt.is_empty() {
        return (wv, wt);
    }
    let floor = 1e-12 * (span * span).max(1e-300);
    let mut kept: Vec<Tri> = Vec::new();
    for tri in &wt {
        let a = wv[tri[0]];
        let b = wv[tri[1]];
        let c = wv[tri[2]];
        if norm(cross(sub(b, a), sub(c, a))) > floor {
            kept.push(*tri);
        }
    }
    if kept.is_empty() {
        return (wv, kept);
    }
    let mut seen: HashSet<[usize; 3]> = HashSet::new();
    let mut unique: Vec<Tri> = Vec::new();
    for tri in &kept {
        let mut key = *tri;
        key.sort();
        if seen.insert(key) {
            unique.push(*tri);
        }
    }
    (wv, unique)
}

fn subdivide(
    v: &[Pt],
    t: &[Tri],
    pairs: &[(usize, usize, (Pt, Pt))],
    snap_bits: Option<i32>,
) -> (Vec<Pt>, Vec<Tri>) {
    let gs = snap_bits.map(|bits| grid_scale(v, bits)).unwrap_or(1.0);
    let mut segs_per_tri: HashMap<usize, Vec<(Pt, Pt)>> = HashMap::new();
    for (i, j, seg) in pairs {
        segs_per_tri.entry(*i).or_default().push(*seg);
        segs_per_tri.entry(*j).or_default().push(*seg);
    }
    let edge_key = |vi: usize, vj: usize| -> (usize, usize) {
        if vi < vj {
            (vi, vj)
        } else {
            (vj, vi)
        }
    };
    let mut edge_splits: HashMap<(usize, usize), Vec<Pt>> = HashMap::new();
    for (i, j, seg) in pairs {
        for k in [*i, *j] {
            let tri = t[k];
            for p in [seg.0, seg.1] {
                for m in 0..3 {
                    let a = v[tri[m]];
                    let b = v[tri[(m + 1) % 3]];
                    let ab = sub(b, a);
                    if dot(ab, ab) < 1e-24 {
                        continue;
                    }
                    let tt = dot(sub(p, a), ab) / dot(ab, ab);
                    let proj = add(a, scale(ab, tt));
                    if (-1e-7..=1.0 + 1e-7).contains(&tt) && norm(sub(p, proj)) <= 1e-7 {
                        edge_splits
                            .entry(edge_key(tri[m], tri[(m + 1) % 3]))
                            .or_default()
                            .push(p);
                        break;
                    }
                }
            }
        }
    }

    let mut coords: Vec<Pt> = Vec::new();
    let mut tri_list: Vec<Tri> = Vec::new();
    for fi in 0..t.len() {
        let tri3 = [v[t[fi][0]], v[t[fi][1]], v[t[fi][2]]];
        let segs = segs_per_tri.get(&fi).cloned().unwrap_or_default();
        let mut boundary: Vec<Pt> = Vec::new();
        for m in 0..3 {
            let ek = edge_key(t[fi][m], t[fi][(m + 1) % 3]);
            if let Some(list) = edge_splits.get(&ek) {
                boundary.extend(list.iter().copied());
            }
        }
        if segs.is_empty() && boundary.is_empty() {
            let base = coords.len();
            coords.extend(tri3);
            tri_list.push([base, base + 1, base + 2]);
            continue;
        }
        let (tri2, _) = project_to_2d(tri3, (tri3[0], tri3[1]));
        let mut chords = Vec::with_capacity(segs.len());
        for (p, q) in &segs {
            let (_, seg2) = project_to_2d(tri3, (*p, *q));
            chords.push(seg2);
        }
        let mut boundary2 = Vec::with_capacity(boundary.len());
        for bp in &boundary {
            let (_, bp2) = project_to_2d(tri3, (*bp, *bp));
            boundary2.push(bp2.0);
        }
        let subs = subdivide_triangle_2d(tri2, &chords, &boundary2);
        for sub in subs {
            let mut idxs: Vec<usize> = Vec::new();
            for p2 in sub {
                let v0 = [tri2[1][0] - tri2[0][0], tri2[1][1] - tri2[0][1]];
                let v1 = [tri2[2][0] - tri2[0][0], tri2[2][1] - tri2[0][1]];
                let vv = [p2[0] - tri2[0][0], p2[1] - tri2[0][1]];
                let denom = v0[0] * v1[1] - v1[0] * v0[1];
                if denom.abs() < 1e-18 {
                    idxs.clear();
                    break;
                }
                let w0 = (vv[0] * v1[1] - v1[0] * vv[1]) / denom;
                let w1 = (v0[0] * vv[1] - vv[0] * v0[1]) / denom;
                let w2 = 1.0 - w0 - w1;
                let mut p3 = add(
                    add(scale(tri3[0], w2), scale(tri3[1], w0)),
                    scale(tri3[2], w1),
                );
                if gs != 1.0 {
                    let rounded = py_round3([p3[0] * gs, p3[1] * gs, p3[2] * gs]);
                    p3 = [rounded[0] / gs, rounded[1] / gs, rounded[2] / gs];
                }
                let base = coords.len();
                coords.push(p3);
                idxs.push(base);
            }
            if idxs.len() == 3 {
                tri_list.push([idxs[0], idxs[1], idxs[2]]);
            }
        }
    }
    weld(&coords, &tri_list, None)
}

fn iterative_snap_round(
    v: &[Pt],
    t: &[Tri],
    snap_bits: i32,
    max_rounds: usize,
) -> (Vec<Pt>, Vec<Tri>, usize) {
    let mut v = v.to_vec();
    let mut t = t.to_vec();
    let mut rounds = 0usize;
    let mut prev: Option<usize> = None;
    for r in 0..max_rounds {
        let pairs = detect_pairs_with_segments(&v, &t);
        if pairs.is_empty() {
            break;
        }
        rounds = r + 1;
        let cur = pairs.len();
        if let Some(p) = prev {
            if cur >= p {
                break;
            }
        }
        prev = Some(cur);
        v = snap_round(&v, &t, &pairs, snap_bits);
        let (nv, nt) = eliminate_degenerate(&v, &t, INNER_WELD_TOL_REL);
        v = nv;
        t = nt;
    }
    (v, t, rounds)
}

/// `autorefine(v, t)`; returns the processed mesh and the Python-shaped report.
pub fn autorefine(v: &[[f64; 3]], t: &[[i64; 3]]) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, serde_json::Value) {
    use serde_json::{json, Value};
    let mut vv: Vec<Pt> = v.to_vec();
    let mut tt: Vec<Tri> = t
        .iter()
        .map(|r| [r[0] as usize, r[1] as usize, r[2] as usize])
        .collect();
    let faces_before = tt.len();
    let mut report = json!({
        "iterations": 0,
        "si_before": 0,
        "si_after": 0,
        "faces_before": faces_before,
        "faces_after": faces_before,
        "converged": false,
        "capped": false,
        "snap_rounds_per_iteration": [],
        "grid_scales_used": [],
    });
    let pairs = detect_pairs_with_segments(&vv, &tt);
    report["si_before"] = Value::from(pairs.len() as i64);
    if pairs.is_empty() {
        report["converged"] = Value::Bool(true);
        report["si_after"] = Value::from(0i64);
        return (
            vv,
            tt.iter().map(|r| [r[0] as i64, r[1] as i64, r[2] as i64]).collect(),
            report,
        );
    }
    let mut prev_pairs = pairs.len() + 1;
    for it in 0..MAX_ITERATIONS {
        let pairs = detect_pairs_with_segments(&vv, &tt);
        if pairs.is_empty() {
            report["converged"] = Value::Bool(true);
            break;
        }
        if pairs.len() >= prev_pairs {
            break;
        }
        prev_pairs = pairs.len();
        let (nv, nt, rounds) =
            iterative_snap_round(&vv, &tt, DEFAULT_SNAP_BITS, MAX_SNAP_ROUNDS);
        vv = nv;
        tt = nt;
        report["iterations"] = Value::from(it as i64 + 1);
        if let Some(list) = report["snap_rounds_per_iteration"].as_array_mut() {
            list.push(Value::from(rounds as i64));
        }
        if let Some(list) = report["grid_scales_used"].as_array_mut() {
            list.push(Value::from(DEFAULT_SNAP_BITS));
        }
        let pairs = detect_pairs_with_segments(&vv, &tt);
        if pairs.is_empty() {
            report["converged"] = Value::Bool(true);
            break;
        }
        if tt.len() > faces_before * MAX_FACES_GROWTH {
            report["capped"] = Value::Bool(true);
            break;
        }
        let (nv, nt) = subdivide(&vv, &tt, &pairs, Some(DEFAULT_SNAP_BITS));
        let (nv, nt) = eliminate_degenerate(&nv, &nt, INNER_WELD_TOL_REL);
        vv = nv;
        tt = nt;
        if tt.len() > faces_before * MAX_FACES_GROWTH {
            report["capped"] = Value::Bool(true);
            break;
        }
    }
    let pairs = detect_pairs_with_segments(&vv, &tt);
    if pairs.is_empty() {
        report["converged"] = Value::Bool(true);
    }
    report["si_after"] = Value::from(pairs.len() as i64);
    report["faces_after"] = Value::from(tt.len() as i64);
    (
        vv,
        tt.iter().map(|r| [r[0] as i64, r[1] as i64, r[2] as i64]).collect(),
        report,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crossing_pair() -> (Vec<Pt>, Vec<Tri>) {
        (
            vec![
                [0.0, 0.0, 0.0],
                [2.0, 0.0, 0.0],
                [0.0, 2.0, 0.0],
                [0.5, 0.2, -1.0],
                [0.5, 0.2, 1.0],
                [0.5, 1.4, -0.2],
            ],
            vec![[0, 1, 2], [3, 4, 5]],
        )
    }

    #[test]
    fn detects_one_proper_pair() {
        let (v, t) = crossing_pair();
        let pairs = detect_pairs_with_segments(&v, &t);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, 0);
        assert_eq!(pairs[0].1, 1);
    }

    #[test]
    fn resolves_the_pair_by_subdivision() {
        let (v, t) = crossing_pair();
        let (_ov, ot, rep) = autorefine(
            &v,
            &t.iter()
                .map(|r| [r[0] as i64, r[1] as i64, r[2] as i64])
                .collect::<Vec<_>>(),
        );
        assert_eq!(rep["si_before"].as_i64(), Some(1));
        assert_eq!(rep["si_after"].as_i64(), Some(0));
        assert_eq!(rep["iterations"].as_i64(), Some(1));
        assert_eq!(rep["snap_rounds_per_iteration"][0].as_i64(), Some(2));
        assert_eq!(ot.len(), 6);
    }

    #[test]
    fn empty_input_is_reported_not_panicking() {
        let (v, t, rep) = autorefine(&[], &[]);
        assert!(v.is_empty());
        assert!(t.is_empty());
        assert_eq!(rep["si_before"].as_i64(), Some(0));
        assert_eq!(rep["converged"].as_bool(), Some(true));
    }
}
