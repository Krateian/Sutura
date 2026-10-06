// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tamponade (Method #17, alias Lava): volumetric inside-out hole sealing.
//!
//! L2a scope: holes only. The intact shell is never re-emitted — every input
//! triangle is copied back verbatim, and one plug is stitched to each open
//! boundary loop. A local occupancy grid is built around the loop; the
//! generalized winding number classifies interior (source) and exterior
//! (sink) cells, wall-crossing cell edges are removed, and the minimum
//! s-t cut is used only to DECIDE whether the loop's throat region really
//! separates source from sink (i.e. whether it is a hole worth sealing). The
//! plug itself is a rim-only triangulation (Liepa-style minimum-area dynamic
//! programme, ear-clip fallback) that adds no vertices at all, so every plug
//! vertex lies exactly on the input surface. A per-loop safety gate rejects a
//! plug that would introduce a boundary edge, a zero-area triangle or a
//! winding inconsistent with the rim.

use crate::flap::dp::min_area_triangulation;
use crate::mincut::BkGraph;
use crate::winding::MeshBvh;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// Boundary loops up to this size are triangulated by the exact O(n^3)
/// minimum-area DP; larger loops use the O(n^2) ear-clipping fallback.
const PLUG_DP_CAP: usize = 300;
/// Plug triangles with an area at or below this (in the mesh's units, squared)
/// are treated as degenerate and cause the loop's plug to be rejected.
const PLUG_MIN_AREA: f64 = 1e-12;

#[derive(Clone, Debug)]
pub struct TamponadeConfig {
    pub h_min: Option<f64>,
    pub melt_si: bool,
    pub max_nodes: Option<usize>,
    pub time_budget_s: Option<f64>,
}

impl Default for TamponadeConfig {
    fn default() -> Self {
        Self {
            h_min: None,
            melt_si: true,
            max_nodes: None,
            time_budget_s: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TamponadeInfo {
    pub plugs: usize,
    pub plugs_rejected: usize,
    pub plugs_inconclusive: usize,
    pub plug_faces: usize,
    pub melt_layers_tried: usize,
    pub melt_layers_accepted: usize,
    pub melt_layers_rolled_back: usize,
    pub micro_welds_ok: usize,
    pub micro_welds_failed: usize,
    pub healthy_faces_kept: usize,
    pub seconds: f64,
    pub reason: String,
}

/// Compute a robust median edge length of the input mesh.
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
        let d01 = ((v1[0] - v0[0]).powi(2) + (v1[1] - v0[1]).powi(2) + (v1[2] - v0[2]).powi(2)).sqrt();
        let d12 = ((v2[0] - v1[0]).powi(2) + (v2[1] - v1[1]).powi(2) + (v2[2] - v1[2]).powi(2)).sqrt();
        let d20 = ((v0[0] - v2[0]).powi(2) + (v0[1] - v2[1]).powi(2) + (v0[2] - v2[2]).powi(2)).sqrt();
        lens.push(d01);
        lens.push(d12);
        lens.push(d20);
    }

    if lens.is_empty() {
        return 1.0;
    }
    lens.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    lens[lens.len() / 2].max(1e-6)
}

/// 3D axis-aligned bounding box.
#[derive(Clone, Copy, Debug)]
pub struct BBox {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl BBox {
    pub fn empty() -> Self {
        Self {
            min: [f64::INFINITY; 3],
            max: [-f64::INFINITY; 3],
        }
    }

    pub fn expand_point(&mut self, p: [f64; 3]) {
        for i in 0..3 {
            if p[i] < self.min[i] {
                self.min[i] = p[i];
            }
            if p[i] > self.max[i] {
                self.max[i] = p[i];
            }
        }
    }

    pub fn pad(&mut self, pad: f64) {
        for i in 0..3 {
            self.min[i] -= pad;
            self.max[i] += pad;
        }
    }

    pub fn diagonal(&self) -> f64 {
        let dx = self.max[0] - self.min[0];
        let dy = self.max[1] - self.min[1];
        let dz = self.max[2] - self.min[2];
        (dx * dx + dy * dy + dz * dz).sqrt()
    }
}

/// Directed boundary edges, in the orientation of their single incident
/// triangle. An edge shared by two triangles (in either orientation) is not a
/// boundary edge.
pub fn directed_boundary_edges(tris: &[[u32; 3]]) -> Vec<(u32, u32)> {
    let mut count: HashMap<(u32, u32), i32> = HashMap::with_capacity(tris.len() * 2);
    for t in tris {
        for &(a, b) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a < b { (a, b) } else { (b, a) };
            *count.entry(key).or_insert(0) += 1;
        }
    }
    let mut out = Vec::new();
    for t in tris {
        for &(a, b) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a < b { (a, b) } else { (b, a) };
            if count.get(&key) == Some(&1) {
                out.push((a, b));
            }
        }
    }
    out
}

