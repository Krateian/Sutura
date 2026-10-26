// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Exact B5 certificate: every PLC triangle is covered by recovered tet faces.
//!
//! The missing-face test only proves that no tet edge pierces a PLC triangle.
//! This module proves the stronger property directly: the unique tet faces
//! that are coplanar with a PLC triangle and lie inside it are interior
//! disjoint and their exact rational area sums to the exact rational area of
//! the PLC triangle.  A gap would make the sum smaller; an overlap is rejected
//! explicitly.  All decisions use `BigRational`; no tolerance is involved.

use crate::cdt3d::dt::sorted_face;
use crate::cdt3d::plc::Plc;
use crate::point::{f64_to_rat, Point3};
use crate::predicates3d::orient3d_sign;
use num_rational::BigRational;
use num_traits::{Signed, Zero};
use std::collections::{BTreeMap, BTreeSet};

type P2 = [BigRational; 2];
type Tri2 = [P2; 3];

struct Cand {
    key: [usize; 3],
    tri: Tri2,
    lo: [f64; 3],
    hi: [f64; 3],
    area2: BigRational,
}

#[derive(Clone)]
struct Grid2 {
    origin: [f64; 2],
    inv: [f64; 2],
    res: [i64; 2],
    axes: (usize, usize),
}

impl Grid2 {
    fn new(lo: [f64; 3], hi: [f64; 3], axes: (usize, usize), count: usize) -> Self {
        let target = (count.max(1) as f64).sqrt().ceil().clamp(1.0, 512.0) as i64;
        let mut origin = [0.0; 2];
        let mut inv = [0.0; 2];
        let mut res = [target; 2];
        for (k, &a) in [axes.0, axes.1].iter().enumerate() {
            origin[k] = lo[a];
            let extent = hi[a] - lo[a];
            if extent > 0.0 && extent.is_finite() {
                inv[k] = target as f64 / extent;
            } else {
                res[k] = 1;
                inv[k] = 0.0;
            }
        }
        Self {
            origin,
            inv,
            res,
            axes,
        }
    }

    fn cell(&self, p: [f64; 3], k: usize) -> i64 {
        let a = if k == 0 { self.axes.0 } else { self.axes.1 };
        if self.inv[k] == 0.0 {
            0
        } else {
            let c = ((p[a] - self.origin[k]) * self.inv[k]).floor() as i64;
            c.clamp(0, self.res[k] - 1)
        }
    }

    fn cells(&self, lo: [f64; 3], hi: [f64; 3]) -> Vec<[i64; 2]> {
        let (x0, x1) = (self.cell(lo, 0), self.cell(hi, 0));
        let (y0, y1) = (self.cell(lo, 1), self.cell(hi, 1));
        let mut out = Vec::new();
        for y in y0..=y1 {
            for x in x0..=x1 {
                out.push([x, y]);
            }
        }
        out
    }
}

/// Result of the exact area-coverage certificate.
#[derive(Clone, Debug, Default)]
pub struct CoverageReport {
    pub plc_triangles: usize,
    pub certified_triangles: usize,
    pub failed_triangles: usize,
    pub uncovered: usize,
    pub area_mismatch: usize,
    pub overlapping: usize,
    pub partial: usize,
    pub certified: bool,
    pub failures: Vec<usize>,
    pub constraint_faces: Vec<[usize; 3]>,
}

fn orient2(a: &P2, b: &P2, c: &P2) -> BigRational {
    (&b[0] - &a[0]) * (&c[1] - &a[1]) - (&b[1] - &a[1]) * (&c[0] - &a[0])
}

fn sign(x: &BigRational) -> i8 {
    if x.is_zero() {
        0
    } else if x.is_positive() {
        1
    } else {
        -1
    }
}

fn point_in_or_on(p: &P2, t: &Tri2) -> bool {
    let o = sign(&orient2(&t[0], &t[1], &t[2]));
    if o == 0 {
        return false;
    }
    [(&t[0], &t[1]), (&t[1], &t[2]), (&t[2], &t[0])]
        .iter()
        .all(|(a, b)| {
            let s = sign(&orient2(a, b, p));
            s == 0 || s == o
        })
}

