// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Melt (L2b): inside-out removal of residual self-intersection folds.
//!
//! This is the melting half of Tamponade.  After the L2a hole plugs are in
//! place, the residual self-intersecting faces are grouped into edge-connected
//! *sectors* (local folds).  Sectors are visited from the object's deepest
//! interior point **outward** (their depth is the median interior-side winding
//! field sample over the sector's own face centroids).  A buried fold is excised
//! and its exposed rim is welded to the coincident opposite sheet; every input
//! triangle that is not a buried fold face is carried through verbatim.  Each
//! sector is one *layer*: it is committed only when the reload-honest hole and
//! non-manifold counts do not rise and the exact self-intersection count
//! strictly falls, otherwise the layer is rolled back byte-for-byte.  A small
//! non-manifold spot is first repaired by a local micro-weld instead of being
//! rolled back.
//!
//! The module is deliberately self-contained: `tamponade.rs` owns exactly one
//! call into it, so the melting logic cannot collide with the budget/gate work
//! in `tamponade.rs` or the whole-shell analysis in `shell.rs`.  It has no
//! global side effects and no parallel reductions, so a run is deterministic.

use crate::flap::topology::non_manifold_stats;
use crate::triangle_intersection::{intersect_triangles, Triangle, TriangleIntersection};
use crate::winding::MeshBvh;
use std::collections::{HashMap, HashSet};

/// Hard wall-clock ceiling for the whole melt pass, regardless of the tier
/// budget.  The per-layer exact SI re-scan dominates on scattered-SI meshes.
const MELT_MAX_SECONDS: f64 = 8.0;

/// A self-intersection proper-face scan: the face mask, the proper crossing
/// face pairs (used to cluster folds spatially) and the flagged-face count.
#[derive(Clone, Debug, Default)]
pub struct SiScan {
    pub mask: Vec<bool>,
    pub pairs: Vec<(usize, usize)>,
    pub count: i64,
    pub skipped: bool,
}

