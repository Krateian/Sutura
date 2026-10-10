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
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

/// Boundary loops up to this size are triangulated by the exact O(n^3)
/// minimum-area DP; larger loops use the O(n^2) ear-clipping fallback.
const PLUG_DP_CAP: usize = 300;
/// Plug triangles with an area at or below this (in the mesh's units, squared)
/// are treated as degenerate and cause the loop's plug to be rejected.
const PLUG_MIN_AREA: f64 = 1e-12;
/// Boundary loops at or below this vertex count are treated as genuine holes
/// without the min-cut test. A tiny rim cannot separate a meaningful interior
/// from the exterior, and the coarse occupancy grid used by the cut rejects
/// real tiny holes (e.g. artec_copper-key's four 3-vertex rims) as
/// `NoSeparation`.
const TINY_LOOP_MAX: usize = 8;
/// "Invisible plug" gate (a): a rim whose diameter exceeds this multiple of the
/// median edge length is too large to be a plug a rim-only triangulation can
/// close invisibly, and is left for the surface-based tiers (Flap/Graft). The
/// check runs BEFORE the expensive min-cut so an oversized rim costs nothing.
const PLUG_MAX_LOOP_DIAM_FACTOR: f64 = 120.0;
/// "Invisible plug" gate (b): a plug whose interior deviates from the best-fit
/// plane of its rim neighbourhood by more than this multiple of the median edge
/// length is rejected - it would visibly bridge away from the local surface.
const PLUG_MAX_DEV_FACTOR: f64 = 1.0;

// --- T3 seal-level ("line 2") reference surface ---------------------------
/// Ring band of intact faces (face-ring index from the rim) the reference
/// surface is fitted through. Ring 0 is the rim-adjacent (damaged) band and is
/// always skipped; rings 2..=3 are intact surrounding tissue.
const REF_RING_MIN: usize = 2;
const REF_RING_MAX: usize = 3;
/// Minimum number of distinct intact ring points needed to fit the quadric.
const REF_MIN_POINTS: usize = 8;
/// Ring-face sliver brake: a face whose area is below this fraction of `h^2`
/// is a sliver and does not contribute to the reference.
const REF_SLIVER_AREA_FRAC: f64 = 0.01;
/// A ring whose face normals spread more than this from the mean is too noisy
/// (sharp edge, saddle) and yields `no reference`.
const REF_NORMAL_SPREAD_DEG: f64 = 60.0;
/// The quadric fit residual (rms over the ring points) must not exceed this
/// multiple of `h`, else the surrounding surface is not locally smooth.
const REF_RMS_FACTOR: f64 = 1.0;
/// Laser-ray divergence brake: the MAD of the tangent-ray meeting heights must
/// not exceed this multiple of `h`. A saddle makes opposite tangents meet at
/// opposite heights, so it is rejected (`no reference`).
const REF_RAY_SPREAD_FACTOR: f64 = 1.5;
/// Ridge regularisation of the quadric normal equations (relative to the trace).
const REF_RIDGE: f64 = 1e-6;
/// A rim/tail vertex deeper than this fraction of `h` below the reference is
/// part of the overshoot tail and is lifted onto the reference surface.
const TRIM_EPS_FACTOR: f64 = 0.02;
/// Per-loop cap on lifted tail vertices (a healthy mesh is never touched).
const TRIM_MAX_VERTICES: usize = 64;

