//! Deterministic 2:1 balanced Morton-keyed adaptive octree.
//!
//! Provides spatial subdivision down to a specified minimum leaf size `h_min`,
//! with narrow-band refinement along the mesh surface and hole membranes,
//! enforced 2:1 balancing (restricted octree) across shared faces and edges,
//! and canonical Morton-order traversal.

use crate::winding::{Aabb, MeshBvh};
use rayon::prelude::*;
use std::collections::HashSet;

/// Default maximum allowable nodes to prevent out-of-memory on runaway inputs.
pub const DEFAULT_MAX_NODES: usize = 20_000_000;

/// Interleave bits of 3 21-bit unsigned integers into a 63-bit Morton code.
#[inline]
pub fn morton_encode(x: u32, y: u32, z: u32) -> u64 {
    let mut m = 0u64;
    for i in 0..21 {
        m |= (((x >> i) & 1) as u64) << (3 * i);
        m |= (((y >> i) & 1) as u64) << (3 * i + 1);
        m |= (((z >> i) & 1) as u64) << (3 * i + 2);
    }
    m
}

/// A node in the adaptive octree.
#[derive(Clone, Copy, Debug)]
pub struct OctreeNode {
    /// Integer grid coordinates at depth `depth`.
    pub x: u32,
    pub y: u32,
    pub z: u32,
    /// Depth in the tree (0 = root).
    pub depth: u8,
    /// Indices of the 8 children in the node pool, or `[u32::MAX; 8]` for leaves.
    pub children: [u32; 8],
}

impl OctreeNode {
    #[inline]
    pub fn is_leaf(&self) -> bool {
        self.children[0] == u32::MAX
    }
}

/// Adaptive octree hierarchy.
pub struct Octree {
    /// Cubic bounding box origin (minimum corner).
    pub origin: [f64; 3],
    /// Cubic bounding box edge length.
    pub size: f64,
    /// Maximum tree depth.
    pub max_depth: u8,
    /// Achieved minimum leaf cell size.
    pub h_min: f64,
    /// Linear node pool.
    pub nodes: Vec<OctreeNode>,
    /// Maximum nodes limit.
    pub max_nodes: usize,
    /// Nodes split by surface narrow-band criterion.
    pub split_by_band: usize,
    /// Nodes split by open boundary edge proximity.
    pub split_by_boundary: usize,
    /// Nodes split by ambiguous winding number across hole membranes.
    pub split_by_ambiguous_w: usize,
    /// Nodes split to enforce 2:1 balancing.
    pub split_by_balance: usize,
}

impl Octree {
    /// Initialize an octree covering `bbox` expanded with padding.
    pub fn new(bbox: &Aabb, h_min: f64, max_nodes: Option<usize>) -> Result<Self, String> {
        let max_nodes = max_nodes.unwrap_or(DEFAULT_MAX_NODES);
        if bbox.is_empty() {
            return Err("Empty bounding box for octree construction".to_string());
        }

        let span_x = bbox.max[0] - bbox.min[0];
        let span_y = bbox.max[1] - bbox.min[1];
        let span_z = bbox.max[2] - bbox.min[2];
        let max_span = span_x.max(span_y).max(span_z);

        // Generous padding: 10% on each side + at least 3*h_min margin so zero-crossings
        // never reach the octree boundary.
        let pad = (max_span * 0.10).max(3.0 * h_min);
        let size = max_span + 2.0 * pad;
        let origin = [
            bbox.min[0] - pad,
            bbox.min[1] - pad,
            bbox.min[2] - pad,
        ];

        // Determine max_depth such that size / 2^max_depth <= h_min
        let mut depth = 1u8;
        while (size / (1u64 << depth) as f64) > h_min && depth < 18 {
            depth += 1;
        }
        let actual_h_min = size / (1u64 << depth) as f64;

        let root = OctreeNode {
            x: 0,
            y: 0,
            z: 0,
            depth: 0,
            children: [u32::MAX; 8],
        };

        let mut nodes = Vec::with_capacity(4096);
        nodes.push(root);

        Ok(Octree {
            origin,
            size,
            max_depth: depth,
            h_min: actual_h_min,
            nodes,
            max_nodes,
            split_by_band: 0,
            split_by_boundary: 0,
            split_by_ambiguous_w: 0,
            split_by_balance: 0,
        })
    }

    /// Size of a cell at given depth.
    #[inline]
    pub fn cell_size(&self, depth: u8) -> f64 {
        self.size / (1u64 << depth) as f64
    }