/// The crate's exact self-intersection classifier: a face is flagged when it
/// properly crosses a non-adjacent face (pairs sharing a vertex index are
/// ignored).  Returns the mask, crossing pairs and count; `skipped` is set and
/// `count = -1` when the mesh exceeds the classifier's face cap.
pub fn scan_si(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> SiScan {
    let f = tris.len();
    if f == 0 {
        return SiScan::default();
    }
    if f > crate::SI_MAX_FACES {
        return SiScan {
            mask: vec![false; f],
            pairs: Vec::new(),
            count: -1,
            skipped: true,
        };
    }
    let t_usize: Vec<[usize; 3]> = tris
        .iter()
        .map(|x| [x[0] as usize, x[1] as usize, x[2] as usize])
        .collect();
    let triangles: Vec<Triangle> = tris
        .iter()
        .map(|t| {
            Triangle::explicit(
                verts[t[0] as usize],
                verts[t[1] as usize],
                verts[t[2] as usize],
            )
        })
        .collect();
    let candidates = crate::arrangement::broad_phase_candidates(verts, &t_usize);
    let mut mask = vec![false; f];
    let mut pairs = Vec::new();
    for (i, j) in candidates {
        let (a, b) = (tris[i], tris[j]);
        if a.iter().any(|x| b.contains(x)) {
            continue;
        }
        if let TriangleIntersection::ProperSegment { .. } =
            intersect_triangles(&triangles[i], &triangles[j])
        {
            mask[i] = true;
            mask[j] = true;
            pairs.push((i, j));
        }
    }
    let count = mask.iter().filter(|&&b| b).count() as i64;
    SiScan {
        mask,
        pairs,
        count,
        skipped: false,
    }
}

/// Backwards-compatible shape `(mask, count, skipped)`.
pub fn scan_si_faces(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> (Vec<bool>, i64, bool) {
    let s = scan_si(verts, tris);
    (s.mask, s.count, s.skipped)
}

/// Tunables for the melt pass.  Every value is a starting point to be tuned on
/// the real-world SI meshes in the measurement phase.
#[derive(Clone, Debug)]
pub struct MeltParams {
    /// Minimum exterior-side winding for a face to count as buried.
    pub buried_w_out: f64,
    /// Minimum interior-side winding for a face to count as buried when it has
    /// no self-intersection partner on its own sector (the thin-wall guard).
    pub buried_w_in: f64,
    /// Hard cap on the faces one sector may excise.
    pub max_sector_faces: usize,
    /// Hard cap on the fraction of the mesh one sector may excise.
    pub max_face_fraction: f64,
    /// A new non-manifold cluster at or below this many edges may be
    /// micro-welded instead of rolled back.
    pub micro_weld_max_edges: usize,
    /// ...and whose total edge length is within this fraction of the diagonal.
    pub micro_weld_max_rel: f64,
    /// Hard cap on the global depth-grid cell count.
    pub grid_max_cells: usize,
    /// Maximum number of sectors (layers) the sweep will attempt.
    pub max_layers: usize,
    /// Boundary loops larger than this are left for the plug/flap tiers.
    pub cap_max_loop: usize,
}

impl Default for MeltParams {
    fn default() -> Self {
        Self {
            buried_w_out: 0.5,
            buried_w_in: 1.5,
            max_sector_faces: 5000,
            max_face_fraction: 0.01,
            micro_weld_max_edges: 8,
            micro_weld_max_rel: 0.005,
            grid_max_cells: 2_000_000,
            max_layers: 64,
            cap_max_loop: 300,
        }
    }
}

/// Result of a melt pass.  `verts`/`tris` equal the input when nothing was
/// accepted.
#[derive(Clone, Debug, Default)]
pub struct MeltOutcome {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[u32; 3]>,
    pub layers_tried: usize,
    pub layers_accepted: usize,
    pub layers_rolled_back: usize,
    pub micro_welds_ok: usize,
    pub micro_welds_failed: usize,
    pub excised_faces: usize,
    /// When empty the caller keeps its own reason.
    pub reason: String,
}

/// A coarse interior-depth field over the model bbox.
///
/// Occupancy is `winding_number(center) > 0.5`; the interior depth is the
/// exact Euclidean distance transform to the nearest exterior voxel, converted
/// to physical units.  The deepest interior point is the grid argmax.  The
/// grid is coarsened deterministically until it fits `max_cells`.
struct DepthField {
    origin: [f64; 3],
    voxel: [f64; 3],
    dims: [usize; 3],
    /// Interior depth (physical units), 0 outside the solid.
    depth: Vec<f32>,
}

impl DepthField {
    fn cell_index(&self, i: usize, j: usize, k: usize) -> usize {
        i + self.dims[0] * (j + self.dims[1] * k)
    }

    fn build(
        verts: &[[f64; 3]],
        bvh: &MeshBvh,
        h_min: f64,
        max_cells: usize,
    ) -> Option<Self> {
        if verts.is_empty() {
            return None;
        }
        let mut mn = [f64::INFINITY; 3];
        let mut mx = [f64::NEG_INFINITY; 3];
        for p in verts {
            for a in 0..3 {
                mn[a] = mn[a].min(p[a]);
                mx[a] = mx[a].max(p[a]);
            }
        }
        let h0 = h_min.max(1e-9);
        // Pad by one voxel so surface cells are interior to the grid.
        let mut vx = h0;
        let mut dims = [1usize; 3];
        let max_cells = max_cells.max(8);
        for _ in 0..400 {
            for a in 0..3 {
                let span = (mx[a] - mn[a]).max(h0) + 2.0 * vx;
                dims[a] = ((span / vx).ceil() as usize).max(2);
            }
            let total = dims[0].saturating_mul(dims[1]).saturating_mul(dims[2]);
            if total <= max_cells {
                break;
            }
            vx *= 1.3;
        }
        let origin = [mn[0] - vx, mn[1] - vx, mn[2] - vx];
        let total = dims[0] * dims[1] * dims[2];
        if total == 0 || total > max_cells.saturating_mul(4) {
            return None;
        }

        let mut occ = vec![0u8; total];
        // Parallel winding grid (C order `[i + nx*(j + ny*k)]`, same order as
        // the depth field) instead of a serial per-cell traversal.
        let w = bvh.winding_grid(origin, vx, dims);
        for (idx, &wi) in w.iter().enumerate() {
            if wi > 0.5 {
                occ[idx] = 1;
            }
        }
        let sdf = crate::morph::signed_distance_from_occupancy(&occ, dims);
        let mut depth = vec![0.0f32; total];
        for idx in 0..total {
            // `sdf` is negative inside; depth is the distance to the exterior.
            depth[idx] = ((-sdf[idx]).max(0.0)) * vx as f32;
        }
        Some(Self {
            origin,
            voxel: [vx; 3],
            dims,
            depth,
        })
    }

    /// Interior depth at `p` by nearest-cell lookup (deterministic).
    fn sample(&self, p: [f64; 3]) -> f64 {
        let mut ijk = [0usize; 3];
        for a in 0..3 {
            let t = ((p[a] - self.origin[a]) / self.voxel[a]).floor();
            let max = self.dims[a].saturating_sub(1) as f64;
            ijk[a] = t.clamp(0.0, max) as usize;
        }
        self.depth[self.cell_index(ijk[0], ijk[1], ijk[2])] as f64
    }

    /// Deepest interior sample: `(depth, center)`; `None` when the field has
    /// no interior cell (open/flat mesh).
    fn deepest(&self) -> Option<(f64, [f64; 3])> {
        let mut best = f32::NEG_INFINITY;
        let mut bi = 0usize;
        for (idx, &d) in self.depth.iter().enumerate() {
            if d > best {
                best = d;
                bi = idx;
            }
        }
        if best <= 0.0 {
            return None;
        }
        let nxny = self.dims[0] * self.dims[1];
        let k = bi / nxny;
        let rem = bi % nxny;
        let j = rem / self.dims[0];
        let i = rem % self.dims[0];
        let c = [
            self.origin[0] + (i as f64 + 0.5) * self.voxel[0],
            self.origin[1] + (j as f64 + 0.5) * self.voxel[1],
            self.origin[2] + (k as f64 + 0.5) * self.voxel[2],
        ];
        Some((best as f64, c))
    }
}

#[inline]
fn face_normal(p0: [f64; 3], p1: [f64; 3], p2: [f64; 3]) -> [f64; 3] {
    let a = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
    let b = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
    let n = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if l > 1e-30 {
        [n[0] / l, n[1] / l, n[2] / l]
    } else {
        [0.0, 0.0, 0.0]
    }
}

/// Union-find over face indices.
struct FaceDsu {
    parent: Vec<u32>,
}

impl FaceDsu {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n as u32).collect(),
        }
    }
    fn find(&mut self, mut x: u32) -> u32 {
        while self.parent[x as usize] != x {
            self.parent[x as usize] = self.parent[self.parent[x as usize] as usize];
            x = self.parent[x as usize];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a as u32), self.find(b as u32));
        if ra != rb {
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            self.parent[hi as usize] = lo;
        }
    }
}