fn point_strictly_inside(p: &P2, t: &Tri2) -> bool {
    let o = sign(&orient2(&t[0], &t[1], &t[2]));
    if o == 0 {
        return false;
    }
    [(&t[0], &t[1]), (&t[1], &t[2]), (&t[2], &t[0])]
        .iter()
        .all(|(a, b)| sign(&orient2(a, b, p)) == o)
}

fn centroid(t: &Tri2) -> P2 {
    let three = BigRational::from_integer(3.into());
    [
        (&t[0][0] + &t[1][0] + &t[2][0]) / &three,
        (&t[0][1] + &t[1][1] + &t[2][1]) / &three,
    ]
}

fn segments_properly_cross(a0: &P2, a1: &P2, b0: &P2, b1: &P2) -> bool {
    let o1 = sign(&orient2(a0, a1, b0));
    let o2 = sign(&orient2(a0, a1, b1));
    let o3 = sign(&orient2(b0, b1, a0));
    let o4 = sign(&orient2(b0, b1, a1));
    o1 * o2 < 0 && o3 * o4 < 0
}

fn triangles_overlap(a: &Tri2, b: &Tri2) -> bool {
    if point_strictly_inside(&centroid(a), b) || point_strictly_inside(&centroid(b), a) {
        return true;
    }
    for i in 0..3 {
        for j in 0..3 {
            if segments_properly_cross(
                &a[i],
                &a[(i + 1) % 3],
                &b[j],
                &b[(j + 1) % 3],
            ) {
                return true;
            }
        }
    }
    false
}

fn triangle_inside(a: &Tri2, b: &Tri2) -> bool {
    a.iter().all(|p| point_in_or_on(p, b))
}

fn area2(t: &Tri2) -> BigRational {
    orient2(&t[0], &t[1], &t[2]).abs()
}

fn project(q: &[BigRational; 3], drop: usize) -> P2 {
    match drop {
        0 => [q[1].clone(), q[2].clone()],
        1 => [q[0].clone(), q[2].clone()],
        _ => [q[0].clone(), q[1].clone()],
    }
}

fn aabb2(t: &[[f64; 3]], tri: &[usize; 3]) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for &v in tri {
        for a in 0..3 {
            lo[a] = lo[a].min(t[v][a]).next_down();
            hi[a] = hi[a].max(t[v][a]).next_up();
        }
    }
    (lo, hi)
}

fn aabb_positive_overlap(a0: &[f64; 3], a1: &[f64; 3], b0: &[f64; 3], b1: &[f64; 3]) -> bool {
    (0..3).all(|k| a0[k] < b1[k] && b0[k] < a1[k])
}

fn drop_axis(t: &[[f64; 3]; 3]) -> Result<usize, String> {
    let ab = [t[1][0] - t[0][0], t[1][1] - t[0][1], t[1][2] - t[0][2]];
    let ac = [t[2][0] - t[0][0], t[2][1] - t[0][1], t[2][2] - t[0][2]];
    let n = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    let axis = (0..3)
        .max_by(|&i, &j| n[i].abs().partial_cmp(&n[j].abs()).unwrap())
        .unwrap();
    if n[axis] == 0.0 {
        return Err("degenerate PLC triangle has no projection axis".into());
    }
    Ok(axis)
}