    /// Minimum corner of cell in world coordinates.
    #[inline]
    pub fn cell_min(&self, x: u32, y: u32, z: u32, depth: u8) -> [f64; 3] {
        let h = self.cell_size(depth);
        [
            self.origin[0] + x as f64 * h,
            self.origin[1] + y as f64 * h,
            self.origin[2] + z as f64 * h,
        ]
    }

    /// Center of cell in world coordinates.
    #[inline]
    pub fn cell_center(&self, x: u32, y: u32, z: u32, depth: u8) -> [f64; 3] {
        let h = self.cell_size(depth);
        [
            self.origin[0] + (x as f64 + 0.5) * h,
            self.origin[1] + (y as f64 + 0.5) * h,
            self.origin[2] + (z as f64 + 0.5) * h,
        ]
    }

    /// Find the leaf node containing integer coordinate (X, Y, Z) at resolution `max_depth`.
    #[inline]
    pub fn find_leaf_at(&self, x_max: u32, y_max: u32, z_max: u32) -> usize {
        let mut curr = 0usize;
        let mut d = 0u8;
        let md = self.max_depth;

        while !self.nodes[curr].is_leaf() {
            let shift = md - 1 - d;
            let ox = ((x_max >> shift) & 1) as usize;
            let oy = ((y_max >> shift) & 1) as usize;
            let oz = ((z_max >> shift) & 1) as usize;
            let octant = ox | (oy << 1) | (oz << 2);
            curr = self.nodes[curr].children[octant] as usize;
            d += 1;
        }
        curr
    }

    /// Split a leaf node into 8 children.
    pub fn split_node(&mut self, node_idx: usize) -> Result<(), String> {
        if !self.nodes[node_idx].is_leaf() {
            return Ok(());
        }
        if self.nodes.len() + 8 > self.max_nodes {
            return Err(format!(
                "Octree exceeded max_nodes limit: {} (reached {})",
                self.max_nodes,
                self.nodes.len()
            ));
        }

        let parent = self.nodes[node_idx];
        let next_depth = parent.depth + 1;
        let base_x = parent.x << 1;
        let base_y = parent.y << 1;
        let base_z = parent.z << 1;

        let start_child = self.nodes.len() as u32;
        let mut child_indices = [u32::MAX; 8];

        for octant in 0..8 {
            let ox = (octant & 1) as u32;
            let oy = ((octant >> 1) & 1) as u32;
            let oz = ((octant >> 2) & 1) as u32;
            let child = OctreeNode {
                x: base_x + ox,
                y: base_y + oy,
                z: base_z + oz,
                depth: next_depth,
                children: [u32::MAX; 8],
            };
            child_indices[octant] = start_child + octant as u32;
            self.nodes.push(child);
        }

        self.nodes[node_idx].children = child_indices;
        Ok(())
    }
}

/// Open boundary edges of a triangle mesh (edges with half-edge count == 1).
#[derive(Clone, Debug)]
pub struct MeshBoundaries {
    pub bbox: Aabb,
    pub segments: Vec<([f64; 3], [f64; 3])>,
}

impl MeshBoundaries {
    pub fn empty() -> Self {
        MeshBoundaries {
            bbox: Aabb::empty(),
            segments: Vec::new(),
        }
    }