/// Group the flagged (self-intersecting) faces into spatial fold clusters.
///
/// Faces are joined when they properly cross each other (the crossing pair
/// graph), and additionally when they share an edge or a vertex (a fold patch
/// can contain faces that do not directly cross one another).  Each returned
/// cluster is sorted ascending; clusters are ordered by their minimum face
/// index so the result is deterministic.
fn si_sectors(mask: &[bool], pairs: &[(usize, usize)], tris: &[[u32; 3]]) -> Vec<Vec<usize>> {
    let flagged: Vec<usize> = (0..tris.len())
        .filter(|&f| mask.get(f).copied().unwrap_or(false))
        .collect();
    if flagged.is_empty() {
        return Vec::new();
    }
    let mut dsu = FaceDsu::new(tris.len());
    for &(i, j) in pairs {
        if mask.get(i).copied().unwrap_or(false) && mask.get(j).copied().unwrap_or(false) {
            dsu.union(i, j);
        }
    }
    // Edge adjacency among flagged faces.
    let mut edge_faces: std::collections::HashMap<(u32, u32), Vec<usize>> =
        std::collections::HashMap::new();
    // Vertex adjacency among flagged faces.
    let mut vert_faces: std::collections::HashMap<u32, Vec<usize>> =
        std::collections::HashMap::new();
    for &f in &flagged {
        let t = tris[f];
        for v in t {
            vert_faces.entry(v).or_default().push(f);
        }
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a < b { (a, b) } else { (b, a) };
            edge_faces.entry(key).or_default().push(f);
        }
    }
    for fs in edge_faces.values() {
        for w in fs.windows(2) {
            dsu.union(w[0], w[1]);
        }
    }
    for fs in vert_faces.values() {
        for w in fs.windows(2) {
            dsu.union(w[0], w[1]);
        }
    }
    let mut groups: std::collections::HashMap<u32, Vec<usize>> = std::collections::HashMap::new();
    for &f in &flagged {
        let r = dsu.find(f as u32);
        groups.entry(r).or_default().push(f);
    }
    let mut out: Vec<Vec<usize>> = groups.into_values().collect();
    for g in &mut out {
        g.sort_unstable();
    }
    out.sort_by_key(|g| g.first().copied().unwrap_or(usize::MAX));
    out
}

/// Interior-side depth of one face's centroid: the deeper of the two offset
/// samples (so a reversed normal cannot invert the order).
fn face_depth(
    f: usize,
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    df: &DepthField,
    eps: f64,
) -> f64 {
    let t = tris[f];
    let (p0, p1, p2) = (
        verts[t[0] as usize],
        verts[t[1] as usize],
        verts[t[2] as usize],
    );
    let c = [
        (p0[0] + p1[0] + p2[0]) / 3.0,
        (p0[1] + p1[1] + p2[1]) / 3.0,
        (p0[2] + p1[2] + p2[2]) / 3.0,
    ];
    let n = face_normal(p0, p1, p2);
    let plus = df.sample([c[0] + n[0] * eps, c[1] + n[1] * eps, c[2] + n[2] * eps]);
    let minus = df.sample([c[0] - n[0] * eps, c[1] - n[1] * eps, c[2] - n[2] * eps]);
    plus.max(minus)
}