// --- per-loop wall-clock allowance ----------------------------------------
/// When a budget is set, each boundary loop gets its own allowance (seconds):
/// a flat base plus a term proportional to the rim vertex count, so a single
/// genuine hole is not cut short by the mesh size. plate_a_dent's 29-vertex rim
/// needs ~2.7 s and the removed mesh-scaled cap gave it only 0.44 s. The whole
/// run is still capped by `TamponadeConfig::time_budget_s` (the intensity
/// preset), so the loop allowance only ever loosens an under-sized preset.
const LOOP_BUDGET_BASE_S: f64 = 0.30;
const LOOP_BUDGET_PER_VERTEX_S: f64 = 0.10;
/// Deviation gate against the reference surface (allows the curved patch).
const REF_DEV_FACTOR: f64 = 3.0;

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
    pub plugs_rejected_size: usize,
    pub plugs_rejected_dev: usize,
    pub plug_faces: usize,
    pub melt_layers_tried: usize,
    pub melt_layers_accepted: usize,
    pub melt_layers_rolled_back: usize,
    pub micro_welds_ok: usize,
    pub micro_welds_failed: usize,
    /// Input faces the melt pass excised (accepted layers only).
    pub excised_faces: usize,
    pub healthy_faces_kept: usize,
    /// Holes sealed using a fitted reference ("laser") surface (line 2).
    pub reference_used: usize,
    /// Holes whose reference surface was rejected -> rim-only fallback.
    pub reference_fallback: usize,
    /// Rim/tail vertices lifted onto the reference surface (overshoot trim).
    pub trimmed_vertices: usize,
    /// Interior patch vertices placed on the reference surface.
    pub patch_vertices: usize,
    pub seconds: f64,
    pub reason: String,
    /// One entry per boundary loop: its vertex count and the decision taken
    /// (`sealed`, `tiny_sealed`, or a rejection reason). Diagnostic only.
    pub loop_diag: Vec<String>,
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
    // Emit in the block-stacked half-edge order used by the Python verifier's
    // `defects._boundary_edges` (all (0,1) edges, then all (1,2), then all
    // (2,0)). The loop walker's greedy choice at a shared rim vertex depends on
    // this order, so the CLI and the Rust kernel must decompose a rim the same
    // way.
    let mut out = Vec::new();
    for &(i, j) in &[(0usize, 1usize), (1, 2), (2, 0)] {
        for t in tris {
            let a = t[i];
            let b = t[j];
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
///
/// This is a deliberate port of the Python verifier's `defects._boundary_loops`
/// (insertion-ordered adjacency, vertex-marking greedy) so the kernel and the
/// verifier decompose a shared-vertex / non-manifold rim identically. The
/// earlier edge-marking walker chose a different loop through a shared vertex,
/// which made the plug miss the real holes.
pub fn build_boundary_loops(directed: &[(u32, u32)]) -> Vec<Vec<u32>> {
    let mut order: Vec<u32> = Vec::new();
    let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(a, b) in directed {
        if a == b {
            continue;
        }
        for (x, y) in [(a, b), (b, a)] {
            if !adj.contains_key(&x) {
                order.push(x);
                adj.insert(x, Vec::new());
            }
            adj.get_mut(&x).unwrap().push(y);
        }
    }

    let mut used: HashSet<u32> = HashSet::new();
    let mut loops = Vec::new();
    for &start in &order {
        if used.contains(&start) {
            continue;
        }
        let mut loop_v = vec![start];
        used.insert(start);
        let mut prev: Option<u32> = None;
        let mut cur = start;
        loop {
            let mut nxt: Option<u32> = None;
            if let Some(nbrs) = adj.get(&cur) {
                for &nb in nbrs {
                    if Some(nb) == prev {
                        continue;
                    }
                    if nb == start && loop_v.len() > 2 {
                        nxt = Some(nb);
                        break;
                    }
                    if !used.contains(&nb) {
                        nxt = Some(nb);
                        break;
                    }
                }
            }
            match nxt {
                None => break,
                Some(n) if n == start => break,
                Some(n) => {
                    used.insert(n);
                    loop_v.push(n);
                    prev = Some(cur);
                    cur = n;
                }
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
    /// The per-loop deadline passed while evaluating the cut; the caller stops
    /// (this loop and all later loops are left open).
    Timeout,
}

#[inline]
fn deadline_passed(deadline: Option<Instant>) -> bool {
    matches!(deadline, Some(d) if Instant::now() >= d)
}

/// Path-halving union-find over the occupancy-grid cells, used to group the
/// non-wall cells into wall-bounded free-space components.
struct UnionFind {
    parent: Vec<u32>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self { parent: (0..n as u32).collect() }
    }

    fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] as usize != root {
            root = self.parent[root] as usize;
        }
        let mut cur = x;
        while self.parent[cur] as usize != root {
            let next = self.parent[cur] as usize;
            self.parent[cur] = root as u32;
            cur = next;
        }
        root
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            // Attach the larger root under the smaller: deterministic and
            // shallow for the row-major scan order.
            if ra < rb {
                self.parent[rb] = ra as u32;
            } else {
                self.parent[ra] = rb as u32;
            }
        }
    }
}

/// Occupancy-grid cell count `throat_separates` would build for a rim (used
/// only by the profiling instrumentation).
fn throat_cells(loop_v: &[u32], verts: &[[f64; 3]], h_global: f64) -> usize {
    let mut bbox = BBox::empty();
    for &v in loop_v {
        bbox.expand_point(verts[v as usize]);
    }
    if !bbox.min[0].is_finite() {
        return 0;
    }
    let loop_diag = bbox.diagonal().max(h_global);
    let h_desired = (loop_diag / 24.0).max(1e-9);
    let pad = (3.0 * h_desired).max(1.5 * loop_diag);
    bbox.pad(pad);
    let span = [
        (bbox.max[0] - bbox.min[0]).max(h_desired),
        (bbox.max[1] - bbox.min[1]).max(h_desired),
        (bbox.max[2] - bbox.min[2]).max(h_desired),
    ];
    let mut h = h_desired;
    for &s in &span {
        h = h.max(s / 64.0);
    }
    let nx = ((span[0] / h).ceil() as usize).clamp(2, 64);
    let ny = ((span[1] / h).ceil() as usize).clamp(2, 64);
    let nz = ((span[2] / h).ceil() as usize).clamp(2, 64);
    nx * ny * nz
}

fn throat_separates(
    verts: &[[f64; 3]],
    loop_v: &[u32],
    h_global: f64,
    _model_diag: f64,
    bvh: &MeshBvh,
    config: &TamponadeConfig,
    percell: bool,
    deadline: Option<Instant>,
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
    let prof = std::env::var("SUTURA_TAMP_PROFILE").is_ok();

    let center_of = |idx: usize| -> [f64; 3] {
        let iz = idx / (nx * ny);
        let rem = idx % (nx * ny);
        let iy = rem / nx;
        let ix = rem % nx;
        cell_center(ix, iy, iz)
    };
    let border_of = |idx: usize| -> bool {
        let iz = idx / (nx * ny);
        let rem = idx % (nx * ny);
        let iy = rem / nx;
        let ix = rem % nx;
        ix == 0 || ix == nx - 1 || iy == 0 || iy == ny - 1 || iz == 0 || iz == nz - 1
    };
    let tag = |w: f64, border: bool| -> i8 {
        if w > 0.5 {
            1
        } else if border || w < 0.1 {
            2
        } else {
            0
        }
    };

    // Open-space connections only: an edge that crosses an input triangle is a
    // wall and is not traversable, so the cut can only pass through the hole.
    // Union-finding the traversable edges yields the wall-bounded free-space
    // components in the same pass that builds the graph.
    let te0 = Instant::now();
    let face_cap = h_eff * h_eff;
    let mut uf = UnionFind::new(total);
    // `wall_bits` records the blocked forward neighbours of each cell (bit 0
    // +x, bit 1 +y, bit 2 +z); `near_surface` marks both endpoints of every
    // blocked edge, the seeds of the distance-to-surface flood fill.
    let mut wall_bits: Vec<u8> = vec![0u8; total];
    let mut near_surface: Vec<bool> = vec![false; total];
    for iz in 0..nz {
        if deadline_passed(deadline) {
            return CutVerdict::Timeout;
        }
        for iy in 0..ny {
            for ix in 0..nx {
                let idx = cell_idx(ix, iy, iz);
                let c = cell_center(ix, iy, iz);
                if ix + 1 < nx {
                    let c2 = cell_center(ix + 1, iy, iz);
                    let j = cell_idx(ix + 1, iy, iz);
                    if crosses_wall(bvh, c, c2) {
                        wall_bits[idx] |= 1;
                        near_surface[idx] = true;
                        near_surface[j] = true;
                    } else {
                        g.add_edge(idx, j, face_cap, face_cap);
                        uf.union(idx, j);
                    }
                }
                if iy + 1 < ny {
                    let c2 = cell_center(ix, iy + 1, iz);
                    let j = cell_idx(ix, iy + 1, iz);
                    if crosses_wall(bvh, c, c2) {
                        wall_bits[idx] |= 2;
                        near_surface[idx] = true;
                        near_surface[j] = true;
                    } else {
                        g.add_edge(idx, j, face_cap, face_cap);
                        uf.union(idx, j);
                    }
                }
                if iz + 1 < nz {
                    let c2 = cell_center(ix, iy, iz + 1);
                    let j = cell_idx(ix, iy, iz + 1);
                    if crosses_wall(bvh, c, c2) {
                        wall_bits[idx] |= 4;
                        near_surface[idx] = true;
                        near_surface[j] = true;
                    } else {
                        g.add_edge(idx, j, face_cap, face_cap);
                        uf.union(idx, j);
                    }
                }
            }
        }
    }
    let edges_s = te0.elapsed().as_secs_f64();

    // Distance to the nearest surface, flooded over free space (6-connected
    // through non-wall edges). The deepest cell of a free-space component is
    // the most surface-independent sample, so its winding decides the whole
    // component.
    let mut dist = vec![u32::MAX; total];
    {
        let mut q: VecDeque<usize> = VecDeque::new();
        for idx in 0..total {
            if near_surface[idx] {
                dist[idx] = 0;
                q.push_back(idx);
            }
        }
        while let Some(idx) = q.pop_front() {
            let d = dist[idx];
            let iz = idx / (nx * ny);
            let rem = idx % (nx * ny);
            let iy = rem / nx;
            let ix = rem % nx;
            let bits = wall_bits[idx];
            if ix + 1 < nx && bits & 1 == 0 {
                let j = idx + 1;
                if dist[j] == u32::MAX {
                    dist[j] = d + 1;
                    q.push_back(j);
                }
            }
            if ix > 0 && wall_bits[idx - 1] & 1 == 0 {
                let j = idx - 1;
                if dist[j] == u32::MAX {
                    dist[j] = d + 1;
                    q.push_back(j);
                }
            }
            if iy + 1 < ny && bits & 2 == 0 {
                let j = idx + nx;
                if dist[j] == u32::MAX {
                    dist[j] = d + 1;
                    q.push_back(j);
                }
            }
            if iy > 0 && wall_bits[idx - nx] & 2 == 0 {
                let j = idx - nx;
                if dist[j] == u32::MAX {
                    dist[j] = d + 1;
                    q.push_back(j);
                }
            }
            if iz + 1 < nz && bits & 4 == 0 {
                let j = idx + nx * ny;
                if dist[j] == u32::MAX {
                    dist[j] = d + 1;
                    q.push_back(j);
                }
            }
            if iz > 0 && wall_bits[idx - nx * ny] & 4 == 0 {
                let j = idx - nx * ny;
                if dist[j] == u32::MAX {
                    dist[j] = d + 1;
                    q.push_back(j);
                }
            }
        }
    }

    // Winding only tags each cell. A wall-bounded free-space component is
    // inside/outside-homogeneous, so the winding is sampled coarsely per
    // component instead of once per cell; a component whose samples are
    // inconclusive falls back to the exact per-cell path, which stays
    // available under SUTURA_TAMP_PERCELL.
    let tw0 = Instant::now();
    let mut cls = vec![0i8; total]; // 0 neutral, 1 source, 2 sink
    if percell {
        for iz in 0..nz {
            if deadline_passed(deadline) {
                return CutVerdict::Timeout;
            }
            let mut pts: Vec<[f64; 3]> = Vec::with_capacity(nx * ny);
            for iy in 0..ny {
                for ix in 0..nx {
                    pts.push(cell_center(ix, iy, iz));
                }
            }
            let ws = bvh.winding_points_par(&pts);
            let mut k = 0usize;
            for iy in 0..ny {
                for ix in 0..nx {
                    let idx = cell_idx(ix, iy, iz);
                    cls[idx] = tag(ws[k], border_of(idx));
                    k += 1;
                }
            }
        }
    } else {
        // A wall-bounded free-space component is inside/outside-homogeneous,
        // so the winding is evaluated on a deterministic coarse lattice (a few
        // thousand samples) rather than per cell, plus each component's
        // deepest-from-surface cell. A component is tagged only when its
        // in-band (neutral) samples are ignored and every remaining sample
        // agrees; a component that spans the surface (a real throat), or whose
        // samples disagree, is re-evaluated per cell exactly as before, so the
        // occupancy grid is unchanged.
        const LAT_SAMPLES: usize = 4096;
        let mut stride = 1usize;
        while stride < 16
            && nx.div_ceil(stride) * ny.div_ceil(stride) * nz.div_ceil(stride) > LAT_SAMPLES
        {
            stride += 1;
        }
        let mut deepest: HashMap<usize, (u32, usize)> = HashMap::new();
        let mut sample_idx: Vec<usize> = Vec::new();
        let mut owner: Vec<usize> = Vec::new();
        for iz in 0..nz {
            for iy in 0..ny {
                for ix in 0..nx {
                    let idx = cell_idx(ix, iy, iz);
                    if dist[idx] == u32::MAX {
                        continue;
                    }
                    let r = uf.find(idx);
                    let e = deepest.entry(r).or_insert((0, idx));
                    if dist[idx] > e.0 {
                        *e = (dist[idx], idx);
                    }
                    if ix % stride == 0 && iy % stride == 0 && iz % stride == 0 {
                        sample_idx.push(idx);
                        owner.push(r);
                    }
                }
            }
        }
        let mut roots: Vec<usize> = deepest.keys().copied().collect();
        roots.sort_unstable();
        for &r in &roots {
            sample_idx.push(deepest.get(&r).unwrap().1);
            owner.push(r);
        }
        let sample_pts: Vec<[f64; 3]> = sample_idx.iter().map(|&c| center_of(c)).collect();
        if deadline_passed(deadline) {
            return CutVerdict::Timeout;
        }
        let ws = bvh.winding_points_par(&sample_pts);
        let mut comp_tag: HashMap<usize, i8> = HashMap::new();
        let mut comp_bad: HashSet<usize> = HashSet::new();
        for k in 0..sample_idx.len() {
            let r = owner[k];
            let t = tag(ws[k], border_of(sample_idx[k]));
            if t == 0 {
                // A cell inside the winding band carries no verdict and must
                // not by itself block the component, or else every component
                // that merely touches the surface would fall back.
                continue;
            }
            match comp_tag.get(&r) {
                None => {
                    comp_tag.insert(r, t);
                }
                Some(&prev) => {
                    if prev != t {
                        comp_bad.insert(r);
                    }
                }
            }
        }
        for r in &comp_bad {
            comp_tag.remove(r);
        }
        if prof {
            eprintln!(
                "[tamp]     components roots={} resolved={} samples={}",
                roots.len(),
                comp_tag.len(),
                sample_idx.len()
            );
        }
        // Resolved components: every cell shares the component tag.
        for idx in 0..total {
            if let Some(&t) = comp_tag.get(&uf.find(idx)) {
                cls[idx] = t;
            }
        }
        // Unresolved components: exact per-cell evaluation, one z-slab at a
        // time so the intra-cut deadline also bounds the fallback.
        for iz in 0..nz {
            if deadline_passed(deadline) {
                return CutVerdict::Timeout;
            }
            let mut pts: Vec<[f64; 3]> = Vec::new();
            let mut idxs: Vec<usize> = Vec::new();
            for iy in 0..ny {
                for ix in 0..nx {
                    let idx = cell_idx(ix, iy, iz);
                    if comp_tag.contains_key(&uf.find(idx)) {
                        continue;
                    }
                    pts.push(center_of(idx));
                    idxs.push(idx);
                }
            }
            if pts.is_empty() {
                continue;
            }
            let ws2 = bvh.winding_points_par(&pts);
            for (k, &idx) in idxs.iter().enumerate() {
                cls[idx] = tag(ws2[k], border_of(idx));
            }
        }
    }
    let winding_s = tw0.elapsed().as_secs_f64();

    let mut n_src = 0usize;
    let mut n_sink = 0usize;
    for idx in 0..total {
        match cls[idx] {
            1 => {
                g.add_tweights(idx, TERM, 0.0);
                n_src += 1;
            }
            2 => {
                g.add_tweights(idx, 0.0, TERM);
                n_sink += 1;
            }
            _ => {}
        }
    }

    // If the window cannot even see both an interior (source) and an exterior
    // (sink) cell, the cut has nothing to decide; the loop is treated as
    // unknown rather than "no hole".
    if n_src == 0 || n_sink == 0 {
        return CutVerdict::Inconclusive;
    }

    if clamped {
        return CutVerdict::Inconclusive;
    }
    let tm0 = Instant::now();
    let flow = g.maxflow();
    let flow_s = tm0.elapsed().as_secs_f64();
    if prof {
        eprintln!(
            "[tamp]     cut grid {}x{}x{}={} winding_s={:.3} edges_s={:.3} flow_s={:.3} flow={:.3e}",
            nx, ny, nz, total, winding_s, edges_s, flow_s, flow
        );
    }
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

/// Undirected edge-use multiplicities for a triangle soup.
fn edge_use_counts(tris: &[[u32; 3]]) -> HashMap<(u32, u32), i32> {
    let mut c: HashMap<(u32, u32), i32> = HashMap::with_capacity(tris.len() * 3);
    for t in tris {
        for &(a, b) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            if a == b {
                continue;
            }
            *c.entry(ekey2(a, b)).or_insert(0) += 1;
        }
    }
    c
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

/// Triangulate one boundary loop with rim vertices only.
///
/// Returns `Err(reason)` when the loop cannot be sealed cleanly
/// (triangulation failure, degenerate triangle, or inconsistent winding), in
/// which case the caller leaves the hole open. `allow_tie` accepts a
/// winding-orientation tie for small loops: the structural gate already
/// guarantees the plug's boundary is exactly the rim, and the caller's
/// edge-collision guard guarantees no edge gains a third face, so the
/// orientation alone cannot create a non-manifold edge.
fn triangulate_rim_loop(
    rim: &[u32],
    verts: &[[f64; 3]],
    directed_set: &HashSet<(u32, u32)>,
    allow_tie: bool,
) -> Result<Vec<[u32; 3]>, &'static str> {
    let m = rim.len();
    if m < 3 {
        return Err("too_small");
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
        return Err("dp_fail");
    }

    // The rim may be consistently or inconsistently wound (scans and legacy
    // fixtures). Both orientations share the same structure/areas; keep the
    // one that cancels the most input boundary edges, and require a strict
    // majority so a genuinely pinched loop is left open (unless `allow_tie`).
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
        return Err("degenerate");
    }
    let majority = cb.max(cf) * 2 > m;
    let tie_ok = allow_tie && cb == cf && cb > 0;
    if !majority && !tie_ok {
        return Err("winding");
    }
    Ok(if cf > cb { flipped } else { base })
}

