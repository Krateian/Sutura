// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shell analysis using dual-sided generalized winding numbers (Idea A).
//!
//! Replaces full volumetric rebuilds with inside-out shell analysis:
//! - 0 <-> 1: Real outer surface (KEEP verbatim).
//! - 1 <-> 2+: Buried/embedded shell (DISSOLVE / DISCARD).
//! - 0 <-> 1 (bounded void): Reversed inner shell = cavity (KEEP by default).
//! - Coincident double walls:
//!   - Matching vertices, opposite normal: dissolve both (merging adjacent faces).
//!   - Matching vertices, same normal: keep one, discard duplicates.
//! - Holes on shells to be removed are ignored (not sealed).
//! - Seams on penetrating SI bodies are split exactly along intersection curves
//!   (reusing triangle_intersection / cdt2d); no rim-loop plugs on penetration seams.

use crate::arrangement::rat3_to_f64;
use crate::cdt2d::{ConstrainedSegment, HostFrame, Point2D, SegmentSource, Triangulation, orient2d};
use crate::point::Point3;
use crate::raycast::{Stabber, DEFAULT_DIRS};
use crate::winding::MeshBvh;
use num_rational::BigRational;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

/// Classification of an individual face in the mesh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaceClass {
    /// Outer surface separating exterior void (w ~ 0) from solid interior (w ~ 1). Keep verbatim.
    Outer,
    /// Buried/embedded face where both sides are inside solid interior (w >= 1). Discard.
    Buried,
    /// Cavity face bounding an internal enclosed void (w ~ 0 with no ray escape). Keep.
    Cavity,
    /// Coincident face sharing exact 3 vertices with another face with opposite normal. Dissolve.
    CoincidentOpposite,
    /// Coincident duplicate face sharing exact 3 vertices with same normal. Keep one, discard copy.
    CoincidentSame,
    /// Degenerate face (zero area or unclassifiable). Discard.
    Degenerate,
}

/// Configuration options for shell analysis.
#[derive(Clone, Debug)]
pub struct ShellConfig {
    /// Decision threshold slack around integer winding numbers (default 0.35).
    pub delta: f64,
    /// Scale factor for query offset epsilon relative to inradius / median edge (default 0.05).
    pub eps_scale: f64,
    /// Whether to preserve internal cavities (default true).
    pub keep_cavities: bool,
    /// Whether to check ray escape to bounding box border for cavity detection (default true).
    pub check_ray_escape: bool,
    /// Whether to attempt exact arrangement split on proper self-intersections (default true).
    pub split_intersections: bool,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            delta: 0.35,
            eps_scale: 0.05,
            keep_cavities: true,
            check_ray_escape: true,
            split_intersections: true,
        }
    }
}

/// Detailed result of shell analysis.
#[derive(Clone, Debug, Default)]
pub struct ShellAnalysisResult {
    /// Classification per input triangle.
    pub classes: Vec<FaceClass>,
    /// Number of faces classified as Outer.
    pub outer_count: usize,
    /// Number of faces classified as Buried.
    pub buried_count: usize,
    /// Number of faces classified as Cavity.
    pub cavity_count: usize,
    /// Number of opposite-normal coincident faces dissolved.
    pub coincident_opp_count: usize,
    /// Number of duplicate coincident faces removed.
    pub coincident_same_count: usize,
    /// Indices of input triangles that survive.
    pub surviving_indices: Vec<usize>,
    /// Whether any surviving face has winding >= 1.5 on both sides, or unresolved coincident opposite pairs.
    pub has_internal_walls: bool,
    /// Number of surviving faces that are internal walls.
    pub internal_walls_count: usize,
    /// Number of unresolved coincident opposite-normal faces in surviving mesh.
    pub unresolved_coincident_count: usize,
    /// Whether all shells are disjoint (Phase P-a qualified: no cross-part SI, volume overlap < 0.1%, no buried faces).
    pub is_disjoint: bool,
    /// Diagnostic / status reason ("ok", "no_change", or abort reason).
    pub reason: String,
}

#[inline]
fn cross3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn norm3(a: [f64; 3]) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

#[inline]
fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn norm3_sq(a: [f64; 3]) -> f64 {
    a[0] * a[0] + a[1] * a[1] + a[2] * a[2]
}