/// Chain boundary edges into closed vertex loops, ignoring edge orientation so
/// that inconsistently wound inputs (mixed face normals) still yield clean
/// rims. A non-manifold vertex with several incident boundary edges is split
/// greedily; every used edge appears in at most one loop.
pub fn build_boundary_loops(directed: &[(u32, u32)]) -> Vec<Vec<u32>> {
    let key = |a: u32, b: u32| if a < b { (a, b) } else { (b, a) };

    let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(a, b) in directed {
        if a == b {
            continue;
        }
        adj.entry(a).or_default().push(b);
        adj.entry(b).or_default().push(a);
    }

    let mut used: HashSet<(u32, u32)> = HashSet::new();
    let mut loops = Vec::new();

    for &(start_a, start_b) in directed {
        if start_a == start_b || used.contains(&key(start_a, start_b)) {
            continue;
        }
        used.insert(key(start_a, start_b));

        let mut loop_v = vec![start_a, start_b];
        let mut prev = start_a;
        let mut cur = start_b;
        let mut guard = 0usize;
        let guard_max = directed.len() + 2;

        while cur != start_a {
            guard += 1;
            if guard > guard_max {
                break;
            }
            let nbrs = match adj.get(&cur) {
                Some(v) => v,
                None => break,
            };
            let mut nxt = None;
            for &cand in nbrs {
                if cand == prev {
                    continue;
                }
                if !used.contains(&key(cur, cand)) {
                    nxt = Some(cand);
                    break;
                }
            }
            let nxt = match nxt {
                Some(c) => c,
                None => break,
            };
            used.insert(key(cur, nxt));
            prev = cur;
            cur = nxt;
            if cur != start_a {
                loop_v.push(cur);
            }
        }

        if loop_v.len() >= 3 {
            loops.push(loop_v);
        }
    }

    loops
}

fn crosses_wall(bvh: &MeshBvh, p1: [f64; 3], p2: [f64; 3]) -> bool {
    let dir = [p2[0] - p1[0], p2[1] - p1[1], p2[2] - p1[2]];
    let len = (dir[0].powi(2) + dir[1].powi(2) + dir[2].powi(2)).sqrt();
    if len < 1e-12 {
        return false;
    }
    let udir = [dir[0] / len, dir[1] / len, dir[2] / len];
    let hit = bvh.intersect_ray(p1, udir, len, 1e-5);
    hit.crossings > 0
}

/// Decide whether one open boundary loop is a genuine hole to seal: build a
/// local occupancy grid around the loop, classify interior (source) and
/// exterior (sink) cells by the generalized winding number, treat cell edges
/// crossing an input triangle as impassable walls, and report whether a
/// minimum s-t cut of finite, non-trivial value separates the two. The cut is
/// used ONLY as this decision; no vertex is ever placed at the cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CutVerdict {
    /// A finite, non-trivial cut separates the loop's interior from outside.
    Separates,
    /// No path connects the interior tag to the border (flow ~ 0).
    NoSeparation,
    /// The decision cannot be trusted (flow overflowed, the grid budget was
    /// exceeded, or the clamp left too few cells across the throat); the
    /// caller seals with the safe rim-only plug instead of rejecting.
    Inconclusive,
}