    pub fn from_mesh(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> Self {
        let mut edge_counts = std::collections::HashMap::new();
        for t in tris {
            for i in 0..3 {
                let u = t[i];
                let v = t[(i + 1) % 3];
                let key = if u < v { (u, v) } else { (v, u) };
                *edge_counts.entry(key).or_insert(0usize) += 1;
            }
        }

        let mut segments = Vec::new();
        let mut bbox = Aabb::empty();
        for ((u, v), count) in edge_counts {
            if count == 1 {
                let p0 = verts[u];
                let p1 = verts[v];
                bbox.expand_point(p0);
                bbox.expand_point(p1);
                segments.push((p0, p1));
            }
        }

        MeshBoundaries { bbox, segments }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Fast test: returns true if point `p` is within distance `dist_thresh` of any boundary edge.
    pub fn is_within_dist(&self, p: [f64; 3], dist_thresh: f64) -> bool {
        if self.segments.is_empty() {
            return false;
        }
        let d2_box = self.bbox.dist2(p);
        let thresh2 = dist_thresh * dist_thresh;
        if d2_box > thresh2 {
            return false;
        }

        for &(p0, p1) in &self.segments {
            let v = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
            let w = [p[0] - p0[0], p[1] - p0[1], p[2] - p0[2]];
            let c1 = w[0] * v[0] + w[1] * v[1] + w[2] * v[2];
            let c2 = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
            let t = if c2 > 1e-15 { (c1 / c2).clamp(0.0, 1.0) } else { 0.0 };
            let proj = [p0[0] + t * v[0], p0[1] + t * v[1], p0[2] + t * v[2]];
            let d2 = (p[0] - proj[0]).powi(2) + (p[1] - proj[1]).powi(2) + (p[2] - proj[2]).powi(2);
            if d2 <= thresh2 {
                return true;
            }
        }
        false
    }
}

/// Reason a leaf node was selected for octree subdivision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitReason {
    None,
    Band,
    Boundary,
    AmbiguousW,
}

impl Octree {
    /// Build narrow-band adaptive refinement along the surface and hole membranes.
    ///
    /// Per LEAD REVIEW C2 & C3 and STOP+REDIRECT:
    /// - Narrow-band threshold: cell circumradius `sqrt(3)/2 * h + r`.
    /// - Open boundary threshold: cell circumradius around boundary edges.
    /// - Ambiguous winding check: restricted strictly to tubular neighborhood of open
    ///   boundary loops (`dist <= 4 * h_min` from an open boundary edge, `0.3 < w < 0.7`).
    pub fn build_narrow_band(
        &mut self,
        bvh: &MeshBvh,
        _mesh_bbox: &Aabb,
        boundaries: &MeshBoundaries,
        r: f64,
    ) -> Result<(), String> {
        let sqrt3 = 3.0f64.sqrt();

        // Level-by-level refinement from depth 0 up to max_depth - 1
        for d in 0..self.max_depth {
            let leaf_indices: Vec<usize> = (0..self.nodes.len())
                .filter(|&i| self.nodes[i].is_leaf() && self.nodes[i].depth == d)
                .collect();

            if leaf_indices.is_empty() {
                break;
            }

            let h = self.cell_size(d);
            let circumradius = (sqrt3 * 0.5) * h + r + 1e-6;
            let boundary_h = (sqrt3 * 0.5) * h;
            let ambig_thresh = 4.0 * self.h_min;

            // Parallel predicate evaluation over leaves at depth d
            let decisions: Vec<SplitReason> = leaf_indices
                .par_iter()
                .map(|&idx| {
                    let n = &self.nodes[idx];
                    let c = self.cell_center(n.x, n.y, n.z, n.depth);

                    // 1. Surface narrow band
                    let dist = bvh.unsigned_distance_at(c);
                    if dist <= circumradius {
                        return SplitReason::Band;
                    }

                    // 2. Open boundary edge proximity
                    if boundaries.is_within_dist(c, boundary_h) {
                        return SplitReason::Boundary;
                    }

                    // 3. Ambiguous winding check across hole membranes (only near boundary loops!)
                    if boundaries.is_within_dist(c, ambig_thresh) {
                        let w = bvh.winding_at_tol(c, 0.1);
                        if w > 0.3 && w < 0.7 {
                            return SplitReason::AmbiguousW;
                        }
                    }

                    SplitReason::None
                })
                .collect();

            for (i, &reason) in decisions.iter().enumerate() {
                if reason != SplitReason::None {
                    self.split_node(leaf_indices[i])?;
                    match reason {
                        SplitReason::Band => self.split_by_band += 1,
                        SplitReason::Boundary => self.split_by_boundary += 1,
                        SplitReason::AmbiguousW => self.split_by_ambiguous_w += 1,
                        SplitReason::None => {}
                    }
                }
            }
        }

        // Apply 2:1 balancing
        let bal_splits = self.enforce_2to1_balance()?;
        self.split_by_balance += bal_splits;

        Ok(())
    }

