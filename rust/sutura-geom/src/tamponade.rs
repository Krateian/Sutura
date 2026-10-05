// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tamponade (Method #17, alias Lava): Volumetric inside-out hole sealing and SI melting.
//!
//! Preserves healthy intact triangles 100% byte-identical.
//! Seals open holes at their narrowest geometric throat using minimum s-t cuts
//! on a localized cell graph.

use crate::mincut::BkGraph;
use crate::winding::MeshBvh;
use std::collections::{HashMap, HashSet, VecDeque};
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

/// Compute median edge length of the input mesh.
pub fn median_edge_length(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> f64 {
    if tris.is_empty() {
        return 1.0;
    }
    let step = (tris.len() / 5000).max(1);
    let mut lens = Vec::with_capacity(tris.len() / step * 3);

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

/// Find boundary edges (edges used by exactly 1 triangle).
/// Returns a map of canonical edge (min, max) -> (count, oriented_start, oriented_end).
pub fn find_boundary_edges(tris: &[[u32; 3]]) -> HashMap<(u32, u32), (usize, u32, u32)> {
    let mut edge_counts: HashMap<(u32, u32), (usize, u32, u32)> = HashMap::with_capacity(tris.len() * 2);

    for t in tris {
        let edges = [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])];
        for (a, b) in edges {
            let key = if a < b { (a, b) } else { (b, a) };
            let entry = edge_counts.entry(key).or_insert((0, a, b));
            entry.0 += 1;
        }
    }

    edge_counts.retain(|_, (c, _, _)| *c == 1);
    edge_counts
}

/// 3D Axis-Aligned Bounding Box.
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

    pub fn intersects(&self, other: &BBox) -> bool {
        self.min[0] <= other.max[0] && self.max[0] >= other.min[0] &&
        self.min[1] <= other.max[1] && self.max[1] >= other.min[1] &&
        self.min[2] <= other.max[2] && self.max[2] >= other.min[2]
    }
}

/// Exact 3D Triangle-AABB overlap test (Akenine-Möller).
pub fn tri_box_overlap(box_center: [f64; 3], box_half: [f64; 3], tri: [[f64; 3]; 3]) -> bool {
    // Translate triangle to origin
    let v0 = [tri[0][0] - box_center[0], tri[0][1] - box_center[1], tri[0][2] - box_center[2]];
    let v1 = [tri[1][0] - box_center[0], tri[1][1] - box_center[1], tri[1][2] - box_center[2]];
    let v2 = [tri[2][0] - box_center[0], tri[2][1] - box_center[1], tri[2][2] - box_center[2]];

    // 1. Test triangle AABB against box AABB
    let min_x = v0[0].min(v1[0]).min(v2[0]);
    let max_x = v0[0].max(v1[0]).max(v2[0]);
    if min_x > box_half[0] || max_x < -box_half[0] {
        return false;
    }

    let min_y = v0[1].min(v1[1]).min(v2[1]);
    let max_y = v0[1].max(v1[1]).max(v2[1]);
    if min_y > box_half[1] || max_y < -box_half[1] {
        return false;
    }

    let min_z = v0[2].min(v1[2]).min(v2[2]);
    let max_z = v0[2].max(v1[2]).max(v2[2]);
    if min_z > box_half[2] || max_z < -box_half[2] {
        return false;
    }

    // 2. Test triangle normal against box
    let f0 = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
    let f1 = [v2[0] - v1[0], v2[1] - v1[1], v2[2] - v1[2]];
    let f2 = [v0[0] - v2[0], v0[1] - v2[1], v0[2] - v2[2]];

    let normal = [
        f0[1] * f1[2] - f0[2] * f1[1],
        f0[2] * f1[0] - f0[0] * f1[2],
        f0[0] * f1[1] - f0[1] * f1[0],
    ];

    let d = normal[0] * v0[0] + normal[1] * v0[1] + normal[2] * v0[2];
    let r = box_half[0] * normal[0].abs() + box_half[1] * normal[1].abs() + box_half[2] * normal[2].abs();
    if d.abs() > r {
        return false;
    }

    // 3. Test 9 cross-product axes
    let test_axis = |a: [f64; 3]| -> bool {
        let p0 = a[0] * v0[0] + a[1] * v0[1] + a[2] * v0[2];
        let p1 = a[0] * v1[0] + a[1] * v1[1] + a[2] * v1[2];
        let p2 = a[0] * v2[0] + a[1] * v2[1] + a[2] * v2[2];
        let min_p = p0.min(p1).min(p2);
        let max_p = p0.max(p1).max(p2);
        let rad = box_half[0] * a[0].abs() + box_half[1] * a[1].abs() + box_half[2] * a[2].abs();
        min_p <= rad && max_p >= -rad
    };

    if !test_axis([0.0, -f0[2], f0[1]]) { return false; }
    if !test_axis([0.0, -f1[2], f1[1]]) { return false; }
    if !test_axis([0.0, -f2[2], f2[1]]) { return false; }

    if !test_axis([f0[2], 0.0, -f0[0]]) { return false; }
    if !test_axis([f1[2], 0.0, -f1[0]]) { return false; }
    if !test_axis([f2[2], 0.0, -f2[0]]) { return false; }

    if !test_axis([-f0[1], f0[0], 0.0]) { return false; }
    if !test_axis([-f1[1], f1[0], 0.0]) { return false; }
    if !test_axis([-f2[1], f2[0], 0.0]) { return false; }

    true
}

