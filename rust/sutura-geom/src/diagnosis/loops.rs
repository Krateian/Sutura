//! Diagnosis v2 phase 2: boundary-loop classification.
//!
//! Loop extraction shares `hole::boundary_topology` with the native hole
//! closer.  Each loop is described by its size, physical extent, planarity,
//! simplicity/pinch state and a normalized cap-difficulty score (a value in
//! `[0, 1]`, not a pass/fail threshold).

use std::collections::HashSet;

use crate::dual_contour::jacobi_eigen;
use crate::hole::boundary_topology;

use super::{Bbox, Region};

/// Vertex mask bit used by this channel (phase 1 uses bits 2, 4).
pub const VERTEX_MASK_BIT: u8 = 32;

/// Above this many vertices the exact pairwise diameter is replaced by the
/// bounding-box diagonal and the channel is marked estimated.
pub const EXACT_DIAMETER_MAX: usize = 1024;

/// JSON/memory caps: the scalar aggregates always cover every loop, but only
/// this many loop records and regions are materialised.
pub const MAX_LOOP_ITEMS: usize = 512;
pub const MAX_LOOP_REGIONS: usize = 256;

#[derive(Clone, Debug)]
pub struct LoopInfo {
    pub length: usize,
    pub perimeter: f64,
    pub diameter: f64,
    pub ratio: f64,
    pub planar: bool,
    pub simple: bool,
    pub pinched: bool,
    pub rms: f64,
    pub min_angle: f64,
    pub difficulty: f64,
    pub estimated: bool,
    pub bbox: [f64; 6],
    pub centroid: [f64; 3],
    pub kind: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct Loops {
    pub count: usize,
    pub total_length: usize,
    pub largest: usize,
    pub planar: usize,
    pub pinched: usize,
    pub open_edges: usize,
    pub estimated: bool,
    pub items: Vec<LoopInfo>,
}

#[inline]
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    dot(sub(a, b), sub(a, b)).sqrt()
}

#[inline]
fn angle_between(u: [f64; 3], v: [f64; 3]) -> f64 {
    let nu = dot(u, u).sqrt();
    let nv = dot(v, v).sqrt();
    if nu <= 0.0 || nv <= 0.0 {
        return std::f64::consts::PI;
    }
    (dot(u, v) / (nu * nv)).clamp(-1.0, 1.0).acos()
}

fn newell_area(points: &[[f64; 3]]) -> f64 {
    let k = points.len();
    if k < 3 {
        return 0.0;
    }
    let mut n = [0.0f64; 3];
    for i in 0..k {
        let p = points[i];
        let q = points[(i + 1) % k];
        n[0] += (p[1] - q[1]) * (p[2] + q[2]);
        n[1] += (p[2] - q[2]) * (p[0] + q[0]);
        n[2] += (p[0] - q[0]) * (p[1] + q[1]);
    }
    0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()
}

fn covariance(points: &[[f64; 3]], centroid: [f64; 3]) -> [[f64; 3]; 3] {
    let mut c = [[0.0f64; 3]; 3];
    for p in points {
        let d = sub(*p, centroid);
        for i in 0..3 {
            for j in 0..3 {
                c[i][j] += d[i] * d[j];
            }
        }
    }
    let inv = 1.0 / points.len().max(1) as f64;
    for row in &mut c {
        for x in row.iter_mut() {
            *x *= inv;
        }
    }
    c
}

/// Rotate the cycle to start at its lexicographically smallest vertex
/// (ties broken by the following vertices).  The boundary walk's start is
/// face-order dependent, so summing the cycle in raw traversal order drifts at
/// the last bit when the mesh is permuted; this makes every accumulated value
/// (centroid, perimeter, Newell area, covariance) bit-exact under rotation.
fn canonical_rotation(points: &[[f64; 3]]) -> Vec<[f64; 3]> {
    let k = points.len();
    if k < 2 {
        return points.to_vec();
    }
    let cyclic_less = |a: usize, b: usize| -> bool {
        for d in 0..k {
            let p = points[(a + d) % k];
            let q = points[(b + d) % k];
            for c in 0..3 {
                match p[c].total_cmp(&q[c]) {
                    std::cmp::Ordering::Less => return true,
                    std::cmp::Ordering::Greater => return false,
                    std::cmp::Ordering::Equal => {}
                }
            }
        }
        false
    };
    let mut best = 0usize;
    for i in 1..k {
        if cyclic_less(i, best) {
            best = i;
        }
    }
    (0..k).map(|j| points[(best + j) % k]).collect()
}