#[inline]
fn sub3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn add3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
fn scale3(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

/// Compute median edge length across triangle soup.
pub fn median_edge_length(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> f64 {
    if tris.is_empty() {
        return 1.0;
    }
    let step = (tris.len() / 5000).max(1);
    let mut lens = Vec::with_capacity(tris.len() / step * 3 + 3);

    for i in (0..tris.len()).step_by(step) {
        let t = tris[i];
        let v0 = verts[t[0] as usize];
        let v1 = verts[t[1] as usize];
        let v2 = verts[t[2] as usize];
        lens.push(norm3(sub3(v1, v0)));
        lens.push(norm3(sub3(v2, v1)));
        lens.push(norm3(sub3(v0, v2)));
    }
    if lens.is_empty() {
        return 1.0;
    }
    lens.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    lens[lens.len() / 2].max(1e-6)
}

/// Canonical key for a triangle's unordered vertex set (sorted indices).
#[inline]
fn triangle_canonical_key(t: [u32; 3]) -> (u32, u32, u32) {
    let mut arr = t;
    arr.sort_unstable();
    (arr[0], arr[1], arr[2])
}

/// Check cyclic parity: returns true if (a, b, c) has positive permutation relative to sorted (u, v, w).
#[inline]
fn cyclic_parity(t: [u32; 3]) -> bool {
    let (u, v, w) = (t[0], t[1], t[2]);
    if u < v && v < w {
        true
    } else if v < w && w < u {
        true
    } else if w < u && u < v {
        true
    } else {
        false
    }
}

/// Compute connected components (shells) by manifold half-edge connectivity.
pub fn compute_shell_components(tris: &[[u32; 3]]) -> (Vec<usize>, usize) {
    let m = tris.len();
    if m == 0 {
        return (Vec::new(), 0);
    }
    let mut half_edges: HashMap<(u32, u32), Vec<usize>> = HashMap::with_capacity(m * 3);
    for (i, t) in tris.iter().enumerate() {
        half_edges.entry((t[0], t[1])).or_default().push(i);
        half_edges.entry((t[1], t[2])).or_default().push(i);
        half_edges.entry((t[2], t[0])).or_default().push(i);
    }

    let mut parent: Vec<usize> = (0..m).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    fn union(p: &mut [usize], a: usize, b: usize) {
        let ra = find(p, a);
        let rb = find(p, b);
        if ra != rb {
            p[ra] = rb;
        }
    }

    for (&(u, v), f_list) in &half_edges {
        if let Some(opp_list) = half_edges.get(&(v, u)) {
            for &f1 in f_list {
                for &f2 in opp_list {
                    union(&mut parent, f1, f2);
                }
            }
        }
    }

    let mut comp_map: HashMap<usize, usize> = HashMap::new();
    let mut shell_ids = Vec::with_capacity(m);
    for i in 0..m {
        let root = find(&mut parent, i);
        let next_id = comp_map.len();
        let cid = *comp_map.entry(root).or_insert(next_id);
        shell_ids.push(cid);
    }
    let num_shells = comp_map.len();
    (shell_ids, num_shells)
}

fn plane_key(n: [f64; 3], p: [f64; 3], h_med: f64) -> (i64, i64, i64, i64) {
    let mut un = n;
    let l = norm3(un);
    if l < 1e-12 {
        return (0, 0, 0, 0);
    }
    un = scale3(un, 1.0 / l);
    let flip = if un[0].abs() > 1e-4 {
        un[0] < 0.0
    } else if un[1].abs() > 1e-4 {
        un[1] < 0.0
    } else {
        un[2] < 0.0
    };
    if flip {
        un = scale3(un, -1.0);
    }
    let d = un[0] * p[0] + un[1] * p[1] + un[2] * p[2];
    let quant_n = 1000.0;
    let quant_d = (100.0 / h_med).max(1.0);
    (
        (un[0] * quant_n).round() as i64,
        (un[1] * quant_n).round() as i64,
        (un[2] * quant_n).round() as i64,
        (d * quant_d).round() as i64,
    )
}

fn point_strictly_in_triangle_2d(p: [f64; 2], a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> bool {
    let s1 = crate::predicates2d::orient2d_sign(
        &crate::predicates2d::Point2::Explicit(a),
        &crate::predicates2d::Point2::Explicit(b),
        &crate::predicates2d::Point2::Explicit(p),
    );
    let s2 = crate::predicates2d::orient2d_sign(
        &crate::predicates2d::Point2::Explicit(b),
        &crate::predicates2d::Point2::Explicit(c),
        &crate::predicates2d::Point2::Explicit(p),
    );
    let s3 = crate::predicates2d::orient2d_sign(
        &crate::predicates2d::Point2::Explicit(c),
        &crate::predicates2d::Point2::Explicit(a),
        &crate::predicates2d::Point2::Explicit(p),
    );
    (s1 > 1e-12 && s2 > 1e-12 && s3 > 1e-12) || (s1 < -1e-12 && s2 < -1e-12 && s3 < -1e-12)
}

fn segments_properly_cross_2d(p1: [f64; 2], p2: [f64; 2], q1: [f64; 2], q2: [f64; 2]) -> bool {
    let s1 = crate::predicates2d::orient2d_sign(
        &crate::predicates2d::Point2::Explicit(p1),
        &crate::predicates2d::Point2::Explicit(p2),
        &crate::predicates2d::Point2::Explicit(q1),
    );
    let s2 = crate::predicates2d::orient2d_sign(
        &crate::predicates2d::Point2::Explicit(p1),
        &crate::predicates2d::Point2::Explicit(p2),
        &crate::predicates2d::Point2::Explicit(q2),
    );
    let s3 = crate::predicates2d::orient2d_sign(
        &crate::predicates2d::Point2::Explicit(q1),
        &crate::predicates2d::Point2::Explicit(q2),
        &crate::predicates2d::Point2::Explicit(p1),
    );
    let s4 = crate::predicates2d::orient2d_sign(
        &crate::predicates2d::Point2::Explicit(q1),
        &crate::predicates2d::Point2::Explicit(q2),
        &crate::predicates2d::Point2::Explicit(p2),
    );
    ((s1 > 1e-12 && s2 < -1e-12) || (s1 < -1e-12 && s2 > 1e-12))
        && ((s3 > 1e-12 && s4 < -1e-12) || (s3 < -1e-12 && s4 > 1e-12))
}

fn triangles_proper_overlap_2d(a: &[[f64; 2]; 3], b: &[[f64; 2]; 3]) -> bool {
    let c_a = [
        (a[0][0] + a[1][0] + a[2][0]) / 3.0,
        (a[0][1] + a[1][1] + a[2][1]) / 3.0,
    ];
    let c_b = [
        (b[0][0] + b[1][0] + b[2][0]) / 3.0,
        (b[0][1] + b[1][1] + b[2][1]) / 3.0,
    ];
    if point_strictly_in_triangle_2d(c_a, b[0], b[1], b[2])
        || point_strictly_in_triangle_2d(c_b, a[0], a[1], a[2])
    {
        return true;
    }
    for i in 0..3 {
        if point_strictly_in_triangle_2d(a[i], b[0], b[1], b[2]) {
            return true;
        }
        if point_strictly_in_triangle_2d(b[i], a[0], a[1], a[2]) {
            return true;
        }
    }
    let edges_a = [(0, 1), (1, 2), (2, 0)];
    let edges_b = [(0, 1), (1, 2), (2, 0)];
    for &(i1, j1) in &edges_a {
        for &(i2, j2) in &edges_b {
            if segments_properly_cross_2d(a[i1], a[j1], b[i2], b[j2]) {
                return true;
            }
        }
    }
    false
}

fn project_tri_2d(tri: [[f64; 3]; 3], normal: [f64; 3]) -> [[f64; 2]; 3] {
    let mut drop = 0;
    if normal[1].abs() > normal[drop].abs() {
        drop = 1;
    }
    if normal[2].abs() > normal[drop].abs() {
        drop = 2;
    }
    let keep = match drop {
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

/// Run shell analysis over a triangle mesh.
pub fn analyze_shells(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    config: &ShellConfig,
) -> ShellAnalysisResult {
    let mut res = ShellAnalysisResult {
        classes: vec![FaceClass::Outer; tris.len()],
        ..Default::default()
    };

    if tris.is_empty() || verts.is_empty() {
        res.reason = "empty_mesh".to_string();
        return res;
    }

    let m = tris.len();
    let h_med = median_edge_length(verts, tris);

    let (shell_map, num_shells) = compute_shell_components(tris);

    let mut shell_bboxes = vec![crate::winding::Aabb::empty(); num_shells];
    for (i, &t) in tris.iter().enumerate() {
        let sid = shell_map[i];
        shell_bboxes[sid].expand_point(verts[t[0] as usize]);
        shell_bboxes[sid].expand_point(verts[t[1] as usize]);
        shell_bboxes[sid].expand_point(verts[t[2] as usize]);
    }

    let mut shell_has_neighbor = vec![false; num_shells];
    let mut shell_enclosed = vec![false; num_shells];
    if num_shells > 1 {
        for s1 in 0..num_shells {
            let b1 = &shell_bboxes[s1];
            for s2 in (s1 + 1)..num_shells {
                let b2 = &shell_bboxes[s2];
                let near = b1.min[0] <= b2.max[0] + h_med
                    && b1.max[0] >= b2.min[0] - h_med
                    && b1.min[1] <= b2.max[1] + h_med
                    && b1.max[1] >= b2.min[1] - h_med
                    && b1.min[2] <= b2.max[2] + h_med
                    && b1.max[2] >= b2.min[2] - h_med;
                if near {
                    shell_has_neighbor[s1] = true;
                    shell_has_neighbor[s2] = true;
                }
            }
        }
        for s1 in 0..num_shells {
            let b1 = &shell_bboxes[s1];
            for s2 in 0..num_shells {
                if s1 == s2 {
                    continue;
                }
                let b2 = &shell_bboxes[s2];
                // Check if b1 is strictly contained inside b2
                if b1.min[0] >= b2.min[0] - 1e-6
                    && b1.min[1] >= b2.min[1] - 1e-6
                    && b1.min[2] >= b2.min[2] - 1e-6
                    && b1.max[0] <= b2.max[0] + 1e-6
                    && b1.max[1] <= b2.max[1] + 1e-6
                    && b1.max[2] <= b2.max[2] + 1e-6
                {
                    shell_enclosed[s1] = true;
                    break;
                }
            }
        }
    }

    // Build MeshBvh
    let tris_usize: Vec<[usize; 3]> = tris
        .iter()
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    let bvh = MeshBvh::from_arrays(verts, &tris_usize);

    let mut bbox = crate::winding::Aabb::empty();
    for v in verts {
        bbox.expand_point(*v);
    }

    let stabber = if config.check_ray_escape {
        Some(Stabber::new(
            &bbox,
            DEFAULT_DIRS,
            42,
            false,
            crate::raycast::DEFAULT_ESCAPE_WEIGHT,
        ))
    } else {
        None
    };

    // -------------------------------------------------------------------------
    // 1. Coincident face pre-pass (Mandatory C2)
    // -------------------------------------------------------------------------
    let mut key_map: HashMap<(u32, u32, u32), Vec<usize>> = HashMap::with_capacity(m);
    for (i, &t) in tris.iter().enumerate() {
        let key = triangle_canonical_key(t);
        key_map.entry(key).or_default().push(i);
    }

    for (_key, group) in &key_map {
        if group.len() <= 1 {
            continue;
        }
        let ref_idx = group[0];
        let t = tris[ref_idx];
        let v0 = verts[t[0] as usize];
        let v1 = verts[t[1] as usize];
        let v2 = verts[t[2] as usize];
        let c = scale3(add3(add3(v0, v1), v2), 1.0 / 3.0);
        let normal = cross3(sub3(v1, v0), sub3(v2, v0));
        let area = norm3(normal) * 0.5;
        if area <= 1e-14 {
            for &fi in group {
                res.classes[fi] = FaceClass::Degenerate;
            }
            continue;
        }
        let unorm = scale3(normal, 0.5 / area);
        let eps = (h_med * 0.05).min(0.01);
        let p_front = add3(c, scale3(unorm, eps));
        let p_back = sub3(c, scale3(unorm, eps));
        let w_front = bvh.winding_at(p_front);
        let w_back = bvh.winding_at(p_back);
        let w_min = w_front.min(w_back);
        let w_max = w_front.max(w_back);

        if w_min >= 1.0 - config.delta {
            // Interior wall embedded inside solid volume: dissolve ALL copies!
            for &fi in group {
                res.classes[fi] = FaceClass::CoincidentOpposite;
            }
            res.coincident_opp_count += group.len();
        } else if w_min < config.delta && w_max >= 1.0 - config.delta {
            // Outer boundary separating void from solid:
            // Outward normal must point toward void (w < delta).
            let outward_parity = if w_front < config.delta {
                cyclic_parity(t)
            } else {
                !cyclic_parity(t)
            };
            let mut kept = false;
            for &fi in group {
                if !kept && cyclic_parity(tris[fi]) == outward_parity {
                    res.classes[fi] = FaceClass::Outer;
                    kept = true;
                } else {
                    res.classes[fi] = FaceClass::CoincidentSame;
                    res.coincident_same_count += 1;
                }
            }
            if !kept {
                res.classes[group[0]] = FaceClass::Outer;
                for &fi in &group[1..] {
                    res.classes[fi] = FaceClass::CoincidentSame;
                    res.coincident_same_count += 1;
                }
            }
        } else {
            for &fi in group {
                res.classes[fi] = FaceClass::CoincidentOpposite;
            }
            res.coincident_opp_count += group.len();
        }
    }

    // Coplanar overlapping faces check (Addendum item 3)
    let tri_aabbs: Vec<crate::winding::Aabb> = tris
        .iter()
        .map(|t| {
            let mut b = crate::winding::Aabb::empty();
            b.expand_point(verts[t[0] as usize]);
            b.expand_point(verts[t[1] as usize]);
            b.expand_point(verts[t[2] as usize]);
            b
        })
        .collect();

    let mut plane_map: HashMap<(i64, i64, i64, i64), Vec<usize>> = HashMap::with_capacity(m);
    for (i, &t) in tris.iter().enumerate() {
        let sid = shell_map[i];
        if num_shells > 1 && !shell_has_neighbor[sid] {
            continue;
        }
        let v0 = verts[t[0] as usize];
        let v1 = verts[t[1] as usize];
        let v2 = verts[t[2] as usize];
        let normal = cross3(sub3(v1, v0), sub3(v2, v0));
        let pkey = plane_key(normal, v0, h_med);
        plane_map.entry(pkey).or_default().push(i);
    }

    let mut unresolved_coplanar = false;
    for group in plane_map.values() {
        if group.len() <= 1 {
            continue;
        }
        for j in 0..group.len() {
            let i1 = group[j];
            let s1 = shell_map[i1];
            let b1 = &tri_aabbs[i1];
            for k in (j + 1)..group.len() {
                let i2 = group[k];
                let s2 = shell_map[i2];
                if s1 == s2 {
                    continue;
                }
                let b2 = &tri_aabbs[i2];
                if b1.min[0] > b2.max[0]
                    || b1.max[0] < b2.min[0]
                    || b1.min[1] > b2.max[1]
                    || b1.max[1] < b2.min[1]
                    || b1.min[2] > b2.max[2]
                    || b1.max[2] < b2.min[2]
                {
                    continue;
                }
                if triangle_canonical_key(tris[i1]) == triangle_canonical_key(tris[i2]) {
                    continue;
                }
                let norm1 = cross3(
                    sub3(verts[tris[i1][1] as usize], verts[tris[i1][0] as usize]),
                    sub3(verts[tris[i1][2] as usize], verts[tris[i1][0] as usize]),
                );
                let p1 = project_tri_2d(
                    [
                        verts[tris[i1][0] as usize],
                        verts[tris[i1][1] as usize],
                        verts[tris[i1][2] as usize],
                    ],
                    norm1,
                );
                let p2 = project_tri_2d(
                    [
                        verts[tris[i2][0] as usize],
                        verts[tris[i2][1] as usize],
                        verts[tris[i2][2] as usize],
                    ],
                    norm1,
                );
                if triangles_proper_overlap_2d(&p1, &p2) {
                    unresolved_coplanar = true;
                    res.unresolved_coincident_count += 1;
                    break;
                }
            }
            if unresolved_coplanar {
                break;
            }
        }
        if unresolved_coplanar {
            break;
        }
    }

    // Precompute signed volume of each shell (divergence theorem) to check outward normal orientation
    let mut shell_signed_vol = vec![0.0f64; num_shells];
    for (i, t) in tris.iter().enumerate() {
        let sid = shell_map[i];
        let v0 = verts[t[0] as usize];
        let v1 = verts[t[1] as usize];
        let v2 = verts[t[2] as usize];
        let c = [
            v1[1] * v2[2] - v1[2] * v2[1],
            v2[0] * v1[2] - v2[2] * v1[0],
            v1[0] * v2[1] - v1[1] * v2[0],
        ];
        shell_signed_vol[sid] += v0[0] * c[0] + v0[1] * c[1] + v0[2] * c[2];
    }
    for sid in 0..num_shells {
        shell_signed_vol[sid] /= 6.0;
    }

    // Precompute which faces overlap any OTHER shell's AABB
    let mut face_overlaps_other_shell = vec![false; m];
    if num_shells > 1 {
        for (i, _) in tris.iter().enumerate() {
            let sid = shell_map[i];
            if !shell_has_neighbor[sid] {
                continue;
            }
            if shell_enclosed[sid] {
                face_overlaps_other_shell[i] = true;
                continue;
            }
            let b = &tri_aabbs[i];
            for other_s in 0..num_shells {
                if other_s == sid {
                    continue;
                }
                let ob = &shell_bboxes[other_s];
                if b.min[0] <= ob.max[0] && b.max[0] >= ob.min[0]
                    && b.min[1] <= ob.max[1] && b.max[1] >= ob.min[1]
                    && b.min[2] <= ob.max[2] && b.max[2] >= ob.min[2]
                {
                    face_overlaps_other_shell[i] = true;
                    break;
                }
            }
        }
    }

    // -------------------------------------------------------------------------
    // 2. Dual-sided generalized winding number query
    // -------------------------------------------------------------------------
    let delta = config.delta;
    let eps_scale = config.eps_scale;

    let face_results: Vec<(FaceClass, f64, f64, f64, bool)> = (0..m)
        .into_par_iter()
        .map(|i| {
            if res.classes[i] == FaceClass::CoincidentOpposite
                || res.classes[i] == FaceClass::CoincidentSame
            {
                return (res.classes[i], 0.0, 0.0, 0.0, false);
            }

            let sid = shell_map[i];
            // Fast path: if this shell is outward-oriented and face i does not overlap any other shell,
            // it is guaranteed to be an outer boundary face separating void (w=0) from solid (w=1).
            if num_shells > 1
                && !face_overlaps_other_shell[i]
                && shell_signed_vol[sid] > 0.0
            {
                return (FaceClass::Outer, 0.0, 0.0, 1.0, false);
            }

            let t = tris[i];
            let v0 = verts[t[0] as usize];
            let v1 = verts[t[1] as usize];
            let v2 = verts[t[2] as usize];

            let e01 = sub3(v1, v0);
            let e12 = sub3(v2, v1);
            let e20 = sub3(v0, v2);

            let normal = cross3(e01, sub3(v2, v0));
            let area2 = norm3(normal);
            if area2 < 1e-14 {
                return (FaceClass::Degenerate, 0.0, 0.0, 0.0, false);
            }
            let area = area2 * 0.5;
            let unorm = scale3(normal, 1.0 / area2);

            let peri = norm3(e01) + norm3(e12) + norm3(e20);
            let inradius = if peri > 1e-14 {
                (2.0 * area) / peri
            } else {
                h_med
            };

            let mut eps = (inradius * eps_scale)
                .max(1e-6 * h_med)
                .min(0.05 * h_med);

            let c = scale3(add3(add3(v0, v1), v2), 1.0 / 3.0);

            // Thin gaps clamping (Addendum item 5)
            if num_shells > 1 && shell_has_neighbor[shell_map[i]] {
                let max_search_dist = eps / 0.45;
                let (_, dist, _) = bvh.closest_point_excluding_within(
                    c,
                    shell_map[i],
                    &shell_map,
                    max_search_dist,
                );
                if dist.is_finite() && dist > 0.0 {
                    eps = eps.min(0.45 * dist);
                }
            }
            eps = eps.max(1e-7 * h_med);

            let p_front = add3(c, scale3(unorm, eps));
            let p_back = sub3(c, scale3(unorm, eps));

            let w_front = bvh.winding_at(p_front);
            let w_back = bvh.winding_at(p_back);

            // Inconsistent orientation / flipped normals check (Addendum item 6)
            let inverted = w_front < -delta || w_back < -delta;

            let w_min = w_front.min(w_back);
            let w_max = w_front.max(w_back);

            // Case A: Outer surface (separates void w < delta from solid w >= 1 - delta)
            if w_min < delta && w_max >= 1.0 - delta {
                if config.check_ray_escape && shell_enclosed[shell_map[i]] {
                    if let Some(st) = &stabber {
                        let p_void = if w_front < delta { p_front } else { p_back };
                        let vote = st.vote(&bvh, p_void);
                        if vote.escape_votes == 0 {
                            let cls = if config.keep_cavities {
                                FaceClass::Cavity
                            } else {
                                FaceClass::Buried
                            };
                            return (cls, w_min, w_front, w_back, inverted);
                        }
                    }
                }
                return (FaceClass::Outer, w_min, w_front, w_back, inverted);
            }

            // Case B: Buried / embedded shell (both sides inside solid volume)
            if w_min >= 1.0 - delta {
                return (FaceClass::Buried, w_min, w_front, w_back, inverted);
            }

            // Case C: Debris in void (both sides outside, w < delta)
            if w_max < delta {
                return (FaceClass::Buried, w_min, w_front, w_back, inverted);
            }

            // Default: outer boundary
            (FaceClass::Outer, w_min, w_front, w_back, inverted)
        })
        .collect();

    for (i, r) in face_results.iter().enumerate() {
        if res.classes[i] != FaceClass::CoincidentOpposite
            && res.classes[i] != FaceClass::CoincidentSame
        {
            res.classes[i] = r.0;
        }
    }

    let has_inverted = face_results.iter().any(|r| r.4);

    // Tally counts & surviving indices
    for (i, &cls) in res.classes.iter().enumerate() {
        match cls {
            FaceClass::Outer => {
                res.outer_count += 1;
                res.surviving_indices.push(i);
            }
            FaceClass::Cavity => {
                res.cavity_count += 1;
                res.surviving_indices.push(i);
            }
            FaceClass::Buried => {
                res.buried_count += 1;
            }
            FaceClass::CoincidentOpposite => {}
            FaceClass::CoincidentSame => {}
            FaceClass::Degenerate => {}
        }
    }

    for &idx in &res.surviving_indices {
        let w_min = face_results[idx].1;
        if w_min >= 1.5 - delta {
            res.internal_walls_count += 1;
        }
    }

    if has_inverted {
        res.has_internal_walls = true;
        res.reason = "inconsistent_orientation".to_string();
    } else if unresolved_coplanar {
        res.has_internal_walls = true;
        res.reason = "unresolved_coplanar_overlapping".to_string();
    } else if res.internal_walls_count > 0 || res.unresolved_coincident_count > 0 {
        res.has_internal_walls = true;
        res.reason = "internal_walls_present".to_string();
    } else {
        res.has_internal_walls = false;
        res.reason = "ok".to_string();
    }

    if res.has_internal_walls {
        res.surviving_indices = (0..m).collect();
    }
    res.is_disjoint = check_is_disjoint(verts, tris, &shell_map, num_shells, &shell_bboxes, &res);
    res
}

/// Check whether a multi-part mesh qualifies as Phase P-a (disjoint parts).
///
/// Parts are disjoint only if:
/// 1. No internal walls, no buried faces, no coincident interface walls, no unresolved faces.
/// 2. No cross-part proper self-intersections (exact triangle intersection).
/// 3. Volume overlap between overlapping part pairs is < 0.1% (computed via divergence theorem).
fn check_is_disjoint(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    shell_map: &[usize],
    num_shells: usize,
    shell_bboxes: &[crate::winding::Aabb],
    res: &ShellAnalysisResult,
) -> bool {
    if res.has_internal_walls
        || res.internal_walls_count > 0
        || res.buried_count > 0
        || res.coincident_opp_count > 0
        || res.coincident_same_count > 0
        || res.unresolved_coincident_count > 0
        || res.reason != "ok"
    {
        return false;
    }

    if num_shells <= 1 {
        return true;
    }

    // Step 1: Check if any pair of shells has overlapping AABBs
    let mut overlapping_pairs = Vec::new();
    for s1 in 0..num_shells {
        let b1 = &shell_bboxes[s1];
        for s2 in (s1 + 1)..num_shells {
            let b2 = &shell_bboxes[s2];
            let overlap = b1.min[0] <= b2.max[0] && b1.max[0] >= b2.min[0]
                && b1.min[1] <= b2.max[1] && b1.max[1] >= b2.min[1]
                && b1.min[2] <= b2.max[2] && b1.max[2] >= b2.min[2];
            if overlap {
                overlapping_pairs.push((s1, s2));
            }
        }
    }

    if overlapping_pairs.is_empty() {
        return true;
    }

    // Step 2: For overlapping shells, check for cross-part proper self-intersections (exact)
    let triangles: Vec<crate::triangle_intersection::Triangle> = tris
        .iter()
        .map(|t| {
            crate::triangle_intersection::Triangle::explicit(
                verts[t[0] as usize],
                verts[t[1] as usize],
                verts[t[2] as usize],
            )
        })
        .collect();
    let tris_usize: Vec<[usize; 3]> = tris
        .iter()
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    let pairs = crate::arrangement::broad_phase_candidates(verts, &tris_usize);
    for (i, j) in pairs {
        let s1 = shell_map[i];
        let s2 = shell_map[j];
        if s1 == s2 {
            continue;
        }
        let pair = if s1 < s2 { (s1, s2) } else { (s2, s1) };
        if !overlapping_pairs.contains(&pair) {
            continue;
        }
        if let crate::triangle_intersection::TriangleIntersection::ProperSegment { .. } =
            crate::triangle_intersection::intersect_triangles(&triangles[i], &triangles[j])
        {
            return false;
        }
    }

    // Step 3: Volume overlap < 0.1% via divergence theorem
    let mut shell_vols = vec![0.0f64; num_shells];
    for (i, t) in tris.iter().enumerate() {
        let sid = shell_map[i];
        let v0 = verts[t[0] as usize];
        let v1 = verts[t[1] as usize];
        let v2 = verts[t[2] as usize];
        let c = [
            v1[1] * v2[2] - v1[2] * v2[1],
            v2[0] * v1[2] - v2[2] * v1[0],
            v1[0] * v2[1] - v1[1] * v2[0],
        ];
        shell_vols[sid] += v0[0] * c[0] + v0[1] * c[1] + v0[2] * c[2];
    }
    for sid in 0..num_shells {
        shell_vols[sid] = (shell_vols[sid] / 6.0).abs();
    }

    for &(s1, s2) in &overlapping_pairs {
        let b1 = &shell_bboxes[s1];
        let b2 = &shell_bboxes[s2];
        let s1_in_b2 = b1.min[0] >= b2.min[0] - 1e-6
            && b1.min[1] >= b2.min[1] - 1e-6
            && b1.min[2] >= b2.min[2] - 1e-6
            && b1.max[0] <= b2.max[0] + 1e-6
            && b1.max[1] <= b2.max[1] + 1e-6
            && b1.max[2] <= b2.max[2] + 1e-6;
        let s2_in_b1 = b2.min[0] >= b1.min[0] - 1e-6
            && b2.min[1] >= b1.min[1] - 1e-6
            && b2.min[2] >= b1.min[2] - 1e-6
            && b2.max[0] <= b1.max[0] + 1e-6
            && b2.max[1] <= b1.max[1] + 1e-6
            && b2.max[2] <= b1.max[2] + 1e-6;
        if s1_in_b2 || s2_in_b1 {
            let min_vol = shell_vols[s1].min(shell_vols[s2]);
            let total_vol = shell_vols[s1] + shell_vols[s2];
            if total_vol > 0.0 && (min_vol / total_vol) >= 0.001 {
                return false;
            }
        }
    }

    true
}

/// Weld bit-identical f64 vertices (normalizing -0.0) and drop degenerate triangles.
fn weld_points_f64(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
    let mut coord_map: HashMap<[u64; 3], u32> = HashMap::with_capacity(verts.len());
    let mut remap: Vec<u32> = Vec::with_capacity(verts.len());
    let mut uniq_verts: Vec<[f64; 3]> = Vec::new();

    for v in verts {
        let x = if v[0] == 0.0 { 0.0 } else { v[0] };
        let y = if v[1] == 0.0 { 0.0 } else { v[1] };
        let z = if v[2] == 0.0 { 0.0 } else { v[2] };
        let key = [x.to_bits(), y.to_bits(), z.to_bits()];
        let next_id = uniq_verts.len() as u32;
        let id = *coord_map.entry(key).or_insert_with(|| {
            uniq_verts.push([x, y, z]);
            next_id
        });
        remap.push(id);
    }

    let mut out_tris = Vec::with_capacity(tris.len());
    for t in tris {
        let u = remap[t[0] as usize];
        let v = remap[t[1] as usize];
        let w = remap[t[2] as usize];
        if u != v && v != w && w != u {
            out_tris.push([u, v, w]);
        }
    }

    (uniq_verts, out_tris)
}

/// Weld f64 vertices within a spatial tolerance and drop degenerate triangles.
fn weld_points_tol(verts: &[[f64; 3]], tris: &[[u32; 3]], tol: f64) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
    let inv_tol = 1.0 / tol;
    let mut grid: HashMap<(i64, i64, i64), Vec<u32>> = HashMap::new();
    let mut uniq_verts: Vec<[f64; 3]> = Vec::new();
    let mut remap: Vec<u32> = Vec::with_capacity(verts.len());

    for &v in verts {
        let gx = (v[0] * inv_tol).round() as i64;
        let gy = (v[1] * inv_tol).round() as i64;
        let gz = (v[2] * inv_tol).round() as i64;

        let mut matched = None;
        'search: for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(candidates) = grid.get(&(gx + dx, gy + dy, gz + dz)) {
                        for &cand in candidates {
                            let vc = uniq_verts[cand as usize];
                            let dist_sq = (v[0] - vc[0]).powi(2) + (v[1] - vc[1]).powi(2) + (v[2] - vc[2]).powi(2);
                            if dist_sq < tol * tol {
                                matched = Some(cand);
                                break 'search;
                            }
                        }
                    }
                }
            }
        }

        let id = match matched {
            Some(cand) => cand,
            None => {
                let new_id = uniq_verts.len() as u32;
                uniq_verts.push(v);
                grid.entry((gx, gy, gz)).or_default().push(new_id);
                new_id
            }
        };
        remap.push(id);
    }

    let mut out_tris = Vec::with_capacity(tris.len());
    for t in tris {
        let u = remap[t[0] as usize];
        let v = remap[t[1] as usize];
        let w = remap[t[2] as usize];
        if u != v && v != w && w != u {
            out_tris.push([u, v, w]);
        }
    }

    (uniq_verts, out_tris)
}