/// Invisible-plug deviation gate. The local surface plane is the best-fit plane
/// of the rim itself (Newell normal anchored at the rim centroid); a plug is
/// accepted when every triangle centroid lies within `PLUG_MAX_DEV_FACTOR`
/// median edge lengths of that plane. This is what rejects a membrane that
/// bulges off a strongly non-planar (curved-surface) rim. A degenerate rim
/// normal cannot be judged and is accepted.
fn plug_deviation_ok(cand: &[[u32; 3]], rim: &[u32], verts: &[[f64; 3]], h: f64) -> bool {
    let pts: Vec<[f64; 3]> = rim.iter().map(|&v| verts[v as usize]).collect();
    let n0 = newell_normal(&pts);
    let nl = norm3(n0);
    if nl < 1e-20 {
        return true;
    }
    let n = [n0[0] / nl, n0[1] / nl, n0[2] / nl];

    let mut c0 = [0.0f64; 3];
    for p in &pts {
        c0[0] += p[0];
        c0[1] += p[1];
        c0[2] += p[2];
    }
    let inv = 1.0 / pts.len() as f64;
    c0 = [c0[0] * inv, c0[1] * inv, c0[2] * inv];

    let limit = PLUG_MAX_DEV_FACTOR * h;
    plug_within_plane(cand, verts, c0, n, limit)
}

/// True when every candidate triangle centroid is within `limit` of the plane
/// through `c0` with unit normal `n`.
fn plug_within_plane(
    cand: &[[u32; 3]],
    verts: &[[f64; 3]],
    c0: [f64; 3],
    n: [f64; 3],
    limit: f64,
) -> bool {
    for t in cand {
        let a = verts[t[0] as usize];
        let b = verts[t[1] as usize];
        let c = verts[t[2] as usize];
        let cen = [
            (a[0] + b[0] + c[0]) / 3.0,
            (a[1] + b[1] + c[1]) / 3.0,
            (a[2] + b[2] + c[2]) / 3.0,
        ];
        let d = (cen[0] - c0[0]) * n[0] + (cen[1] - c0[1]) * n[1] + (cen[2] - c0[2]) * n[2];
        if d.abs() > limit {
            return false;
        }
    }
    true
}

// ==========================================================================
// T3 reference surface ("laser"), overshoot trim and reference-following plug
// ==========================================================================

/// A fitted smooth "laser" reference surface over a hole, used to seal at the
/// level of the intact surrounding tissue (Arif's "line 2") instead of the rim
/// point closest to the centre ("line 1").
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct ReferenceSurface {
    origin: [f64; 3],
    u: [f64; 3],
    v: [f64; 3],
    n: [f64; 3],
    /// Height `h(u,v) = c0 + c1 u + c2 v + c3 u^2 + c4 u v + c5 v^2` along `n`.
    coeffs: [f64; 6],
    rms: f64,
    /// Sign of the mean curvature at the centre (concave < 0).
    concave: bool,
}

impl ReferenceSurface {
    fn eval(&self, u: f64, v: f64) -> f64 {
        let c = &self.coeffs;
        c[0] + c[1] * u + c[2] * v + c[3] * u * u + c[4] * u * v + c[5] * v * v
    }