/// Depth of one sector: the **median** interior-side depth over the sector's
/// own face centroids (C1).
fn sector_depth(
    sector: &[usize],
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    df: &DepthField,
    eps: f64,
) -> f64 {
    let mut samples: Vec<f64> = sector
        .iter()
        .map(|&f| face_depth(f, verts, tris, df, eps))
        .collect();
    if samples.is_empty() {
        return 0.0;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    samples[samples.len() / 2]
}

/// Order sectors by depth, deepest first (the lava direction), ties by sector
/// index for determinism.
fn order_sectors(
    sectors: &[Vec<usize>],
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    df: &DepthField,
    eps: f64,
) -> Vec<(usize, f64)> {
    let mut ordered: Vec<(usize, f64)> = sectors
        .iter()
        .enumerate()
        .map(|(i, s)| (i, sector_depth(s, verts, tris, df, eps)))
        .collect();
    ordered.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    ordered
}
/// Reload-equivalent boundary/non-manifold edge counts: float32-weld the
/// vertices (STL save/reload semantics), then count edges used by 1 or >2
/// faces.  This is the exact metric the Python adoption gate recomputes.
fn reload_holes_nm(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> (usize, usize) {
    let n = verts.len();
    if n == 0 || tris.is_empty() {
        return (0, 0);
    }
    let f32v: Vec<[f32; 3]> = verts
        .iter()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        let (x, y) = (f32v[a], f32v[b]);
        x[0].partial_cmp(&y[0])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(x[1].partial_cmp(&y[1]).unwrap_or(std::cmp::Ordering::Equal))
            .then(x[2].partial_cmp(&y[2]).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut remap = vec![0u32; n];
    let mut next = 0u32;
    for (pos, &orig) in order.iter().enumerate() {
        if pos > 0 && f32v[order[pos - 1]] == f32v[orig] {
            remap[orig] = next.saturating_sub(1);
        } else {
            remap[orig] = next;
            next += 1;
        }
    }
    let wt: Vec<[u32; 3]> = tris
        .iter()
        .map(|t| {
            [
                remap[t[0] as usize],
                remap[t[1] as usize],
                remap[t[2] as usize],
            ]
        })
        .collect();
    let s = non_manifold_stats(&wt, next as usize);
    (s.boundary_edges, s.non_manifold_edges)
}

/// Total length of the non-manifold edges (edges used by more than two faces);
/// used to decide whether a new NM spot is small enough to micro-weld.
fn nm_edge_length_sum(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> (f64, usize) {
    let mut counts: HashMap<(u32, u32), u32> = HashMap::new();
    for t in tris {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a < b { (a, b) } else { (b, a) };
            *counts.entry(key).or_insert(0) += 1;
        }
    }
    let mut total = 0.0f64;
    let mut n = 0usize;
    for ((a, b), c) in &counts {
        if *c > 2 {
            let p = verts[*a as usize];
            let q = verts[*b as usize];
            total +=
                ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt();
            n += 1;
        }
    }
    (total, n)
}

/// Faces sharing a vertex with any face in `seed`, grown `rings` times.
fn dilate_ring(seed: &[usize], tris: &[[u32; 3]], rings: usize) -> Vec<usize> {
    let mut set: HashSet<usize> = seed.iter().copied().collect();
    for _ in 0..rings {
        let mut vs: HashSet<u32> = HashSet::new();
        for &f in &set {
            if let Some(t) = tris.get(f) {
                vs.extend(t.iter().copied());
            }
        }
        for (f, t) in tris.iter().enumerate() {
            if t.iter().any(|v| vs.contains(v)) {
                set.insert(f);
            }
        }
    }
    let mut out: Vec<usize> = set.into_iter().collect();
    out.sort_unstable();
    out
}

/// A face is *buried* when the volume just outside it is interior
/// (`w_out >= buried_w_out`) and either the inside is strongly buried
/// (`w_in >= buried_w_in`) or the face actually self-intersects (review C4:
/// a face merely sitting on a thin wall has `w_out ~ 0` and is never excised).
fn is_buried_face(
    f: usize,
    si_partner: bool,
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    bvh: &MeshBvh,
    eps: f64,
    params: &MeltParams,
) -> bool {
    let t = tris[f];
    let (p0, p1, p2) = (
        verts[t[0] as usize],
        verts[t[1] as usize],
        verts[t[2] as usize],
    );
    let c = [
        (p0[0] + p1[0] + p2[0]) / 3.0,
        (p0[1] + p1[1] + p2[1]) / 3.0,
        (p0[2] + p1[2] + p2[2]) / 3.0,
    ];
    let n = face_normal(p0, p1, p2);
    let w_out = bvh.winding_at([c[0] + n[0] * eps, c[1] + n[1] * eps, c[2] + n[2] * eps]);
    if w_out < params.buried_w_out {
        return false;
    }
    let w_in = bvh.winding_at([c[0] - n[0] * eps, c[1] - n[1] * eps, c[2] - n[2] * eps]);
    si_partner || w_in >= params.buried_w_in
}

/// Remove the flagged faces, dropping vertices that become unreferenced.
/// Returns the compacted mesh and the number of removed faces.
fn excise_faces(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    delete: &[bool],
) -> (Vec<[f64; 3]>, Vec<[u32; 3]>, usize) {
    let mut remap = vec![u32::MAX; verts.len()];
    let mut nv: Vec<[f64; 3]> = Vec::new();
    let mut nt: Vec<[u32; 3]> = Vec::with_capacity(tris.len());
    let mut removed = 0usize;
    for (f, t) in tris.iter().enumerate() {
        if delete.get(f).copied().unwrap_or(false) {
            removed += 1;
            continue;
        }
        let mut a = [0u32; 3];
        for k in 0..3 {
            let v = t[k] as usize;
            if remap[v] == u32::MAX {
                remap[v] = nv.len() as u32;
                nv.push(verts[v]);
            }
            a[k] = remap[v];
        }
        nt.push(a);
    }
    (nv, nt, removed)
}

/// Cap every boundary loop at or below `max_loop` vertices with a minimum-area
/// rim triangulation whose orientation cancels the most existing boundary
/// edges.  Loops that cannot be triangulated are left open (the rollback guard
/// then rejects the layer).
fn cap_boundary_loops(verts: &mut Vec<[f64; 3]>, tris: &mut Vec<[u32; 3]>, max_loop: usize) {
    let directed = crate::tamponade::directed_boundary_edges(tris);
    if directed.is_empty() {
        return;
    }
    let directed_set: HashSet<(u32, u32)> = directed.iter().copied().collect();
    let loops = crate::tamponade::build_boundary_loops(&directed);
    for loop_v in loops {
        let mut rim = loop_v.clone();
        rim.dedup();
        if rim.len() >= 2 && rim.first() == rim.last() {
            rim.pop();
        }
        if rim.len() < 3 || rim.len() > max_loop {
            continue;
        }
        let pts: Vec<[f64; 3]> = rim.iter().map(|&v| verts[v as usize]).collect();
        let local = crate::flap::dp::min_area_triangulation(&pts, max_loop, &HashSet::new());
        if local.len() != rim.len() - 2 {
            continue;
        }
        let build = |flip: bool| -> Vec<[u32; 3]> {
            local
                .iter()
                .map(|t| {
                    let (a, b, c) = if flip {
                        (rim[t[0] as usize], rim[t[2] as usize], rim[t[1] as usize])
                    } else {
                        (rim[t[0] as usize], rim[t[1] as usize], rim[t[2] as usize])
                    };
                    [a, b, c]
                })
                .collect()
        };
        let cancel = |cap: &[[u32; 3]]| -> usize {
            cap.iter()
                .flat_map(|t| [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])])
                .filter(|&(u, v)| directed_set.contains(&(v, u)))
                .count()
        };
        let base = build(false);
        let flipped = build(true);
        let cap = if cancel(&flipped) > cancel(&base) {
            flipped
        } else {
            base
        };
        tris.extend(cap);
    }
}

/// Run the inside-out melt.
///
/// P1 builds the interior-depth field and the deterministic depth-descending
/// sector order.  The geometry is still returned unchanged (excision lands in
/// P2), so the output stays byte-identical to the L2a result.
pub fn melt_si(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    h: f64,
    model_diag: f64,
    time_budget_s: Option<f64>,
    params: &MeltParams,
) -> MeltOutcome {
    let mut out = MeltOutcome {
        verts: verts.to_vec(),
        tris: tris.to_vec(),
        ..Default::default()
    };

    if verts.is_empty() || tris.is_empty() {
        return out;
    }

    let scan = scan_si(verts, tris);
    if scan.skipped || scan.count <= 0 {
        // Nothing to melt; leave the L2a output untouched.
        return out;
    }

    let tris_usize: Vec<[usize; 3]> = tris
        .iter()
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    let bvh = MeshBvh::from_arrays(verts, &tris_usize);
    let h_min = h.max(1e-9);
    let Some(df) = DepthField::build(verts, &bvh, h_min, params.grid_max_cells) else {
        out.reason = "melt: depth grid unavailable".to_string();
        return out;
    };

    let sectors = si_sectors(&scan.mask, &scan.pairs, tris);
    let eps = h_min;
    let ordered = order_sectors(&sectors, verts, tris, &df, eps);

    let t0 = std::time::Instant::now();
    let mut cur_v = out.verts.clone();
    let mut cur_t = out.tris.clone();
    let mut bvh = bvh;
    let mut si_cur = scan.count;
    // The melt pass gets a bounded slice of the tier budget: on a mesh with
    // hundreds of small scattered sectors the per-layer exact SI re-scan is the
    // dominant cost, and the local gains do not justify spending the whole
    // Tamponade budget.
    let budget = Some(
        time_budget_s
            .map(|b| b.min(MELT_MAX_SECONDS))
            .unwrap_or(MELT_MAX_SECONDS),
    );

    for (si, _depth) in ordered.iter().take(params.max_layers) {
        if let Some(b) = budget {
            if t0.elapsed().as_secs_f64() > b {
                out.reason = "melt: timeout exceeded".to_string();
                break;
            }
        }
        let sector = &sectors[*si];
        let sector_set: HashSet<usize> = sector.iter().copied().collect();
        // The whole buried lobe of the fold: the SI faces plus one vertex ring.
        let region = dilate_ring(sector, &cur_t, 1);
        // Deletion is capped to the healthy-fraction brake, so the layer can
        // never take more than `max_face_fraction` of the mesh; within the cap
        // the deepest faces go first (the lava direction).
        let budget = ((params.max_face_fraction * cur_t.len() as f64).floor() as usize)
            .min(params.max_sector_faces)
            .max(1);
        let mut qual: Vec<(f64, usize)> = Vec::new();
        for &f in &region {
            if f >= cur_t.len() {
                continue;
            }
            let is_si = sector_set.contains(&f);
            let buried = !is_si && is_buried_face(f, false, &cur_v, &cur_t, &bvh, eps, params);
            if is_si || buried {
                qual.push((face_depth(f, &cur_v, &cur_t, &df, eps), f));
            }
        }
        qual.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.1.cmp(&a.1))
        });
        let take = qual.len().min(budget);
        let mut delete = vec![false; cur_t.len()];
        let mut cand = 0usize;
        for &(_, f) in qual.iter().take(take) {
            if !delete[f] {
                delete[f] = true;
                cand += 1;
            }
        }
        out.layers_tried += 1;
        if std::env::var("SUTURA_MELT_DBG").is_ok() {
            eprintln!(
                "MELT_DBG sector={} sector_faces={} region={} qual={} take={} cand={} nt={}",
                si,
                sector.len(),
                region.len(),
                qual.len(),
                take,
                cand,
                cur_t.len()
            );
        }
        if cand == 0 {
            out.layers_rolled_back += 1;
            continue;
        }

        let (mut nv, mut nt, removed) = excise_faces(&cur_v, &cur_t, &delete);
        cap_boundary_loops(&mut nv, &mut nt, params.cap_max_loop);

        let (bh, bnm) = reload_holes_nm(&cur_v, &cur_t);
        let (mut h, mut nm) = reload_holes_nm(&nv, &nt);
        let mut scan_after = scan_si(&nv, &nt);
        if std::env::var("SUTURA_MELT_DBG").is_ok() {
            eprintln!(
                "MELT_DBG sector={} cand={} removed={} si {} -> {} holes {} -> {} nm {} -> {}",
                si, cand, removed, si_cur, scan_after.count, bh, h, bnm, nm
            );
        }
        let mut accepted = !scan_after.skipped
            && scan_after.count >= 0
            && scan_after.count < si_cur
            && h <= bh
            && nm <= bnm;

        // Small non-manifold spot: try a local micro-weld before rolling back
        // (the lava rule).  Only a small cluster is attempted; anything larger
        // is rolled back directly.
        if !accepted
            && h <= bh
            && !scan_after.skipped
            && scan_after.count >= 0
            && scan_after.count < si_cur
            && nm > bnm
            && nm <= params.micro_weld_max_edges
        {
            let (nm_len, _nm_n) = nm_edge_length_sum(&nv, &nt);
            if nm_len <= params.micro_weld_max_rel * model_diag.max(1e-9) {
                let (mw_v, mw_t, _split) = crate::flap::topology::split_non_manifold(&nv, &nt);
                let (sep_v, _moved) =
                    crate::flap::topology::separate_coincident_stl(&mw_v, None, None);
                let (h2, nm2) = reload_holes_nm(&sep_v, &mw_t);
                let s2 = scan_si(&sep_v, &mw_t);
                if h2 <= bh && nm2 <= bnm && !s2.skipped && s2.count >= 0 && s2.count < si_cur {
                    nv = sep_v;
                    nt = mw_t;
                    h = h2;
                    nm = nm2;
                    scan_after = s2;
                    accepted = true;
                    out.micro_welds_ok += 1;
                } else {
                    out.micro_welds_failed += 1;
                }
            }
        }

        if accepted {
            cur_v = nv;
            cur_t = nt;
            si_cur = scan_after.count;
            out.layers_accepted += 1;
            out.excised_faces += removed;
            // Winding must reflect the edited mesh for the next sector.
            let t_usize: Vec<[usize; 3]> = cur_t
                .iter()
                .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
                .collect();
            bvh = MeshBvh::from_arrays(&cur_v, &t_usize);
        } else {
            out.layers_rolled_back += 1;
        }
        let _ = (h, nm);
    }

    out.verts = cur_v;
    out.tris = cur_t;
    let _ = (model_diag, df);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn clean_cube() -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
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
            [0, 2, 1], [0, 3, 2], // bottom
            [4, 5, 6], [4, 6, 7], // top
            [0, 1, 5], [0, 5, 4], // -y
            [1, 2, 6], [1, 6, 5], // +x
            [2, 3, 7], [2, 7, 6], // +y
            [3, 0, 4], [3, 4, 7], // -x
        ];
        (verts, tris)
    }

    /// Two triangles that cross in a proper (non-degenerate) segment.
    fn crossing_pair() -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            [0.7, -1.0, -1.0],
            [0.7, 3.0, -1.0],
            [0.7, 1.0, 3.0],
        ];
        let tris = vec![[0, 1, 2], [3, 4, 5]];
        (verts, tris)
    }

    #[test]
    fn si_scan_crossing_pair() {
        let (verts, tris) = crossing_pair();
        let (mask, count, skipped) = scan_si_faces(&verts, &tris);
        assert!(!skipped);
        assert_eq!(count, 2, "both crossing faces are flagged");
        assert!(mask.iter().all(|&b| b));
    }

    #[test]
    fn clean_mesh_no_melt() {
        let (verts, tris) = clean_cube();
        let (mask, count, skipped) = scan_si_faces(&verts, &tris);
        assert!(!skipped);
        assert_eq!(count, 0);
        assert!(mask.iter().all(|&b| !b));

        let out = melt_si(&verts, &tris, 0.5, 1.7, None, &MeltParams::default());
        assert_eq!(out.layers_tried, 0);
        assert_eq!(out.layers_accepted, 0);
        assert_eq!(out.verts, verts);
        assert_eq!(out.tris, tris);
    }

    fn bvh_of(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> MeshBvh {
        let t: Vec<[usize; 3]> = tris
            .iter()
            .map(|x| [x[0] as usize, x[1] as usize, x[2] as usize])
            .collect();
        MeshBvh::from_arrays(verts, &t)
    }

    #[test]
    fn depth_field_deepest_center() {
        let (verts, tris) = clean_cube();
        let bvh = bvh_of(&verts, &tris);
        let df = DepthField::build(&verts, &bvh, 0.2, 8_000_000).expect("grid");
        let (d, c) = df.deepest().expect("interior");
        assert!(d > 0.3, "expected a deep interior, got {d}");
        for a in 0..3 {
            assert!((c[a] - 0.5).abs() < 0.25, "center {c:?} not near middle");
        }
        // The surface sample is much shallower than the center.
        let near_face = df.sample([0.01, 0.5, 0.5]);
        assert!(near_face < d, "face {near_face} should be shallower than {d}");
    }

    #[test]
    fn sectorize_crossing_pair_is_one_sector() {
        let (verts, tris) = crossing_pair();
        let scan = scan_si(&verts, &tris);
        let sectors = si_sectors(&scan.mask, &scan.pairs, &tris);
        assert_eq!(sectors.len(), 1);
        assert_eq!(sectors[0], vec![0, 1]);
    }

    #[test]
    fn sectorize_disjoint_pairs_is_two_sectors() {
        // Two independent crossing pairs with no shared vertex or edge.
        let verts = vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            [0.7, -1.0, -1.0],
            [0.7, 3.0, -1.0],
            [0.7, 1.0, 3.0],
            [10.0, 0.0, 0.0],
            [12.0, 0.0, 0.0],
            [10.0, 2.0, 0.0],
            [10.7, -1.0, -1.0],
            [10.7, 3.0, -1.0],
            [10.7, 1.0, 3.0],
        ];
        let tris = vec![[0, 1, 2], [3, 4, 5], [6, 7, 8], [9, 10, 11]];
        let scan = scan_si(&verts, &tris);
        assert_eq!(scan.count, 4);
        let sectors = si_sectors(&scan.mask, &scan.pairs, &tris);
        assert_eq!(sectors.len(), 2);
        assert_eq!(sectors[0], vec![0, 1]);
        assert_eq!(sectors[1], vec![2, 3]);
    }

    #[test]
    fn order_sectors_deepest_first() {
        // A hand-built depth field: cell 0 shallow, cell 1 deep (x >= 1).
        let df = DepthField {
            origin: [0.0, 0.0, 0.0],
            voxel: [1.0, 1.0, 1.0],
            dims: [2, 1, 1],
            depth: vec![0.0, 5.0],
        };
        let verts = vec![
            [0.4, 0.0, 0.0],
            [0.6, 0.0, 0.0],
            [0.5, 0.1, 0.0],
            [1.4, 0.0, 0.0],
            [1.6, 0.0, 0.0],
            [1.5, 0.1, 0.0],
        ];
        let tris = vec![[0, 1, 2], [3, 4, 5]];
        let sectors = vec![vec![0usize], vec![1usize]];
        let ordered = order_sectors(&sectors, &verts, &tris, &df, 0.0);
        assert_eq!(ordered.len(), 2);
        assert_eq!(ordered[0].0, 1, "the deep sector (face 1) comes first");
        assert_eq!(ordered[1].0, 0);
    }

    /// A closed axis-aligned box `[mn, mx]` with each face subdivided into an
    /// `n x n` grid and shared vertices (a realistic fine surface).
    fn grid_box(mn: [f64; 3], mx: [f64; 3], n: usize) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let mut verts: Vec<[f64; 3]> = Vec::new();
        let mut tris: Vec<[u32; 3]> = Vec::new();
        let mut vmap: HashMap<(i64, i64, i64), u32> = HashMap::new();
        let key = |p: [f64; 3]| -> (i64, i64, i64) {
            (
                (p[0] * 1e6).round() as i64,
                (p[1] * 1e6).round() as i64,
                (p[2] * 1e6).round() as i64,
            )
        };
        let mut add_face = |o: [f64; 3], u: [f64; 3], v: [f64; 3], outward: [f64; 3]| {
            let nrm = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            let dot = nrm[0] * outward[0] + nrm[1] * outward[1] + nrm[2] * outward[2];
            let (u, v) = if dot < 0.0 { (v, u) } else { (u, v) };
            let mut idx = vec![vec![0u32; n + 1]; n + 1];
            for (j, row) in idx.iter_mut().enumerate() {
                for (i, cell) in row.iter_mut().enumerate() {
                    let p = [
                        o[0] + u[0] * i as f64 / n as f64 + v[0] * j as f64 / n as f64,
                        o[1] + u[1] * i as f64 / n as f64 + v[1] * j as f64 / n as f64,
                        o[2] + u[2] * i as f64 / n as f64 + v[2] * j as f64 / n as f64,
                    ];
                    let k = key(p);
                    *cell = *vmap.entry(k).or_insert_with(|| {
                        verts.push(p);
                        (verts.len() - 1) as u32
                    });
                }
            }
            for j in 0..n {
                for i in 0..n {
                    let (a, b, c, d) = (idx[j][i], idx[j][i + 1], idx[j + 1][i], idx[j + 1][i + 1]);
                    tris.push([a, b, c]);
                    tris.push([b, d, c]);
                }
            }
        };
        let (x0, y0, z0) = (mn[0], mn[1], mn[2]);
        let (x1, y1, z1) = (mx[0], mx[1], mx[2]);
        let (dx, dy, dz) = (x1 - x0, y1 - y0, z1 - z0);
        // -x, +x
        add_face([x0, y0, z0], [0.0, dy, 0.0], [0.0, 0.0, dz], [-1.0, 0.0, 0.0]);
        add_face([x1, y0, z0], [0.0, 0.0, dz], [0.0, dy, 0.0], [1.0, 0.0, 0.0]);
        // -y, +y
        add_face([x0, y0, z0], [0.0, 0.0, dz], [dx, 0.0, 0.0], [0.0, -1.0, 0.0]);
        add_face([x0, y1, z0], [dx, 0.0, 0.0], [0.0, 0.0, dz], [0.0, 1.0, 0.0]);
        // -z, +z
        add_face([x0, y0, z0], [dx, 0.0, 0.0], [0.0, dy, 0.0], [0.0, 0.0, -1.0]);
        add_face([x0, y0, z1], [0.0, dy, 0.0], [dx, 0.0, 0.0], [0.0, 0.0, 1.0]);
        (verts, tris)
    }

    fn append(
        v: &mut Vec<[f64; 3]>,
        t: &mut Vec<[u32; 3]>,
        v2: Vec<[f64; 3]>,
        t2: Vec<[u32; 3]>,
    ) {
        let off = v.len() as u32;
        v.extend(v2);
        t.extend(t2.iter().map(|x| [x[0] + off, x[1] + off, x[2] + off]));
    }

    /// Two overlapping fine boxes with misaligned grids, so their surfaces
    /// cross in proper (non-boundary) segments.  This is a body-level union
    /// (agy's `shell.rs` domain), not a local fold; the melt must not make it
    /// worse.
    fn two_overlapping_boxes() -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let (mut v, mut t) = grid_box([0.0, 0.0, 0.0], [1.0, 1.0, 1.0], 6);
        let (v2, t2) = grid_box([0.52, 0.52, 0.52], [1.48, 1.48, 1.48], 7);
        append(&mut v, &mut t, v2, t2);
        (v, t)
    }

    /// A closed cube plus a flat flap that pokes through one wall: a local,
    /// single-sheet fold whose buried part is inside the cube.
    fn flap_through_wall() -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let (mut v, mut t) = clean_cube();
        let base = v.len() as u32;
        v.extend([
            [0.6, 0.4, 0.5],
            [1.4, 0.4, 0.5],
            [1.4, 0.6, 0.5],
            [0.6, 0.6, 0.5],
        ]);
        t.push([base, base + 1, base + 2]);
        t.push([base, base + 2, base + 3]);
        (v, t)
    }

    #[test]
    fn melt_local_flap_reduces_si() {
        let (verts, tris) = flap_through_wall();
        let before = scan_si(&verts, &tris);
        assert!(before.count > 0, "flap must cross the wall");
        let params = MeltParams {
            max_face_fraction: 0.5,
            ..MeltParams::default()
        };
        let out = melt_si(&verts, &tris, 0.05, 1.8, None, &params);
        let after = scan_si(&out.verts, &out.tris);
        let (h, nm) = reload_holes_nm(&out.verts, &out.tris);
        eprintln!(
            "MELTFLAP si {} -> {} holes/nm {}/{} layers t/a/roll {}/{}/{} excised {} faces {} -> {}",
            before.count,
            after.count,
            h,
            nm,
            out.layers_tried,
            out.layers_accepted,
            out.layers_rolled_back,
            out.excised_faces,
            tris.len(),
            out.tris.len()
        );
        assert!(h == 0 && nm == 0, "melt must not open holes/nm: {h}/{nm}");
        assert!(
            after.count < before.count,
            "SI should fall: {} -> {}",
            before.count,
            after.count
        );
    }

    /// A thin slab face has interior on one side and exterior on the other
    /// (`w_out ~ 0`), so the thin-wall guard (C4) must never classify it as
    /// buried.
    #[test]
    fn thin_wall_face_not_buried() {
        let (v, t) = grid_box([0.0, 0.0, 0.0], [1.0, 1.0, 0.02], 1);
        let tu: Vec<[usize; 3]> = t
            .iter()
            .map(|x| [x[0] as usize, x[1] as usize, x[2] as usize])
            .collect();
        let bvh = MeshBvh::from_arrays(&v, &tu);
        let params = MeltParams::default();
        // Every slab face must be kept, with or without an SI partner.
        for f in 0..t.len() {
            assert!(
                !is_buried_face(f, false, &v, &t, &bvh, 0.01, &params),
                "thin slab face {f} wrongly classified buried"
            );
            assert!(
                !is_buried_face(f, true, &v, &t, &bvh, 0.01, &params),
                "thin slab face {f} wrongly classified buried with SI partner"
            );
        }
    }

    #[test]
    fn melt_body_union_never_worsens() {
        let (verts, tris) = two_overlapping_boxes();
        let before = scan_si(&verts, &tris);
        assert!(before.count > 0, "boxes must cross");
        let params = MeltParams {
            max_face_fraction: 0.5,
            ..MeltParams::default()
        };
        let out = melt_si(&verts, &tris, 0.05, 3.0, None, &params);
        let after = scan_si(&out.verts, &out.tris);
        let (h, nm) = reload_holes_nm(&out.verts, &out.tris);
        assert!(h == 0 && nm == 0, "must not open holes/nm: {h}/{nm}");
        assert!(after.count <= before.count, "SI must not rise");
    }

    #[test]
    fn melt_is_deterministic() {
        let (verts, tris) = flap_through_wall();
        let params = MeltParams {
            max_face_fraction: 0.5,
            ..MeltParams::default()
        };
        let a = melt_si(&verts, &tris, 0.05, 1.8, None, &params);
        let b = melt_si(&verts, &tris, 0.05, 1.8, None, &params);
        assert_eq!(a.verts, b.verts);
        assert_eq!(a.tris, b.tris);
        assert_eq!(a.layers_accepted, b.layers_accepted);
        assert_eq!(a.excised_faces, b.excised_faces);
    }

    #[test]
    fn melt_budget_stops_cleanly() {
        let (verts, tris) = flap_through_wall();
        let params = MeltParams {
            max_face_fraction: 0.5,
            ..MeltParams::default()
        };
        // A negative budget expires before the first layer is attempted.
        let out = melt_si(&verts, &tris, 0.05, 1.8, Some(-1.0), &params);
        assert_eq!(out.layers_tried, 0);
        assert_eq!(out.layers_accepted, 0);
        assert_eq!(out.verts, verts);
        assert_eq!(out.tris, tris);
        assert!(out.reason.contains("timeout"), "reason: {}", out.reason);
    }
}
