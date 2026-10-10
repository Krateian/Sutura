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

use crate::raycast::{Stabber, DEFAULT_DIRS};
use crate::winding::MeshBvh;
use rayon::prelude::*;
use std::collections::HashMap;

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
        if group.len() == 2 {
            let i1 = group[0];
            let i2 = group[1];
            let parity1 = cyclic_parity(tris[i1]);
            let parity2 = cyclic_parity(tris[i2]);
            if parity1 != parity2 {
                let t = tris[i1];
                let v0 = verts[t[0] as usize];
                let v1 = verts[t[1] as usize];
                let v2 = verts[t[2] as usize];
                let c = scale3(add3(add3(v0, v1), v2), 1.0 / 3.0);
                let normal = cross3(sub3(v1, v0), sub3(v2, v0));
                let area = norm3(normal) * 0.5;
                if area > 1e-14 {
                    let unorm = scale3(normal, 0.5 / area);
                    let eps = (h_med * 0.05).min(0.01);
                    let p_front = add3(c, scale3(unorm, eps));
                    let p_back = sub3(c, scale3(unorm, eps));
                    let w_front = bvh.winding_at(p_front);
                    let w_back = bvh.winding_at(p_back);
                    let w_min = w_front.min(w_back);
                    if w_min >= 1.0 - config.delta {
                        res.classes[i1] = FaceClass::CoincidentOpposite;
                        res.classes[i2] = FaceClass::CoincidentOpposite;
                        res.coincident_opp_count += 2;
                        continue;
                    }
                }
            } else {
                res.classes[i1] = FaceClass::Outer;
                res.classes[i2] = FaceClass::CoincidentSame;
                res.coincident_same_count += 1;
                continue;
            }
        } else {
            res.unresolved_coincident_count += group.len();
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

/// Complete shell analysis & filter pre-pass for Tamponade (Idea A).
pub fn shell_analyze_and_filter(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    config: &ShellConfig,
) -> (Vec<[f64; 3]>, Vec<[u32; 3]>, ShellAnalysisResult) {
    if tris.is_empty() || verts.is_empty() {
        return (verts.to_vec(), tris.to_vec(), ShellAnalysisResult::default());
    }

    let tris_usize: Vec<[usize; 3]> = tris
        .iter()
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();

    // Flower / C1: If proper self-intersections exist, split along exact intersection curves first
    if config.split_intersections {
        let (_si_mask, si_count, _) = crate::self_intersections_core(verts, &tris_usize);
        if si_count > 0 {
            if let Ok((pool, out_tris, rep)) = crate::arrangement::arrangement_lite_core(verts, &tris_usize) {
            if rep.si_pairs_detected > 0 {
                let split_verts: Vec<[f64; 3]> = pool.iter().map(crate::arrangement::rat3_to_f64).collect();
                let split_tris: Vec<[u32; 3]> = out_tris.iter().map(|t| [t[0] as u32, t[1] as u32, t[2] as u32]).collect();
                let mut split_cfg = config.clone();
                split_cfg.split_intersections = false;
                let split_res = analyze_shells(&split_verts, &split_tris, &split_cfg);
                if !split_res.has_internal_walls && split_res.reason == "ok" {
                    let mut final_tris = Vec::with_capacity(split_res.surviving_indices.len());
                    for &idx in &split_res.surviving_indices {
                        final_tris.push(split_tris[idx]);
                    }
                    return (split_verts, final_tris, split_res);
                }
            }
        }
    }
}

    let analysis = analyze_shells(verts, tris, config);

    // Fast path: if no faces need deletion, return original mesh verbatim
    if analysis.buried_count == 0
        && analysis.coincident_opp_count == 0
        && analysis.coincident_same_count == 0
    {
        return (verts.to_vec(), tris.to_vec(), analysis);
    }

    let mut out_tris = Vec::with_capacity(analysis.surviving_indices.len());
    for &idx in &analysis.surviving_indices {
        out_tris.push(tris[idx]);
    }

    (verts.to_vec(), out_tris, analysis)
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
    fn test_half_cut_onion_fallback() {
        // Addendum item 3: nested solids cut by one plane (z = 0), so cut faces of all layers
        // are coplanar and overlapping. Coplanar overlapping faces -> unresolved -> fallback.
        let outer = unit_box([-2.0, -2.0, 0.0], [4.0, 4.0, 2.0]);
        let inner = unit_box([-1.0, -1.0, 0.0], [2.0, 2.0, 1.0]);
        let (verts, tris) = combine_meshes(outer, inner);

        let config = ShellConfig::default();
        let (_out_v, out_t, res) = shell_analyze_and_filter(&verts, &tris, &config);

        assert!(res.has_internal_walls);
        assert_eq!(res.reason, "unresolved_coplanar_overlapping");
        // Must fallback, never emit half-union:
        assert_eq!(out_t.len(), tris.len());
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
}