    /// Signed distance of `p` along `n` from the reference surface.
    fn signed_height(&self, p: [f64; 3]) -> f64 {
        let d = [p[0] - self.origin[0], p[1] - self.origin[1], p[2] - self.origin[2]];
        dot3(d, self.n) - self.eval(dot3(d, self.u), dot3(d, self.v))
    }

    /// Project `p` onto the reference surface along `n` (an overshoot tail is
    /// lifted onto line 2 without changing its in-plane position).
    fn place(&self, p: [f64; 3]) -> [f64; 3] {
        let d = [p[0] - self.origin[0], p[1] - self.origin[1], p[2] - self.origin[2]];
        let su = dot3(d, self.u);
        let sv = dot3(d, self.v);
        let h = self.eval(su, sv);
        [
            self.origin[0] + su * self.u[0] + sv * self.v[0] + h * self.n[0],
            self.origin[1] + su * self.u[1] + sv * self.v[1] + h * self.n[1],
            self.origin[2] + su * self.u[2] + sv * self.v[2] + h * self.n[2],
        ]
    }
}

/// Vertex->face and edge->face adjacency of the input mesh, built once.
struct Adjacency {
    vert_faces: Vec<Vec<u32>>,
    edge_faces: HashMap<(u32, u32), Vec<u32>>,
    edge_mult: HashMap<(u32, u32), i32>,
}

fn build_adjacency(nverts: usize, tris: &[[u32; 3]]) -> Adjacency {
    let mut vert_faces: Vec<Vec<u32>> = vec![Vec::new(); nverts];
    let mut edge_faces: HashMap<(u32, u32), Vec<u32>> = HashMap::with_capacity(tris.len() * 3);
    let mut edge_mult: HashMap<(u32, u32), i32> = HashMap::with_capacity(tris.len() * 3);
    for (fi, t) in tris.iter().enumerate() {
        for &v in t {
            vert_faces[v as usize].push(fi as u32);
        }
        for &(a, b) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            if a == b {
                continue;
            }
            let e = ekey2(a, b);
            edge_faces.entry(e).or_default().push(fi as u32);
            *edge_mult.entry(e).or_insert(0) += 1;
        }
    }
    Adjacency { vert_faces, edge_faces, edge_mult }
}

/// Face normal (unnormalised) for a triangle.
fn face_normal(verts: &[[f64; 3]], t: &[u32; 3]) -> [f64; 3] {
    let a = verts[t[0] as usize];
    let b = verts[t[1] as usize];
    let c = verts[t[2] as usize];
    cross3([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]])
}

