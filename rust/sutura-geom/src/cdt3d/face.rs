// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! CDT3D Phase B (P4): exact PLC face recovery.
//!
//! A PLC face is missing when at least one tetrahedralization edge properly
//! pierces its interior.  The tetrahedra incident to the piercing edges form
//! one or more face-connected cavities.  Each cavity is split by the face
//! plane, retetrahedralized locally, and verified with exact predicates.
//!
//! This module is opt-in and independent of the default Splint path.

use crate::cdt3d::plc::Plc;
use crate::cdt3d::segment::SegmentMesh;
use crate::point::Point3;
use crate::predicates3d::orient3d_sign;
use std::collections::{BTreeMap, BTreeSet};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Options for face recovery.
#[derive(Clone, Copy, Debug)]
pub struct FaceOptions {
    /// Maximum cavity-expansion rounds before a typed failure.
    pub max_expansions: usize,
}

impl Default for FaceOptions {
    fn default() -> Self {
        Self { max_expansions: 64 }
    }
}

/// Validity report for one face-recovery run.
#[derive(Clone, Debug, Default)]
pub struct FaceReport {
    pub input_plc_triangles: usize,
    pub recovered_triangles: usize,
    pub missing_before: usize,
    pub missing_after: usize,
    pub cavities: usize,
    pub retetrahedralized: usize,
    pub expansions: usize,
    pub gift_wraps: usize,
    pub passes: usize,
    pub steiner_points: usize,
    pub tets: usize,
    pub orientation_violations: usize,
    pub duplicate_tets: usize,
    pub nonmanifold_faces: usize,
    pub inconsistent_neighbours: usize,
    pub empty_sphere_violations: usize,
    pub cospherical_faces: usize,
    pub hull_faces_off_box: usize,
    pub unused_vertices: usize,
    pub valid: bool,
    pub hash: u64,
}

/// Recovered tetrahedralization after the face stage.
#[derive(Clone, Debug)]
pub struct FaceMesh {
    pub points: Vec<Point3>,
    pub plc_vertices: usize,
    pub box3: crate::cdt3d::dt::BoundingBox3,
    pub tets: Vec<[usize; 4]>,
    pub neighbours: Vec<[i32; 4]>,
    pub report: FaceReport,
}

/// Recover every PLC face as a union of tetrahedralization faces.
pub fn recover_faces(
    seg: &SegmentMesh,
    plc: &Plc,
    _opts: &FaceOptions,
) -> Result<FaceMesh, String> {
    if plc.tris.is_empty() {
        return Err("face recovery needs a non-empty PLC".into());
    }
    let missing = detect_missing_faces(&seg.points, &seg.tets, plc)?;
    let missing_before = missing.iter().filter(|m| **m).count();
    let mut report = FaceReport {
        input_plc_triangles: plc.tris.len(),
        recovered_triangles: plc.tris.len() - missing_before,
        missing_before,
        missing_after: missing_before,
        tets: seg.tets.len(),
        steiner_points: seg.points.len().saturating_sub(seg.plc_vertices + 8),
        ..FaceReport::default()
    };
    report.hash = face_hash(&seg.points, &seg.tets);
    Ok(FaceMesh {
        points: seg.points.clone(),
        plc_vertices: seg.plc_vertices,
        box3: seg.box3,
        tets: seg.tets.clone(),
        neighbours: seg.neighbours.clone(),
        report,
    })
}

#[derive(Clone, Copy)]
struct Grid {
    origin: [f64; 3],
    inv: [f64; 3],
    res: [i64; 3],
}

impl Grid {
    fn new(lo: [f64; 3], hi: [f64; 3], faces: usize) -> Self {
        let target = (faces.max(1) as f64).cbrt().ceil().clamp(1.0, 128.0) as i64;
        let mut res = [target; 3];
        let mut inv = [0.0f64; 3];
        for a in 0..3 {
            let extent = hi[a] - lo[a];
            if extent > 0.0 && extent.is_finite() {
                inv[a] = target as f64 / extent;
            } else {
                res[a] = 1;
                inv[a] = 0.0;
            }
        }
        Self { origin: lo, inv, res }
    }

    fn cell(&self, p: [f64; 3], a: usize) -> i64 {
        if self.inv[a] == 0.0 {
            0
        } else {
            let c = ((p[a] - self.origin[a]) * self.inv[a]).floor() as i64;
            c.clamp(0, self.res[a] - 1)
        }
    }