/// Triangulate a simple planar polygon with collinear boundary vertices in the UV
/// space of parent triangle `t` using ear clipping.
///
/// Collinear chords and zero-area triangles are strictly rejected (cr > 1e-12, norm3_sq > 1e-16).
fn ear_clip_polygon_uv(
    verts: &[[f64; 3]],
    t: [u32; 3],
    poly: &[u32],
) -> Vec<[u32; 3]> {
    let m = poly.len();
    if m < 3 {
        return Vec::new();
    }
    if m == 3 {
        let p0 = verts[poly[0] as usize];
        let p1 = verts[poly[1] as usize];
        let p2 = verts[poly[2] as usize];
        let n = cross3(sub3(p1, p0), sub3(p2, p0));
        if norm3_sq(n) > 1e-16 {
            return vec![[poly[0], poly[1], poly[2]]];
        } else {
            return Vec::new();
        }
    }

    // Compute parent triangle's orthonormal UV basis
    let pt0 = verts[t[0] as usize];
    let pt1 = verts[t[1] as usize];
    let pt2 = verts[t[2] as usize];

    let e1 = sub3(pt1, pt0);
    let l1 = norm3(e1);
    if l1 < 1e-12 {
        return Vec::new();
    }
    let u_axis = scale3(e1, 1.0 / l1);
    let norm_vec = cross3(e1, sub3(pt2, pt0));
    let ln = norm3(norm_vec);
    if ln < 1e-12 {
        return Vec::new();
    }
    let n_axis = scale3(norm_vec, 1.0 / ln);
    let v_axis = cross3(n_axis, u_axis);

    // Project polygon vertices into 2D UV
    let mut pts_2d: Vec<[f64; 2]> = Vec::with_capacity(m);
    for &idx in poly {
        let p = verts[idx as usize];
        let d = sub3(p, pt0);
        pts_2d.push([dot3(d, u_axis), dot3(d, v_axis)]);
    }

    let mut order: Vec<usize> = (0..m).collect();
    let mut out: Vec<[u32; 3]> = Vec::with_capacity(m - 2);

    let mut guard = 0usize;
    while order.len() > 3 {
        guard += 1;
        if guard > m * m + 10 {
            break;
        }
        let l = order.len();
        let mut best_i = None;
        let mut best_cr = 0.0f64;

        for i in 0..l {
            let ia = order[(i + l - 1) % l];
            let ib = order[i];
            let ic = order[(i + 1) % l];

            let a = pts_2d[ia];
            let b = pts_2d[ib];
            let c = pts_2d[ic];

            // 2D cross product: must be strictly positive (strictly turning left).
            // Collinear boundary vertices on the same edge have cr == 0, so they are never clipped.
            let cr = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
            if cr <= 1e-12 {
                continue;
            }

            // Check if any other vertex in the active polygon lies inside or on the boundary
            let mut ok = true;
            for &j in &order {
                if j == ia || j == ib || j == ic {
                    continue;
                }
                let q = pts_2d[j];
                let d1 = (b[0] - a[0]) * (q[1] - a[1]) - (b[1] - a[1]) * (q[0] - a[0]);
                let d2 = (c[0] - b[0]) * (q[1] - b[1]) - (c[1] - b[1]) * (q[0] - b[0]);
                let d3 = (a[0] - c[0]) * (q[1] - c[1]) - (a[1] - c[1]) * (q[0] - c[0]);

                // If q is inside or on the boundary of triangle abc (including chord ac)
                if d1 >= -1e-14 && d2 >= -1e-14 && d3 >= -1e-14 {
                    ok = false;
                    break;
                }
            }

            if ok && cr > best_cr {
                best_cr = cr;
                best_i = Some(i);
            }
        }

        if let Some(i) = best_i {
            let ia = order[(i + l - 1) % l];
            let ib = order[i];
            let ic = order[(i + 1) % l];
            let v_a = poly[ia];
            let v_b = poly[ib];
            let v_c = poly[ic];

            // 3D area check: reject degenerate zero-area triangles
            let pa = verts[v_a as usize];
            let pb = verts[v_b as usize];
            let pc = verts[v_c as usize];
            let n = cross3(sub3(pb, pa), sub3(pc, pa));
            if norm3_sq(n) > 1e-16 {
                out.push([v_a, v_b, v_c]);
            }
            order.remove(i);
        } else {
            // No valid ear found with strict bounds; break to fallback
            break;
        }
    }

    if order.len() == 3 {
        let v_a = poly[order[0]];
        let v_b = poly[order[1]];
        let v_c = poly[order[2]];
        let pa = verts[v_a as usize];
        let pb = verts[v_b as usize];
        let pc = verts[v_c as usize];
        let n = cross3(sub3(pb, pa), sub3(pc, pa));
        if norm3_sq(n) > 1e-16 {
            out.push([v_a, v_b, v_c]);
        }
    } else if order.len() > 3 {
        // Fallback: fan from order[0] only to non-collinear vertices with positive area
        let p0_idx = poly[order[0]];
        let pa = verts[p0_idx as usize];
        for k in 1..order.len() - 1 {
            let u_idx = poly[order[k]];
            let v_idx = poly[order[k + 1]];
            let pb = verts[u_idx as usize];
            let pc = verts[v_idx as usize];
            let n = cross3(sub3(pb, pa), sub3(pc, pa));
            if norm3_sq(n) > 1e-16 {
                out.push([p0_idx, u_idx, v_idx]);
            }
        }
    }

    out
}