/// Build an orthonormal frame around unit normal `n`.
fn frame_from_normal(n: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let refv = if n[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
    let u0 = cross3(refv, n);
    let ul = norm3(u0).max(1e-20);
    let u = [u0[0] / ul, u0[1] / ul, u0[2] / ul];
    let v = cross3(n, u);
    (u, v)
}

/// Solve a small dense system by Gaussian elimination with partial pivoting.
fn solve6(mut a: [[f64; 6]; 6], mut b: [f64; 6]) -> Option<[f64; 6]> {
    for col in 0..6 {
        let mut piv = col;
        let mut best = a[col][col].abs();
        for r in (col + 1)..6 {
            if a[r][col].abs() > best {
                best = a[r][col].abs();
                piv = r;
            }
        }
        if best < 1e-18 {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        let d = a[col][col];
        for c in col..6 {
            a[col][c] /= d;
        }
        b[col] /= d;
        for r in 0..6 {
            if r != col {
                let f = a[r][col];
                if f != 0.0 {
                    for c in col..6 {
                        a[r][c] -= f * a[col][c];
                    }
                    b[r] -= f * b[col];
                }
            }
        }
    }
    Some(b)
}

/// Least-squares quadric fit over `(u,v,d)` samples with a ridge term.
fn fit_quadric(samples: &[(f64, f64, f64)]) -> Option<([f64; 6], f64)> {
    if samples.len() < REF_MIN_POINTS {
        return None;
    }
    let mut ata = [[0.0f64; 6]; 6];
    let mut atb = [0.0f64; 6];
    for &(u, v, d) in samples {
        let row = [1.0, u, v, u * u, u * v, v * v];
        for i in 0..6 {
            for j in 0..6 {
                ata[i][j] += row[i] * row[j];
            }
            atb[i] += row[i] * d;
        }
    }
    let tr: f64 = (0..6).map(|i| ata[i][i]).sum();
    let lam = REF_RIDGE * tr / 6.0 + 1e-12;
    for i in 0..6 {
        ata[i][i] += lam;
    }
    let c = solve6(ata, atb)?;
    let mut ss = 0.0;
    for &(u, v, d) in samples {
        let r = d - (c[0] + c[1] * u + c[2] * v + c[3] * u * u + c[4] * u * v + c[5] * v * v);
        ss += r * r;
    }
    Some((c, (ss / samples.len() as f64).sqrt()))
}

/// Fit the "laser" reference surface for one rim. `None` -> `no reference`
/// (the caller keeps the rim-only plug).
fn build_reference(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    rim: &[u32],
    h: f64,
    adj: &Adjacency,
) -> Option<ReferenceSurface> {
    let rim_pts: Vec<[f64; 3]> = rim.iter().map(|&v| verts[v as usize]).collect();
    let n0 = newell_normal(&rim_pts);
    let nl = norm3(n0);
    if nl < 1e-20 {
        return None;
    }
    let mut c = [0.0f64; 3];
    for p in &rim_pts {
        c[0] += p[0];
        c[1] += p[1];
        c[2] += p[2];
    }
    let inv = 1.0 / rim_pts.len().max(1) as f64;
    c = [c[0] * inv, c[1] * inv, c[2] * inv];

    let rim_set: HashSet<u32> = rim.iter().copied().collect();
    let mut dist: HashMap<u32, usize> = HashMap::new();
    let mut queue: VecDeque<u32> = VecDeque::new();
    for &v in rim {
        for &f in &adj.vert_faces[v as usize] {
            if dist.insert(f, 0).is_none() {
                queue.push_back(f);
            }
        }
    }
    while let Some(f) = queue.pop_front() {
        let d = dist[&f];
        if d >= REF_RING_MAX {
            continue;
        }
        let t = tris[f as usize];
        for &(a, b) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            if let Some(fs) = adj.edge_faces.get(&ekey2(a, b)) {
                for &nf in fs {
                    if let std::collections::hash_map::Entry::Vacant(e) = dist.entry(nf) {
                        e.insert(d + 1);
                        queue.push_back(nf);
                    }
                }
            }
        }
    }

    // Pass 1: collect the intact ring faces (edge-manifold, non-sliver).
    let mut ring_faces: Vec<(u32, [f64; 3], f64)> = Vec::new();
    for (&f, &d) in &dist {
        if d < REF_RING_MIN {
            continue;
        }
        let t = tris[f as usize];
        let mut bad = false;
        for &(a, b) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            if adj.edge_mult.get(&ekey2(a, b)).copied().unwrap_or(0) != 2 {
                bad = true;
                break;
            }
        }
        if bad {
            continue;
        }
        let area = tri_area(verts[t[0] as usize], verts[t[1] as usize], verts[t[2] as usize]);
        if area <= REF_SLIVER_AREA_FRAC * h * h {
            continue;
        }
        let fnrm = face_normal(verts, &t);
        if norm3(fnrm) < 1e-20 {
            continue;
        }
        ring_faces.push((f, fnrm, area));
    }
    if ring_faces.is_empty() {
        return None;
    }

    let mut points: Vec<[f64; 3]> = Vec::new();
    let mut point_normals: Vec<[f64; 3]> = Vec::new();
    let mut point_key: HashMap<u32, usize> = HashMap::new();
    let mut n_mean = [0.0f64; 3];
    let mut w_sum = 0.0;
    for &(f, fnrm, area) in &ring_faces {
        n_mean[0] += fnrm[0];
        n_mean[1] += fnrm[1];
        n_mean[2] += fnrm[2];
        w_sum += area;
        let t = tris[f as usize];
        for &v in &t {
            if rim_set.contains(&v) {
                continue;
            }
            let slot = *point_key.entry(v).or_insert_with(|| {
                points.push(verts[v as usize]);
                point_normals.push([0.0; 3]);
                points.len() - 1
            });
            point_normals[slot][0] += fnrm[0];
            point_normals[slot][1] += fnrm[1];
            point_normals[slot][2] += fnrm[2];
        }
    }
    if points.len() < REF_MIN_POINTS || w_sum <= 0.0 {
        return None;
    }

    let nm = norm3(n_mean);
    if nm < 1e-20 {
        return None;
    }
    let n = [n_mean[0] / nm, n_mean[1] / nm, n_mean[2] / nm];
    let (u, v) = frame_from_normal(n);

    let mut max_dev = 0.0f64;
    for pn in point_normals.iter() {
        let pl = norm3(*pn);
        if pl < 1e-20 {
            continue;
        }
        let pu = [pn[0] / pl, pn[1] / pl, pn[2] / pl];
        let cosang = dot3(pu, n).abs().clamp(-1.0, 1.0);
        let ang = cosang.acos().to_degrees();
        if ang > max_dev {
            max_dev = ang;
        }
    }
    if max_dev > REF_NORMAL_SPREAD_DEG {
        return None;
    }

    let mut rays: Vec<f64> = Vec::new();
    for (i, p) in points.iter().enumerate() {
        let pn = point_normals[i];
        let pl = norm3(pn);
        if pl < 1e-20 {
            continue;
        }
        let np = [pn[0] / pl, pn[1] / pl, pn[2] / pl];
        let cp = [c[0] - p[0], c[1] - p[1], c[2] - p[2]];
        let rin = [cp[0] - dot3(cp, n) * n[0], cp[1] - dot3(cp, n) * n[1], cp[2] - dot3(cp, n) * n[2]];
        let rl = norm3(rin);
        if rl < 1e-12 {
            continue;
        }
        let rin = [rin[0] / rl, rin[1] / rl, rin[2] / rl];
        let t = [rin[0] - dot3(rin, np) * np[0], rin[1] - dot3(rin, np) * np[1], rin[2] - dot3(rin, np) * np[2]];
        let tl = norm3(t);
        if tl < 0.2 {
            continue;
        }
        let t = [t[0] / tl, t[1] / tl, t[2] / tl];
        let b = dot3(t, n);
        let denom = 1.0 - b * b;
        if denom < 1e-6 {
            continue;
        }
        let a0 = [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
        let dd = dot3(t, a0);
        let ee = dot3(n, a0);
        let s = -(dd + b * ee) / denom;
        if s <= 0.0 {
            continue;
        }
        let w = -ee + b * s;
        if w.is_finite() {
            rays.push(w);
        }
    }
    if rays.len() < 3 {
        return None;
    }
    rays.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // Fit the quadric over the ring points; the fit predicts the hole-centre
    // height from the surrounding curvature.
    let mut samples: Vec<(f64, f64, f64)> = Vec::with_capacity(points.len());
    for p in &points {
        let d = [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
        samples.push((dot3(d, u), dot3(d, v), dot3(d, n)));
    }
    let (coeffs, rms) = fit_quadric(&samples)?;
    if rms > REF_RMS_FACTOR * h {
        return None;
    }

    // Laser consistency veto: each "laser" ray extends the tangent plane at a
    // ring point to the hole axis. For a bowl/dome/cylinder they all agree in
    // sign (the surface continues one way); a saddle makes opposite sides
    // predict opposite heights, so the sign of the residual to the fitted
    // centre height flips -> `no reference`.
    let c0 = coeffs[0];
    let tol = REF_RAY_SPREAD_FACTOR * h;
    let mut below = false;
    let mut above = false;
    for &w in &rays {
        let r = w - c0;
        if r < -tol {
            below = true;
        } else if r > tol {
            above = true;
        }
    }
    if below && above {
        return None;
    }

    let concave = (coeffs[3] + coeffs[5]) < 0.0;
    Some(ReferenceSurface { origin: c, u, v, n, coeffs, rms, concave })
}

/// Deviation gate against the reference surface.
fn reference_dev_ok(cand: &[[u32; 3]], refs: &ReferenceSurface, verts: &[[f64; 3]], h: f64) -> bool {
    let limit = REF_DEV_FACTOR * h;
    for t in cand {
        let a = verts[t[0] as usize];
        let b = verts[t[1] as usize];
        let cc = verts[t[2] as usize];
        let cen = [
            (a[0] + b[0] + cc[0]) / 3.0,
            (a[1] + b[1] + cc[1]) / 3.0,
            (a[2] + b[2] + cc[2]) / 3.0,
        ];
        if refs.signed_height(cen).abs() > limit {
            return false;
        }
    }
    true
}

/// Cancellation-count variant over a local triangulation whose extra interior
/// vertex index is beyond the rim; rim edges are matched by position in `rim`.
fn cancellation_count_local(
    tris: &[[u32; 3]],
    rim: &[u32],
    directed_set: &HashSet<(u32, u32)>,
) -> usize {
    let m = rim.len();
    let mut dir: HashSet<(u32, u32)> = HashSet::with_capacity(tris.len() * 3);
    for t in tris {
        dir.insert((t[0], t[1]));
        dir.insert((t[1], t[2]));
        dir.insert((t[2], t[0]));
    }
    let mut cancels = 0usize;
    for i in 0..m {
        let a = rim[i];
        let b = rim[(i + 1) % m];
        if a == b {
            continue;
        }
        let forward = i as u32;
        let back = ((i + 1) % m) as u32;
        if directed_set.contains(&(a, b)) {
            if dir.contains(&(back, forward)) {
                cancels += 1;
            }
        } else if directed_set.contains(&(b, a)) && dir.contains(&(forward, back)) {
            cancels += 1;
        }
    }
    cancels
}

/// Fan patch from a single interior vertex placed ON the reference surface.
/// Returns `(triangles, centre)` where the centre index is `rim.len()`.
fn fan_patch(
    rim: &[u32],
    refs: &ReferenceSurface,
    directed_set: &HashSet<(u32, u32)>,
) -> Option<(Vec<[u32; 3]>, [f64; 3])> {
    let m = rim.len();
    if m < 4 {
        return None;
    }
    let centre = refs.place(refs.origin);
    let cm = m as u32;
    let base: Vec<[u32; 3]> = (0..m).map(|i| [i as u32, ((i + 1) % m) as u32, cm]).collect();
    let flip: Vec<[u32; 3]> = base.iter().map(|t| [t[0], t[2], t[1]]).collect();
    let cb = cancellation_count_local(&base, rim, directed_set);
    let cf = cancellation_count_local(&flip, rim, directed_set);
    let majority = cb.max(cf) * 2 > m;
    let tie_ok = cb == cf && cb > 0;
    if !majority && !tie_ok {
        return None;
    }
    Some((if cf > cb { flip } else { base }, centre))
}

/// Count the vertex-connected components of a triangle mesh (union-find over
/// the three vertices of every face). Used to detect a "single closed-intended
/// shell": exactly one boundary loop on exactly one component is a genuine hole
/// in an otherwise-closed body, so the min-cut test -- whose only job is to
/// distinguish a hole from an intentional opening -- has nothing to decide and
/// can be skipped when a reference surface is available.
fn count_shell_components(nverts: usize, tris: &[[u32; 3]]) -> usize {
    fn find(parent: &mut [u32], mut x: u32) -> u32 {
        while parent[x as usize] != x {
            let gp = parent[parent[x as usize] as usize];
            parent[x as usize] = gp;
            x = gp;
        }
        x
    }
    let mut parent: Vec<u32> = (0..nverts as u32).collect();
    let mut used = vec![false; nverts];
    for t in tris {
        for &v in t {
            used[v as usize] = true;
        }
        let a = find(&mut parent, t[0]);
        for &v in &t[1..] {
            let b = find(&mut parent, v);
            if a != b {
                parent[b as usize] = a;
            }
        }
    }
    let mut roots: HashSet<u32> = HashSet::new();
    for v in 0..nverts {
        if used[v] {
            roots.insert(find(&mut parent, v as u32));
        }
    }
    roots.len()
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
    let loops = if directed.is_empty() {
        Vec::new()
    } else {
        build_boundary_loops(&directed)
    };
    // A closed, boundary-free input has no holes to plug.  It can still carry
    // residual self-intersections, so the melt pass below must still run;
    // only skip entirely when melting is disabled too.
    if loops.is_empty() && !config.melt_si {
        info.reason = "no_holes".to_string();
        info.seconds = t0.elapsed().as_secs_f64();
        return (verts.to_vec(), tris.to_vec(), info);
    }

    let directed_set: HashSet<(u32, u32)> = directed.iter().cloned().collect();

    // A single boundary loop on a single connected component is a genuine hole
    // in an otherwise-closed shell: no other loop/component could be an
    // intentional opening, so the min-cut can be skipped when a reference
    // surface is available (lead decision, 10 Oct).
    let single_shell =
        loops.len() == 1 && count_shell_components(verts.len(), tris) == 1;

    let h = match config.h_min {
        Some(val) if val > 1e-6 => val,
        _ => median_edge_length(verts, tris),
    };

    let tris_usize: Vec<[usize; 3]> = tris
        .iter()
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    let t_bvh0 = Instant::now();
    let bvh = MeshBvh::from_arrays(verts, &tris_usize);
    let bvh_s = t_bvh0.elapsed().as_secs_f64();
    let profile = std::env::var("SUTURA_TAMP_PROFILE").is_ok();
    let mut cut_s = 0.0f64;
    let mut plug_s = 0.0f64;
    let mut cut_cells_total = 0usize;
    if profile {
        eprintln!(
            "[tamp] faces={} loops={} bvh_s={:.3}",
            tris.len(),
            loops.len(),
            bvh_s
        );
    }

    let mut model_box = BBox::empty();
    for v in verts {
        model_box.expand_point(*v);
    }
    let model_diag = model_box.diagonal().max(h);

    let mut out_verts = verts.to_vec();
    let mut out_tris = tris.to_vec();
    let mut plug_count = 0usize;
    let mut plug_rejected_count = 0usize;
    let mut plug_inconclusive_count = 0usize;
    let mut plug_rejected_size_count = 0usize;
    let mut plug_rejected_dev_count = 0usize;
    let mut plug_face_count = 0usize;

    // Input adjacency for the T3 reference-surface fit (vertex/edge -> faces).
    let adj = if loops.is_empty() { None } else { Some(build_adjacency(verts.len(), tris)) };

    // Running undirected edge multiplicities, so a plug can be rejected before
    // it pushes any edge past two faces (i.e. manufactures a non-manifold
    // edge). Pre-existing non-manifold edges of the input are preserved.
    let mut edge_count = edge_use_counts(&out_tris);

    let deadline = config
        .time_budget_s
        .map(|b| t0 + Duration::from_secs_f64(b.max(0.0)));
    // Debug switch that restores the per-cell winding evaluation, for
    // byte-identical regression comparison against the per-component path.
    let percell = std::env::var("SUTURA_TAMP_PERCELL").is_ok();

    for loop_v in &loops {
        if deadline_passed(deadline) {
            info.reason = "timeout exceeded".to_string();
            break;
        }

        let mut rim: Vec<u32> = loop_v.clone();
        rim.dedup();
        if rim.len() >= 2 && rim.first() == rim.last() {
            rim.pop();
        }
        if rim.len() < 3 {
            continue;
        }

        // Invisible-plug gate (a): a rim larger than `PLUG_MAX_LOOP_DIAM_FACTOR`
        // median edges cannot be closed without a visible membrane, so it is
        // left for the surface-based tiers. Checked before the min-cut so an
        // oversized rim costs nothing.
        let mut rim_box = BBox::empty();
        for &v in &rim {
            rim_box.expand_point(verts[v as usize]);
        }
        let rim_diag = rim_box.diagonal();
        if profile {
            eprintln!(
                "[tamp]   loop m={} rim_diag_h={:.1}",
                rim.len(),
                rim_diag / h.max(1e-12)
            );
        }
        if rim_diag > PLUG_MAX_LOOP_DIAM_FACTOR * h {
            info.loop_diag
                .push(format!("m={} rejected:too_large", rim.len()));
            plug_rejected_size_count += 1;
            continue;
        }

        // The min-cut decides only WHETHER this loop is a real hole (its
        // throat separates interior from exterior); it never places a vertex.
        // A loop small enough that the coarse cut grid cannot resolve it is
        // treated as a hole without the test. An inconclusive cut is also
        // treated as a hole (seal with the safe rim plug) rather than rejected,
        // matching R1 usefulness.
        let tiny = rim.len() <= TINY_LOOP_MAX;

        // Single-shell fast path: build the reference first; when a single
        // boundary loop sits on a single connected component it is a genuine
        // hole, so a fitted reference surface decides the seal and the expensive
        // min-cut is skipped entirely. Multi-loop / multi-component inputs keep
        // the cut (another loop could be an intentional opening).
        let mut reference = if single_shell {
            adj.as_ref()
                .and_then(|a| build_reference(verts, tris, &rim, h, a))
        } else {
            None
        };
        let mincut_skipped = !tiny && single_shell && reference.is_some();

        // Per-loop allowance, clamped by the global preset deadline. Only
        // applied when a budget is set (`None` = unlimited, e.g. direct library
        // calls) so the engine default is unchanged.
        let loop_deadline = match deadline {
            Some(global) => {
                let allow =
                    LOOP_BUDGET_BASE_S + LOOP_BUDGET_PER_VERTEX_S * rim.len() as f64;
                let local = Instant::now() + Duration::from_secs_f64(allow);
                Some(if local < global { local } else { global })
            }
            None => None,
        };

        if !tiny && !mincut_skipped {
            let tc0 = Instant::now();
            let verdict =
                throat_separates(verts, &rim, h, model_diag, &bvh, config, percell, loop_deadline);
            let dt = tc0.elapsed().as_secs_f64();
            cut_s += dt;
            if profile {
                cut_cells_total += throat_cells(&rim, verts, h);
                eprintln!(
                    "[tamp]   loop m={} cut {:?} cut_s={:.3}",
                    rim.len(),
                    verdict,
                    dt
                );
            }
            match verdict {
                CutVerdict::NoSeparation => {
                    info.loop_diag
                        .push(format!("m={} rejected:no_separation", rim.len()));
                    plug_rejected_count += 1;
                    continue;
                }
                CutVerdict::Timeout => {
                    // The loop's own allowance expired. If the global preset
                    // deadline is also gone, stop; otherwise leave this loop
                    // open and try the next one.
                    if deadline_passed(deadline) {
                        info.reason = "timeout exceeded".to_string();
                        break;
                    }
                    info.loop_diag
                        .push(format!("m={} rejected:loop_timeout", rim.len()));
                    continue;
                }
                CutVerdict::Inconclusive => {
                    plug_inconclusive_count += 1;
                }
                CutVerdict::Separates => {}
            }
        } else if mincut_skipped && profile {
            eprintln!(
                "[tamp]   loop m={} mincut skipped (single shell + reference)",
                rim.len()
            );
        }

        let tp0 = Instant::now();
        // T3: fit a smooth "laser" reference surface (line 2) through the intact
        // ring 2..=3 tissue. When it is available, seal at the reference level —
        // a concave dent and a convex bump alike; only a saddle / noisy ring
        // yields no reference (`build_reference` -> None) and keeps the rim-only
        // plug. Already built above on the single-shell fast path.
        if reference.is_none() && !single_shell {
            reference = adj
                .as_ref()
                .and_then(|a| build_reference(verts, tris, &rim, h, a));
        }
        let mut sealed = false;
        if let Some(ref refs) = reference {
            // Overshoot trim: lift tail rim vertices that sit clearly below
            // the reference (the extruded spike that overhangs the hole).
            let mut trimmed = 0usize;
            let mut moved: HashSet<u32> = HashSet::new();
            for &v in &rim {
                if trimmed >= TRIM_MAX_VERTICES {
                    break;
                }
                let p = out_verts[v as usize];
                if refs.signed_height(p) < -TRIM_EPS_FACTOR * h {
                    out_verts[v as usize] = refs.place(p);
                    if moved.insert(v) {
                        trimmed += 1;
                    }
                }
            }
            info.trimmed_vertices += trimmed;

            // Candidate 1: rim-only DP on the (possibly trimmed) rim.
            let mut cand: Option<Vec<[u32; 3]>> = None;
            let mut added: Vec<[f64; 3]> = Vec::new();
            if let Ok(dp) = triangulate_rim_loop(&rim, &out_verts, &directed_set, tiny) {
                if reference_dev_ok(&dp, refs, &out_verts, h) {
                    cand = Some(dp);
                }
            }
            // Candidate 2: a fan to a single interior vertex ON the reference
            // surface (handles concave craters and convex bumps a rim DP cannot
            // represent without bulging / flattening).
            if cand.is_none() {
                if let Some((fan_local, centre)) = fan_patch(&rim, refs, &directed_set) {
                    let new_idx = out_verts.len() as u32;
                    let m = rim.len() as u32;
                    let fan: Vec<[u32; 3]> = fan_local
                        .iter()
                        .map(|t| {
                            let f = |x: u32| if x == m { new_idx } else { rim[x as usize] };
                            [f(t[0]), f(t[1]), f(t[2])]
                        })
                        .collect();
                    added.push(centre);
                    cand = Some(fan);
                }
            }

            if let Some(cand) = cand {
                let mut verts_ext = out_verts.clone();
                verts_ext.extend_from_slice(&added);
                let dev_ok = reference_dev_ok(&cand, refs, &verts_ext, h);
                let cand_counts = edge_use_counts(&cand);
                let collides = cand_counts
                    .iter()
                    .any(|(e, &d)| edge_count.get(e).copied().unwrap_or(0) + d > 2);
                if !dev_ok {
                    info.loop_diag
                        .push(format!("m={} rejected:deviation", rim.len()));
                    plug_rejected_dev_count += 1;
                    info.reference_fallback += 1;
                } else if collides {
                    info.loop_diag
                        .push(format!("m={} rejected:edge_collision", rim.len()));
                    plug_rejected_count += 1;
                    info.reference_fallback += 1;
                } else {
                    for (e, &d) in &cand_counts {
                        *edge_count.entry(*e).or_insert(0) += d;
                    }
                    let n_added = added.len();
                    out_verts.extend_from_slice(&added);
                    info.patch_vertices += n_added;
                    plug_face_count += cand.len();
                    out_tris.extend(cand);
                    plug_count += 1;
                    info.reference_used += 1;
                    sealed = true;
                    info.loop_diag.push(format!(
                        "m={} sealed:ref{}",
                        rim.len(),
                        if n_added > 0 { ":fan" } else { "" }
                    ));
                }
            } else {
                info.reference_fallback += 1;
            }
        } else {
            info.reference_fallback += 1;
        }

        // Fallback: rim-only plug (byte-identical to the pre-T3 path).
        if !sealed && reference.is_none() {
            let tri = triangulate_rim_loop(&rim, verts, &directed_set, tiny);
            match tri {
                Ok(cand) => {
                    if !plug_deviation_ok(&cand, &rim, verts, h) {
                        info.loop_diag
                            .push(format!("m={} rejected:deviation", rim.len()));
                        plug_rejected_dev_count += 1;
                    } else {
                        let cand_counts = edge_use_counts(&cand);
                        let collides = cand_counts
                            .iter()
                            .any(|(e, &d)| edge_count.get(e).copied().unwrap_or(0) + d > 2);
                        if collides {
                            info.loop_diag
                                .push(format!("m={} rejected:edge_collision", rim.len()));
                            plug_rejected_count += 1;
                        } else {
                            for (e, &d) in &cand_counts {
                                *edge_count.entry(*e).or_insert(0) += d;
                            }
                            plug_face_count += cand.len();
                            out_tris.extend(cand);
                            plug_count += 1;
                            info.loop_diag.push(format!(
                                "m={} sealed{}",
                                rim.len(),
                                if tiny { ":tiny" } else { "" }
                            ));
                        }
                    }
                }
                Err(why) => {
                    info.loop_diag
                        .push(format!("m={} rejected:{}", rim.len(), why));
                    plug_rejected_count += 1;
                }
            }
        } else if !sealed && reference.is_some() {
            // A reference existed but neither candidate was acceptable; fall
            // back to the rim-only plug so a legitimate hole is still sealed.
            let tri = triangulate_rim_loop(&rim, verts, &directed_set, tiny);
            match tri {
                Ok(cand) => {
                    if !plug_deviation_ok(&cand, &rim, verts, h) {
                        info.loop_diag
                            .push(format!("m={} rejected:deviation", rim.len()));
                        plug_rejected_dev_count += 1;
                    } else {
                        let cand_counts = edge_use_counts(&cand);
                        let collides = cand_counts
                            .iter()
                            .any(|(e, &d)| edge_count.get(e).copied().unwrap_or(0) + d > 2);
                        if collides {
                            info.loop_diag
                                .push(format!("m={} rejected:edge_collision", rim.len()));
                            plug_rejected_count += 1;
                        } else {
                            for (e, &d) in &cand_counts {
                                *edge_count.entry(*e).or_insert(0) += d;
                            }
                            plug_face_count += cand.len();
                            out_tris.extend(cand);
                            plug_count += 1;
                            info.loop_diag.push(format!(
                                "m={} sealed:fallback",
                                rim.len()
                            ));
                        }
                    }
                }
                Err(why) => {
                    info.loop_diag
                        .push(format!("m={} rejected:{}", rim.len(), why));
                    plug_rejected_count += 1;
                }
            }
        }
        let plug_dt = tp0.elapsed().as_secs_f64();
        plug_s += plug_dt;
        if profile {
            eprintln!("[tamp]   loop m={} plug_s={:.3}", rim.len(), plug_dt);
        }
    }

    info.plugs = plug_count;
    info.plugs_rejected = plug_rejected_count;
    info.plugs_inconclusive = plug_inconclusive_count;
    info.plugs_rejected_size = plug_rejected_size_count;
    info.plugs_rejected_dev = plug_rejected_dev_count;
    info.plug_faces = plug_face_count;

    // L2b: melt residual self-intersection folds, inside-out, after the hole
    // plugs are in place.  All logic lives in `melt.rs`; this hook only carries
    // the counters back.  A no-op when melting is disabled or there is no SI.
    if config.melt_si {
        let melt_params = crate::melt::MeltParams::default();
        let m = crate::melt::melt_si(
            &out_verts,
            &out_tris,
            h,
            model_diag,
            config.time_budget_s,
            &melt_params,
        );
        out_verts = m.verts;
        out_tris = m.tris;
        info.melt_layers_tried = m.layers_tried;
        info.melt_layers_accepted = m.layers_accepted;
        info.melt_layers_rolled_back = m.layers_rolled_back;
        info.micro_welds_ok = m.micro_welds_ok;
        info.micro_welds_failed = m.micro_welds_failed;
        info.excised_faces = m.excised_faces;
        info.healthy_faces_kept = info.healthy_faces_kept.saturating_sub(m.excised_faces);
        if !m.reason.is_empty() {
            info.reason = m.reason;
        }
    }

    if info.reason.is_empty() {
        info.reason = if loops.is_empty() {
            "no_holes".to_string()
        } else {
            "ok".to_string()
        };
    }
    info.seconds = t0.elapsed().as_secs_f64();
    if profile {
        eprintln!(
            "[tamp] total_s={:.3} bvh_s={:.3} cut_s={:.3} ({} cells) plug_s={:.3} plugs={} plug_faces={} merged_s={:.3}",
            info.seconds,
            bvh_s,
            cut_s,
            cut_cells_total,
            plug_s,
            plug_count,
            plug_face_count,
            info.seconds - bvh_s - cut_s - plug_s
        );
    }
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
        let tris = triangulate_rim_loop(&rim, &verts, &directed_set, false)
            .expect("slot must seal");
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
    fn test_boundary_loops_disjoint_squares() {
        // Two disjoint square rims must decompose into two 4-loops and must not
        // be merged: the half-edges are emitted block-stacked, matching the
        // Python verifier's `defects._boundary_edges`.
        let tris = vec![[0u32, 1, 2], [0, 2, 3], [4, 5, 6], [4, 6, 7]];
        let directed = directed_boundary_edges(&tris);
        assert_eq!(
            directed,
            vec![(0, 1), (4, 5), (1, 2), (2, 3), (5, 6), (6, 7), (3, 0), (7, 4)]
        );
        let loops = build_boundary_loops(&directed);
        assert_eq!(loops, vec![vec![0u32, 1, 2, 3], vec![4u32, 5, 6, 7]]);
    }

    #[test]
    fn test_boundary_loops_shared_vertex_fixture() {
        // Two rims meeting at the shared vertices 0 and 3. The old edge-marking
        // walker over the per-face half-edge order merged them into a single
        // 7-vertex loop; the block-stacked order plus the vertex-marking walker
        // (a port of the Python verifier's `defects._boundary_loops`) returns
        // this decomposition. Regression for the thingi10k_100827 hole mismatch
        // where the kernel's loops did not match `defects.detect`.
        let tris = vec![[1u32, 3, 2], [0, 3, 4], [0, 1, 2]];
        let directed = directed_boundary_edges(&tris);
        assert_eq!(
            directed,
            vec![(1, 3), (0, 3), (0, 1), (3, 2), (3, 4), (4, 0), (2, 0)]
        );
        let loops = build_boundary_loops(&directed);
        assert_eq!(loops, vec![vec![1u32, 3, 0]]);
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

    #[test]
    fn test_oversized_loop_rejected_by_size_gate() {
        // The open-top cube's rim is ~1.4 across; with h_min = 0.001 the
        // size limit (120 h = 0.12) is far below the rim, so the loop is left
        // for the surface-based tiers without ever building the min-cut.
        let (verts, tris) = unit_cube_open_top();
        let config = TamponadeConfig { h_min: Some(0.001), ..Default::default() };
        let (_out_verts, out_tris, info) = tamponade_repair(&verts, &tris, &config);
        assert_eq!(info.plugs, 0);
        assert_eq!(info.plugs_rejected_size, 1);
        assert_eq!(info.plug_faces, 0);
        assert_eq!(out_tris, tris);
    }

    #[test]
    fn test_deviation_gate_rejects_proud_plug() {
        let h = 1.0;
        let verts: Vec<[f64; 3]> = vec![
            [0.0, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            [10.0, 10.0, 0.0],
            [0.0, 0.0, 15.0],
        ];
        let c0 = [0.0, 0.0, 0.0];
        let n = [0.0, 0.0, 1.0];
        // Coplanar triangle -> accepted.
        let flat: Vec<[u32; 3]> = vec![[0, 1, 2]];
        assert!(plug_within_plane(&flat, &verts, c0, n, PLUG_MAX_DEV_FACTOR * h));
        // Triangle whose centroid is 5 h off the plane -> rejected.
        let proud: Vec<[u32; 3]> = vec![[0, 1, 3]];
        assert!(!plug_within_plane(&proud, &verts, c0, n, PLUG_MAX_DEV_FACTOR * h));
    }

    #[test]
    fn test_component_winding_matches_per_cell() {
        // The per-component winding classification must equal the per-cell
        // evaluation: same cut verdict for several fixtures and rims. Runs both
        // paths explicitly (no env) so it is immune to test-suite ordering.
        let (cube_v, cube_t) = unit_cube_open_top();
        let (solid_v, solid_t) = unit_solid_cube();
        let (sphere_v, sphere_t) = unit_sphere();
        let fixtures: Vec<(Vec<[f64; 3]>, Vec<[u32; 3]>, Vec<u32>)> = vec![
            (cube_v, cube_t, vec![4, 5, 6, 7]),
            (solid_v, solid_t, vec![4, 5, 6, 7]),
            (sphere_v, sphere_t, vec![0, 1, 2, 3, 4, 5]),
        ];
        let config = TamponadeConfig::default();
        for (verts, tris, rim) in fixtures {
            let idx: Vec<[usize; 3]> = tris
                .iter()
                .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
                .collect();
            let bvh = MeshBvh::from_arrays(&verts, &idx);
            for &h in &[0.05_f64, 0.2_f64] {
                let comp = throat_separates(&verts, &rim, h, 1.0, &bvh, &config, false, None);
                let percell = throat_separates(&verts, &rim, h, 1.0, &bvh, &config, true, None);
                assert_eq!(
                    comp, percell,
                    "component vs per-cell verdict differ (h={}, rim={:?})",
                    h, rim
                );
            }
        }
    }

    // ---- T3 reference-surface ("laser") tests ----------------------------

    /// A curved (or flat) `n x n` grid over [-1,1]^2 with a circular central
    /// hole, `z = a (x^2 + y^2)` (`a > 0` concave bowl, `a = 0` flat).
    fn grid_with_central_hole(n: usize, a: f64) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let step = 2.0 / n as f64;
        let mut verts = Vec::new();
        for j in 0..=n {
            for i in 0..=n {
                let x = -1.0 + i as f64 * step;
                let y = -1.0 + j as f64 * step;
                verts.push([x, y, a * (x * x + y * y)]);
            }
        }
        let idx = |i: usize, j: usize| (j * (n + 1) + i) as u32;
        let mut tris = Vec::new();
        for j in 0..n {
            for i in 0..n {
                let cx = -1.0 + (i as f64 + 0.5) * step;
                let cy = -1.0 + (j as f64 + 0.5) * step;
                if (cx * cx + cy * cy).sqrt() < 0.32 {
                    continue;
                }
                let a0 = idx(i, j);
                let b = idx(i + 1, j);
                let c = idx(i + 1, j + 1);
                let d = idx(i, j + 1);
                tris.push([a0, b, c]);
                tris.push([a0, c, d]);
            }
        }
        (verts, tris)
    }

    fn boundary_edge_count_near(tris: &[[u32; 3]], verts: &[[f64; 3]], r: f64) -> usize {
        let c = [0.0, 0.0, 0.0];
        directed_boundary_edges(tris)
            .iter()
            .filter(|(a, b)| {
                let pa = verts[*a as usize];
                let pb = verts[*b as usize];
                let da = ((pa[0] - c[0]).powi(2) + (pa[1] - c[1]).powi(2)).sqrt();
                let db = ((pb[0] - c[0]).powi(2) + (pb[1] - c[1]).powi(2)).sqrt();
                da < r && db < r
            })
            .count()
    }

    #[test]
    fn test_concave_crater_sealed_with_reference() {
        let (verts, tris) = grid_with_central_hole(16, 1.0);
        let before_hole = boundary_edge_count_near(&tris, &verts, 0.6);
        assert!(before_hole > 0, "fixture must have a central hole boundary");
        let config = TamponadeConfig { h_min: Some(0.02), ..Default::default() };
        let (out_verts, out_tris, info) = tamponade_repair(&verts, &tris, &config);
        // The fitted reference was used and the central hole is gone.
        assert!(info.reference_used >= 1, "reference not used: {info:?}");
        assert_eq!(boundary_edge_count_near(&out_tris, &out_verts, 0.6), 0);
        assert_eq!(boundary_edge_count(&out_tris), boundary_edge_count(&tris) - before_hole);
    }

    #[test]
    fn test_overshoot_tail_is_trimmed() {
        let (mut verts, tris) = grid_with_central_hole(16, 1.0);
        // Push one rim vertex far below the reference (an extruded tail).
        // The central-hole rim vertices are those with an incident boundary
        // edge; find the first and lower it by 0.5.
        let bd = directed_boundary_edges(&tris);
        let rim_v = {
            let mut best: Option<u32> = None;
            for (a, b) in &bd {
                for v in [*a, *b] {
                    let p = verts[v as usize];
                    if p[0].abs() < 0.5 && p[1].abs() < 0.5 {
                        best = Some(v);
                    }
                }
            }
            best.expect("central rim vertex")
        };
        verts[rim_v as usize][2] -= 0.5;
        let config = TamponadeConfig { h_min: Some(0.02), ..Default::default() };
        let (_out_v, _out_t, info) = tamponade_repair(&verts, &tris, &config);
        assert!(info.reference_used >= 1, "reference not used: {info:?}");
        assert!(info.trimmed_vertices >= 1, "tail not trimmed: {info:?}");
    }

    #[test]
    fn test_convex_bump_sealed_with_reference() {
        // Convex dome (`z = -r^2`) with the hole at its peak: the intact
        // surrounding tissue rises toward the hole, so the reference surface
        // (line 2) extrapolates to the peak. A convex hole must be sealed on
        // that reference too -- exactly like a concave dent -- not bridged by a
        // flat rim-only cap below the bump, and a lowered tail rim vertex is
        // trimmed onto the reference (no fin left in the air).
        let (mut verts, tris) = grid_with_central_hole(16, -1.0);
        let bd = directed_boundary_edges(&tris);
        let mut rim_vs: HashSet<u32> = HashSet::new();
        for (a, b) in &bd {
            for v in [*a, *b] {
                let p = verts[v as usize];
                if p[0].abs() < 0.6 && p[1].abs() < 0.6 {
                    rim_vs.insert(v);
                }
            }
        }
        assert!(!rim_vs.is_empty(), "fixture must have a central hole rim");
        // Overshoot tail: lower one rim vertex well below the reference.
        let tail_v = *rim_vs.iter().min().unwrap();
        verts[tail_v as usize][2] -= 0.5;

        let config = TamponadeConfig { h_min: Some(0.02), ..Default::default() };
        let (out_v, out_t, info) = tamponade_repair(&verts, &tris, &config);

        assert!(info.reference_used >= 1, "convex bump must use the reference: {info:?}");
        assert!(info.trimmed_vertices >= 1, "overshoot tail not trimmed: {info:?}");
        assert_eq!(boundary_edge_count_near(&out_t, &out_v, 0.6), 0);

        // No flat cap below the bump: a repaired vertex inside the former hole
        // reaches the reference peak (z ~ 0), not just the rim-chord height.
        let peak = out_v
            .iter()
            .filter(|p| p[0].abs() < 0.32 && p[1].abs() < 0.32)
            .map(|p| p[2])
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(peak > -0.05, "hole capped below the bump (peak z={peak})");
    }

    #[test]
    fn test_saddle_has_no_reference() {
        // Saddle z = x y: opposite tangent rays meet at opposite heights, so
        // the reference is rejected and the rim-only plug is used.
        let n = 16usize;
        let step = 2.0 / n as f64;
        let mut verts = Vec::new();
        for j in 0..=n {
            for i in 0..=n {
                let x = -1.0 + i as f64 * step;
                let y = -1.0 + j as f64 * step;
                verts.push([x, y, x * y]);
            }
        }
        let idx = |i: usize, j: usize| (j * (n + 1) + i) as u32;
        let mut tris = Vec::new();
        for j in 0..n {
            for i in 0..n {
                let cx = -1.0 + (i as f64 + 0.5) * step;
                let cy = -1.0 + (j as f64 + 0.5) * step;
                if (cx * cx + cy * cy).sqrt() < 0.32 {
                    continue;
                }
                let a0 = idx(i, j);
                let b = idx(i + 1, j);
                let c = idx(i + 1, j + 1);
                let d = idx(i, j + 1);
                tris.push([a0, b, c]);
                tris.push([a0, c, d]);
            }
        }
        let config = TamponadeConfig { h_min: Some(0.02), ..Default::default() };
        let (_v, _t, info) = tamponade_repair(&verts, &tris, &config);
        assert_eq!(info.reference_used, 0, "saddle must fall back: {info:?}");
        assert!(info.reference_fallback >= 1);
    }
}