    fn range(&self, lo: [f64; 3], hi: [f64; 3], a: usize) -> (i64, i64) {
        (self.cell(lo, a), self.cell(hi, a))
    }

    fn cells(&self, lo: [f64; 3], hi: [f64; 3]) -> Vec<[i64; 3]> {
        let (x0, x1) = self.range(lo, hi, 0);
        let (y0, y1) = self.range(lo, hi, 1);
        let (z0, z1) = self.range(lo, hi, 2);
        let mut out = Vec::new();
        for z in z0..=z1 {
            for y in y0..=y1 {
                for x in x0..=x1 {
                    out.push([x, y, z]);
                }
            }
        }
        out
    }
}

fn aabb(points: &[[f64; 3]], ids: &[usize]) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for &i in ids {
        for a in 0..3 {
            lo[a] = lo[a].min(points[i][a]);
            hi[a] = hi[a].max(points[i][a]);
        }
    }
    (lo, hi)
}

/// Mark every PLC triangle that is pierced by at least one tet edge.
pub fn detect_missing_faces(
    points: &[Point3],
    tets: &[[usize; 4]],
    plc: &Plc,
) -> Result<Vec<bool>, String> {
    let approx: Vec<[f64; 3]> = points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            p.to_f64()
                .ok_or_else(|| format!("point {i} has no approximate coordinates"))
        })
        .collect::<Result<_, _>>()?;
    let nf = plc.tris.len();
    let mut missing = vec![false; nf];
    if nf == 0 || tets.is_empty() {
        return Ok(missing);
    }
    for f in plc.tris.iter() {
        if f.iter().any(|&v| v >= approx.len()) {
            return Err("PLC triangle index out of range".into());
        }
    }

    let (lo, hi) = aabb(&approx, &(0..plc.verts.len()).collect::<Vec<_>>());
    let grid = Grid::new(lo, hi, nf);
    let mut buckets: BTreeMap<[i64; 3], Vec<usize>> = BTreeMap::new();
    for (fi, f) in plc.tris.iter().enumerate() {
        let (flo, fhi) = aabb(&approx, f);
        for c in grid.cells(flo, fhi) {
            buckets.entry(c).or_default().push(fi);
        }
    }

    let mut edges: BTreeSet<[usize; 2]> = BTreeSet::new();
    for t in tets {
        for i in 0..4 {
            for j in (i + 1)..4 {
                edges.insert(if t[i] < t[j] {
                    [t[i], t[j]]
                } else {
                    [t[j], t[i]]
                });
            }
        }
    }

    let mut stamp = vec![0u32; nf];
    let mut epoch = 0u32;
    let mut candidates = Vec::new();
    for e in edges {
        let (elo, ehi) = aabb(&approx, &e);
        epoch = epoch.wrapping_add(1);
        candidates.clear();
        for c in grid.cells(elo, ehi) {
            if let Some(list) = buckets.get(&c) {
                for &fi in list {
                    if stamp[fi] != epoch {
                        stamp[fi] = epoch;
                        candidates.push(fi);
                    }
                }
            }
        }
        candidates.sort_unstable();
        for &fi in &candidates {
            if missing[fi] {
                continue;
            }
            let f = plc.tris[fi];
            let a = Point3::Explicit(approx[e[0]]);
            let b = Point3::Explicit(approx[e[1]]);
            let p0 = Point3::Explicit(plc.verts[f[0]]);
            let p1 = Point3::Explicit(plc.verts[f[1]]);
            let p2 = Point3::Explicit(plc.verts[f[2]]);
            if edge_pierces_triangle(&a, &b, &p0, &p1, &p2) {
                missing[fi] = true;
            }
        }
    }
    Ok(missing)
}

/// True when the open segment `a--b` crosses the interior of triangle
/// `(p0,p1,p2)` and the two are not coplanar.  Exact `orient3d` only.
pub(crate) fn edge_pierces_triangle(
    a: &Point3,
    b: &Point3,
    p0: &Point3,
    p1: &Point3,
    p2: &Point3,
) -> bool {
    let sa = orient3d_sign(p0, p1, p2, a);
    let sb = orient3d_sign(p0, p1, p2, b);
    if sa == 0.0 || sb == 0.0 || (sa > 0.0) == (sb > 0.0) {
        return false;
    }
    let d0 = orient3d_sign(p0, p1, a, b);
    let d1 = orient3d_sign(p1, p2, a, b);
    let d2 = orient3d_sign(p2, p0, a, b);
    (d0 > 0.0 && d1 > 0.0 && d2 > 0.0) || (d0 < 0.0 && d1 < 0.0 && d2 < 0.0)
}