/// Exact coverage certificate for every PLC triangle.
pub fn certify_face_coverage(
    points: &[Point3],
    tets: &[[usize; 4]],
    plc: &Plc,
) -> Result<CoverageReport, String> {
    let mut report = CoverageReport {
        plc_triangles: plc.tris.len(),
        ..CoverageReport::default()
    };
    if plc.tris.is_empty() {
        report.certified = true;
        return Ok(report);
    }

    let approx: Vec<[f64; 3]> = points
        .iter()
        .map(|p| {
            p.to_f64()
                .ok_or_else(|| "point has no approximate coordinates".to_string())
        })
        .collect::<Result<_, _>>()?;
    let rat: Vec<Option<[BigRational; 3]>> = points.iter().map(|p| p.to_rational()).collect();

    let mut unique_faces: BTreeSet<[usize; 3]> = BTreeSet::new();
    for t in tets {
        for lv in 0..4 {
            unique_faces.insert(sorted_face(t, lv));
        }
    }
    let faces: Vec<[usize; 3]> = unique_faces.into_iter().collect();

    let mut vertex_faces: Vec<Vec<usize>> = vec![Vec::new(); points.len()];
    let mut edge_faces: BTreeMap<[usize; 2], Vec<usize>> = BTreeMap::new();
    for (ui, face) in faces.iter().enumerate() {
        for &v in face {
            vertex_faces[v].push(ui);
        }
        for k in 0..3 {
            let a = face[k];
            let b = face[(k + 1) % 3];
            let key = if a < b { [a, b] } else { [b, a] };
            edge_faces.entry(key).or_default().push(ui);
        }
    }
    for list in &mut vertex_faces {
        list.sort_unstable();
    }
    for list in edge_faces.values_mut() {
        list.sort_unstable();
        list.dedup();
    }
    let mut stamp = vec![0u32; faces.len()];
    let mut epoch = 0u32;
    let mut candidates: Vec<Cand> = Vec::new();
    let mut constraint_set: BTreeSet<[usize; 3]> = BTreeSet::new();
    let mut queue: Vec<usize> = Vec::new();
    for (fi, f) in plc.tris.iter().enumerate() {
        let fv = [
            Point3::Explicit(plc.verts[f[0]]),
            Point3::Explicit(plc.verts[f[1]]),
            Point3::Explicit(plc.verts[f[2]]),
        ];
        let drop = match drop_axis(&[plc.verts[f[0]], plc.verts[f[1]], plc.verts[f[2]]]) {
            Ok(a) => a,
            Err(_) => {
                report.failed_triangles += 1;
                report.failures.push(fi);
                continue;
            }
        };
        let fq = [
            [
                f64_to_rat(plc.verts[f[0]][0]),
                f64_to_rat(plc.verts[f[0]][1]),
                f64_to_rat(plc.verts[f[0]][2]),
            ],
            [
                f64_to_rat(plc.verts[f[1]][0]),
                f64_to_rat(plc.verts[f[1]][1]),
                f64_to_rat(plc.verts[f[1]][2]),
            ],
            [
                f64_to_rat(plc.verts[f[2]][0]),
                f64_to_rat(plc.verts[f[2]][1]),
                f64_to_rat(plc.verts[f[2]][2]),
            ],
        ];
        let f2: Tri2 = [
            project(&fq[0], drop),
            project(&fq[1], drop),
            project(&fq[2], drop),
        ];
        let mut flo = [f64::INFINITY; 3];
        let mut fhi = [f64::NEG_INFINITY; 3];
        for k in 0..3 {
            for a in 0..3 {
                flo[a] = flo[a].min(plc.verts[f[k]][a]);
                fhi[a] = fhi[a].max(plc.verts[f[k]][a]);
            }
        }
        for a in 0..3 {
            flo[a] = flo[a].next_down();
            fhi[a] = fhi[a].next_up();
        }

        epoch = epoch.wrapping_add(1);
        candidates.clear();
        queue.clear();
        let mut partial = false;

        let mut consider = |ui: usize,
                            candidates: &mut Vec<Cand>,
                            queue: &mut Vec<usize>,
                            partial: &mut bool|
         -> Result<(), String> {
            if stamp[ui] == epoch {
                return Ok(());
            }
            stamp[ui] = epoch;
            let face = faces[ui];
            for &v in &face {
                if orient3d_sign(&fv[0], &fv[1], &fv[2], &points[v]) != 0.0 {
                    return Ok(());
                }
            }
            let Some(q0) = rat[face[0]].as_ref() else {
                return Err("point has no exact coordinates".into());
            };
            let Some(q1) = rat[face[1]].as_ref() else {
                return Err("point has no exact coordinates".into());
            };
            let Some(q2) = rat[face[2]].as_ref() else {
                return Err("point has no exact coordinates".into());
            };
            let t2: Tri2 = [project(q0, drop), project(q1, drop), project(q2, drop)];
            let (clo, chi) = aabb2(&approx, &face);
            if triangle_inside(&t2, &f2) {
                candidates.push(Cand {
                    key: face,
                    area2: area2(&t2),
                    tri: t2,
                    lo: clo,
                    hi: chi,
                });
                queue.push(ui);
            } else if aabb_positive_overlap(&clo, &chi, &flo, &fhi)
                && triangles_overlap(&t2, &f2)
            {
                *partial = true;
            }
            Ok(())
        };

        for &corner in f {
            for &ui in &vertex_faces[corner] {
                consider(ui, &mut candidates, &mut queue, &mut partial)?;
            }
        }
        while let Some(ui) = queue.pop() {
            let face = faces[ui];
            for k in 0..3 {
                let a = face[k];
                let b = face[(k + 1) % 3];
                let key = if a < b { [a, b] } else { [b, a] };
                if let Some(list) = edge_faces.get(&key) {
                    for &uj in list {
                        consider(uj, &mut candidates, &mut queue, &mut partial)?;
                    }
                }
            }
        }

        for c in &candidates {
            constraint_set.insert(c.key);
        }
        let mut bad = false;
        if partial {
            report.partial += 1;
            bad = true;
        }
        if candidates.is_empty() && !bad {
            report.uncovered += 1;
            bad = true;
        }
        if !bad {
            let mut sum = BigRational::zero();
            for c in &candidates {
                sum += &c.area2;
            }
            let mut order: Vec<usize> = (0..candidates.len()).collect();
            order.sort_by(|&i, &j| {
                candidates[i]
                    .lo[0]
                    .partial_cmp(&candidates[j].lo[0])
                    .unwrap()
                    .then(i.cmp(&j))
            });
            'outer: for oi in 0..order.len() {
                let i = order[oi];
                for oj in (oi + 1)..order.len() {
                    let j = order[oj];
                    if candidates[j].lo[0] >= candidates[i].hi[0] {
                        break;
                    }
                    if candidates[j].lo[1] >= candidates[i].hi[1]
                        || candidates[i].lo[1] >= candidates[j].hi[1]
                        || candidates[j].lo[2] >= candidates[i].hi[2]
                        || candidates[i].lo[2] >= candidates[j].hi[2]
                    {
                        continue;
                    }
                    if triangles_overlap(&candidates[i].tri, &candidates[j].tri) {
                        report.overlapping += 1;
                        bad = true;
                        break 'outer;
                    }
                }
            }
            if !bad && sum != area2(&f2) {
                report.area_mismatch += 1;
                bad = true;
            }
        }

        if bad {
            report.failed_triangles += 1;
            report.failures.push(fi);
        } else {
            report.certified_triangles += 1;
        }
    }

    report.certified = report.failed_triangles == 0;
    report.constraint_faces = constraint_set.into_iter().collect();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(x: i64) -> BigRational {
        BigRational::from_integer(x.into())
    }

    fn p(x: i64, y: i64) -> P2 {
        [q(x), q(y)]
    }

    #[test]
    fn exact_area_and_overlap_helpers() {
        let a = [p(0, 0), p(4, 0), p(0, 4)];
        let b = [p(0, 0), p(2, 0), p(0, 2)];
        let c = [p(10, 10), p(12, 10), p(10, 12)];
        assert_eq!(area2(&a), q(16));
        assert!(triangle_inside(&b, &a));
        assert!(triangles_overlap(&b, &a));
        assert!(!triangles_overlap(&c, &a));

        let left = [p(0, 0), p(4, 0), p(0, 4)];
        let right = [p(4, 0), p(8, 0), p(4, 4)];
        assert!(!triangles_overlap(&left, &right));

        let edge_shared = [p(0, 0), p(2, 0), p(2, 2)];
        let other = [p(0, 0), p(2, 0), p(1, -2)];
        assert!(!triangles_overlap(&edge_shared, &other));
    }

    #[test]
    fn exact_centroid_detects_boundary_vertex_containment() {
        let big = [p(0, 0), p(6, 0), p(0, 6)];
        let mid = [p(2, 0), p(3, 0), p(0, 3)];
        assert!(triangles_overlap(&mid, &big));
    }
}