    /// Enforce 2:1 balancing (restricted octree) across shared faces and edges.
    pub fn enforce_2to1_balance(&mut self) -> Result<usize, String> {
        let md = self.max_depth;
        let mut total_balance_splits = 0usize;

        loop {
            let mut to_split = HashSet::new();

            // Check all leaves
            for i in 0..self.nodes.len() {
                if !self.nodes[i].is_leaf() {
                    continue;
                }

                let d = self.nodes[i].depth;
                if d <= 1 {
                    continue;
                }

                let shift = (md - d) as u32;
                let step = 1u32 << shift;
                let x0 = self.nodes[i].x << shift;
                let y0 = self.nodes[i].y << shift;
                let z0 = self.nodes[i].z << shift;
                let x1 = x0 + step;
                let y1 = y0 + step;
                let z1 = z0 + step;
                let xm = x0 + (step >> 1);
                let ym = y0 + (step >> 1);
                let zm = z0 + (step >> 1);

                let max_coord = 1u32 << md;

                // 6 Face neighbors
                let face_pts = [
                    (x0 > 0).then(|| (x0 - 1, ym, zm)),
                    (x1 < max_coord).then(|| (x1, ym, zm)),
                    (y0 > 0).then(|| (xm, y0 - 1, zm)),
                    (y1 < max_coord).then(|| (xm, y1, zm)),
                    (z0 > 0).then(|| (xm, ym, z0 - 1)),
                    (z1 < max_coord).then(|| (xm, ym, z1)),
                ];

                for opt_pt in face_pts.into_iter().flatten() {
                    let nb_idx = self.find_leaf_at(opt_pt.0, opt_pt.1, opt_pt.2);
                    if self.nodes[nb_idx].depth + 1 < d {
                        to_split.insert(nb_idx);
                    }
                }

                // 12 Edge neighbors
                let edge_pts = [
                    // Along Z
                    (x0 > 0 && y0 > 0).then(|| (x0 - 1, y0 - 1, zm)),
                    (x1 < max_coord && y0 > 0).then(|| (x1, y0 - 1, zm)),
                    (x0 > 0 && y1 < max_coord).then(|| (x0 - 1, y1, zm)),
                    (x1 < max_coord && y1 < max_coord).then(|| (x1, y1, zm)),
                    // Along Y
                    (x0 > 0 && z0 > 0).then(|| (x0 - 1, ym, z0 - 1)),
                    (x1 < max_coord && z0 > 0).then(|| (x1, ym, z0 - 1)),
                    (x0 > 0 && z1 < max_coord).then(|| (x0 - 1, ym, z1)),
                    (x1 < max_coord && z1 < max_coord).then(|| (x1, ym, z1)),
                    // Along X
                    (y0 > 0 && z0 > 0).then(|| (xm, y0 - 1, z0 - 1)),
                    (y1 < max_coord && z0 > 0).then(|| (xm, y1, z0 - 1)),
                    (y0 > 0 && z1 < max_coord).then(|| (xm, y0 - 1, z1)),
                    (y1 < max_coord && z1 < max_coord).then(|| (xm, y1, z1)),
                ];

                for opt_pt in edge_pts.into_iter().flatten() {
                    let nb_idx = self.find_leaf_at(opt_pt.0, opt_pt.1, opt_pt.2);
                    if self.nodes[nb_idx].depth + 1 < d {
                        to_split.insert(nb_idx);
                    }
                }
            }

            if to_split.is_empty() {
                break;
            }

            // Split violating neighbors deterministically
            let mut sorted_splits: Vec<usize> = to_split.into_iter().collect();
            sorted_splits.sort_unstable();

            for idx in sorted_splits {
                self.split_node(idx)?;
                total_balance_splits += 1;
            }
        }

        Ok(total_balance_splits)
    }

    /// Return all active leaf indices sorted canonically by Morton code.
    pub fn sorted_leaves(&self) -> Vec<usize> {
        let md = self.max_depth;
        let mut leaves: Vec<(u64, usize)> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.is_leaf())
            .map(|(idx, n)| {
                let shift = (md - n.depth) as u32;
                let morton = morton_encode(n.x << shift, n.y << shift, n.z << shift);
                (morton, idx)
            })
            .collect();

        leaves.sort_by_key(|&(m, _)| m);
        leaves.into_iter().map(|(_, idx)| idx).collect()
    }

    /// Total node count.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Total leaf count.
    #[inline]
    pub fn leaf_count(&self) -> usize {
        self.nodes.iter().filter(|n| n.is_leaf()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn morton_roundtrip_order() {
        let m0 = morton_encode(0, 0, 0);
        let m1 = morton_encode(1, 0, 0);
        let m2 = morton_encode(0, 1, 0);
        let m3 = morton_encode(1, 1, 0);
        assert!(m0 < m1);
        assert!(m1 < m2);
        assert!(m2 < m3);
    }

    #[test]
    fn octree_split_and_find_leaf() {
        let bbox = Aabb {
            min: [0.0, 0.0, 0.0],
            max: [10.0, 10.0, 10.0],
        };
        let mut tree = Octree::new(&bbox, 1.0, Some(10_000)).unwrap();
        assert_eq!(tree.node_count(), 1);
        assert_eq!(tree.leaf_count(), 1);

        tree.split_node(0).unwrap();
        assert_eq!(tree.node_count(), 9);
        assert_eq!(tree.leaf_count(), 8);

        let md = tree.max_depth;
        let max_coord = 1u32 << md;
        let leaf = tree.find_leaf_at(max_coord / 4, max_coord / 4, max_coord / 4);
        assert!(tree.nodes[leaf].is_leaf());
        assert_eq!(tree.nodes[leaf].depth, 1);
    }
}