/// Subdivide triangles along edges where intermediate vertices lie strictly
/// on the edge interior, preventing T-junctions along intersection seams.
///
/// Only input/sub-segment constraint edges are eligible for splitting; internal
/// subdivision chords are excluded from re-splitting to prevent cascades.
fn conforming_edge_split(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    constraint_edges: &mut HashSet<(u32, u32)>,
) -> Vec<[u32; 3]> {
    let tol = 1e-5;
    let mut all_edges: HashMap<(u32, u32), Vec<(f64, u32)>> = HashMap::new();

    for t in tris {
        for k in 0..3 {
            let u = t[k];
            let v = t[(k + 1) % 3];
            let key = (u.min(v), u.max(v));
            // Only input / sub-segment edges are eligible for splitting
            if constraint_edges.contains(&key) {
                all_edges.entry(key).or_default();
            }
        }
    }

    for (&(u, v), splits) in all_edges.iter_mut() {
        let pu = verts[u as usize];
        let pv = verts[v as usize];
        let edge_vec = sub3(pv, pu);
        let lsq = norm3_sq(edge_vec);
        if lsq < 1e-12 {
            continue;
        }

        let min_x = pu[0].min(pv[0]) - tol;
        let max_x = pu[0].max(pv[0]) + tol;
        let min_y = pu[1].min(pv[1]) - tol;
        let max_y = pu[1].max(pv[1]) + tol;
        let min_z = pu[2].min(pv[2]) - tol;
        let max_z = pu[2].max(pv[2]) + tol;

        for (w_idx, pw) in verts.iter().enumerate() {
            let w = w_idx as u32;
            if w == u || w == v {
                continue;
            }
            if pw[0] < min_x || pw[0] > max_x
                || pw[1] < min_y || pw[1] > max_y
                || pw[2] < min_z || pw[2] > max_z
            {
                continue;
            }

            let diff = sub3(*pw, pu);
            let t_val = dot3(diff, edge_vec) / lsq;
            if t_val > 1e-4 && t_val < 1.0 - 1e-4 {
                let proj = add3(pu, scale3(edge_vec, t_val));
                let dist_sq = norm3_sq(sub3(*pw, proj));
                if dist_sq < tol * tol {
                    splits.push((t_val, w));
                }
            }
        }
    }

    let has_any_splits = all_edges.values().any(|s| !s.is_empty());
    if !has_any_splits {
        return tris.to_vec();
    }

    let mut edge_splits: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for ((u, v), mut splits) in all_edges {
        if !splits.is_empty() {
            splits.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            edge_splits.insert((u, v), splits.into_iter().map(|(_, w)| w).collect());
        }
    }

    // Update constraint edges: replace split parent edges with their consecutive sub-segments
    for (&(u, v), splits) in &edge_splits {
        constraint_edges.remove(&(u, v));
        let mut chain = Vec::with_capacity(splits.len() + 2);
        chain.push(u);
        chain.extend_from_slice(splits);
        chain.push(v);
        for w in chain.windows(2) {
            let sub_key = (w[0].min(w[1]), w[0].max(w[1]));
            constraint_edges.insert(sub_key);
        }
    }

    let mut out_tris = Vec::with_capacity(tris.len());
    for t in tris {
        let e0_key = (t[0].min(t[1]), t[0].max(t[1]));
        let e1_key = (t[1].min(t[2]), t[1].max(t[2]));
        let e2_key = (t[2].min(t[0]), t[2].max(t[0]));
        let s0 = edge_splits.get(&e0_key);
        let s1 = edge_splits.get(&e1_key);
        let s2 = edge_splits.get(&e2_key);

        let split_count = s0.is_some() as usize + s1.is_some() as usize + s2.is_some() as usize;
        if split_count == 0 {
            let norm = cross3(
                sub3(verts[t[1] as usize], verts[t[0] as usize]),
                sub3(verts[t[2] as usize], verts[t[0] as usize]),
            );
            if norm3_sq(norm) > 1e-16 {
                out_tris.push(*t);
            }
        } else if split_count == 1 {
            if let Some(splits) = s0 {
                let mut w = vec![t[0]];
                if t[0] < t[1] {
                    w.extend_from_slice(splits);
                } else {
                    for &x in splits.iter().rev() {
                        w.push(x);
                    }
                }
                w.push(t[1]);
                for j in 0..w.len() - 1 {
                    let norm = cross3(
                        sub3(verts[w[j + 1] as usize], verts[w[j] as usize]),
                        sub3(verts[t[2] as usize], verts[w[j] as usize]),
                    );
                    if norm3_sq(norm) > 1e-16 {
                        out_tris.push([w[j], w[j + 1], t[2]]);
                    }
                }
            } else if let Some(splits) = s1 {
                let mut w = vec![t[1]];
                if t[1] < t[2] {
                    w.extend_from_slice(splits);
                } else {
                    for &x in splits.iter().rev() {
                        w.push(x);
                    }
                }
                w.push(t[2]);
                for j in 0..w.len() - 1 {
                    let norm = cross3(
                        sub3(verts[w[j] as usize], verts[t[0] as usize]),
                        sub3(verts[w[j + 1] as usize], verts[t[0] as usize]),
                    );
                    if norm3_sq(norm) > 1e-16 {
                        out_tris.push([t[0], w[j], w[j + 1]]);
                    }
                }
            } else if let Some(splits) = s2 {
                let mut w = vec![t[2]];
                if t[2] < t[0] {
                    w.extend_from_slice(splits);
                } else {
                    for &x in splits.iter().rev() {
                        w.push(x);
                    }
                }
                w.push(t[0]);
                for j in 0..w.len() - 1 {
                    let norm = cross3(
                        sub3(verts[w[j + 1] as usize], verts[w[j] as usize]),
                        sub3(verts[t[1] as usize], verts[w[j] as usize]),
                    );
                    if norm3_sq(norm) > 1e-16 {
                        out_tris.push([w[j], w[j + 1], t[1]]);
                    }
                }
            }
        } else {
            let mut poly: Vec<u32> = Vec::with_capacity(9);
            for k in 0..3 {
                let u = t[k];
                let v = t[(k + 1) % 3];
                poly.push(u);
                let key = (u.min(v), u.max(v));
                if let Some(splits) = edge_splits.get(&key) {
                    if u < v {
                        poly.extend_from_slice(splits);
                    } else {
                        for &w in splits.iter().rev() {
                            poly.push(w);
                        }
                    }
                }
            }
            poly.dedup();
            if poly.len() > 1 && poly.first() == poly.last() {
                poly.pop();
            }
            let clipped = ear_clip_polygon_uv(verts, *t, &poly);
            out_tris.extend(clipped);
        }
    }

    out_tris
}

fn project_tri_uv(
    verts: &[[f64; 3]],
    t: [u32; 3],
    origin: [f64; 3],
    plane_u: [f64; 3],
    plane_v: [f64; 3],
) -> [[f64; 2]; 3] {
    let p0 = verts[t[0] as usize];
    let p1 = verts[t[1] as usize];
    let p2 = verts[t[2] as usize];
    [
        [dot3(sub3(p0, origin), plane_u), dot3(sub3(p0, origin), plane_v)],
        [dot3(sub3(p1, origin), plane_u), dot3(sub3(p1, origin), plane_v)],
        [dot3(sub3(p2, origin), plane_u), dot3(sub3(p2, origin), plane_v)],
    ]
}