fn throat_separates(
    verts: &[[f64; 3]],
    loop_v: &[u32],
    h_global: f64,
    _model_diag: f64,
    bvh: &MeshBvh,
    config: &TamponadeConfig,
) -> CutVerdict {
    const TERM: f64 = 1.0e12;

    let mut bbox = BBox::empty();
    for &v in loop_v {
        bbox.expand_point(verts[v as usize]);
    }
    if !bbox.min[0].is_finite() {
        return CutVerdict::NoSeparation;
    }

    // Size the local window from the LOOP, not the model: resolve the loop
    // diameter with ~24 cells and pad by 1.5 loop diameters on every side.
    let loop_diag = bbox.diagonal().max(h_global);
    let h_desired = (loop_diag / 24.0).max(1e-9);
    let pad = (3.0 * h_desired).max(1.5 * loop_diag);
    bbox.pad(pad);

    // Coarsen the voxel until no axis exceeds 64 cells (window <= 64^3).
    let span = [
        (bbox.max[0] - bbox.min[0]).max(h_desired),
        (bbox.max[1] - bbox.min[1]).max(h_desired),
        (bbox.max[2] - bbox.min[2]).max(h_desired),
    ];
    let mut h = h_desired;
    for &s in &span {
        h = h.max(s / 64.0);
    }
    // A clamp that leaves fewer than ~8 cells across the loop cannot resolve
    // the throat, so the cut is treated as unknown.
    let clamped = h > 1.0001 * h_desired && h > loop_diag / 8.0;

    let dx = span[0];
    let dy = span[1];
    let dz = span[2];

    let nx = ((dx / h).ceil() as usize).clamp(2, 64);
    let ny = ((dy / h).ceil() as usize).clamp(2, 64);
    let nz = ((dz / h).ceil() as usize).clamp(2, 64);

    let total = nx * ny * nz;
    let cap = config.max_nodes.unwrap_or(400_000);
    if total > cap {
        return CutVerdict::Inconclusive;
    }

    let hx = dx / nx as f64;
    let hy = dy / ny as f64;
    let hz = dz / nz as f64;
    let h_eff = (hx * hy * hz).cbrt();

    let cell_idx = |ix: usize, iy: usize, iz: usize| -> usize { ix + iy * nx + iz * (nx * ny) };
    let cell_center = |ix: usize, iy: usize, iz: usize| -> [f64; 3] {
        [
            bbox.min[0] + (ix as f64 + 0.5) * hx,
            bbox.min[1] + (iy as f64 + 0.5) * hy,
            bbox.min[2] + (iz as f64 + 0.5) * hz,
        ]
    };

    let mut g = BkGraph::new(total);
    let mut n_src = 0usize;
    let mut n_sink = 0usize;

    for iz in 0..nz {
        for iy in 0..ny {
            for ix in 0..nx {
                let idx = cell_idx(ix, iy, iz);
                let c = cell_center(ix, iy, iz);
                let is_border = ix == 0 || ix == nx - 1 || iy == 0 || iy == ny - 1 || iz == 0 || iz == nz - 1;
                let w = bvh.winding_at(c);

                if w > 0.5 {
                    g.add_tweights(idx, TERM, 0.0);
                    n_src += 1;
                } else if is_border || w < 0.1 {
                    g.add_tweights(idx, 0.0, TERM);
                    n_sink += 1;
                }
            }
        }
    }

    // If the window cannot even see both an interior (source) and an exterior
    // (sink) cell, the cut has nothing to decide; the loop is treated as
    // unknown rather than "no hole".
    if n_src == 0 || n_sink == 0 {
        return CutVerdict::Inconclusive;
    }

    // Open-space connections only: an edge that crosses an input triangle is a
    // wall and is not traversable, so the cut can only pass through the hole.
    let face_cap = h_eff * h_eff;
    for iz in 0..nz {
        for iy in 0..ny {
            for ix in 0..nx {
                let idx = cell_idx(ix, iy, iz);
                let c = cell_center(ix, iy, iz);
                if ix + 1 < nx {
                    let c2 = cell_center(ix + 1, iy, iz);
                    if !crosses_wall(bvh, c, c2) {
                        g.add_edge(idx, cell_idx(ix + 1, iy, iz), face_cap, face_cap);
                    }
                }
                if iy + 1 < ny {
                    let c2 = cell_center(ix, iy + 1, iz);
                    if !crosses_wall(bvh, c, c2) {
                        g.add_edge(idx, cell_idx(ix, iy + 1, iz), face_cap, face_cap);
                    }
                }
                if iz + 1 < nz {
                    let c2 = cell_center(ix, iy, iz + 1);
                    if !crosses_wall(bvh, c, c2) {
                        g.add_edge(idx, cell_idx(ix, iy, iz + 1), face_cap, face_cap);
                    }
                }
            }
        }
    }

    if clamped {
        return CutVerdict::Inconclusive;
    }
    let flow = g.maxflow();
    if !flow.is_finite() || flow >= 1e11 {
        CutVerdict::Inconclusive
    } else if flow > 1e-12 {
        CutVerdict::Separates
    } else {
        CutVerdict::NoSeparation
    }
}

/// Cross product z/area helper: the area of triangle `(a, b, c)`.
#[inline]
fn tri_area(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let cx = ab[1] * ac[2] - ab[2] * ac[1];
    let cy = ab[2] * ac[0] - ab[0] * ac[2];
    let cz = ab[0] * ac[1] - ab[1] * ac[0];
    0.5 * (cx * cx + cy * cy + cz * cz).sqrt()
}

#[inline]
fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
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

