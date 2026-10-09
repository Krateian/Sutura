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

use crate::triangle_intersection::{intersect_triangles, Triangle, TriangleIntersection};
use crate::winding::MeshBvh;

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
}

impl Default for MeltParams {
    fn default() -> Self {
        Self {
            buried_w_out: 0.5,
            buried_w_in: 1.5,
            max_sector_faces: 5000,
            max_face_fraction: 0.02,
            micro_weld_max_edges: 8,
            micro_weld_max_rel: 0.005,
            grid_max_cells: 8_000_000,
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
        for k in 0..dims[2] {
            for j in 0..dims[1] {
                for i in 0..dims[0] {
                    let c = [
                        origin[0] + (i as f64 + 0.5) * vx,
                        origin[1] + (j as f64 + 0.5) * vx,
                        origin[2] + (k as f64 + 0.5) * vx,
                    ];
                    if bvh.winding_at(c) > 0.5 {
                        occ[i + dims[0] * (j + dims[1] * k)] = 1;
                    }
                }
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

/// Depth of one sector: the **median** interior-side depth over the sector's
/// own face centroids (C1).  The interior side is the offset sample with the
/// larger depth, so a reversed normal cannot invert the order.
fn sector_depth(
    sector: &[usize],
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    df: &DepthField,
    eps: f64,
) -> f64 {
    let mut samples: Vec<f64> = Vec::with_capacity(sector.len());
    for &f in sector {
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
        samples.push(plus.max(minus));
    }
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
    _time_budget_s: Option<f64>,
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

    // Excision and the per-sector rollback loop are implemented in P2.  The
    // analysis above is deterministic and cheap; keep it wired so P2 only adds
    // the mutation step.
    let _ = (tris_usize, df, ordered, model_diag);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn scaffold_is_byte_identical_on_si_mesh() {
        // P0 is a guarded no-op, so an SI mesh is currently returned unchanged.
        let (verts, tris) = crossing_pair();
        let out = melt_si(&verts, &tris, 0.5, 3.0, None, &MeltParams::default());
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
}