/// Resolve coplanar contacts between different bodies/shells using exact 2D overlay.
pub fn resolve_coplanar_contacts(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
) -> (Vec<[f64; 3]>, Vec<[u32; 3]>, bool) {
    if tris.is_empty() || verts.len() < 3 {
        return (verts.to_vec(), tris.to_vec(), false);
    }

    let (shell_map, num_shells) = compute_shell_components(tris);
    if num_shells <= 1 {
        return (verts.to_vec(), tris.to_vec(), false);
    }

    let h_med = median_edge_length(verts, tris);

    // Group triangles by plane key
    let mut plane_map: HashMap<(i64, i64, i64, i64), Vec<usize>> = HashMap::new();
    for (i, &t) in tris.iter().enumerate() {
        let v0 = verts[t[0] as usize];
        let v1 = verts[t[1] as usize];
        let v2 = verts[t[2] as usize];
        let norm = cross3(sub3(v1, v0), sub3(v2, v0));
        if norm3_sq(norm) < 1e-16 {
            continue;
        }
        let pkey = plane_key(norm, v0, h_med);
        plane_map.entry(pkey).or_default().push(i);
    }

    let mut any_modified = false;
    let mut replaced_tris: HashSet<usize> = HashSet::new();
    let mut new_verts: Vec<[f64; 3]> = Vec::new();
    let mut new_tris: Vec<[u32; 3]> = Vec::new();

    for (_pkey, plane_tris) in plane_map {
        if plane_tris.len() <= 1 {
            continue;
        }
        let first_shell = shell_map[plane_tris[0]];
        if !plane_tris.iter().any(|&i| shell_map[i] != first_shell) {
            continue;
        }

        // Find best reference normal for this plane
        let mut max_asq = 0.0;
        let mut best_norm = [0.0, 0.0, 0.0];
        let mut origin = [0.0, 0.0, 0.0];
        for &idx in &plane_tris {
            let t = tris[idx];
            let v0 = verts[t[0] as usize];
            let v1 = verts[t[1] as usize];
            let v2 = verts[t[2] as usize];
            let n = cross3(sub3(v1, v0), sub3(v2, v0));
            let asq = norm3_sq(n);
            if asq > max_asq {
                max_asq = asq;
                best_norm = n;
                origin = v0;
            }
        }
        if max_asq < 1e-16 {
            continue;
        }
        let plane_n = scale3(best_norm, 1.0 / norm3(best_norm));

        let mut a = [1.0, 0.0, 0.0];
        if plane_n[0].abs() > 0.8 {
            a = [0.0, 1.0, 0.0];
        }
        let u_raw = sub3(a, scale3(plane_n, dot3(a, plane_n)));
        let u_len = norm3(u_raw);
        if u_len < 1e-12 {
            continue;
        }
        let plane_u = scale3(u_raw, 1.0 / u_len);
        let plane_v = cross3(plane_n, plane_u);

        // Compute 2D projected triangles and AABBs in this plane
        let projected: Vec<[[f64; 2]; 3]> = plane_tris
            .iter()
            .map(|&idx| project_tri_uv(verts, tris[idx], origin, plane_u, plane_v))
            .collect();
        let bboxes: Vec<[f64; 4]> = projected
            .iter()
            .map(|p| {
                let umin = p[0][0].min(p[1][0]).min(p[2][0]);
                let umax = p[0][0].max(p[1][0]).max(p[2][0]);
                let vmin = p[0][1].min(p[1][1]).min(p[2][1]);
                let vmax = p[0][1].max(p[1][1]).max(p[2][1]);
                [umin, umax, vmin, vmax]
            })
            .collect();

        // Build adjacency graph for triangles on this plane
        let mut adj = vec![Vec::new(); plane_tris.len()];
        for j in 0..plane_tris.len() {
            let tj = tris[plane_tris[j]];
            let bj = &bboxes[j];
            for k in (j + 1)..plane_tris.len() {
                let tk = tris[plane_tris[k]];
                let bk = &bboxes[k];
                if bj[0] > bk[1] + 1e-5
                    || bj[1] < bk[0] - 1e-5
                    || bj[2] > bk[3] + 1e-5
                    || bj[3] < bk[2] - 1e-5
                {
                    continue;
                }
                let share_vert = tj.iter().any(|&v| tk.contains(&v));
                let area_overlap = triangles_proper_overlap_2d(&projected[j], &projected[k])
                    || triangle_canonical_key(tj) == triangle_canonical_key(tk);
                if share_vert || area_overlap {
                    adj[j].push(k);
                    adj[k].push(j);
                }
            }
        }

        // Find connected clusters of triangles
        let mut visited = vec![false; plane_tris.len()];
        for start in 0..plane_tris.len() {
            if visited[start] {
                continue;
            }
            let mut cluster_indices = Vec::new();
            let mut q = vec![start];
            visited[start] = true;
            while let Some(curr) = q.pop() {
                cluster_indices.push(curr);
                for &nbr in &adj[curr] {
                    if !visited[nbr] {
                        visited[nbr] = true;
                        q.push(nbr);
                    }
                }
            }

            // Check if this cluster contains faces from multiple shells that actually overlap
            let mut has_cross_shell_overlap = false;
            let cluster_shells: HashSet<usize> = cluster_indices
                .iter()
                .map(|&ci| shell_map[plane_tris[ci]])
                .collect();
            if cluster_shells.len() > 1 {
                for a_idx in 0..cluster_indices.len() {
                    let ci1 = cluster_indices[a_idx];
                    let s1 = shell_map[plane_tris[ci1]];
                    let p1 = &projected[ci1];
                    let t1 = tris[plane_tris[ci1]];
                    for b_idx in (a_idx + 1)..cluster_indices.len() {
                        let ci2 = cluster_indices[b_idx];
                        let s2 = shell_map[plane_tris[ci2]];
                        if s1 == s2 {
                            continue;
                        }
                        let p2 = &projected[ci2];
                        let t2 = tris[plane_tris[ci2]];
                        if triangles_proper_overlap_2d(p1, p2)
                            || triangle_canonical_key(t1) == triangle_canonical_key(t2)
                        {
                            has_cross_shell_overlap = true;
                            break;
                        }
                    }
                    if has_cross_shell_overlap {
                        break;
                    }
                }
            }

            if !has_cross_shell_overlap {
                continue; // no cross-shell contact in this cluster; keep verbatim!
            }

            // Compute local bounding box of this cluster in (u, v)
            let mut umin = f64::INFINITY;
            let mut umax = f64::NEG_INFINITY;
            let mut vmin = f64::INFINITY;
            let mut vmax = f64::NEG_INFINITY;
            for &ci in &cluster_indices {
                let bb = &bboxes[ci];
                umin = umin.min(bb[0]);
                umax = umax.max(bb[1]);
                vmin = vmin.min(bb[2]);
                vmax = vmax.max(bb[3]);
            }

            let l = (umax - umin).max(vmax - vmin).max(1e-3);
            let cu = (umin + umax) * 0.5;
            let cv = (vmin + vmax) * 0.5;
            let m_pad = 20.0 * l;

            let h0 = add3(origin, add3(scale3(plane_u, cu - m_pad), scale3(plane_v, cv - m_pad)));
            let h1 = add3(origin, add3(scale3(plane_u, cu + 3.0 * m_pad), scale3(plane_v, cv - m_pad)));
            let h2 = add3(origin, add3(scale3(plane_u, cu - m_pad), scale3(plane_v, cv + 3.0 * m_pad)));

            let p0 = Point3::Explicit(h0);
            let p1 = Point3::Explicit(h1);
            let p2 = Point3::Explicit(h2);

            let host_frame = match HostFrame::from_points(&p0, &p1, &p2) {
                Some(f) => f,
                None => continue,
            };
            let mut cdt = match Triangulation::from_host(&p0, &p1, &p2) {
                Some(c) => c,
                None => continue,
            };

            let mut orig_tris_info = Vec::with_capacity(cluster_indices.len());
            let mut constraint_set: HashSet<(usize, usize)> = HashSet::new();
            let mut constraints: Vec<ConstrainedSegment> = Vec::new();

            for &ci in &cluster_indices {
                let tri_idx = plane_tris[ci];
                let t = tris[tri_idx];
                let v0 = verts[t[0] as usize];
                let v1 = verts[t[1] as usize];
                let v2 = verts[t[2] as usize];

                let i0 = match cdt.insert_vertex(&Point3::Explicit(v0)) {
                    Some(i) => i,
                    None => continue,
                };
                let i1 = match cdt.insert_vertex(&Point3::Explicit(v1)) {
                    Some(i) => i,
                    None => continue,
                };
                let i2 = match cdt.insert_vertex(&Point3::Explicit(v2)) {
                    Some(i) => i,
                    None => continue,
                };

                orig_tris_info.push((tri_idx, [i0, i1, i2]));

                let edges = [(i0, i1, v0, v1), (i1, i2, v1, v2), (i2, i0, v2, v0)];
                for (ea, eb, eva, evb) in edges {
                    if ea == eb {
                        continue;
                    }
                    let key = (ea.min(eb), ea.max(eb));
                    if constraint_set.insert(key) {
                        constraints.push(ConstrainedSegment {
                            endpoints: (ea, eb),
                            source: SegmentSource::CoplanarEdge {
                                endpoints: (eva, evb),
                            },
                        });
                    }
                }
            }

            cdt.add_constraints_batch(&constraints);

            let sub_tris = cdt.triangles();
            let cdt_verts = cdt.vertices();

            let mut orig_with_sigma = Vec::with_capacity(orig_tris_info.len());
            for (idx, [i0, i1, i2]) in orig_tris_info {
                let p0 = &cdt_verts[i0].st;
                let p1 = &cdt_verts[i1].st;
                let p2 = &cdt_verts[i2].st;
                let s_orig = orient2d(p0, p1, p2);
                let sigma = if s_orig.is_positive() {
                    1
                } else if s_orig.is_negative() {
                    -1
                } else {
                    0
                };
                orig_with_sigma.push((idx, [i0, i1, i2], sigma));
            }

            let three = BigRational::from_integer(3.into());
            let mut emitted_sub_tris: Vec<[usize; 3]> = Vec::new();

            for tri in sub_tris {
                let (u, v, w) = (tri[0], tri[1], tri[2]);
                if u <= 2 || v <= 2 || w <= 2 {
                    continue; // touches host boundary
                }

                let v_u = &cdt_verts[u].st;
                let v_v = &cdt_verts[v].st;
                let v_w = &cdt_verts[w].st;
                let ct = Point2D {
                    s: (&v_u.s + &v_v.s + &v_w.s) / &three,
                    t: (&v_u.t + &v_v.t + &v_w.t) / &three,
                };

                let mut c_pos = 0;
                let mut c_neg = 0;
                for &(_, [i0, i1, i2], sigma) in &orig_with_sigma {
                    if sigma == 0 {
                        continue;
                    }
                    let p0 = &cdt_verts[i0].st;
                    let p1 = &cdt_verts[i1].st;
                    let p2 = &cdt_verts[i2].st;
                    let s1 = orient2d(p0, p1, &ct);
                    let s2 = orient2d(p1, p2, &ct);
                    let s3 = orient2d(p2, p0, &ct);
                    let inside = if sigma > 0 {
                        s1.is_positive() && s2.is_positive() && s3.is_positive()
                    } else {
                        s1.is_negative() && s2.is_negative() && s3.is_negative()
                    };
                    if inside {
                        if sigma > 0 {
                            c_pos += 1;
                        } else {
                            c_neg += 1;
                        }
                    }
                }

                let desired_sign = if c_pos > 0 && c_neg == 0 {
                    1
                } else if c_pos == 0 && c_neg > 0 {
                    -1
                } else {
                    0
                };

                if desired_sign != 0 {
                    let s_sub = orient2d(v_u, v_v, v_w);
                    if desired_sign == 1 {
                        if s_sub.is_positive() {
                            emitted_sub_tris.push([u, v, w]);
                        } else if s_sub.is_negative() {
                            emitted_sub_tris.push([u, w, v]);
                        }
                    } else if desired_sign == -1 {
                        if s_sub.is_negative() {
                            emitted_sub_tris.push([u, v, w]);
                        } else if s_sub.is_positive() {
                            emitted_sub_tris.push([u, w, v]);
                        }
                    }
                }
            }

            // Map emitted CDT vertices to output mesh vertices
            let mut cdt_to_mesh: HashMap<usize, u32> = HashMap::new();
            for t in &emitted_sub_tris {
                for &k in t {
                    cdt_to_mesh.entry(k).or_insert_with(|| {
                        let pt = match &cdt_verts[k].provenance {
                            Some(Point3::Explicit(p)) => *p,
                            _ => rat3_to_f64(&host_frame.point3d(&cdt_verts[k].st.s, &cdt_verts[k].st.t)),
                        };
                        let next_idx = (verts.len() + new_verts.len()) as u32;
                        new_verts.push(pt);
                        next_idx
                    });
                }
            }

            for t in emitted_sub_tris {
                let ma = cdt_to_mesh[&t[0]];
                let mb = cdt_to_mesh[&t[1]];
                let mc = cdt_to_mesh[&t[2]];
                if ma != mb && mb != mc && mc != ma {
                    new_tris.push([ma, mb, mc]);
                }
            }

            for &ci in &cluster_indices {
                replaced_tris.insert(plane_tris[ci]);
            }
            any_modified = true;
        }
    }

    if !any_modified {
        return (verts.to_vec(), tris.to_vec(), false);
    }

    let mut all_verts = verts.to_vec();
    all_verts.extend(new_verts);

    let mut all_tris = Vec::with_capacity(tris.len() - replaced_tris.len() + new_tris.len());
    for i in 0..tris.len() {
        if !replaced_tris.contains(&i) {
            all_tris.push(tris[i]);
        }
    }
    all_tris.extend(new_tris);

    let (welded_verts, welded_tris) = weld_points_f64(&all_verts, &all_tris);
    let mut constraint_edges: HashSet<(u32, u32)> = HashSet::new();
    for t in &welded_tris {
        for k in 0..3 {
            let u = t[k];
            let v = t[(k + 1) % 3];
            constraint_edges.insert((u.min(v), u.max(v)));
        }
    }
    let conformed_tris = conforming_edge_split(&welded_verts, &welded_tris, &mut constraint_edges);
    let (final_verts, final_tris) = weld_points_f64(&welded_verts, &conformed_tris);

    (final_verts, final_tris, true)
}