/// Newell's method: a robust best-fit normal of a (possibly non-planar) 3-D
/// polygon given by its vertices in loop order.
fn newell_normal(p: &[[f64; 3]]) -> [f64; 3] {
    let m = p.len();
    let mut n = [0.0f64; 3];
    for i in 0..m {
        let a = p[i];
        let b = p[(i + 1) % m];
        n[0] += (a[1] - b[1]) * (a[2] + b[2]);
        n[1] += (a[2] - b[2]) * (a[0] + b[0]);
        n[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    n
}

/// Ear-clipping triangulation of a simple 3-D polygon `p`, used as the
/// O(n^2) fallback above `PLUG_DP_CAP`. Returns local index triples with a
/// consistent orientation, or an empty vec when no ear can be found.
fn ear_clip(p: &[[f64; 3]]) -> Vec<[u32; 3]> {
    let m = p.len();
    if m < 3 {
        return Vec::new();
    }
    let n0 = newell_normal(p);
    let nl = norm3(n0);
    if nl < 1e-20 {
        return Vec::new();
    }
    let n = [n0[0] / nl, n0[1] / nl, n0[2] / nl];
    let refv = if n[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
    let u0 = cross3(refv, n);
    let ul = norm3(u0);
    if ul < 1e-20 {
        return Vec::new();
    }
    let u = [u0[0] / ul, u0[1] / ul, u0[2] / ul];
    let v = cross3(n, u);
    let p2: Vec<[f64; 2]> = p
        .iter()
        .map(|q| {
            let d = [q[0] - p[0][0], q[1] - p[0][1], q[2] - p[0][2]];
            [dot3(d, u), dot3(d, v)]
        })
        .collect();

    // Polygon orientation in the projected plane.
    let mut signed = 0.0f64;
    for i in 0..m {
        let a = p2[i];
        let b = p2[(i + 1) % m];
        signed += a[0] * b[1] - b[0] * a[1];
    }
    let mut order: Vec<usize> = if signed >= 0.0 {
        (0..m).collect()
    } else {
        (0..m).rev().collect()
    };

    let mut out: Vec<[u32; 3]> = Vec::with_capacity(m - 2);
    let mut guard = 0usize;
    while order.len() > 3 {
        guard += 1;
        if guard > m * m + 4 {
            return Vec::new();
        }
        let l = order.len();
        let mut clipped = false;
        for i in 0..l {
            let ia = order[(i + l - 1) % l];
            let ib = order[i];
            let ic = order[(i + 1) % l];
            let a = p2[ia];
            let b = p2[ib];
            let c = p2[ic];
            let cr = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
            if cr <= 1e-20 {
                continue;
            }
            let mut ok = true;
            for &j in &order {
                if j == ia || j == ib || j == ic {
                    continue;
                }
                let q = p2[j];
                let d1 = (b[0] - a[0]) * (q[1] - a[1]) - (b[1] - a[1]) * (q[0] - a[0]);
                let d2 = (c[0] - b[0]) * (q[1] - b[1]) - (c[1] - b[1]) * (q[0] - b[0]);
                let d3 = (a[0] - c[0]) * (q[1] - c[1]) - (a[1] - c[1]) * (q[0] - c[0]);
                if d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0 {
                    ok = false;
                    break;
                }
            }
            if ok {
                out.push([ia as u32, ib as u32, ic as u32]);
                order.remove(i);
                clipped = true;
                break;
            }
        }
        if !clipped {
            return Vec::new();
        }
    }
    out.push([order[0] as u32, order[1] as u32, order[2] as u32]);
    out
}

/// Rewind a local triangulation onto the global `rim` indices.
fn orient_tris(local: &[[u32; 3]], rim: &[u32], flip: bool) -> Vec<[u32; 3]> {
    local
        .iter()
        .map(|t| {
            let a = rim[t[0] as usize];
            let b = rim[t[1] as usize];
            let c = rim[t[2] as usize];
            if flip {
                [a, c, b]
            } else {
                [a, b, c]
            }
        })
        .collect()
}

#[inline]
fn ekey2(a: u32, b: u32) -> (u32, u32) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Structural safety gate for one loop's rim-only plug: every triangle must be
/// non-degenerate, and the plug's boundary edges (those used an odd number of
/// times) must be exactly the rim edge set. This guarantees no new boundary
/// edge is introduced and each rim edge gains exactly one cap face. The check
/// is independent of the plug's overall winding.
fn plug_structure_ok(tris: &[[u32; 3]], rim: &[u32], verts: &[[f64; 3]]) -> bool {
    let mut count: HashMap<(u32, u32), i32> = HashMap::with_capacity(tris.len() * 3);
    for t in tris {
        if tri_area(
            verts[t[0] as usize],
            verts[t[1] as usize],
            verts[t[2] as usize],
        ) <= PLUG_MIN_AREA
        {
            return false;
        }
        for &(a, b) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            if a == b {
                return false;
            }
            *count.entry(ekey2(a, b)).or_insert(0) += 1;
        }
    }

    let mut boundary: HashSet<(u32, u32)> = HashSet::new();
    for (&e, &c) in &count {
        if c % 2 == 1 {
            boundary.insert(e);
        }
    }
    let m = rim.len();
    let mut rim_edges: HashSet<(u32, u32)> = HashSet::with_capacity(m);
    for i in 0..m {
        rim_edges.insert(ekey2(rim[i], rim[(i + 1) % m]));
    }
    boundary == rim_edges
}

/// Count how many rim edges the plug caps in the direction opposite to their
/// incident input triangle (a consistent cap cancels every edge; a
/// mixed-winding rim can only cancel a majority).
fn cancellation_count(tris: &[[u32; 3]], rim: &[u32], directed_set: &HashSet<(u32, u32)>) -> usize {
    let mut dir: HashSet<(u32, u32)> = HashSet::with_capacity(tris.len() * 3);
    for t in tris {
        dir.insert((t[0], t[1]));
        dir.insert((t[1], t[2]));
        dir.insert((t[2], t[0]));
    }
    let m = rim.len();
    let mut cancels = 0usize;
    for i in 0..m {
        let a = rim[i];
        let b = rim[(i + 1) % m];
        if a == b {
            continue;
        }
        let boundary = if directed_set.contains(&(a, b)) {
            (a, b)
        } else if directed_set.contains(&(b, a)) {
            (b, a)
        } else {
            continue;
        };
        if dir.contains(&(boundary.1, boundary.0)) && !dir.contains(&boundary) {
            cancels += 1;
        }
    }
    cancels
}

/// Full safety gate: structure plus a strict-majority winding agreement with
/// the rim. A loop failing either is left open. Exercised by the unit tests;
/// the production path uses the two halves directly to pick the orientation.
#[cfg(test)]
fn plug_gate(
    tris: &[[u32; 3]],
    rim: &[u32],
    directed_set: &HashSet<(u32, u32)>,
    verts: &[[f64; 3]],
) -> bool {
    plug_structure_ok(tris, rim, verts)
        && cancellation_count(tris, rim, directed_set) * 2 > rim.len()
}

/// Triangulate one boundary loop with rim vertices only. Returns `None` when
/// the loop cannot be sealed cleanly (triangulation failure, mixed winding or
/// a degenerate triangle), in which case the caller leaves the hole open.
fn triangulate_rim_loop(
    rim: &[u32],
    verts: &[[f64; 3]],
    directed_set: &HashSet<(u32, u32)>,
) -> Option<Vec<[u32; 3]>> {
    let m = rim.len();
    if m < 3 {
        return None;
    }
    let pts: Vec<[f64; 3]> = rim.iter().map(|&v| verts[v as usize]).collect();
    let local = if m <= PLUG_DP_CAP {
        min_area_triangulation(&pts, PLUG_DP_CAP, &HashSet::new())
    } else {
        ear_clip(&pts)
    };
    if local.len() != m - 2 {
        if std::env::var("TAMP_DBG").is_ok() {
            eprintln!("TAMP_TRI m={} local={} dp_fail", m, local.len());
        }
        return None;
    }

    // The rim may be consistently or inconsistently wound (scans and legacy
    // fixtures). Both orientations share the same structure/areas; keep the
    // one that cancels the most input boundary edges, and require a strict
    // majority so a genuinely pinched loop is left open.
    let base = orient_tris(&local, rim, false);
    let struct_ok = plug_structure_ok(&base, rim, verts);
    let flipped = orient_tris(&local, rim, true);
    let cb = cancellation_count(&base, rim, directed_set);
    let cf = cancellation_count(&flipped, rim, directed_set);
    if std::env::var("TAMP_DBG").is_ok() {
        eprintln!(
            "TAMP_TRI m={} struct={} cb={} cf={}",
            m, struct_ok, cb, cf
        );
    }
    if !struct_ok {
        return None;
    }
    if cb.max(cf) * 2 <= m {
        return None;
    }
    Some(if cf > cb { flipped } else { base })
}

/// Main entry point for the L2a Tamponade repair.
pub fn tamponade_repair(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    config: &TamponadeConfig,
) -> (Vec<[f64; 3]>, Vec<[u32; 3]>, TamponadeInfo) {
    let t0 = Instant::now();
    let mut info = TamponadeInfo::default();
    info.healthy_faces_kept = tris.len();

    if tris.is_empty() || verts.is_empty() {
        info.reason = "empty mesh".to_string();
        info.seconds = t0.elapsed().as_secs_f64();
        return (verts.to_vec(), tris.to_vec(), info);
    }

    let directed = directed_boundary_edges(tris);
    if directed.is_empty() {
        info.reason = "no_holes".to_string();
        info.seconds = t0.elapsed().as_secs_f64();
        return (verts.to_vec(), tris.to_vec(), info);
    }

    let directed_set: HashSet<(u32, u32)> = directed.iter().cloned().collect();
    let loops = build_boundary_loops(&directed);
    if loops.is_empty() {
        info.reason = "no_holes".to_string();
        info.seconds = t0.elapsed().as_secs_f64();
        return (verts.to_vec(), tris.to_vec(), info);
    }

    let h = match config.h_min {
        Some(val) if val > 1e-6 => val,
        _ => median_edge_length(verts, tris),
    };

    let tris_usize: Vec<[usize; 3]> = tris
        .iter()
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    let bvh = MeshBvh::from_arrays(verts, &tris_usize);

    let mut model_box = BBox::empty();
    for v in verts {
        model_box.expand_point(*v);
    }
    let model_diag = model_box.diagonal().max(h);

    let out_verts = verts.to_vec();
    let mut out_tris = tris.to_vec();
    let mut plug_count = 0usize;
    let mut plug_rejected_count = 0usize;
    let mut plug_inconclusive_count = 0usize;
    let mut plug_face_count = 0usize;

    for loop_v in &loops {
        if let Some(budget) = config.time_budget_s {
            if t0.elapsed().as_secs_f64() > budget {
                info.reason = "timeout exceeded".to_string();
                break;
            }
        }

        let mut rim: Vec<u32> = loop_v.clone();
        rim.dedup();
        if rim.len() >= 2 && rim.first() == rim.last() {
            rim.pop();
        }
        if rim.len() < 3 {
            continue;
        }

        // The min-cut decides only WHETHER this loop is a real hole (its
        // throat separates interior from exterior); it never places a vertex.
        // An inconclusive cut is treated as a hole (seal with the safe rim
        // plug) rather than rejected, matching R1 usefulness.
        match throat_separates(verts, &rim, h, model_diag, &bvh, config) {
            CutVerdict::NoSeparation => {
                plug_rejected_count += 1;
                continue;
            }
            CutVerdict::Inconclusive => {
                plug_inconclusive_count += 1;
            }
            CutVerdict::Separates => {}
        }

        match triangulate_rim_loop(&rim, verts, &directed_set) {
            Some(tris) => {
                plug_face_count += tris.len();
                out_tris.extend(tris);
                plug_count += 1;
            }
            None => {
                plug_rejected_count += 1;
            }
        }
    }

    info.plugs = plug_count;
    info.plugs_rejected = plug_rejected_count;
    info.plugs_inconclusive = plug_inconclusive_count;
    info.plug_faces = plug_face_count;
    if info.reason.is_empty() {
        info.reason = "ok".to_string();
    }
    info.seconds = t0.elapsed().as_secs_f64();
    (out_verts, out_tris, info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_cube_open_top() -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        let tris = vec![
            [0, 2, 1],
            [0, 3, 2], // bottom z=0
            [0, 1, 5],
            [0, 5, 4], // -y
            [2, 3, 7],
            [2, 7, 6], // +y
            [1, 2, 6],
            [1, 6, 5], // +x
            [3, 0, 4],
            [3, 4, 7], // -x
        ];
        (verts, tris)
    }

    fn unit_solid_cube() -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let (verts, mut tris) = unit_cube_open_top();
        // Add the missing top face (z=1) as two triangles, outward +z.
        tris.push([4, 6, 5]);
        tris.push([4, 7, 6]);
        (verts, tris)
    }

    fn unit_sphere() -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let verts = vec![
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
        ];
        let tris = vec![
            [0, 2, 4],
            [2, 1, 4],
            [1, 3, 4],
            [3, 0, 4],
            [2, 0, 5],
            [1, 2, 5],
            [3, 1, 5],
            [0, 3, 5],
        ];
        (verts, tris)
    }

    fn boundary_edge_count(tris: &[[u32; 3]]) -> usize {
        directed_boundary_edges(tris).len()
    }

    /// Vertices referenced by the added (plug) triangles that follow the
    /// `kept` original triangles.
    fn plug_vertex_set(out_tris: &[[u32; 3]], kept: usize) -> HashSet<u32> {
        let mut vs = HashSet::new();
        for t in &out_tris[kept..] {
            vs.extend(t.iter().copied());
        }
        vs
    }

    #[test]
    fn test_closed_sphere_untouched() {
        let (verts, tris) = unit_sphere();
        let config = TamponadeConfig { h_min: Some(0.2), ..Default::default() };
        let (out_verts, out_tris, info) = tamponade_repair(&verts, &tris, &config);
        assert_eq!(info.reason, "no_holes");
        assert_eq!(info.plugs, 0);
        assert_eq!(info.plug_faces, 0);
        assert_eq!(out_verts.len(), verts.len());
        assert_eq!(out_tris, tris);
    }

    #[test]
    fn test_open_top_cube_sealing() {
        let (verts, tris) = unit_cube_open_top();
        let config = TamponadeConfig { h_min: Some(0.5), ..Default::default() };
        let (out_verts, out_tris, info) = tamponade_repair(&verts, &tris, &config);
        assert_eq!(info.reason, "ok");
        assert_eq!(info.plugs, 1);
        assert_eq!(info.healthy_faces_kept, tris.len());

        // 10 original triangles preserved verbatim at the front.
        for i in 0..tris.len() {
            assert_eq!(out_tris[i], tris[i]);
        }

        // Reload-watertight: no remaining boundary edges.
        assert_eq!(boundary_edge_count(&out_tris), 0);

        // Rim-only plug: no vertex is added, and the plug references exactly
        // the four top-rim vertices (indices 4..8 at z = 1).
        assert_eq!(out_verts.len(), verts.len());
        let plug_vs = plug_vertex_set(&out_tris, tris.len());
        let rim: HashSet<u32> = [4, 5, 6, 7].into_iter().collect();
        assert_eq!(plug_vs, rim);
        for &v in &plug_vs {
            assert!((out_verts[v as usize][2] - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn test_open_top_cube_flush_throat() {
        // Finer grid: the throat decision must still accept the opening.
        let (verts, tris) = unit_cube_open_top();
        let config = TamponadeConfig { h_min: Some(0.25), ..Default::default() };
        let (out_verts, out_tris, info) = tamponade_repair(&verts, &tris, &config);
        assert_eq!(info.plugs, 1);
        assert_eq!(info.plugs_rejected, 0);
        assert_eq!(boundary_edge_count(&out_tris), 0);
        assert_eq!(out_verts.len(), verts.len());
    }

    #[test]
    fn test_solid_cube_untouched() {
        let (verts, tris) = unit_solid_cube();
        let config = TamponadeConfig { h_min: Some(0.5), ..Default::default() };
        let (_out_verts, out_tris, info) = tamponade_repair(&verts, &tris, &config);
        assert_eq!(info.reason, "no_holes");
        assert_eq!(out_tris, tris);
    }

    #[test]
    fn test_partial_top_hole() {
        // A solid cube with only one of the two top triangles removed: a
        // triangular opening that the plug must cap flush at the top.
        let (verts, mut tris) = unit_solid_cube();
        let removed = tris.pop().unwrap(); // drop one top triangle
        let _ = removed;
        let config = TamponadeConfig { h_min: Some(0.5), ..Default::default() };
        let (out_verts, out_tris, info) = tamponade_repair(&verts, &tris, &config);
        assert_eq!(info.reason, "ok");
        assert_eq!(info.plugs, 1);
        assert_eq!(boundary_edge_count(&out_tris), 0);

        // Every original triangle survives verbatim at the front.
        for i in 0..tris.len() {
            assert_eq!(out_tris[i], tris[i]);
        }

        // Rim-only plug: no vertex is added and every plug vertex lies on the
        // top plane (the opening lies at z = 1).
        assert_eq!(out_verts.len(), verts.len());
        for &v in &plug_vertex_set(&out_tris, tris.len()) {
            assert!((out_verts[v as usize][2] - 1.0).abs() < 1e-9);
        }
    }

    /// Extrude a \(1 \times 1\) square cross-section along x over `xs`
    /// stations, optionally omitting the top quads of the given x-segment
    /// indices. Returns a closed solid mesh except where the top is omitted.
    fn extruded_box(xs: &[f64], omit_top_segments: &[usize]) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let mut verts = Vec::new();
        // Cross-section corners in (y,z), counter-clockwise seen from +x.
        let ring = [
            [0.0, 0.0], // 0 (y=0,z=0)
            [1.0, 0.0], // 1 (y=1,z=0)
            [1.0, 1.0], // 2 (y=1,z=1)
            [0.0, 1.0], // 3 (y=0,z=1)
        ];
        for &x in xs {
            for c in &ring {
                verts.push([x, c[0], c[1]]);
            }
        }
        let n = xs.len();
        let vid = |s: usize, c: usize| -> u32 { (s * 4 + c) as u32 };
        let mut tris = Vec::new();
        for s in 0..n - 1 {
            for c in 0..4 {
                let c2 = (c + 1) % 4;
                // side faces: 0 = bottom, 2 = top (c=2 is top quad between
                // corners 2 and 3)
                if c == 2 && omit_top_segments.contains(&s) {
                    continue;
                }
                let a = vid(s, c);
                let b = vid(s, c2);
                let cc = vid(s + 1, c2);
                let d = vid(s + 1, c);
                tris.push([a, b, cc]);
                tris.push([a, cc, d]);
            }
        }
        // End caps.
        tris.push([vid(0, 0), vid(0, 1), vid(0, 2)]);
        tris.push([vid(0, 0), vid(0, 2), vid(0, 3)]);
        tris.push([vid(n - 1, 0), vid(n - 1, 2), vid(n - 1, 1)]);
        tris.push([vid(n - 1, 0), vid(n - 1, 3), vid(n - 1, 2)]);
        (verts, tris)
    }

    #[test]
    fn test_off_center_opening_sealed_on_rim() {
        // A long solid bar (x in [0,4]) whose top is missing over x in [2,4].
        // The plug must cover only that opening (no vertex at the bar centroid).
        let xs = [0.0, 1.0, 2.0, 3.0, 4.0];
        let (verts, tris) = extruded_box(&xs, &[2, 3]);
        assert_eq!(boundary_edge_count(&tris) > 0, true);
        let config = TamponadeConfig { h_min: Some(0.25), ..Default::default() };
        let (out_verts, out_tris, info) = tamponade_repair(&verts, &tris, &config);
        assert_eq!(info.reason, "ok");
        assert_eq!(info.plugs, 1);
        assert_eq!(boundary_edge_count(&out_tris), 0);
        assert_eq!(out_verts.len(), verts.len());
        for &v in &plug_vertex_set(&out_tris, tris.len()) {
            assert!(out_verts[v as usize][0] >= 2.0 - 1e-9, "plug vertex x should follow the opening");
            assert!((out_verts[v as usize][2] - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn test_long_thin_slot_loop_sealed_without_new_vertices() {
        // A 2.0 x 0.02 rectangular rim (8 vertices) is the canonical long,
        // thin slot: the DP must triangulate it with rim vertices only.
        let verts: Vec<[f64; 3]> = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 0.02, 0.0],
            [1.0, 0.02, 0.0],
            [0.0, 0.02, 0.0],
            [0.0, 0.01, 0.0], // slight non-planarity so the fit plane is real
        ];
        let rim: Vec<u32> = (0..verts.len() as u32).collect();
        let directed: Vec<(u32, u32)> = (0..rim.len())
            .map(|i| (rim[i], rim[(i + 1) % rim.len()]))
            .collect();
        let directed_set: HashSet<(u32, u32)> = directed.iter().cloned().collect();
        let tris = triangulate_rim_loop(&rim, &verts, &directed_set).expect("slot must seal");
        assert_eq!(tris.len(), rim.len() - 2);
        for t in &tris {
            for &v in t {
                assert!((v as usize) < verts.len());
            }
        }
        assert!(plug_gate(&tris, &rim, &directed_set, &verts));
        // No directed edge of the plug is left without its reverse except the
        // rim cancellation edges.
        assert_eq!(boundary_edge_count(&tris), rim.len());
    }

    #[test]
    fn test_two_open_ends_get_two_plugs() {
        // The extruded box with the top omitted on two disjoint segments makes
        // two separate openings -> exactly two plugs.
        let xs = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let (verts, tris) = extruded_box(&xs, &[1, 3]);
        let config = TamponadeConfig { h_min: Some(0.25), ..Default::default() };
        let (out_verts, out_tris, info) = tamponade_repair(&verts, &tris, &config);
        assert_eq!(info.reason, "ok");
        assert_eq!(info.plugs, 2);
        assert_eq!(info.plugs_rejected, 0);
        assert_eq!(boundary_edge_count(&out_tris), 0);
        assert_eq!(out_verts.len(), verts.len());
    }
}
