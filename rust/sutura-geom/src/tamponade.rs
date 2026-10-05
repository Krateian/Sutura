// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tamponade (Method #17, alias Lava): volumetric inside-out hole sealing.
//!
//! L2a scope: holes only. The intact shell is never re-emitted — every input
//! triangle is copied back verbatim, and one plug is stitched to each open
//! boundary loop. A local occupancy grid is built around the loop; the
//! generalized winding number classifies interior (source) and exterior
//! (sink) cells, wall-crossing cell edges are removed, and the minimum
//! s-t cut locates the narrowest throat of the opening. The plug is a cap of
//! the rim loop through an apex placed at the throat centroid, so the result
//! is reload-watertight by construction.

use crate::mincut::BkGraph;
use crate::winding::MeshBvh;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

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

/// Localize the narrowest throat of one open boundary loop with a minimum
/// s-t cut over an occupancy grid. Returns the area-weighted centroid of the
/// cut faces, or `None` when no clean cut exists (caller falls back to a flat
/// cap of the rim).
fn find_throat_centroid(
    verts: &[[f64; 3]],
    loop_v: &[u32],
    h: f64,
    model_diag: f64,
    bvh: &MeshBvh,
    config: &TamponadeConfig,
) -> Option<[f64; 3]> {
    const TERM: f64 = 1.0e12;

    let mut bbox = BBox::empty();
    for &v in loop_v {
        bbox.expand_point(verts[v as usize]);
    }
    if !bbox.min[0].is_finite() {
        return None;
    }
    let pad = (3.0 * h).max(0.15 * model_diag);
    bbox.pad(pad);

    let dx = (bbox.max[0] - bbox.min[0]).max(h);
    let dy = (bbox.max[1] - bbox.min[1]).max(h);
    let dz = (bbox.max[2] - bbox.min[2]).max(h);

    let nx = ((dx / h).ceil() as usize).clamp(2, 64);
    let ny = ((dy / h).ceil() as usize).clamp(2, 64);
    let nz = ((dz / h).ceil() as usize).clamp(2, 64);

    let total = nx * ny * nz;
    let cap = config.max_nodes.unwrap_or(400_000);
    if total > cap {
        return None;
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

    for iz in 0..nz {
        for iy in 0..ny {
            for ix in 0..nx {
                let idx = cell_idx(ix, iy, iz);
                let c = cell_center(ix, iy, iz);
                let is_border = ix == 0 || ix == nx - 1 || iy == 0 || iy == ny - 1 || iz == 0 || iz == nz - 1;
                let w = bvh.winding_at(c);

                if w > 0.5 {
                    g.add_tweights(idx, TERM, 0.0);
                } else if is_border || w < 0.1 {
                    g.add_tweights(idx, 0.0, TERM);
                }
            }
        }
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

    let flow = g.maxflow();
    if !flow.is_finite() || flow <= 1e-12 || flow >= 1e11 {
        return None;
    }

    // Area-weighted centroid of the S/T interface faces (non-wall only).
    let mut acc = [0.0f64; 3];
    let mut wsum = 0.0f64;
    for iz in 0..nz {
        for iy in 0..ny {
            for ix in 0..nx {
                let u = cell_idx(ix, iy, iz);
                if !g.is_in_source(u) {
                    continue;
                }
                let cu = cell_center(ix, iy, iz);
                let mut add = |cv: [f64; 3]| {
                    acc[0] += 0.5 * (cu[0] + cv[0]);
                    acc[1] += 0.5 * (cu[1] + cv[1]);
                    acc[2] += 0.5 * (cu[2] + cv[2]);
                    wsum += 1.0;
                };
                if ix + 1 < nx {
                    let v = cell_idx(ix + 1, iy, iz);
                    let cv = cell_center(ix + 1, iy, iz);
                    if !g.is_in_source(v) && !crosses_wall(bvh, cu, cv) {
                        add(cv);
                    }
                }
                if ix > 0 {
                    let v = cell_idx(ix - 1, iy, iz);
                    let cv = cell_center(ix - 1, iy, iz);
                    if !g.is_in_source(v) && !crosses_wall(bvh, cu, cv) {
                        add(cv);
                    }
                }
                if iy + 1 < ny {
                    let v = cell_idx(ix, iy + 1, iz);
                    let cv = cell_center(ix, iy + 1, iz);
                    if !g.is_in_source(v) && !crosses_wall(bvh, cu, cv) {
                        add(cv);
                    }
                }
                if iy > 0 {
                    let v = cell_idx(ix, iy - 1, iz);
                    let cv = cell_center(ix, iy - 1, iz);
                    if !g.is_in_source(v) && !crosses_wall(bvh, cu, cv) {
                        add(cv);
                    }
                }
                if iz + 1 < nz {
                    let v = cell_idx(ix, iy, iz + 1);
                    let cv = cell_center(ix, iy, iz + 1);
                    if !g.is_in_source(v) && !crosses_wall(bvh, cu, cv) {
                        add(cv);
                    }
                }
                if iz > 0 {
                    let v = cell_idx(ix, iy, iz - 1);
                    let cv = cell_center(ix, iy, iz - 1);
                    if !g.is_in_source(v) && !crosses_wall(bvh, cu, cv) {
                        add(cv);
                    }
                }
            }
        }
    }

    if wsum <= 0.0 {
        return None;
    }
    Some([acc[0] / wsum, acc[1] / wsum, acc[2] / wsum])
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

    let mut out_verts = verts.to_vec();
    let mut out_tris = tris.to_vec();
    let mut plug_count = 0usize;
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

        let mut rim_centroid = [0.0f64; 3];
        for &v in &rim {
            let p = out_verts[v as usize];
            rim_centroid[0] += p[0];
            rim_centroid[1] += p[1];
            rim_centroid[2] += p[2];
        }
        let n = rim.len() as f64;
        rim_centroid[0] /= n;
        rim_centroid[1] /= n;
        rim_centroid[2] /= n;

        let apex = find_throat_centroid(verts, &rim, h, model_diag, &bvh, config).unwrap_or(rim_centroid);
        let apex_id = out_verts.len() as u32;
        out_verts.push(apex);

        let m = rim.len();
        for i in 0..m {
            let a = rim[i];
            let b = rim[(i + 1) % m];
            if a == b {
                continue;
            }
            // Boundary edge is directed a -> b by its incident triangle; the
            // cap contributes the opposite orientation b -> a.
            let tri = if directed_set.contains(&(a, b)) {
                [apex_id, b, a]
            } else if directed_set.contains(&(b, a)) {
                [apex_id, a, b]
            } else {
                [apex_id, b, a]
            };
            out_tris.push(tri);
            plug_face_count += 1;
        }
        plug_count += 1;
    }

    info.plugs = plug_count;
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

        // Flat plug flush with the top rim (z = 1).
        assert_eq!(out_verts.len(), verts.len() + 1);
        let apex = out_verts[out_verts.len() - 1];
        assert!((apex[2] - 1.0).abs() < 0.2, "apex z = {} should be near 1.0", apex[2]);
    }

    #[test]
    fn test_open_top_cube_flush_throat() {
        // Finer grid: the cut must still land at the opening.
        let (verts, tris) = unit_cube_open_top();
        let config = TamponadeConfig { h_min: Some(0.25), ..Default::default() };
        let (_out_verts, out_tris, info) = tamponade_repair(&verts, &tris, &config);
        assert_eq!(info.plugs, 1);
        assert_eq!(boundary_edge_count(&out_tris), 0);
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

        // One new apex, near the top plane (the opening lies at z = 1).
        assert_eq!(out_verts.len(), verts.len() + 1);
        let apex = out_verts[out_verts.len() - 1];
        assert!((apex[2] - 1.0).abs() < 0.25, "apex z = {} should be near 1.0", apex[2]);
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
    fn test_off_center_opening_apex_follows_hole() {
        // A long solid bar (x in [0,4]) whose top is missing over x in [2,4].
        // The apex must sit inside that opening, not at the bar centroid x=2.
        let xs = [0.0, 1.0, 2.0, 3.0, 4.0];
        let (verts, tris) = extruded_box(&xs, &[2, 3]);
        assert_eq!(boundary_edge_count(&tris) > 0, true);
        let config = TamponadeConfig { h_min: Some(0.25), ..Default::default() };
        let (out_verts, out_tris, info) = tamponade_repair(&verts, &tris, &config);
        assert_eq!(info.reason, "ok");
        assert_eq!(info.plugs, 1);
        assert_eq!(boundary_edge_count(&out_tris), 0);
        let apex = out_verts[out_verts.len() - 1];
        assert!(apex[0] > 2.5, "apex x = {} should follow the opening", apex[0]);
        assert!((apex[2] - 1.0).abs() < 0.25, "apex z = {} should be near 1.0", apex[2]);
    }

    #[test]
    fn test_two_open_ends_get_two_plugs() {
        // The extruded box with the top omitted on two disjoint segments makes
        // two separate openings -> exactly two plugs.
        let xs = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let (verts, tris) = extruded_box(&xs, &[1, 3]);
        let config = TamponadeConfig { h_min: Some(0.25), ..Default::default() };
        let (_out_verts, out_tris, info) = tamponade_repair(&verts, &tris, &config);
        assert_eq!(info.reason, "ok");
        assert_eq!(info.plugs, 2);
        assert_eq!(boundary_edge_count(&out_tris), 0);
    }
}