fn deduplicate_coplanar_outer_clusters(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> Vec<[u32; 3]> {
    use std::collections::{HashMap, HashSet};

    // 1. Build edge usage
    let mut edge_faces: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (fi, tri) in tris.iter().enumerate() {
        for k in 0..3 {
            let u = tri[k];
            let v = tri[(k + 1) % 3];
            let edge = if u < v { (u, v) } else { (v, u) };
            edge_faces.entry(edge).or_default().push(fi);
        }
    }

    let nm_edges: HashSet<(u32, u32)> = edge_faces
        .iter()
        .filter(|&(_, flist)| flist.len() > 2)
        .map(|(&e, _)| e)
        .collect();

    if nm_edges.is_empty() {
        return tris.to_vec();
    }

    // 2. Candidate faces touching NM edges
    let mut cand_fi = HashSet::new();
    for &e in &nm_edges {
        if let Some(flist) = edge_faces.get(&e) {
            cand_fi.extend(flist.iter().copied());
        }
    }

    // 3. Group by plane
    let mut planes: HashMap<(i64, i64, i64, i64), Vec<usize>> = HashMap::new();
    for &fi in &cand_fi {
        let tri = tris[fi];
        let v0 = verts[tri[0] as usize];
        let v1 = verts[tri[1] as usize];
        let v2 = verts[tri[2] as usize];
        let normal = cross3(sub3(v1, v0), sub3(v2, v0));
        let norm = norm3(normal);
        if norm < 1e-9 {
            continue;
        }
        let unorm = scale3(normal, 1.0 / norm);
        let d = dot3(unorm, v0);
        let key = (
            (unorm[0] * 1000.0).round() as i64,
            (unorm[1] * 1000.0).round() as i64,
            (unorm[2] * 1000.0).round() as i64,
            (d * 1000.0).round() as i64,
        );
        planes.entry(key).or_default().push(fi);
    }

    let mut to_drop = HashSet::new();

    for (_plane_key, flist) in planes {
        if flist.len() <= 1 {
            continue;
        }
        let flist_set: HashSet<usize> = flist.iter().copied().collect();

        // Build clusters connected by internal count-2 edges
        let mut visited = HashSet::new();
        let mut clusters: Vec<Vec<usize>> = Vec::new();

        for &fi in &flist {
            if visited.contains(&fi) {
                continue;
            }
            let mut cluster = Vec::new();
            let mut queue = vec![fi];
            visited.insert(fi);

            while let Some(curr) = queue.pop() {
                cluster.push(curr);
                let tri = tris[curr];
                for k in 0..3 {
                    let u = tri[k];
                    let v = tri[(k + 1) % 3];
                    let edge = if u < v { (u, v) } else { (v, u) };
                    if let Some(neighbors) = edge_faces.get(&edge) {
                        if neighbors.len() == 2 && neighbors.iter().all(|f| flist_set.contains(f)) {
                            for &neighbor in neighbors {
                                if visited.insert(neighbor) {
                                    queue.push(neighbor);
                                }
                            }
                        }
                    }
                }
            }
            clusters.push(cluster);
        }

        // For each cluster, find boundary edges
        let mut cluster_boundaries: Vec<HashSet<(u32, u32)>> = Vec::new();
        for c in &clusters {
            let mut b_edges = HashSet::new();
            for &fi in c {
                let tri = tris[fi];
                for k in 0..3 {
                    let u = tri[k];
                    let v = tri[(k + 1) % 3];
                    let edge = if u < v { (u, v) } else { (v, u) };
                    if !b_edges.remove(&edge) {
                        b_edges.insert(edge);
                    }
                }
            }
            cluster_boundaries.push(b_edges);
        }

        // Check pairs of clusters with identical boundary edges where all boundary edges are NM edges
        for i in 0..clusters.len() {
            if to_drop.iter().any(|f| clusters[i].contains(f)) {
                continue;
            }
            for j in (i + 1)..clusters.len() {
                if to_drop.iter().any(|f| clusters[j].contains(f)) {
                    continue;
                }
                if !cluster_boundaries[i].is_empty()
                    && cluster_boundaries[i] == cluster_boundaries[j]
                    && cluster_boundaries[i].iter().all(|e| nm_edges.contains(e))
                {
                    // Drop cluster j
                    to_drop.extend(clusters[j].iter().copied());
                }
            }
        }
    }

    if to_drop.is_empty() {
        return tris.to_vec();
    }

    tris.iter()
        .enumerate()
        .filter(|(fi, _)| !to_drop.contains(fi))
        .map(|(_, &tri)| tri)
        .collect()
}

/// Complete shell analysis & filter pre-pass for Tamponade (Idea A).
pub fn shell_analyze_and_filter(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    config: &ShellConfig,
) -> (Vec<[f64; 3]>, Vec<[u32; 3]>, ShellAnalysisResult) {
    if tris.is_empty() || verts.is_empty() {
        return (verts.to_vec(), tris.to_vec(), ShellAnalysisResult::default());
    }

    // 1. Try unified exact arrangement pass for self-intersecting or penetrating bodies
    if config.split_intersections {
        let tris_usize: Vec<[usize; 3]> = tris
            .iter()
            .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
            .collect();
        let (_si_mask, si_count, _) = crate::self_intersections_core(verts, &tris_usize);
        if si_count > 0 {
            let prev_opts = crate::cdt2d::experimental::options();
            crate::cdt2d::experimental::set_options(
                prev_opts
                    | crate::cdt2d::experimental::PROPAGATE_EDGE_POINTS
                    | crate::cdt2d::experimental::SPLIT_COPLANAR_OVERLAPS,
            );
            let arr_res = crate::arrangement::arrangement_lite_core(verts, &tris_usize);
            crate::cdt2d::experimental::set_options(prev_opts);

            if let Ok((pool, out_tris, rep)) = arr_res {
                if rep.si_pairs_detected > 0 {
                    let raw_verts: Vec<[f64; 3]> = pool.iter().map(crate::arrangement::rat3_to_f64).collect();
                    let raw_tris: Vec<[u32; 3]> = out_tris.iter().map(|t| [t[0] as u32, t[1] as u32, t[2] as u32]).collect();
                    let (mut stitched_verts, mut stitched_tris) = weld_points_tol(&raw_verts, &raw_tris, 1e-6);
                    let mut constraint_edges: HashSet<(u32, u32)> = HashSet::new();
                    for t in &stitched_tris {
                        for k in 0..3 {
                            let u = t[k];
                            let v = t[(k + 1) % 3];
                            constraint_edges.insert((u.min(v), u.max(v)));
                        }
                    }
                    if std::env::var("SUTURA_SPLIT_PROFILE").is_ok() {
                        eprintln!("[split_pass] initial stitched_tris: {}", stitched_tris.len());
                    }
                    for pass in 0..5 {
                        let next_tris = conforming_edge_split(&stitched_verts, &stitched_tris, &mut constraint_edges);
                        let (w_v, w_t) = weld_points_tol(&stitched_verts, &next_tris, 1e-6);
                        if std::env::var("SUTURA_SPLIT_PROFILE").is_ok() {
                            eprintln!("[split_pass] pass {}: in={} next={} welded={}", pass, stitched_tris.len(), next_tris.len(), w_t.len());
                        }
                        if w_t.len() == stitched_tris.len() {
                            stitched_verts = w_v;
                            stitched_tris = w_t;
                            break;
                        }
                        // Divergence guard as a last-resort safety:
                        if pass > 0 && w_t.len() > stitched_tris.len() * 2 {
                            if std::env::var("SUTURA_SPLIT_PROFILE").is_ok() {
                                eprintln!("[split_pass] divergence guard triggered at pass {}", pass);
                            }
                            break;
                        }
                        stitched_verts = w_v;
                        stitched_tris = w_t;
                    }
                    let mut split_cfg = config.clone();
                    split_cfg.split_intersections = false;
                    let split_res = analyze_shells(&stitched_verts, &stitched_tris, &split_cfg);
                    if !split_res.has_internal_walls && split_res.reason == "ok" {
                        let mut final_tris = Vec::with_capacity(split_res.surviving_indices.len());
                        for &idx in &split_res.surviving_indices {
                            final_tris.push(stitched_tris[idx]);
                        }
                        let final_tris = deduplicate_coplanar_outer_clusters(&stitched_verts, &final_tris);
                        return (stitched_verts, final_tris, split_res);
                    }
                }
            }
        }
    }

    // 2. Pure coplanar contact fallback (e.g. adjacent stacked boxes without transversal penetration)
    let (cop_verts, cop_tris, cop_changed) = resolve_coplanar_contacts(verts, tris);
    let cur_verts = if cop_changed { &cop_verts } else { verts };
    let cur_tris = if cop_changed { &cop_tris } else { tris };

    let analysis = analyze_shells(cur_verts, cur_tris, config);

    // Fast path: if the mesh is disjoint (Phase P-a) or has no faces needing deletion,
    // return mesh verbatim without running expensive arrangement splitting!
    if analysis.is_disjoint
        || (analysis.buried_count == 0
            && analysis.coincident_opp_count == 0
            && analysis.coincident_same_count == 0
            && !analysis.has_internal_walls)
    {
        return (cur_verts.to_vec(), cur_tris.to_vec(), analysis);
    }


    let mut out_tris = Vec::with_capacity(analysis.surviving_indices.len());
    for &idx in &analysis.surviving_indices {
        out_tris.push(cur_tris[idx]);
    }
    let out_tris = deduplicate_coplanar_outer_clusters(cur_verts, &out_tris);
    (cur_verts.to_vec(), out_tris, analysis)
}

/// Filter input triangles to keep only surviving outer and cavity faces (alias).
pub fn filter_surviving_mesh(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    config: &ShellConfig,
) -> (Vec<[f64; 3]>, Vec<[u32; 3]>, ShellAnalysisResult) {
    shell_analyze_and_filter(verts, tris, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate an axis-aligned unit solid box with outward normals.
    fn unit_box(origin: [f64; 3], size: [f64; 3]) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let x0 = origin[0];
        let y0 = origin[1];
        let z0 = origin[2];
        let x1 = origin[0] + size[0];
        let y1 = origin[1] + size[1];
        let z1 = origin[2] + size[2];

        let verts = vec![
            [x0, y0, z0], // 0
            [x1, y0, z0], // 1
            [x1, y1, z0], // 2
            [x0, y1, z0], // 3
            [x0, y0, z1], // 4
            [x1, y0, z1], // 5
            [x1, y1, z1], // 6
            [x0, y1, z1], // 7
        ];

        let tris = vec![
            // bottom (z = z0)
            [0, 2, 1],
            [0, 3, 2],
            // top (z = z1)
            [4, 5, 6],
            [4, 6, 7],
            // front (y = y0)
            [0, 1, 5],
            [0, 5, 4],
            // back (y = y1)
            [2, 3, 7],
            [2, 7, 6],
            // left (x = x0)
            [0, 4, 7],
            [0, 7, 3],
            // right (x = x1)
            [1, 2, 6],
            [1, 6, 5],
        ];

        (verts, tris)
    }

    #[test]
    fn test_single_cube_all_outer() {
        let (verts, tris) = unit_box([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let config = ShellConfig::default();
        let res = analyze_shells(&verts, &tris, &config);

        assert_eq!(res.outer_count, 12);
        assert_eq!(res.buried_count, 0);
        assert_eq!(res.coincident_opp_count, 0);
        assert_eq!(res.surviving_indices.len(), 12);
    }

    #[test]
    fn test_concentric_nested_cubes() {
        // Outer cube: 2x2x2 centered at 0
        let (mut verts, mut tris) = unit_box([-1.0, -1.0, -1.0], [2.0, 2.0, 2.0]);
        // Inner cube: 0.5x0.5x0.5 centered at 0
        let (in_v, in_t) = unit_box([-0.25, -0.25, -0.25], [0.5, 0.5, 0.5]);

        let offset = verts.len() as u32;
        verts.extend(in_v);
        for t in in_t {
            tris.push([t[0] + offset, t[1] + offset, t[2] + offset]);
        }

        assert_eq!(tris.len(), 24);
        let config = ShellConfig::default();
        let res = analyze_shells(&verts, &tris, &config);

        // The 12 faces of the outer cube must be Outer
        assert_eq!(res.outer_count, 12);
        // The 12 faces of the inner cube must be Buried
        assert_eq!(res.buried_count, 12);
        assert_eq!(res.surviving_indices.len(), 12);
    }

    #[test]
    fn test_touching_cubes_coincident_faces_dissolved() {
        // Cube 1 at [0, 0, 0] size [1, 1, 1]
        let (mut verts, mut tris) = unit_box([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        // Cube 2 at [1, 0, 0] size [1, 1, 1]
        let (v2, t2) = unit_box([1.0, 0.0, 0.0], [1.0, 1.0, 1.0]);

        // Welded coordinates: weld coincident vertices at x = 1.0
        let mut weld_map: Vec<u32> = Vec::new();
        for p in &v2 {
            let mut found = None;
            for (idx, existing) in verts.iter().enumerate() {
                if norm3(sub3(*existing, *p)) < 1e-9 {
                    found = Some(idx as u32);
                    break;
                }
            }
            if let Some(idx) = found {
                weld_map.push(idx);
            } else {
                weld_map.push(verts.len() as u32);
                verts.push(*p);
            }
        }

        for t in t2 {
            tris.push([weld_map[t[0] as usize], weld_map[t[1] as usize], weld_map[t[2] as usize]]);
        }

        assert_eq!(tris.len(), 24);
        let config = ShellConfig::default();
        let res = analyze_shells(&verts, &tris, &config);

        // The 4 contact triangles (2 on each cube forming the touching square)
        // must be CoincidentOpposite and dissolved
        assert_eq!(res.coincident_opp_count, 4);
        assert_eq!(res.outer_count, 20);
        assert_eq!(res.surviving_indices.len(), 20);
    }

    #[test]
    fn test_cube_with_interior_cavity() {
        // Outer cube: 2x2x2 centered at 0 (outward normals)
        let (mut verts, mut tris) = unit_box([-1.0, -1.0, -1.0], [2.0, 2.0, 2.0]);
        // Inner cavity: 0.5x0.5x0.5 centered at 0 with reversed normals (inward normals facing the cavity void)
        let (cav_v, cav_t) = unit_box([-0.25, -0.25, -0.25], [0.5, 0.5, 0.5]);

        let offset = verts.len() as u32;
        verts.extend(cav_v);
        for t in cav_t {
            // reverse orientation so normal points into the void
            tris.push([t[0] + offset, t[2] + offset, t[1] + offset]);
        }

        assert_eq!(tris.len(), 24);
        let config = ShellConfig::default();
        let res = analyze_shells(&verts, &tris, &config);

        // 12 outer faces + 12 cavity faces (both preserved)
        assert_eq!(res.outer_count, 12);
        assert_eq!(res.cavity_count, 12);
        assert_eq!(res.surviving_indices.len(), 24);
    }

    /// STL reader with exact vertex welding (handles binary and ASCII).
    fn read_stl_welded(path: &str) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let data = std::fs::read(path).expect("read STL");
        let mut index: HashMap<[u32; 3], u32> = HashMap::new();
        let mut verts = Vec::new();
        let mut tris = Vec::new();

        let is_binary = if data.len() >= 84 {
            let n = u32::from_le_bytes(data[80..84].try_into().unwrap()) as usize;
            84 + 50 * n == data.len()
        } else {
            false
        };

        if is_binary {
            let n = u32::from_le_bytes(data[80..84].try_into().unwrap()) as usize;
            for f in 0..n {
                let base = 84 + 50 * f + 12;
                let mut tri = [0u32; 3];
                for (k, slot) in tri.iter_mut().enumerate() {
                    let o = base + 12 * k;
                    let c: [u32; 3] = std::array::from_fn(|j| {
                        u32::from_le_bytes(data[o + 4 * j..o + 4 * j + 4].try_into().unwrap())
                    });
                    *slot = *index.entry(c).or_insert_with(|| {
                        verts.push(c.map(|b| f32::from_bits(b) as f64));
                        (verts.len() - 1) as u32
                    });
                }
                tris.push(tri);
            }
        } else {
            let text = String::from_utf8_lossy(&data);
            let mut cur_tri = Vec::with_capacity(3);
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("vertex") {
                    let parts: Vec<&str> = trimmed.split_whitespace().collect();
                    if parts.len() >= 4 {
                        let x: f32 = parts[1].parse().unwrap_or(0.0);
                        let y: f32 = parts[2].parse().unwrap_or(0.0);
                        let z: f32 = parts[3].parse().unwrap_or(0.0);
                        let c = [x.to_bits(), y.to_bits(), z.to_bits()];
                        let idx = *index.entry(c).or_insert_with(|| {
                            verts.push([x as f64, y as f64, z as f64]);
                            (verts.len() - 1) as u32
                        });
                        cur_tri.push(idx);
                        if cur_tri.len() == 3 {
                            tris.push([cur_tri[0], cur_tri[1], cur_tri[2]]);
                            cur_tri.clear();
                        }
                    }
                }
            }
        }
        (verts, tris)
    }

    #[test]
    fn test_60916_all_verbatim() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/real-world-samples/thingi10k_60916.stl"
        );
        let (verts, tris) = read_stl_welded(path);
        assert_eq!(tris.len(), 12430);

        let config = ShellConfig::default();
        let res = analyze_shells(&verts, &tris, &config);

        println!(
            "60916: outer={} buried={} cavity={} coinc_opp={} coinc_same={}",
            res.outer_count,
            res.buried_count,
            res.cavity_count,
            res.coincident_opp_count,
            res.coincident_same_count
        );
        // All 12430 faces survive verbatim (100.0% healthy faces)
        assert_eq!(res.surviving_indices.len(), 12430);
    }

    #[test]
    fn test_100045_outer_classification() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/real-world-samples/thingi10k_100045.stl"
        );
        let (verts, tris) = read_stl_welded(path);
        assert_eq!(tris.len(), 420);

        let config = ShellConfig::default();
        let res = analyze_shells(&verts, &tris, &config);

        println!(
            "100045: reason={} outer={} buried={} cavity={} coinc_opp={} coinc_same={}",
            res.reason,
            res.outer_count,
            res.buried_count,
            res.cavity_count,
            res.coincident_opp_count,
            res.coincident_same_count
        );
        // 100045 has interpenetrating internal shells (Delta = 6.918%) with coplanar overlapping faces,
        // so it must trigger has_internal_walls and unresolved_coplanar_overlapping fallback:
        assert!(res.has_internal_walls);
        assert_eq!(res.reason, "unresolved_coplanar_overlapping");
        assert_eq!(res.surviving_indices.len(), 420);
    }

    #[test]
    fn test_248395_shell_analysis() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/real-world-samples/thingi10k_248395.stl"
        );
        let (verts, tris) = read_stl_welded(path);
        assert_eq!(tris.len(), 13264);

        let config = ShellConfig::default();
        let res = analyze_shells(&verts, &tris, &config);

        println!("248395 res.reason: {}", res.reason);
        println!(
            "248395: outer={} buried={} cavity={} coinc_opp={} coinc_same={}",
            res.outer_count,
            res.buried_count,
            res.cavity_count,
            res.coincident_opp_count,
            res.coincident_same_count
        );
        let buried_indices: Vec<usize> = res.classes.iter().enumerate()
            .filter(|(_, &c)| c == FaceClass::Buried)
            .map(|(i, _)| i)
            .collect();
        println!("248395 buried faces count: {}, first 10: {:?}", buried_indices.len(), &buried_indices[..10.min(buried_indices.len())]);
        assert_eq!(res.cavity_count, 0);
        assert!(res.outer_count >= 12300);
        assert!(res.surviving_indices.len() >= 12300);
        assert!(!res.has_internal_walls);
        assert_eq!(res.reason, "ok");
    }

    #[test]
    fn test_1038441_shell_analysis() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/real-world-samples/thingi10k_1038441.stl"
        );
        let (verts, tris) = read_stl_welded(path);
        let config = ShellConfig::default();
        let res = analyze_shells(&verts, &tris, &config);
        println!(
            "1038441: outer={} buried={} cavity={} coinc_opp={} coinc_same={}",
            res.outer_count,
            res.buried_count,
            res.cavity_count,
            res.coincident_opp_count,
            res.coincident_same_count
        );
    }

    /// Generate a hollow box (outer solid with reversed inner void, forming a wall).
    fn hollow_box(origin: [f64; 3], size: [f64; 3], wall_thickness: f64) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let (mut verts, mut tris) = unit_box(origin, size);
        let inner_origin = [
            origin[0] + wall_thickness,
            origin[1] + wall_thickness,
            origin[2] + wall_thickness,
        ];
        let inner_size = [
            size[0] - 2.0 * wall_thickness,
            size[1] - 2.0 * wall_thickness,
            size[2] - 2.0 * wall_thickness,
        ];
        let (in_v, in_t) = unit_box(inner_origin, inner_size);
        let offset = verts.len() as u32;
        verts.extend(in_v);
        // Reversed normal for inner cavity
        for t in in_t {
            tris.push([offset + t[0], offset + t[2], offset + t[1]]);
        }
        (verts, tris)
    }

    /// Combine two meshes into one triangle soup.
    fn combine_meshes(
        m1: (Vec<[f64; 3]>, Vec<[u32; 3]>),
        m2: (Vec<[f64; 3]>, Vec<[u32; 3]>),
    ) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let (mut v1, mut t1) = m1;
        let (v2, t2) = m2;
        let offset = v1.len() as u32;
        v1.extend(v2);
        for t in t2 {
            t1.push([offset + t[0], offset + t[1], offset + t[2]]);
        }
        (v1, t1)
    }

    #[test]
    fn test_onion_with_air_gaps() {
        // Addendum item 1: concentric thin-walled layers with air between layers.
        // Every face has air on one side and wall on the other (0 <-> 1).
        // ALL layers must stay 100% verbatim, parts unchanged.
        let layer1 = hollow_box([-2.0, -2.0, -2.0], [4.0, 4.0, 4.0], 0.2); // wall thickness 0.2
        let layer2 = hollow_box([-1.4, -1.4, -1.4], [2.8, 2.8, 2.8], 0.2); // gap of 0.2 from 1.6 to 1.4
        let (verts, tris) = combine_meshes(layer1, layer2);
        assert_eq!(tris.len(), 48);

        let config = ShellConfig::default();
        let (_out_v, out_t, res) = shell_analyze_and_filter(&verts, &tris, &config);

        assert_eq!(res.buried_count, 0);
        assert_eq!(res.surviving_indices.len(), 48);
        assert_eq!(out_t.len(), 48);
        println!("onion res.reason={}, internal_walls_count={}, unresolved_coinc={}",
            res.reason, res.internal_walls_count, res.unresolved_coincident_count);
        assert!(!res.has_internal_walls);
        assert_eq!(res.reason, "ok");
    }

    #[test]
    fn test_nested_solid_shells_sloppy_onion() {
        // Addendum item 2: nested solid shells, all outward-oriented, no gaps.
        // Inner shells are fully enclosed (w >= 1 on both sides) -> dissolving them is correct.
        // Output = outer box verbatim, 0 holes/NM.
        let b1 = unit_box([-3.0, -3.0, -3.0], [6.0, 6.0, 6.0]);
        let b2 = unit_box([-2.0, -2.0, -2.0], [4.0, 4.0, 4.0]);
        let b3 = unit_box([-1.0, -1.0, -1.0], [2.0, 2.0, 2.0]);
        let (verts, tris) = combine_meshes(combine_meshes(b1, b2), b3);
        assert_eq!(tris.len(), 36);

        let config = ShellConfig::default();
        let (_out_v, out_t, res) = shell_analyze_and_filter(&verts, &tris, &config);

        assert_eq!(res.outer_count, 12);
        assert_eq!(res.buried_count, 24);
        assert_eq!(out_t.len(), 12); // outer box verbatim
        assert!(!res.has_internal_walls);
        assert_eq!(res.reason, "ok");
    }

    #[test]
    fn test_half_cut_onion_resolved() {
        // Addendum item 3: nested solids cut by one plane (z = 0), resolved via coplanar 2D overlay.
        let outer = unit_box([-2.0, -2.0, 0.0], [4.0, 4.0, 2.0]);
        let inner = unit_box([-1.0, -1.0, 0.0], [2.0, 2.0, 1.0]);
        let (verts, tris) = combine_meshes(outer, inner);

        let config = ShellConfig::default();
        let (_out_v, out_t, res) = shell_analyze_and_filter(&verts, &tris, &config);

        assert!(!res.has_internal_walls);
        assert_eq!(res.reason, "ok");
        let (holes, nm, parts) = check_mesh_closed_manifold_single_part(&out_t);
        assert_eq!(holes, 0, "Must have 0 holes, found {}", holes);
        assert_eq!(nm, 0, "Must have 0 non-manifold edges, found {}", nm);
        assert_eq!(parts, 1, "Must be 1 single part, found {}", parts);
    }

    #[test]
    fn test_flower_three_boxes_crossing_body() {
        // Addendum item 4: petals that interpenetrate the centre and each other, partly outside.
        // Visible parts stay verbatim, seams via exact split (C1).
        let center = unit_box([-1.0, -1.0, -1.0], [2.0, 2.0, 2.0]);
        let petal1 = unit_box([0.5, -0.3, -0.3], [1.5, 0.6, 0.6]);
        let petal2 = unit_box([-0.3, 0.5, -0.3], [0.6, 1.5, 0.6]);
        let petal3 = unit_box([-0.3, -0.3, 0.5], [0.6, 0.6, 1.5]);
        let (verts, tris) = combine_meshes(
            combine_meshes(center, petal1),
            combine_meshes(petal2, petal3),
        );

        let config = ShellConfig::default();
        let (_out_v, out_t, res) = shell_analyze_and_filter(&verts, &tris, &config);

        assert!(!res.has_internal_walls);
        assert_eq!(res.reason, "ok");
        // Buried parts dissolved, outer preserved with exact seams
        assert!(out_t.len() > 0);
        let directed = crate::tamponade::directed_boundary_edges(&out_t);
        assert!(directed.is_empty(), "Flower must be watertight with 0 holes! holes={}", directed.len());
    }

    #[test]
    fn test_thin_gaps_clamped_eps() {
        // Addendum item 5: gap between layers smaller than query offset eps.
        // eps must be clamped below half the local gap, layers must not be merged.
        let l1 = hollow_box([-1.0, -1.0, -1.0], [2.0, 2.0, 2.0], 0.05); // outer size 2.0, inner hollow 1.9
        let l2 = hollow_box([-0.85, -0.85, -0.85], [1.7, 1.7, 1.7], 0.05); // gap is 0.1, h_med ~ 1.5
        let (verts, tris) = combine_meshes(l1, l2);

        let config = ShellConfig::default();
        let (_out_v, out_t, res) = shell_analyze_and_filter(&verts, &tris, &config);

        assert_eq!(res.buried_count, 0);
        assert_eq!(res.surviving_indices.len(), 48);
        assert_eq!(out_t.len(), 48);
        assert!(!res.has_internal_walls);
        assert_eq!(res.reason, "ok");
    }

    #[test]
    fn test_inconsistent_orientation_inner_shell_fallback() {
        // Addendum item 6: Inconsistently oriented inner shell (flipped normals, winding -1 / 0 inside)
        // -> ambiguous -> fallback, never 'fix' it by deleting.
        let outer = hollow_box([-3.0, -3.0, -3.0], [6.0, 6.0, 6.0], 1.0);
        let (in_v, in_t) = unit_box([-1.0, -1.0, -1.0], [2.0, 2.0, 2.0]);
        // Flip inner normals -> inside winding is -1, outside is 0
        let mut flipped_in_t = Vec::new();
        for t in in_t {
            flipped_in_t.push([t[0], t[2], t[1]]);
        }
        let (verts, tris) = combine_meshes(outer, (in_v, flipped_in_t));

        let config = ShellConfig::default();
        let (_out_v, out_t, res) = shell_analyze_and_filter(&verts, &tris, &config);

        assert!(res.has_internal_walls);
        assert_eq!(res.reason, "inconsistent_orientation");
        assert_eq!(out_t.len(), tris.len()); // never delete inner shell
    }

    #[test]
    fn test_two_boxes_overlapping_thin_slab_not_pa() {
        // Review #2 item 1: two boxes that overlap by a thin slab -> NOT P-a disjoint
        let b1 = unit_box([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        // Box 2 at [0.9, 0.0, 0.0] overlaps Box 1 by a thin slab x in [0.9, 1.0]
        let b2 = unit_box([0.9, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let (verts, tris) = combine_meshes(b1, b2);

        let config = ShellConfig::default();
        let (_out_v, _out_t, res) = shell_analyze_and_filter(&verts, &tris, &config);

        assert!(
            !res.is_disjoint,
            "Two boxes overlapping by a thin slab must NOT qualify as P-a disjoint"
        );
    }

    #[test]
    fn test_two_boxes_separated_is_pa() {
        // Two disjoint boxes separated by space -> IS P-a disjoint
        let b1 = unit_box([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let b2 = unit_box([2.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let (verts, tris) = combine_meshes(b1, b2);

        let config = ShellConfig::default();
        let (_out_v, _out_t, res) = shell_analyze_and_filter(&verts, &tris, &config);

        assert!(
            res.is_disjoint,
            "Two separated boxes must qualify as P-a disjoint"
        );
    }

    fn check_mesh_closed_manifold_single_part(tris: &[[u32; 3]]) -> (usize, usize, usize) {
        let mut directed: HashMap<(u32, u32), usize> = HashMap::new();
        let mut undirected: HashMap<(u32, u32), usize> = HashMap::new();
        for t in tris {
            for k in 0..3 {
                let (u, v) = (t[k], t[(k + 1) % 3]);
                *directed.entry((u, v)).or_insert(0) += 1;
                *undirected.entry((u.min(v), u.max(v))).or_insert(0) += 1;
            }
        }
        let boundary_edges = undirected.values().filter(|&&c| c == 1).count();
        let nm_edges = undirected.values().filter(|&&c| c > 2).count()
            + directed.values().filter(|&&c| c > 1).count();
        let (_, parts) = compute_shell_components(tris);
        (boundary_edges, nm_edges, parts)
    }

    fn quant_pt(p: [f64; 3]) -> (i64, i64, i64) {
        ((p[0] * 1e6).round() as i64, (p[1] * 1e6).round() as i64, (p[2] * 1e6).round() as i64)
    }

    #[test]
    fn test_two_interpenetrating_boxes_pc() {
        // Two interpenetrating boxes: [0, 1]^3 and [0.4, 1.4]^3
        let b1 = unit_box([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let b2 = unit_box([0.4, 0.4, 0.4], [1.0, 1.0, 1.0]);
        let (verts, tris) = combine_meshes(b1, b2);

        let tris_usize: Vec<[usize; 3]> = tris.iter().map(|t| [t[0] as usize, t[1] as usize, t[2] as usize]).collect();
        let (si_mask, si_count, _) = crate::self_intersections_core(&verts, &tris_usize);
        assert!(si_count > 0, "Boxes must penetrate");

        let mut config = ShellConfig::default();
        config.split_intersections = true;
        let (out_v, out_t, res) = shell_analyze_and_filter(&verts, &tris, &config);

        assert_eq!(res.reason, "ok");
        assert!(!res.has_internal_walls);

        let (holes, nm, parts) = check_mesh_closed_manifold_single_part(&out_t);
        assert_eq!(holes, 0, "Must have 0 holes, found {}", holes);
        assert_eq!(nm, 0, "Must have 0 non-manifold edges, found {}", nm);
        assert_eq!(parts, 1, "Must be 1 single part, found {}", parts);

        // Check non-SI outer faces are preserved verbatim
        let mut out_faces = std::collections::HashSet::new();
        for t in &out_t {
            let p0 = quant_pt(out_v[t[0] as usize]);
            let p1 = quant_pt(out_v[t[1] as usize]);
            let p2 = quant_pt(out_v[t[2] as usize]);
            out_faces.insert((p0, p1, p2));
            out_faces.insert((p1, p2, p0));
            out_faces.insert((p2, p0, p1));
        }

        let mut non_si_outer_verbatim = 0;
        for (i, &is_si) in si_mask.iter().enumerate() {
            if !is_si {
                let t = tris[i];
                let q0 = quant_pt(verts[t[0] as usize]);
                let q1 = quant_pt(verts[t[1] as usize]);
                let q2 = quant_pt(verts[t[2] as usize]);
                if out_faces.contains(&(q0, q1, q2)) {
                    non_si_outer_verbatim += 1;
                }
            }
        }
        assert_eq!(non_si_outer_verbatim, 12, "All 12 outer non-SI faces must be verbatim in output");
    }

    #[test]
    fn test_box_through_sphere_pc() {
        // Octahedron sphere penetrated by an elongated box
        let sphere_v = vec![
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
        ];
        let sphere_t = vec![
            [0, 2, 4],
            [2, 1, 4],
            [1, 3, 4],
            [3, 0, 4],
            [2, 0, 5],
            [1, 2, 5],
            [3, 1, 5],
            [0, 3, 5],
        ];
        let (box_v, box_t) = unit_box([-0.3, -0.3, -1.2], [0.6, 0.6, 2.4]);
        let (verts, tris) = combine_meshes((sphere_v, sphere_t), (box_v, box_t));

        let tris_usize: Vec<[usize; 3]> = tris.iter().map(|t| [t[0] as usize, t[1] as usize, t[2] as usize]).collect();
        let (si_mask, si_count, _) = crate::self_intersections_core(&verts, &tris_usize);
        assert!(si_count > 0, "Box and sphere must penetrate");

        let mut config = ShellConfig::default();
        config.split_intersections = true;
        let (out_v, out_t, res) = shell_analyze_and_filter(&verts, &tris, &config);

        assert_eq!(res.reason, "ok");
        assert!(!res.has_internal_walls);

        let (holes, nm, parts) = check_mesh_closed_manifold_single_part(&out_t);
        assert_eq!(holes, 0, "Must have 0 holes, found {}", holes);
        assert_eq!(nm, 0, "Must have 0 non-manifold edges, found {}", nm);
        assert_eq!(parts, 1, "Must be 1 single part, found {}", parts);

        // Check non-SI outer faces are preserved verbatim
        let mut out_faces = std::collections::HashSet::new();
        for t in &out_t {
            let p0 = quant_pt(out_v[t[0] as usize]);
            let p1 = quant_pt(out_v[t[1] as usize]);
            let p2 = quant_pt(out_v[t[2] as usize]);
            out_faces.insert((p0, p1, p2));
            out_faces.insert((p1, p2, p0));
            out_faces.insert((p2, p0, p1));
        }

        let mut non_si_outer_verbatim = 0;
        for (i, &is_si) in si_mask.iter().enumerate() {
            if !is_si {
                let t = tris[i];
                let q0 = quant_pt(verts[t[0] as usize]);
                let q1 = quant_pt(verts[t[1] as usize]);
                let q2 = quant_pt(verts[t[2] as usize]);
                if out_faces.contains(&(q0, q1, q2)) {
                    non_si_outer_verbatim += 1;
                }
            }
        }
        assert!(non_si_outer_verbatim >= 2, "Non-SI outer end faces of the box must be verbatim");
    }

    #[test]
    fn test_two_boxes_coplanar_contact_diff_tri() {
        // Box 1: [0, 1]^3. Top face at z = 1.0 has 2 triangles.
        let b1 = unit_box([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);

        // Box 2: [0, 1] x [0, 1] x [1, 2].
        // Bottom face at z = 1.0 has 4 triangles meeting at center (0.5, 0.5, 1.0).
        let b2_v = vec![
            [0.0, 0.0, 1.0], // 0
            [1.0, 0.0, 1.0], // 1
            [1.0, 1.0, 1.0], // 2
            [0.0, 1.0, 1.0], // 3
            [0.0, 0.0, 2.0], // 4
            [1.0, 0.0, 2.0], // 5
            [1.0, 1.0, 2.0], // 6
            [0.0, 1.0, 2.0], // 7
            [0.5, 0.5, 1.0], // 8 center vertex on bottom face
        ];
        let b2_t = vec![
            // bottom face (z = 1.0) with 4 triangles
            [8, 1, 0],
            [8, 2, 1],
            [8, 3, 2],
            [8, 0, 3],
            // top (z = 2.0)
            [4, 5, 6],
            [4, 6, 7],
            // front (y = 0.0)
            [0, 1, 5],
            [0, 5, 4],
            // back (y = 1.0)
            [2, 3, 7],
            [2, 7, 6],
            // left (x = 0.0)
            [0, 4, 7],
            [0, 7, 3],
            // right (x = 1.0)
            [1, 2, 6],
            [1, 6, 5],
        ];

        let (verts, tris) = combine_meshes(b1, (b2_v, b2_t));

        let mut expected_non_contact = std::collections::HashSet::new();
        // b1 non-contact faces: bottom(0, 1), front(4, 5), back(6, 7), left(8, 9), right(10, 11)
        for i in [0, 1, 4, 5, 6, 7, 8, 9, 10, 11] {
            let t = tris[i];
            let q0 = quant_pt(verts[t[0] as usize]);
            let q1 = quant_pt(verts[t[1] as usize]);
            let q2 = quant_pt(verts[t[2] as usize]);
            expected_non_contact.insert((q0, q1, q2));
        }
        // b2 non-contact faces: top(16, 17), front(18, 19), back(20, 21), left(22, 23), right(24, 25)
        for i in [16, 17, 18, 19, 20, 21, 22, 23, 24, 25] {
            let t = tris[i];
            let q0 = quant_pt(verts[t[0] as usize]);
            let q1 = quant_pt(verts[t[1] as usize]);
            let q2 = quant_pt(verts[t[2] as usize]);
            expected_non_contact.insert((q0, q1, q2));
        }
        assert_eq!(expected_non_contact.len(), 20);

        let config = ShellConfig::default();
        let (out_v, out_t, res) = shell_analyze_and_filter(&verts, &tris, &config);

        assert_eq!(res.reason, "ok");
        assert!(!res.has_internal_walls);

        let (holes, nm, parts) = check_mesh_closed_manifold_single_part(&out_t);
        assert_eq!(holes, 0, "Must have 0 holes, found {}", holes);
        assert_eq!(nm, 0, "Must have 0 non-manifold edges, found {}", nm);
        assert_eq!(parts, 1, "Must be 1 single part, found {}", parts);

        let mut out_faces = std::collections::HashSet::new();
        for t in &out_t {
            let p0 = quant_pt(out_v[t[0] as usize]);
            let p1 = quant_pt(out_v[t[1] as usize]);
            let p2 = quant_pt(out_v[t[2] as usize]);
            out_faces.insert((p0, p1, p2));
            out_faces.insert((p1, p2, p0));
            out_faces.insert((p2, p0, p1));
        }

        let mut verbatim_count = 0;
        for &(q0, q1, q2) in &expected_non_contact {
            if out_faces.contains(&(q0, q1, q2)) {
                verbatim_count += 1;
            }
        }
        assert_eq!(verbatim_count, 20, "All 20 non-contact faces must be verbatim in output");
    }

    #[test]
    fn test_half_overlapping_stacked_boxes() {
        // Box 1: [0, 1]^3
        let b1 = unit_box([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        // Box 2: [0.5, 1.5] x [0, 1] x [1, 2]
        let b2 = unit_box([0.5, 0.0, 1.0], [1.0, 1.0, 1.0]);

        let (verts, tris) = combine_meshes(b1, b2);

        let config = ShellConfig::default();
        let (_out_v, out_t, res) = shell_analyze_and_filter(&verts, &tris, &config);

        assert_eq!(res.reason, "ok");
        assert!(!res.has_internal_walls);

        let (holes, nm, parts) = check_mesh_closed_manifold_single_part(&out_t);
        assert_eq!(holes, 0, "Must have 0 holes, found {}", holes);
        assert_eq!(nm, 0, "Must have 0 non-manifold edges, found {}", nm);
        assert_eq!(parts, 1, "Must be 1 single part, found {}", parts);
    }

    #[test]
    fn test_two_crossing_boxes_with_coplanar_and_transversal() {
        let b1 = unit_box([2.5, 0.0, 0.0], [1.0, 0.3, 6.0]);
        let b2 = unit_box([0.0, 0.0, 2.5], [6.0, 0.3, 1.0]);
        let (verts, tris) = combine_meshes(b1, b2);

        let mut config = ShellConfig::default();
        config.split_intersections = true;
        let (_out_v, out_t, res) = shell_analyze_and_filter(&verts, &tris, &config);

        assert_eq!(res.reason, "ok");
        assert!(!res.has_internal_walls);

        let (holes, nm, parts) = check_mesh_closed_manifold_single_part(&out_t);
        assert_eq!(holes, 0, "Must have 0 holes, found {}", holes);
        assert_eq!(nm, 0, "Must have 0 non-manifold edges, found {}", nm);
        assert_eq!(parts, 1, "Must be 1 single part, found {}", parts);
    }

    #[test]
    fn test_100045_unified_arrangement_shell_filter() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/real-world-samples/thingi10k_100045.stl"
        );
        let (verts, tris) = read_stl_welded(path);
        let mut config = ShellConfig::default();
        config.split_intersections = true;
        let (_out_v, out_t, res) = shell_analyze_and_filter(&verts, &tris, &config);

        assert_eq!(res.reason, "ok");
        assert!(!res.has_internal_walls);

        let (holes, nm, parts) = check_mesh_closed_manifold_single_part(&out_t);
        assert_eq!(holes, 0, "Must have 0 holes, found {}", holes);
        assert_eq!(nm, 0, "Must have 0 non-manifold edges, found {}", nm);
        assert_eq!(parts, 1, "Must be 1 single part, found {}", parts);
    }
}