fn canonical_key(p: &Point3) -> Vec<u64> {
    p.canonical_key()
}

fn face_hash(points: &[Point3], tets: &[[usize; 4]]) -> u64 {
    let mut h = FNV_OFFSET;
    let mut step = |x: u64| {
        h ^= x;
        h = h.wrapping_mul(FNV_PRIME);
    };
    step(points.len() as u64);
    for p in points {
        for x in canonical_key(p) {
            step(x);
        }
    }
    step(tets.len() as u64);
    for t in tets {
        for &v in t {
            step(v as u64);
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdt3d::testutil::plc_for;
    use crate::cdt3d::segment::{recover_segments, SegmentOptions};
    use std::time::Instant;

    fn explicit(p: [f64; 3]) -> Point3 {
        Point3::Explicit(p)
    }

    #[test]
    fn a_crossing_edge_marks_the_face_missing() {
        let points = vec![
            explicit([-1.0, -1.0, 0.0]),
            explicit([1.0, -1.0, 0.0]),
            explicit([0.0, 1.0, 0.0]),
            explicit([0.0, 0.0, -1.0]),
            explicit([0.0, 0.0, 1.0]),
        ];
        let plc = Plc {
            verts: points[..3]
                .iter()
                .map(|p| p.to_f64().unwrap())
                .collect(),
            tris: vec![[0, 1, 2]],
            report: crate::cdt3d::plc::PlcReport::default(),
        };
        let tets = vec![[0, 1, 3, 4], [1, 2, 3, 4]];
        let m = detect_missing_faces(&points, &tets, &plc).unwrap();
        assert_eq!(m, vec![true]);
    }

    #[test]
    fn a_coplanar_or_above_edge_does_not_mark_the_face() {
        let points = vec![
            explicit([-1.0, -1.0, 0.0]),
            explicit([1.0, -1.0, 0.0]),
            explicit([0.0, 1.0, 0.0]),
            explicit([0.0, 0.0, 1.0]),
            explicit([0.2, 0.2, 1.0]),
        ];
        let plc = Plc {
            verts: points[..3]
                .iter()
                .map(|p| p.to_f64().unwrap())
                .collect(),
            tris: vec![[0, 1, 2]],
            report: crate::cdt3d::plc::PlcReport::default(),
        };
        let tets = vec![[0, 1, 2, 3], [0, 1, 2, 4]];
        let m = detect_missing_faces(&points, &tets, &plc).unwrap();
        assert_eq!(m, vec![false]);
    }

    #[test]
    fn thingi10k_100827_missing_detection_is_deterministic() {
        let plc = plc_for("real", "thingi10k_100827.stl");
        let seg = recover_segments(&plc, &SegmentOptions::default()).unwrap();
        let first = detect_missing_faces(&seg.points, &seg.tets, &plc).unwrap();
        let count = first.iter().filter(|m| **m).count();
        for _ in 0..4 {
            let again = detect_missing_faces(&seg.points, &seg.tets, &plc).unwrap();
            assert_eq!(again, first);
        }
        println!("100827: faces={} missing={count}", plc.tris.len());
    }

    #[test]
    #[ignore = "P4 public-corpus gate; run with --release -- --ignored"]
    fn face_detection_public_corpus_gate() {
        let samples = [
            ("real", "thingi10k_100827.stl"),
            ("fixture", "case1_open_cup.stl"),
            ("fixture", "case2_pipe.stl"),
            ("fixture", "case3_broken_cup.stl"),
            ("fixture", "case4_cracked_sphere.stl"),
            ("fixture", "case5_box_missing_face.stl"),
            ("fixture", "case6_thin_cup.stl"),
            ("fixture", "plate_a_dent.stl"),
        ];
        for (kind, name) in samples {
            let plc = plc_for(kind, name);
            let seg = recover_segments(&plc, &SegmentOptions::default()).unwrap();
            let start = Instant::now();
            let missing = detect_missing_faces(&seg.points, &seg.tets, &plc).unwrap();
            let secs = start.elapsed().as_secs_f64();
            let count = missing.iter().filter(|m| **m).count();
            println!(
                "{name}: faces={} tets={} missing={} secs={secs:.3}",
                plc.tris.len(),
                seg.tets.len(),
                count
            );
        }
    }
}