fn loop_metrics(points: &[[f64; 3]]) -> LoopInfo {
    let ordered = canonical_rotation(points);
    let points = &ordered[..];
    let k = points.len();
    let mut bbox = Bbox::empty();
    let mut centroid = [0.0f64; 3];
    for p in points {
        bbox.add(*p);
        for i in 0..3 {
            centroid[i] += p[i];
        }
    }
    if k > 0 {
        for x in &mut centroid {
            *x /= k as f64;
        }
    }

    let mut perimeter = 0.0f64;
    let mut max_edge = 0.0f64;
    let mut min_angle = std::f64::consts::PI;
    for i in 0..k {
        let prev = points[(i + k - 1) % k];
        let cur = points[i];
        let next = points[(i + 1) % k];
        let e = dist(cur, next);
        perimeter += e;
        max_edge = max_edge.max(e);
        min_angle = min_angle.min(angle_between(sub(prev, cur), sub(next, cur)));
    }

    let bbox_diag = bbox.diag();
    let (diameter, estimated) = if k <= EXACT_DIAMETER_MAX {
        let mut d = 0.0f64;
        for i in 0..k {
            for j in (i + 1)..k {
                d = d.max(dist(points[i], points[j]));
            }
        }
        (d, false)
    } else {
        (bbox_diag, true)
    };
    let diam = diameter.max(1e-12);
    let ratio = perimeter / diam;

    let cov = covariance(points, centroid);
    let (mut evals, _) = jacobi_eigen(&cov);
    evals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rms = evals[0].max(0.0).sqrt();
    let planar = rms <= 0.02 * diam;

    let area = newell_area(points);

    let ratio_n = ((ratio - std::f64::consts::PI) / 3.0).clamp(0.0, 1.0);
    let area_n = (area / (0.5 * diam * diam)).clamp(0.0, 1.0);
    let rms_n = (rms / (0.05 * diam)).clamp(0.0, 1.0);
    let angle_n = (1.0 - min_angle / (0.5 * std::f64::consts::PI)).clamp(0.0, 1.0);
    let edge_n = (max_edge / diam).clamp(0.0, 1.0);
    let difficulty =
        0.30 * ratio_n + 0.15 * area_n + 0.25 * rms_n + 0.15 * angle_n + 0.15 * edge_n;

    LoopInfo {
        length: k,
        perimeter,
        diameter,
        ratio,
        planar,
        simple: true,
        pinched: false,
        rms,
        min_angle,
        difficulty: difficulty.clamp(0.0, 1.0),
        estimated,
        bbox: bbox.flat(),
        centroid,
        kind: "boundary_loop",
    }
}

/// Classify every boundary loop, set the loop vertex mask and append regions.
pub fn loop_report(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    comp: &[usize],
    vertex_mask: &mut [u8],
    regions: &mut Vec<Region>,
) -> Loops {
    let mut out = Loops::default();
    if tris.is_empty() {
        return out;
    }
    let itris: Vec<[i64; 3]> = tris
        .iter()
        .map(|t| [t[0] as i64, t[1] as i64, t[2] as i64])
        .collect();
    let topo = match boundary_topology(&itris, false) {
        Ok(t) => t,
        Err(_) => {
            out.estimated = true;
            return out;
        }
    };
    out.open_edges = topo.boundary_edges;
    out.estimated = topo.incomplete;
    let mut records: Vec<(LoopInfo, Vec<usize>, Vec<usize>)> = Vec::new();

    for seq in &topo.loops {
        let mut vertex_ids: Vec<usize> = Vec::with_capacity(seq.len());
        let mut faces: Vec<usize> = Vec::with_capacity(seq.len());
        for &he in seq {
            let f = he / 3;
            let j = he % 3;
            if f < tris.len() {
                vertex_ids.push(tris[f][j]);
                faces.push(f);
            }
        }
        if vertex_ids.len() < 3 {
            continue;
        }
        let points: Vec<[f64; 3]> = vertex_ids.iter().map(|&v| verts[v]).collect();
        let mut info = loop_metrics(&points);
        let unique: HashSet<usize> = vertex_ids.iter().copied().collect();
        info.simple = unique.len() == vertex_ids.len();
        info.pinched = !info.simple;
        if info.pinched {
            info.kind = "pinched_boundary_loop";
        } else if info.planar {
            info.kind = "planar_boundary_loop";
        }
        out.estimated |= info.estimated;

        for &v in &unique {
            if v < vertex_mask.len() {
                vertex_mask[v] |= VERTEX_MASK_BIT;
            }
        }
        faces.sort_unstable();
        faces.dedup();
        let mut vs: Vec<usize> = unique.into_iter().collect();
        vs.sort_unstable();
        let length = info.length;
        let planar = info.planar;
        let pinched = info.pinched;
        records.push((info, vs, faces));

        out.count += 1;
        out.total_length += length;
        out.largest = out.largest.max(length);
        if planar {
            out.planar += 1;
        }
        if pinched {
            out.pinched += 1;
        }
    }
    records.sort_by(|a, b| loop_key_cmp(&a.0, &b.0));
    for (i, (info, vs, faces)) in records.into_iter().enumerate() {
        if i < MAX_LOOP_REGIONS {
            regions.push(super::region_from_vertices(
                "loops",
                info.kind,
                &vs,
                &faces,
                verts,
                info.difficulty,
                1.0,
                comp,
            ));
        }
        if i < MAX_LOOP_ITEMS {
            out.items.push(info);
        }
    }
    out
}

fn loop_key_cmp(a: &LoopInfo, b: &LoopInfo) -> std::cmp::Ordering {
    a.kind
        .cmp(b.kind)
        .then_with(|| a.bbox[0].total_cmp(&b.bbox[0]))
        .then_with(|| a.bbox[1].total_cmp(&b.bbox[1]))
        .then_with(|| a.bbox[2].total_cmp(&b.bbox[2]))
        .then_with(|| a.bbox[3].total_cmp(&b.bbox[3]))
        .then_with(|| a.bbox[4].total_cmp(&b.bbox[4]))
        .then_with(|| a.bbox[5].total_cmp(&b.bbox[5]))
        .then_with(|| a.length.cmp(&b.length))
        .then_with(|| a.centroid[0].total_cmp(&b.centroid[0]))
        .then_with(|| a.centroid[1].total_cmp(&b.centroid[1]))
        .then_with(|| a.centroid[2].total_cmp(&b.centroid[2]))
        .then_with(|| a.perimeter.total_cmp(&b.perimeter))
        .then_with(|| a.rms.total_cmp(&b.rms))
}