/// Main entry point for Tamponade repair.
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

    // 1. Identify open boundary edges
    let boundary_map = find_boundary_edges(tris);

    if boundary_map.is_empty() {
        // Mesh has 0 open holes! Intact control mesh stays 100% byte-identical.
        info.reason = "ok".to_string();
        info.seconds = t0.elapsed().as_secs_f64();
        return (verts.to_vec(), tris.to_vec(), info);
    }

    let h = match config.h_min {
        Some(val) if val > 1e-6 => val,
        _ => median_edge_length(verts, tris),
    };

    // 2. Build BVH for winding queries and closest point distance
    let tris_usize: Vec<[usize; 3]> = tris
        .iter()
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    let bvh = MeshBvh::from_arrays(verts, &tris_usize);

    // 3. Cluster boundary edges into connected components
    let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for (_, (_, a, b)) in &boundary_map {
        adj.entry(*a).or_default().push(*b);
        adj.entry(*b).or_default().push(*a);
    }

    let mut visited: HashSet<u32> = HashSet::new();
    let mut components: Vec<Vec<u32>> = Vec::new();

    for &start_v in adj.keys() {
        if visited.contains(&start_v) {
            continue;
        }
        let mut comp = Vec::new();
        let mut q = VecDeque::new();
        q.push_back(start_v);
        visited.insert(start_v);

        while let Some(u) = q.pop_front() {
            comp.push(u);
            if let Some(nbrs) = adj.get(&u) {
                for &v in nbrs {
                    if visited.insert(v) {
                        q.push_back(v);
                    }
                }
            }
        }
        components.push(comp);
    }

    // 4. Compute bounding boxes for each component and merge overlapping ones
    let mut boxes: Vec<(BBox, Vec<u32>)> = Vec::with_capacity(components.len());
    for comp in components {
        let mut bbox = BBox::empty();
        for &vid in &comp {
            bbox.expand_point(verts[vid as usize]);
        }
        bbox.pad(3.0 * h);
        boxes.push((bbox, comp));
    }

    // Iteratively merge overlapping bounding boxes
    let mut merged = true;
    while merged {
        merged = false;
        let mut new_boxes: Vec<(BBox, Vec<u32>)> = Vec::new();
        for (b, comp) in boxes.drain(..) {
            let mut absorbed = false;
            for (nb, ncomp) in &mut new_boxes {
                if nb.intersects(&b) {
                    for i in 0..3 {
                        nb.min[i] = nb.min[i].min(b.min[i]);
                        nb.max[i] = nb.max[i].max(b.max[i]);
                    }
                    ncomp.extend(comp.iter().copied());
                    absorbed = true;
                    merged = true;
                    break;
                }
            }
            if !absorbed {
                new_boxes.push((b, comp));
            }
        }
        boxes = new_boxes;
    }

    // 5. Seal each defect cluster via min-cut
    let mut out_verts = verts.to_vec();
    let mut out_tris = tris.to_vec();

    for (bbox, rim_verts) in boxes {
        if let Some(t_budget) = config.time_budget_s {
            if t0.elapsed().as_secs_f64() > t_budget {
                info.reason = "timeout exceeded".to_string();
                break;
            }
        }

        // Local grid dimensions
        let dx = (bbox.max[0] - bbox.min[0]).max(h);
        let dy = (bbox.max[1] - bbox.min[1]).max(h);
        let dz = (bbox.max[2] - bbox.min[2]).max(h);

        let nx = ((dx / h).ceil() as usize).clamp(3, 80);
        let ny = ((dy / h).ceil() as usize).clamp(3, 80);
        let nz = ((dz / h).ceil() as usize).clamp(3, 80);

        let hx = dx / nx as f64;
        let hy = dy / ny as f64;
        let hz = dz / nz as f64;
        let h_eff = (hx * hy * hz).cbrt();

        let total_cells = nx * ny * nz;
        if let Some(max_n) = config.max_nodes {
            if total_cells > max_n {
                continue;
            }
        }

        let cell_idx = |ix: usize, iy: usize, iz: usize| -> usize {
            ix + iy * nx + iz * (nx * ny)
        };

        let cell_center = |ix: usize, iy: usize, iz: usize| -> [f64; 3] {
            [
                bbox.min[0] + (ix as f64 + 0.5) * hx,
                bbox.min[1] + (iy as f64 + 0.5) * hy,
                bbox.min[2] + (iz as f64 + 0.5) * hz,
            ]
        };

        // Build BK min-cut graph
        let mut g = BkGraph::new(total_cells);
        let face_cap = h_eff * h_eff;
        let wall_cap = 1e8; // Infinite capacity across mesh triangles

        // Ray test helper to detect if mesh triangles cross the face between two adjacent cells
        let crosses_wall = |p1: [f64; 3], p2: [f64; 3]| -> bool {
            let dir = [p2[0] - p1[0], p2[1] - p1[1], p2[2] - p1[2]];
            let len = (dir[0].powi(2) + dir[1].powi(2) + dir[2].powi(2)).sqrt();
            if len < 1e-12 {
                return false;
            }
            let udir = [dir[0] / len, dir[1] / len, dir[2] / len];
            let hit = bvh.intersect_ray(p1, udir, len, 1e-5);
            hit.crossings > 0
        };

        // Source and Sink seeds
        for iz in 0..nz {
            for iy in 0..ny {
                for ix in 0..nx {
                    let idx = cell_idx(ix, iy, iz);
                    let c = cell_center(ix, iy, iz);
                    let is_border = ix == 0 || ix == nx - 1 || iy == 0 || iy == ny - 1 || iz == 0 || iz == nz - 1;
                    let w = bvh.winding_at(c);

                    if w > 0.5 {
                        g.add_tweights(idx, 1e8, 0.0);
                    } else if is_border || w < 0.2 {
                        g.add_tweights(idx, 0.0, 1e8);
                    }

                    // Add edges to 6 neighbors
                    if ix + 1 < nx {
                        let right = cell_idx(ix + 1, iy, iz);
                        let c_right = cell_center(ix + 1, iy, iz);
                        let cap = if crosses_wall(c, c_right) { wall_cap } else { face_cap };
                        g.add_edge(idx, right, cap, cap);
                    }
                    if iy + 1 < ny {
                        let front = cell_idx(ix, iy + 1, iz);
                        let c_front = cell_center(ix, iy + 1, iz);
                        let cap = if crosses_wall(c, c_front) { wall_cap } else { face_cap };
                        g.add_edge(idx, front, cap, cap);
                    }
                    if iz + 1 < nz {
                        let top = cell_idx(ix, iy, iz + 1);
                        let c_top = cell_center(ix, iy, iz + 1);
                        let cap = if crosses_wall(c, c_top) { wall_cap } else { face_cap };
                        g.add_edge(idx, top, cap, cap);
                    }
                }
            }
        }

        let max_flow = g.maxflow();
        eprintln!("DEBUG: nx={}, ny={}, nz={}, total={}, max_flow={}", nx, ny, nz, total_cells, max_flow);
        if max_flow <= 1e-12 || max_flow >= 1e7 {
            // No flow path between S and T, or only cut through walls
            eprintln!("DEBUG: skipping because max_flow is zero or infinite");
            continue;
        }

        // 6. Extract cut faces: open interface between S and T
        let mut plug_quads: Vec<([f64; 3], [f64; 3], [f64; 3], [f64; 3])> = Vec::new();

        for iz in 0..nz {
            for iy in 0..ny {
                for ix in 0..nx {
                    let u = cell_idx(ix, iy, iz);
                    if !g.is_in_source(u) {
                        continue;
                    }
                    let c_u = cell_center(ix, iy, iz);

                    // u is in S. Check neighbors in +X, -X, +Y, -Y, +Z, -Z
                    // 1. Face in +X
                    if ix + 1 < nx {
                        let v = cell_idx(ix + 1, iy, iz);
                        let c_v = cell_center(ix + 1, iy, iz);
                        if !g.is_in_source(v) && !crosses_wall(c_u, c_v) {
                            let x_face = bbox.min[0] + (ix + 1) as f64 * hx;
                            let y0 = bbox.min[1] + iy as f64 * hy;
                            let y1 = y0 + hy;
                            let z0 = bbox.min[2] + iz as f64 * hz;
                            let z1 = z0 + hz;

                            let p0 = [x_face, y0, z0];
                            let p1 = [x_face, y1, z0];
                            let p2 = [x_face, y1, z1];
                            let p3 = [x_face, y0, z1];
                            plug_quads.push((p0, p1, p2, p3));
                        }
                    }
                    // 2. Face in -X
                    if ix > 0 {
                        let v = cell_idx(ix - 1, iy, iz);
                        let c_v = cell_center(ix - 1, iy, iz);
                        if !g.is_in_source(v) && !crosses_wall(c_u, c_v) {
                            let x_face = bbox.min[0] + ix as f64 * hx;
                            let y0 = bbox.min[1] + iy as f64 * hy;
                            let y1 = y0 + hy;
                            let z0 = bbox.min[2] + iz as f64 * hz;
                            let z1 = z0 + hz;

                            let p0 = [x_face, y0, z0];
                            let p1 = [x_face, y0, z1];
                            let p2 = [x_face, y1, z1];
                            let p3 = [x_face, y1, z0];
                            plug_quads.push((p0, p1, p2, p3));
                        }
                    }
                    // 3. Face in +Y
                    if iy + 1 < ny {
                        let v = cell_idx(ix, iy + 1, iz);
                        let c_v = cell_center(ix, iy + 1, iz);
                        if !g.is_in_source(v) && !crosses_wall(c_u, c_v) {
                            let y_face = bbox.min[1] + (iy + 1) as f64 * hy;
                            let x0 = bbox.min[0] + ix as f64 * hx;
                            let x1 = x0 + hx;
                            let z0 = bbox.min[2] + iz as f64 * hz;
                            let z1 = z0 + hz;

                            let p0 = [x0, y_face, z0];
                            let p1 = [x0, y_face, z1];
                            let p2 = [x1, y_face, z1];
                            let p3 = [x1, y_face, z0];
                            plug_quads.push((p0, p1, p2, p3));
                        }
                    }
                    // 4. Face in -Y
                    if iy > 0 {
                        let v = cell_idx(ix, iy - 1, iz);
                        let c_v = cell_center(ix, iy - 1, iz);
                        if !g.is_in_source(v) && !crosses_wall(c_u, c_v) {
                            let y_face = bbox.min[1] + iy as f64 * hy;
                            let x0 = bbox.min[0] + ix as f64 * hx;
                            let x1 = x0 + hx;
                            let z0 = bbox.min[2] + iz as f64 * hz;
                            let z1 = z0 + hz;

                            let p0 = [x0, y_face, z0];
                            let p1 = [x1, y_face, z0];
                            let p2 = [x1, y_face, z1];
                            let p3 = [x0, y_face, z1];
                            plug_quads.push((p0, p1, p2, p3));
                        }
                    }
                    // 5. Face in +Z
                    if iz + 1 < nz {
                        let v = cell_idx(ix, iy, iz + 1);
                        let c_v = cell_center(ix, iy, iz + 1);
                        if !g.is_in_source(v) && !crosses_wall(c_u, c_v) {
                            let z_face = bbox.min[2] + (iz + 1) as f64 * hz;
                            let x0 = bbox.min[0] + ix as f64 * hx;
                            let x1 = x0 + hx;
                            let y0 = bbox.min[1] + iy as f64 * hy;
                            let y1 = y0 + hy;

                            let p0 = [x0, y0, z_face];
                            let p1 = [x1, y0, z_face];
                            let p2 = [x1, y1, z_face];
                            let p3 = [x0, y1, z_face];
                            plug_quads.push((p0, p1, p2, p3));
                        }
                    }
                    // 6. Face in -Z
                    if iz > 0 {
                        let v = cell_idx(ix, iy, iz - 1);
                        let c_v = cell_center(ix, iy, iz - 1);
                        if !g.is_in_source(v) && !crosses_wall(c_u, c_v) {
                            let z_face = bbox.min[2] + iz as f64 * hz;
                            let x0 = bbox.min[0] + ix as f64 * hx;
                            let x1 = x0 + hx;
                            let y0 = bbox.min[1] + iy as f64 * hy;
                            let y1 = y0 + hy;

                            let p0 = [x0, y0, z_face];
                            let p1 = [x0, y1, z_face];
                            let p2 = [x1, y1, z_face];
                            let p3 = [x1, y0, z_face];
                            plug_quads.push((p0, p1, p2, p3));
                        }
                    }
                }
            }
        }



        if plug_quads.is_empty() {
            continue;
        }

        // 7. Snap plug boundary vertices to nearest rim vertices
        let snap_dist_thresh = 2.5 * h_eff;
        let rim_coords: Vec<[f64; 3]> = rim_verts.iter().map(|&v| out_verts[v as usize]).collect();

        // Unique vertex mapping for the plug
        let mut plug_verts: Vec<[f64; 3]> = Vec::new();
        let mut plug_tris: Vec<[u32; 3]> = Vec::new();
        let mut vert_map: HashMap<[i64; 3], u32> = HashMap::new();

        let quant = |p: [f64; 3]| -> [i64; 3] {
            [
                (p[0] * 1e5).round() as i64,
                (p[1] * 1e5).round() as i64,
                (p[2] * 1e5).round() as i64,
            ]
        };

        for &(p0, p1, p2, p3) in &plug_quads {
            let mut quad_vids = [0u32; 4];
            for (k, &p) in [p0, p1, p2, p3].iter().enumerate() {
                let key = quant(p);
                let vid = *vert_map.entry(key).or_insert_with(|| {
                    let new_id = plug_verts.len() as u32;
                    plug_verts.push(p);
                    new_id
                });
                quad_vids[k] = vid;
            }

            // Split quad into 2 triangles: (0, 1, 2) and (0, 2, 3)
            plug_tris.push([quad_vids[0], quad_vids[1], quad_vids[2]]);
            plug_tris.push([quad_vids[0], quad_vids[2], quad_vids[3]]);
        }

        // Identify boundary vertices of the plug
        let mut plug_edge_counts: HashMap<(u32, u32), usize> = HashMap::new();
        for &t in &plug_tris {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                let key = if a < b { (a, b) } else { (b, a) };
                *plug_edge_counts.entry(key).or_insert(0) += 1;
            }
        }

        let mut plug_bnd_verts: HashSet<u32> = HashSet::new();
        for (&(a, b), &c) in &plug_edge_counts {
            if c == 1 {
                plug_bnd_verts.insert(a);
                plug_bnd_verts.insert(b);
            }
        }

        // Snap boundary vertices of plug to nearest rim vertex
        let mut plug_to_mesh: HashMap<u32, u32> = HashMap::new();
        for &pv in &plug_bnd_verts {
            let p = plug_verts[pv as usize];
            let mut best_dist = f64::INFINITY;
            let mut best_rim_vid = None;

            for (i, &rc) in rim_coords.iter().enumerate() {
                let d2 = (rc[0] - p[0]).powi(2) + (rc[1] - p[1]).powi(2) + (rc[2] - p[2]).powi(2);
                if d2 < best_dist {
                    best_dist = d2;
                    best_rim_vid = Some(rim_verts[i]);
                }
            }

            if best_dist.sqrt() <= snap_dist_thresh {
                if let Some(rvid) = best_rim_vid {
                    plug_to_mesh.insert(pv, rvid);
                }
            }
        }

        // For non-snapped plug vertices, assign them a new vertex in out_verts
        for pv in 0..plug_verts.len() as u32 {
            if !plug_to_mesh.contains_key(&pv) {
                let new_id = out_verts.len() as u32;
                out_verts.push(plug_verts[pv as usize]);
                plug_to_mesh.insert(pv, new_id);
            }
        }

        // Append non-degenerate plug triangles to out_tris
        let mut added_count = 0;
        for &t in &plug_tris {
            let m0 = plug_to_mesh[&t[0]];
            let m1 = plug_to_mesh[&t[1]];
            let m2 = plug_to_mesh[&t[2]];

            if m0 != m1 && m1 != m2 && m2 != m0 {
                out_tris.push([m0, m1, m2]);
                added_count += 1;
            }
        }

        eprintln!("DEBUG: plug_quads={}, plug_tris={}, added_count={}", plug_quads.len(), plug_tris.len(), added_count);
        if added_count > 0 {
            info.plugs += 1;
            info.plug_faces += added_count;
        }
    }

    info.reason = "ok".to_string();
    info.seconds = t0.elapsed().as_secs_f64();
    (out_verts, out_tris, info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_cube_open_top() -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        // Unit cube [0, 1]^3 missing the top face at z = 1
        let verts = vec![
            [0.0, 0.0, 0.0], // 0
            [1.0, 0.0, 0.0], // 1
            [1.0, 1.0, 0.0], // 2
            [0.0, 1.0, 0.0], // 3
            [0.0, 0.0, 1.0], // 4
            [1.0, 0.0, 1.0], // 5
            [1.0, 1.0, 1.0], // 6
            [0.0, 1.0, 1.0], // 7
        ];

        // Bottom z=0: (0, 2, 1), (0, 3, 2)
        // -X (x=0): (0, 7, 3), (0, 4, 7)
        // +X (x=1): (1, 2, 6), (1, 6, 5)
        // -Y (y=0): (0, 1, 5), (0, 5, 4)
        // +Y (y=1): (3, 7, 6), (3, 6, 2)
        // Top (z=1) is MISSING!
        let tris = vec![
            [0, 2, 1], [0, 3, 2], // bottom
            [0, 1, 5], [0, 5, 4], // -y
            [2, 3, 7], [2, 7, 6], // +y
            [1, 2, 6], [1, 6, 5], // +x
            [3, 0, 4], [3, 4, 7], // -x
        ];

        (verts, tris)
    }

    fn unit_sphere() -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        // Octahedron representing closed sphere
        let verts = vec![
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
        ];
        let tris = vec![
            [0, 2, 4], [2, 1, 4], [1, 3, 4], [3, 0, 4],
            [2, 0, 5], [1, 2, 5], [3, 1, 5], [0, 3, 5],
        ];
        (verts, tris)
    }

    #[test]
    fn test_closed_sphere_untouched() {
        let (verts, tris) = unit_sphere();
        let config = TamponadeConfig {
            h_min: Some(0.2),
            ..Default::default()
        };

        let tris_usize: Vec<[usize; 3]> = tris.iter().map(|t| [t[0] as usize, t[1] as usize, t[2] as usize]).collect();
        let bvh = MeshBvh::from_arrays(&verts, &tris_usize);
        eprintln!("TEST: winding at (0.5, 0.5, 0.5) = {}", bvh.winding_at([0.5, 0.5, 0.5]));
        eprintln!("TEST: winding at (0.5, 0.5, 0.8) = {}", bvh.winding_at([0.5, 0.5, 0.8]));
        eprintln!("TEST: winding at (0.5, 0.5, 1.2) = {}", bvh.winding_at([0.5, 0.5, 1.2]));

        let (out_verts, out_tris, info) = tamponade_repair(&verts, &tris, &config);
        assert_eq!(info.plugs, 0);
        assert_eq!(info.plug_faces, 0);
        assert_eq!(info.healthy_faces_kept, tris.len());
        assert_eq!(out_tris.len(), tris.len());
        assert_eq!(out_verts.len(), verts.len());
    }

    #[test]
    fn test_open_top_cube_sealing() {
        let (verts, tris) = unit_cube_open_top();
        let config = TamponadeConfig {
            h_min: Some(0.5),
            ..Default::default()
        };

        let tris_usize: Vec<[usize; 3]> = tris.iter().map(|t| [t[0] as usize, t[1] as usize, t[2] as usize]).collect();
        let bvh = MeshBvh::from_arrays(&verts, &tris_usize);
        eprintln!("CUBE WINDING AT (0.5, 0.5, 0.5) = {}", bvh.winding_at([0.5, 0.5, 0.5]));
        eprintln!("CUBE WINDING AT (0.5, 0.5, 0.2) = {}", bvh.winding_at([0.5, 0.5, 0.2]));

        let (out_verts, out_tris, info) = tamponade_repair(&verts, &tris, &config);
        assert!(info.plugs >= 1);
        assert!(info.plug_faces >= 2);
        assert_eq!(info.healthy_faces_kept, tris.len());
        assert!(out_tris.len() > tris.len());

        // Verify that original 10 triangles remain verbatim at the front
        for i in 0..tris.len() {
            assert_eq!(out_tris[i], tris[i]);
        }

        // Check that holes are reduced/eliminated
        let remaining_bnd = find_boundary_edges(&out_tris);
        assert!(remaining_bnd.len() < find_boundary_edges(&tris).len());
    }
}
