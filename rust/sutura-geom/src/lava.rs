//! Lava: Adaptive octree dual contouring for watertight mesh repair.
//!
//! Reconstructs a watertight 2-manifold surface using a discrete octree sizing
//! field and dual contouring with Hermite QEF minimizers.
//!
//! Key properties:
//! - Narrow-band refinement along the surface and hole membranes.
//! - Generalized Winding Number (GWN) 0.5 level set forms a smooth harmonic
//!   membrane bridging holes.
//! - Deterministic Morton-ordered 2:1 balanced octree (restricted octree).
//! - Crack-free minimal-edge dual face assembly across variable octree depths.
//! - Manifold topology verification with `mesh_is_manifold`.

use crate::dual_contour::{jacobi_eigen, mesh_is_manifold};
use crate::octree::{MeshBoundaries, Octree};
use crate::winding::{Aabb, MeshBvh};
use numpy::{AllowTypeChange, PyArray2, PyArrayLike2};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// Regularised QEF minimiser using Truncated SVD.
///
/// Matrix `a` is the symmetric normal outer-product tensor `sum(n_i * n_i^T)`.
/// Vector `b` is `sum(n_i * (n_i . p_i))`.
/// `mass` is the mean Hermite point (centroid of edge intersections).
/// `tau` is the SVD cutoff ratio relative to max eigenvalue (e.g. 0.05).
///
/// Along under-determined directions (eigenvalues < tau * lambda_max),
/// the minimizer projects directly onto `mass` instead of inverting near-zero noise.
/// The resulting point is then clamped inside [cell_min, cell_max] along the ray from mass.
pub(crate) fn solve_qef_svd(
    a: &[[f64; 3]; 3],
    b: &[f64; 3],
    mass: [f64; 3],
    cell_min: [f64; 3],
    cell_max: [f64; 3],
    tau: f64,
) -> [f64; 3] {
    let (evals, evecs) = jacobi_eigen(a);
    let e0 = evals[0].max(0.0);
    let e1 = evals[1].max(0.0);
    let e2 = evals[2].max(0.0);
    let max_ev = e0.max(e1).max(e2);
    if max_ev < 1e-12 {
        return mass;
    }

    let thresh = tau * max_ev;
    let evals_clean = [e0, e1, e2];
    let mut x = [0.0f64; 3];

    for k in 0..3 {
        let v = [evecs[0][k], evecs[1][k], evecs[2][k]];
        let ev = evals_clean[k];
        let coef = if ev >= thresh {
            (v[0] * b[0] + v[1] * b[1] + v[2] * b[2]) / ev
        } else {
            v[0] * mass[0] + v[1] * mass[1] + v[2] * mass[2]
        };
        for i in 0..3 {
            x[i] += coef * v[i];
        }
    }

    for d in 0..3 {
        if !x[d].is_finite() {
            return mass;
        }
    }

    // Clamp along ray from mass to x to avoid flattening onto cell boundary walls
    let mut t = 1.0f64;
    for d in 0..3 {
        if x[d] < cell_min[d] {
            let diff = mass[d] - x[d];
            if diff > 1e-12 {
                t = t.min((mass[d] - cell_min[d]) / diff);
            } else {
                t = 0.0;
            }
        } else if x[d] > cell_max[d] {
            let diff = x[d] - mass[d];
            if diff > 1e-12 {
                t = t.min((cell_max[d] - mass[d]) / diff);
            } else {
                t = 0.0;
            }
        }
    }
    t = t.clamp(0.0, 1.0);

    [
        mass[0] + t * (x[0] - mass[0]),
        mass[1] + t * (x[1] - mass[1]),
        mass[2] + t * (x[2] - mass[2]),
    ]
}

/// Minimal edge key on the octree at finest resolution `max_depth`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EdgeKey {
    /// Direction: 0 = along X, 1 = along Y, 2 = along Z.
    pub axis: u8,
    /// Minimum corner integer coordinates at resolution `max_depth`.
    pub x: u32,
    pub y: u32,
    pub z: u32,
    /// Length of the edge in integer units at resolution `max_depth`.
    pub len: u32,
}

/// Mesh output from dual contouring.
pub struct LavaMesh {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[u32; 3]>,
    pub nodes: usize,
    pub leaves: usize,
    pub corners: usize,
    pub peak_memory_est_mb: f64,
    pub seconds_build: f64,
    pub seconds_field: f64,
    pub seconds_contour: f64,
    pub seconds_total: f64,
    pub manifold: bool,
    pub h_min: f64,
    pub max_depth: u8,
    pub split_by_band: usize,
    pub split_by_boundary: usize,
    pub split_by_ambiguous_w: usize,
    pub split_by_balance: usize,
}

/// Single-pass adaptive octree dual contouring.
///
/// If `closing_pass` is true, the scalar field uses `sign * d + r` (erosion pass).
/// Otherwise it uses `sign * d - r` (normal / dilation pass).
pub fn lava_octree_single_pass(
    bvh: &MeshBvh,
    mesh_bbox: &Aabb,
    boundaries: &MeshBoundaries,
    h_min: f64,
    r: f64,
    closing_pass: bool,
    max_nodes: Option<usize>,
) -> Result<LavaMesh, String> {
    let t_total_start = Instant::now();

    // 1. Build 2:1 balanced adaptive octree
    let t_build_start = Instant::now();
    let mut tree = Octree::new(mesh_bbox, h_min, max_nodes)?;
    tree.build_narrow_band(bvh, mesh_bbox, boundaries, r)?;
    let seconds_build = t_build_start.elapsed().as_secs_f64();

    dual_contour_octree(&tree, bvh, boundaries, r, closing_pass, seconds_build, t_total_start)
}

/// Dual contouring extraction on an existing 2:1 balanced octree.
pub fn dual_contour_octree(
    tree: &Octree,
    bvh: &MeshBvh,
    boundaries: &MeshBoundaries,
    r: f64,
    closing_pass: bool,
    seconds_build: f64,
    t_total_start: Instant,
) -> Result<LavaMesh, String> {
    let node_count = tree.node_count();
    let split_by_band = tree.split_by_band;
    let split_by_boundary = tree.split_by_boundary;
    let split_by_ambiguous_w = tree.split_by_ambiguous_w;
    let split_by_balance = tree.split_by_balance;
    let leaf_count = tree.leaf_count();
    let md = tree.max_depth;
    let finest_h = tree.cell_size(md);

    // 2. Identify unique corners of active leaves
    let t_field_start = Instant::now();
    let sorted_leaves = tree.sorted_leaves();

    // Corner map: (X, Y, Z) integer grid coords at resolution md -> corner index
    let mut corner_map: HashMap<(u32, u32, u32), usize> = HashMap::with_capacity(leaf_count * 4);
    let mut corner_coords: Vec<(u32, u32, u32)> = Vec::with_capacity(leaf_count * 4);

    // Leaf corner indices: for each leaf, the 8 corner indices in corner_coords
    let mut leaf_corners: Vec<[usize; 8]> = Vec::with_capacity(sorted_leaves.len());

    for &l_idx in &sorted_leaves {
        let node = &tree.nodes[l_idx];
        let shift = (md - node.depth) as u32;
        let step = 1u32 << shift;
        let x0 = node.x << shift;
        let y0 = node.y << shift;
        let z0 = node.z << shift;

        let mut c_indices = [0usize; 8];
        for octant in 0..8 {
            let cx = x0 + ((octant & 1) as u32) * step;
            let cy = y0 + (((octant >> 1) & 1) as u32) * step;
            let cz = z0 + (((octant >> 2) & 1) as u32) * step;

            let next_idx = corner_coords.len();
            let idx = *corner_map.entry((cx, cy, cz)).or_insert_with(|| {
                corner_coords.push((cx, cy, cz));
                next_idx
            });
            c_indices[octant] = idx;
        }
        leaf_corners.push(c_indices);
    }

    let corner_count = corner_coords.len();

    // Evaluate corner fields in parallel
    // Returns (F, winding, unsigned_dist, world_pos)
    let corner_data: Vec<(f64, f64, f64, [f64; 3])> = corner_coords
        .par_iter()
        .map(|&(cx, cy, cz)| {
            let p = [
                tree.origin[0] + cx as f64 * finest_h,
                tree.origin[1] + cy as f64 * finest_h,
                tree.origin[2] + cz as f64 * finest_h,
            ];
            let dist = bvh.unsigned_distance_at(p);
            let in_band = dist <= (r + 2.0 * finest_h) || boundaries.is_within_dist(p, 4.0 * finest_h);
            let w = if in_band {
                bvh.winding_at(p)
            } else {
                bvh.winding_at_tol(p, 0.5)
            };
            let sign = if w >= 0.5 { -1.0 } else { 1.0 };

            let f = if closing_pass {
                sign * dist + r
            } else {
                sign * dist - r
            };

            (f, w, dist, p)
        })
        .collect();

    let seconds_field = t_field_start.elapsed().as_secs_f64();

    // 3. Dual contouring: QEF vertex placement per sign-change leaf
    let t_contour_start = Instant::now();

    // Local edge definitions for an octree cell (corners 0..7)
    const CELL_EDGES: [[usize; 2]; 12] = [
        [0, 1], [2, 3], [4, 5], [6, 7], // along X (edges 0..3)
        [0, 2], [1, 3], [4, 6], [5, 7], // along Y (edges 4..7)
        [0, 4], [1, 5], [2, 6], [3, 7], // along Z (edges 8..11)
    ];

    const FACE_CORNERS: [[usize; 4]; 6] = [
        [0, 2, 6, 4], // -x
        [1, 3, 7, 5], // +x
        [0, 1, 5, 4], // -y
        [2, 3, 7, 6], // +y
        [0, 1, 3, 2], // -z
        [4, 5, 7, 6], // +z
    ];

    const FACE_EDGES: [[usize; 4]; 6] = [
        [4, 10, 6, 8],
        [5, 11, 7, 9],
        [0, 9, 2, 8],
        [1, 11, 3, 10],
        [0, 5, 1, 4],
        [2, 7, 3, 6],
    ];

    // Compute dual vertices for sign-changing leaves in parallel with Rayon.
    // Emits one QEF vertex per surface component of the cell (Manifold Dual Contouring).
    let leaf_qef_results: Vec<(usize, Vec<[f64; 3]>, [i32; 12])> = (0..sorted_leaves.len())
        .into_par_iter()
        .map(|leaf_pos| {
            let l_idx = sorted_leaves[leaf_pos];
            let c_idx = &leaf_corners[leaf_pos];

            let mut present = [false; 12];
            let mut any_present = false;

            for e in 0..12 {
                let ci0 = c_idx[CELL_EDGES[e][0]];
                let ci1 = c_idx[CELL_EDGES[e][1]];
                let f0 = corner_data[ci0].0;
                let f1 = corner_data[ci1].0;
                let s = (f0 <= 0.0 && f1 > 0.0) || (f0 > 0.0 && f1 <= 0.0);
                present[e] = s;
                any_present |= s;
            }

            if !any_present {
                return (l_idx, Vec::new(), [-1; 12]);
            }

            let node = &tree.nodes[l_idx];
            let h_cell = tree.cell_size(node.depth);
            let cell_min = tree.cell_min(node.x, node.y, node.z, node.depth);
            let cell_max = [
                cell_min[0] + h_cell,
                cell_min[1] + h_cell,
                cell_min[2] + h_cell,
            ];

            // Union-find over the 12 local edges using the 6 faces
            let mut parent = [0usize, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
            fn find(parent: &mut [usize; 12], mut x: usize) -> usize {
                while parent[x] != x {
                    parent[x] = parent[parent[x]];
                    x = parent[x];
                }
                x
            }
            let union = |parent: &mut [usize; 12], a: usize, b: usize| {
                let ra = find(parent, a);
                let rb = find(parent, b);
                if ra != rb {
                    parent[ra] = rb;
                }
            };

            for face in 0..6 {
                let fe = FACE_EDGES[face];
                let pc = FACE_CORNERS[face];
                let act: Vec<usize> = fe.iter().copied().filter(|&e| present[e]).collect();
                match act.len() {
                    0 | 1 => {}
                    2 => union(&mut parent, act[0], act[1]),
                    4 => {
                        // Asymptotic decider (Nielson & Hamann):
                        let v0 = corner_data[c_idx[pc[0]]].0;
                        let v1 = corner_data[c_idx[pc[1]]].0;
                        let v2 = corner_data[c_idx[pc[2]]].0;
                        let v3 = corner_data[c_idx[pc[3]]].0;
                        let s = v0 * v2 - v1 * v3;
                        if s > 0.0 {
                            union(&mut parent, fe[0], fe[1]);
                            union(&mut parent, fe[2], fe[3]);
                        } else {
                            union(&mut parent, fe[1], fe[2]);
                            union(&mut parent, fe[3], fe[0]);
                        }
                    }
                    _ => {}
                }
            }

            // Group edges by component root
            let mut comp_of_edge = [-1i32; 12];
            let mut root_to_comp = [-1i32; 12];
            let mut comp_edges: Vec<Vec<usize>> = Vec::new();

            for e in 0..12 {
                if !present[e] {
                    continue;
                }
                let r = find(&mut parent, e);
                if root_to_comp[r] < 0 {
                    root_to_comp[r] = comp_edges.len() as i32;
                    comp_edges.push(Vec::new());
                }
                let cid = root_to_comp[r] as usize;
                comp_of_edge[e] = cid as i32;
                comp_edges[cid].push(e);
            }

            // Compute Hermite point and normal for each present edge
            let mut edge_hermite: [Option<([f64; 3], [f64; 3])>; 12] = [None; 12];
            for e in 0..12 {
                if !present[e] {
                    continue;
                }
                let [e0, e1] = CELL_EDGES[e];
                let ci0 = c_idx[e0];
                let ci1 = c_idx[e1];
                let (f0, w0, d0, p0) = corner_data[ci0];
                let (f1, w1, d1, p1) = corner_data[ci1];

                let edge_len = ((p1[0] - p0[0]).powi(2)
                    + (p1[1] - p0[1]).powi(2)
                    + (p1[2] - p0[2]).powi(2))
                .sqrt();

                let is_hole_membrane = d0 > 0.5 * edge_len
                    && d1 > 0.5 * edge_len
                    && (w0 - 0.5) * (w1 - 0.5) <= 0.0
                    && (w0 - w1).abs() > 1e-6;

                let (p_int, normal) = if is_hole_membrane {
                    let mut t0 = 0.0f64;
                    let mut t1 = 1.0f64;
                    let mut p_mid = [0.0; 3];

                    for _ in 0..8 {
                        let t = (t0 + t1) * 0.5;
                        p_mid = [
                            p0[0] + t * (p1[0] - p0[0]),
                            p0[1] + t * (p1[1] - p0[1]),
                            p0[2] + t * (p1[2] - p0[2]),
                        ];
                        let w_mid = bvh.winding_at(p_mid);
                        if (w0 - 0.5) * (w_mid - 0.5) <= 0.0 {
                            t1 = t;
                        } else {
                            t0 = t;
                        }
                    }

                    let eps = 1e-4 * tree.h_min.max(1e-4);
                    let gw = [
                        (bvh.winding_at([p_mid[0] + eps, p_mid[1], p_mid[2]])
                            - bvh.winding_at([p_mid[0] - eps, p_mid[1], p_mid[2]]))
                            / (2.0 * eps),
                        (bvh.winding_at([p_mid[0], p_mid[1] + eps, p_mid[2]])
                            - bvh.winding_at([p_mid[0], p_mid[1] - eps, p_mid[2]]))
                            / (2.0 * eps),
                        (bvh.winding_at([p_mid[0], p_mid[1], p_mid[2] + eps])
                            - bvh.winding_at([p_mid[0], p_mid[1], p_mid[2] - eps]))
                            / (2.0 * eps),
                    ];
                    let glen = (gw[0] * gw[0] + gw[1] * gw[1] + gw[2] * gw[2]).sqrt();
                    let n = if glen > 1e-9 {
                        [-gw[0] / glen, -gw[1] / glen, -gw[2] / glen]
                    } else {
                        let diff = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
                        let dl = (diff[0] * diff[0] + diff[1] * diff[1] + diff[2] * diff[2]).sqrt();
                        if dl > 1e-12 {
                            [diff[0] / dl, diff[1] / dl, diff[2] / dl]
                        } else {
                            [0.0, 0.0, 1.0]
                        }
                    };
                    (p_mid, n)
                } else {
                    let denom = f1 - f0;
                    let t = if denom.abs() > 1e-12 {
                        (-f0 / denom).clamp(0.0, 1.0)
                    } else {
                        0.5
                    };
                    let p_int = [
                        p0[0] + t * (p1[0] - p0[0]),
                        p0[1] + t * (p1[1] - p0[1]),
                        p0[2] + t * (p1[2] - p0[2]),
                    ];
                    let (closest, _d, fid) = bvh.closest_point(p_int);
                    let n = bvh.tri_normal(fid);
                    (closest, n)
                };

                edge_hermite[e] = Some((p_int, normal));
            }

            // Solve a separate QEF for each surface component of this cell
            let mut comp_verts: Vec<[f64; 3]> = Vec::with_capacity(comp_edges.len());

            for edges in &comp_edges {
                let mut a_mat = [[0.0f64; 3]; 3];
                let mut b_vec = [0.0f64; 3];
                let mut p_sum = [0.0f64; 3];
                let count = edges.len();

                for &e in edges {
                    if let Some((p_int, normal)) = edge_hermite[e] {
                        for i in 0..3 {
                            for j in 0..3 {
                                a_mat[i][j] += normal[i] * normal[j];
                            }
                            b_vec[i] += normal[i] * (normal[0] * p_int[0] + normal[1] * p_int[1] + normal[2] * p_int[2]);
                            p_sum[i] += p_int[i];
                        }
                    }
                }

                if count == 0 {
                    continue;
                }

                let mass = [
                    p_sum[0] / count as f64,
                    p_sum[1] / count as f64,
                    p_sum[2] / count as f64,
                ];

                let x_star = solve_qef_svd(&a_mat, &b_vec, mass, cell_min, cell_max, 0.05);
                comp_verts.push(x_star);
            }

            (l_idx, comp_verts, comp_of_edge)
        })
        .collect();


    let mut out_verts: Vec<[f64; 3]> = Vec::with_capacity(leaf_count * 2);
    let mut leaf_edge_vert: HashMap<(usize, usize), u32> = HashMap::with_capacity(leaf_count * 4);
    let mut leaf_face_vert: HashMap<(usize, usize), u32> = HashMap::with_capacity(leaf_count * 2);
    let mut leaf_default_vert: HashMap<usize, u32> = HashMap::with_capacity(leaf_count);

    for (l_idx, comp_verts, comp_of_edge) in leaf_qef_results {
        if comp_verts.is_empty() {
            continue;
        }
        let base_vid = out_verts.len() as u32;
        for &v in &comp_verts {
            out_verts.push(v);
        }
        leaf_default_vert.insert(l_idx, base_vid);

        for e in 0..12 {
            let cid = comp_of_edge[e];
            if cid >= 0 && (cid as usize) < comp_verts.len() {
                let vid = base_vid + cid as u32;
                leaf_edge_vert.insert((l_idx, e), vid);
            }
        }

        for face in 0..6 {
            for &e in &FACE_EDGES[face] {
                let cid = comp_of_edge[e];
                if cid >= 0 && (cid as usize) < comp_verts.len() {
                    let vid = base_vid + cid as u32;
                    leaf_face_vert.entry((l_idx, face)).or_insert(vid);
                    break;
                }
            }
        }
    }

    // 4. Minimal-edge face generation across 2:1 balanced transitions
    let mut seen_edges: HashSet<EdgeKey> = HashSet::with_capacity(leaf_count * 2);
    let mut out_tris: Vec<[u32; 3]> = Vec::with_capacity(out_verts.len() * 2);

    for (leaf_pos, &l_idx) in sorted_leaves.iter().enumerate() {
        let node = &tree.nodes[l_idx];
        let d = node.depth;
        let shift = (md - d) as u32;
        let step = 1u32 << shift;
        let x0 = node.x << shift;
        let y0 = node.y << shift;
        let z0 = node.z << shift;
        let x1 = x0 + step;
        let y1 = y0 + step;
        let z1 = z0 + step;

        let c_idx = &leaf_corners[leaf_pos];

        // 12 edges for this leaf
        // Format: (axis, x_min, y_min, z_min, len, corner_start, corner_end)
        let edges = [
            // Along X (axis = 0)
            (0u8, x0, y0, z0, step, c_idx[0], c_idx[1]),
            (0u8, x0, y1, z0, step, c_idx[2], c_idx[3]),
            (0u8, x0, y0, z1, step, c_idx[4], c_idx[5]),
            (0u8, x0, y1, z1, step, c_idx[6], c_idx[7]),
            // Along Y (axis = 1)
            (1u8, x0, y0, z0, step, c_idx[0], c_idx[2]),
            (1u8, x1, y0, z0, step, c_idx[1], c_idx[3]),
            (1u8, x0, y0, z1, step, c_idx[4], c_idx[6]),
            (1u8, x1, y0, z1, step, c_idx[5], c_idx[7]),
            // Along Z (axis = 2)
            (2u8, x0, y0, z0, step, c_idx[0], c_idx[4]),
            (2u8, x1, y0, z0, step, c_idx[1], c_idx[5]),
            (2u8, x0, y1, z0, step, c_idx[2], c_idx[6]),
            (2u8, x1, y1, z0, step, c_idx[3], c_idx[7]),
        ];

        for (axis, ex, ey, ez, elen, cs, ce) in edges {
            let key = EdgeKey {
                axis,
                x: ex,
                y: ey,
                z: ez,
                len: elen,
            };

            if !seen_edges.insert(key) {
                continue;
            }

            let f_start = corner_data[cs].0;
            let f_end = corner_data[ce].0;

            // Only process sign-changing edges
            if (f_start <= 0.0 && f_end <= 0.0) || (f_start > 0.0 && f_end > 0.0) {
                continue;
            }

            // Verify if edge is minimal at depth d:
            // Check if any incident quadrant contains a leaf with depth > d.
            // If so, the edge is subdivided into smaller edges and handled at finer depth.
            let max_coord = 1u32 << md;
            let half = elen >> 1;

            let (quad_leaves, is_minimal) = match axis {
                // Along Z: quadrants around (ex, ey)
                2 => {
                    let q0 = (ex < max_coord && ey < max_coord).then(|| tree.find_leaf_at(ex, ey, ez + half));
                    let q1 = (ex > 0 && ey < max_coord).then(|| tree.find_leaf_at(ex - 1, ey, ez + half));
                    let q2 = (ex > 0 && ey > 0).then(|| tree.find_leaf_at(ex - 1, ey - 1, ez + half));
                    let q3 = (ex < max_coord && ey > 0).then(|| tree.find_leaf_at(ex, ey - 1, ez + half));

                    let leaves = [q0, q1, q2, q3];
                    let not_minimal = leaves.into_iter().flatten().any(|l| tree.nodes[l].depth > d);
                    (leaves, !not_minimal)
                }
                // Along Y: quadrants around (ex, ez)
                1 => {
                    let q0 = (ex < max_coord && ez < max_coord).then(|| tree.find_leaf_at(ex, ey + half, ez));
                    let q1 = (ex > 0 && ez < max_coord).then(|| tree.find_leaf_at(ex - 1, ey + half, ez));
                    let q2 = (ex > 0 && ez > 0).then(|| tree.find_leaf_at(ex - 1, ey + half, ez - 1));
                    let q3 = (ex < max_coord && ez > 0).then(|| tree.find_leaf_at(ex, ey + half, ez - 1));

                    let leaves = [q0, q1, q2, q3];
                    let not_minimal = leaves.into_iter().flatten().any(|l| tree.nodes[l].depth > d);
                    (leaves, !not_minimal)
                }
                // Along X: quadrants around (ey, ez)
                _ => {
                    let q0 = (ey < max_coord && ez < max_coord).then(|| tree.find_leaf_at(ex + half, ey, ez));
                    let q1 = (ey > 0 && ez < max_coord).then(|| tree.find_leaf_at(ex + half, ey - 1, ez));
                    let q2 = (ey > 0 && ez > 0).then(|| tree.find_leaf_at(ex + half, ey - 1, ez - 1));
                    let q3 = (ey < max_coord && ez > 0).then(|| tree.find_leaf_at(ex + half, ey, ez - 1));

                    let leaves = [q0, q1, q2, q3];
                    let not_minimal = leaves.into_iter().flatten().any(|l| tree.nodes[l].depth > d);
                    (leaves, !not_minimal)
                }
            };

            if !is_minimal {
                continue;
            }

            // Gather dual vertex indices in cyclic order
            let mut cycle_verts = Vec::with_capacity(4);
            let mut cycle_leaves = Vec::with_capacity(4);
            for opt_leaf in quad_leaves {
                if let Some(leaf_id) = opt_leaf {
                    let node = &tree.nodes[leaf_id];
                    let shift = (md - node.depth) as u32;
                    let step = 1u32 << shift;
                    let x0 = node.x << shift;
                    let y0 = node.y << shift;
                    let z0 = node.z << shift;
                    let x1 = x0 + step;
                    let y1 = y0 + step;
                    let z1 = z0 + step;

                    let opt_local_edge = match axis {
                        0 => {
                            if ey == y0 && ez == z0 { Some(0) }
                            else if ey == y1 && ez == z0 { Some(1) }
                            else if ey == y0 && ez == z1 { Some(2) }
                            else if ey == y1 && ez == z1 { Some(3) }
                            else { None }
                        }
                        1 => {
                            if ex == x0 && ez == z0 { Some(4) }
                            else if ex == x1 && ez == z0 { Some(5) }
                            else if ex == x0 && ez == z1 { Some(6) }
                            else if ex == x1 && ez == z1 { Some(7) }
                            else { None }
                        }
                        _ => {
                            if ex == x0 && ey == y0 { Some(8) }
                            else if ex == x1 && ey == y0 { Some(9) }
                            else if ex == x0 && ey == y1 { Some(10) }
                            else if ex == x1 && ey == y1 { Some(11) }
                            else { None }
                        }
                    };

                    let opt_vid = if let Some(e) = opt_local_edge {
                        leaf_edge_vert.get(&(leaf_id, e)).copied().or_else(|| leaf_default_vert.get(&leaf_id).copied())
                    } else {
                        let opt_face = match axis {
                            0 => if ez == z0 { Some(4) } else if ez == z1 { Some(5) } else if ey == y0 { Some(2) } else if ey == y1 { Some(3) } else { None },
                            1 => if ex == x0 { Some(0) } else if ex == x1 { Some(1) } else if ez == z0 { Some(4) } else if ez == z1 { Some(5) } else { None },
                            _ => if ex == x0 { Some(0) } else if ex == x1 { Some(1) } else if ey == y0 { Some(2) } else if ey == y1 { Some(3) } else { None },
                        };
                        opt_face
                            .and_then(|f| leaf_face_vert.get(&(leaf_id, f)).copied())
                            .or_else(|| leaf_default_vert.get(&leaf_id).copied())
                    };

                    if let Some(vid) = opt_vid {
                        if cycle_verts.last() != Some(&vid) {
                            cycle_verts.push(vid);
                            cycle_leaves.push(leaf_id);
                        }
                    }
                }
            }
            if cycle_verts.len() > 1 && cycle_verts.first() == cycle_verts.last() {
                cycle_verts.pop();
                cycle_leaves.pop();
            }

            // Orient polygons consistently: outward normal follows field gradient
            let reverse_orient = match axis {
                0 => f_end < f_start,
                1 => f_end > f_start,
                _ => f_end < f_start,
            };

            if cycle_verts.len() == 4 {
                let (v0, v1, v2, v3) = (
                    cycle_verts[0],
                    cycle_verts[1],
                    cycle_verts[2],
                    cycle_verts[3],
                );
                let p0 = out_verts[v0 as usize];
                let p1 = out_verts[v1 as usize];
                let p2 = out_verts[v2 as usize];
                let p3 = out_verts[v3 as usize];
                let d02 = (p0[0] - p2[0]).powi(2) + (p0[1] - p2[1]).powi(2) + (p0[2] - p2[2]).powi(2);
                let d13 = (p1[0] - p3[0]).powi(2) + (p1[1] - p3[1]).powi(2) + (p1[2] - p3[2]).powi(2);

                let d0 = tree.nodes[cycle_leaves[0]].depth;
                let d1 = tree.nodes[cycle_leaves[1]].depth;
                let d2 = tree.nodes[cycle_leaves[2]].depth;
                let d3 = tree.nodes[cycle_leaves[3]].depth;
                let coarse_02 = d0 < md && d2 < md;
                let coarse_13 = d1 < md && d3 < md;

                // When two coarse cells meet fine cells across a 2:1 transition,
                // splitting along the coarse diagonal duplicates the chord across both half-edges,
                // creating a non-manifold edge with degree 4. Always split along the fine diagonal.
                let split_02 = if coarse_02 && !coarse_13 {
                    false
                } else if coarse_13 && !coarse_02 {
                    true
                } else {
                    d02 <= d13
                };

                if split_02 {
                    if reverse_orient {
                        out_tris.push([v0, v3, v2]);
                        out_tris.push([v0, v2, v1]);
                    } else {
                        out_tris.push([v0, v1, v2]);
                        out_tris.push([v0, v2, v3]);
                    }
                } else {
                    if reverse_orient {
                        out_tris.push([v1, v0, v3]);
                        out_tris.push([v1, v3, v2]);
                    } else {
                        out_tris.push([v1, v2, v3]);
                        out_tris.push([v1, v3, v0]);
                    }
                }
            } else if cycle_verts.len() == 3 {
                let (v0, v1, v2) = (cycle_verts[0], cycle_verts[1], cycle_verts[2]);
                if reverse_orient {
                    out_tris.push([v0, v2, v1]);
                } else {
                    out_tris.push([v0, v1, v2]);
                }
            }
        }
    }

    let seconds_contour = t_contour_start.elapsed().as_secs_f64();
    let seconds_total = t_total_start.elapsed().as_secs_f64();

    // Verify 2-manifold status
    let manifold = mesh_is_manifold(&out_verts, &out_tris);

    // Peak memory estimate: nodes (48 B) + corners (32 B) + maps/trees
    let est_bytes = (node_count * 48)
        + (corner_count * 32)
        + (leaf_count * 64)
        + (out_verts.len() * 24)
        + (out_tris.len() * 12);
    let peak_memory_est_mb = est_bytes as f64 / (1024.0 * 1024.0);

    Ok(LavaMesh {
        verts: out_verts,
        tris: out_tris,
        nodes: node_count,
        leaves: leaf_count,
        corners: corner_count,
        peak_memory_est_mb,
        seconds_build,
        seconds_field,
        seconds_contour,
        seconds_total,
        manifold,
        h_min: finest_h,
        max_depth: md,
        split_by_band,
        split_by_boundary,
        split_by_ambiguous_w,
        split_by_balance,
    })
}

/// Full Lava Octree repair engine.
///
/// Implements LEAD REVIEW C1:
/// - `r == 0.0`: Single-pass extraction using GWN 0.5 membrane for hole bridging.
/// - `r > 0.0`: Two-pass closing (dilation by `r` $\to$ erosion by `r`).
pub fn lava_octree_core(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    h_min: f64,
    r: f64,
    max_nodes: Option<usize>,
) -> Result<LavaMesh, String> {
    if verts.is_empty() || tris.is_empty() {
        return Err("Input mesh cannot be empty".to_string());
    }

    let bvh = MeshBvh::from_arrays(verts, tris);
    let bbox = bvh.bounds();
    let boundaries = MeshBoundaries::from_mesh(verts, tris);

    if r <= 0.0 {
        // Primary mode: r = 0.0, harmonic GWN 0.5 membrane bridges holes directly
        lava_octree_single_pass(&bvh, &bbox, &boundaries, h_min, 0.0, false, max_nodes)
    } else {
        // C1 Two-pass closing:
        // Pass 1: Dilate (F1 = sign*dist - r)
        let dilated = lava_octree_single_pass(&bvh, &bbox, &boundaries, h_min, r, false, max_nodes)?;

        if dilated.verts.is_empty() || dilated.tris.is_empty() {
            return Err("Dilated pass 1 produced empty surface".to_string());
        }

        // Pass 2: Erode (build MeshBvh on dilated surface, F2 = sign*dist + r)
        let dilated_tris: Vec<[usize; 3]> = dilated
            .tris
            .iter()
            .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
            .collect();
        let dilated_bvh = MeshBvh::from_arrays(&dilated.verts, &dilated_tris);
        let dilated_bbox = dilated_bvh.bounds();
        let dilated_boundaries = MeshBoundaries::from_mesh(&dilated.verts, &dilated_tris);

        let eroded = lava_octree_single_pass(
            &dilated_bvh,
            &dilated_bbox,
            &dilated_boundaries,
            h_min,
            r,
            true, // closing_pass = true -> erode
            max_nodes,
        )?;

        // Aggregate statistics from both passes
        Ok(LavaMesh {
            verts: eroded.verts,
            tris: eroded.tris,
            nodes: dilated.nodes + eroded.nodes,
            leaves: dilated.leaves + eroded.leaves,
            corners: dilated.corners + eroded.corners,
            peak_memory_est_mb: dilated.peak_memory_est_mb.max(eroded.peak_memory_est_mb),
            seconds_build: dilated.seconds_build + eroded.seconds_build,
            seconds_field: dilated.seconds_field + eroded.seconds_field,
            seconds_contour: dilated.seconds_contour + eroded.seconds_contour,
            seconds_total: dilated.seconds_total + eroded.seconds_total,
            manifold: eroded.manifold,
            h_min: eroded.h_min,
            max_depth: eroded.max_depth,
            split_by_band: dilated.split_by_band + eroded.split_by_band,
            split_by_boundary: dilated.split_by_boundary + eroded.split_by_boundary,
            split_by_ambiguous_w: dilated.split_by_ambiguous_w + eroded.split_by_ambiguous_w,
            split_by_balance: dilated.split_by_balance + eroded.split_by_balance,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lava_octree_cube() {
        // Unit cube with 12 triangles
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
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [2, 3, 7],
            [2, 7, 6],
            [1, 2, 6],
            [1, 6, 5],
            [3, 0, 4],
            [3, 4, 7],
        ];

        let mesh = lava_octree_core(&verts, &tris, 0.25, 0.0, Some(100_000)).unwrap();
        assert!(!mesh.verts.is_empty());
        assert!(!mesh.tris.is_empty());
        assert!(mesh.manifold, "Extracted dual contour must be 2-manifold");
    }

    #[test]
    fn lava_octree_bridges_hole() {
        // Cube with missing top face (+Z)
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
            [0, 3, 2],
            // missing: [4, 5, 6], [4, 6, 7]
            [0, 1, 5],
            [0, 5, 4],
            [2, 3, 7],
            [2, 7, 6],
            [1, 2, 6],
            [1, 6, 5],
            [3, 0, 4],
            [3, 4, 7],
        ];

        let mesh = lava_octree_core(&verts, &tris, 0.25, 0.0, Some(100_000)).unwrap();
        assert!(!mesh.verts.is_empty());
        assert!(!mesh.tris.is_empty());
        assert!(mesh.manifold, "GWN 0.5 membrane must close hole into 2-manifold");
    }

    #[test]
    fn lava_octree_two_pass_closing() {
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
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [2, 3, 7],
            [2, 7, 6],
            [1, 2, 6],
            [1, 6, 5],
            [3, 0, 4],
            [3, 4, 7],
        ];

        let mesh = lava_octree_core(&verts, &tris, 0.25, 0.1, Some(100_000)).unwrap();
        assert!(!mesh.verts.is_empty());
        assert!(!mesh.tris.is_empty());
        assert!(mesh.manifold, "Two-pass closed mesh must be 2-manifold");
    }

    #[test]
    fn lava_octree_aborts_on_exceeded_max_nodes() {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
        ];
        let tris = vec![[0, 1, 2]];

        let res = lava_octree_core(&verts, &tris, 0.01, 0.0, Some(5));
        assert!(res.is_err(), "Must abort cleanly when max_nodes is exceeded");
    }

    fn make_sphere(radius: f64, n_lat: usize, n_lon: usize) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let mut verts = Vec::new();
        let mut tris = Vec::new();
        for i in 1..n_lat {
            let theta = std::f64::consts::PI * (i as f64) / (n_lat as f64);
            for j in 0..n_lon {
                let phi = 2.0 * std::f64::consts::PI * (j as f64) / (n_lon as f64);
                verts.push([
                    radius * theta.sin() * phi.cos(),
                    radius * theta.sin() * phi.sin(),
                    radius * theta.cos(),
                ]);
            }
        }
        let top_idx = verts.len();
        verts.push([0.0, 0.0, radius]);
        let bot_idx = verts.len();
        verts.push([0.0, 0.0, -radius]);

        for i in 0..(n_lat - 2) {
            for j in 0..n_lon {
                let j_next = (j + 1) % n_lon;
                let v0 = i * n_lon + j;
                let v1 = i * n_lon + j_next;
                let v2 = (i + 1) * n_lon + j_next;
                let v3 = (i + 1) * n_lon + j;
                tris.push([v0, v2, v1]);
                tris.push([v0, v3, v2]);
            }
        }
        for j in 0..n_lon {
            let j_next = (j + 1) % n_lon;
            tris.push([top_idx, j, j_next]);
            let last_row = (n_lat - 2) * n_lon;
            tris.push([bot_idx, last_row + j_next, last_row + j]);
        }
        (verts, tris)
    }

    fn make_torus(r_major: f64, r_minor: f64, n_u: usize, n_v: usize) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let mut verts = Vec::new();
        let mut tris = Vec::new();
        for i in 0..n_u {
            let u = 2.0 * std::f64::consts::PI * (i as f64) / (n_u as f64);
            for j in 0..n_v {
                let v = 2.0 * std::f64::consts::PI * (j as f64) / (n_v as f64);
                verts.push([
                    (r_major + r_minor * v.cos()) * u.cos(),
                    (r_major + r_minor * v.cos()) * u.sin(),
                    r_minor * v.sin(),
                ]);
            }
        }
        for i in 0..n_u {
            let i_next = (i + 1) % n_u;
            for j in 0..n_v {
                let j_next = (j + 1) % n_v;
                let v0 = i * n_v + j;
                let v1 = i * n_v + j_next;
                let v2 = i_next * n_v + j_next;
                let v3 = i_next * n_v + j;
                tris.push([v0, v2, v1]);
                tris.push([v0, v3, v2]);
            }
        }
        (verts, tris)
    }

    #[test]
    fn lava_octree_sphere_forced_2to1_transition() {
        let (verts, tris) = make_sphere(1.0, 15, 30);
        let bvh = MeshBvh::from_arrays(&verts, &tris);
        let bbox = bvh.bounds();
        let boundaries = MeshBoundaries::empty();

        let mut tree = Octree::new(&bbox, 0.2, Some(100_000)).unwrap();
        let md = tree.max_depth;
        let sqrt3 = 3.0f64.sqrt();

        for d in 0..md {
            let leaf_indices: Vec<usize> = (0..tree.nodes.len())
                .filter(|&i| tree.nodes[i].is_leaf() && tree.nodes[i].depth == d)
                .collect();
            if leaf_indices.is_empty() {
                break;
            }
            let h = tree.cell_size(d);
            let circumradius = (sqrt3 * 0.5) * h + 1e-6;

            for &idx in &leaf_indices {
                let n = &tree.nodes[idx];
                let c = tree.cell_center(n.x, n.y, n.z, n.depth);
                let dist = bvh.unsigned_distance_at(c);
                if dist <= circumradius {
                    // Force 2:1 transition across x = 0: x > 0 refines to md, x <= 0 refines to md - 1
                    if c[0] > 0.0 || d + 1 < md {
                        let _ = tree.split_node(idx);
                    }
                }
            }
        }
        tree.enforce_2to1_balance().unwrap();

        let mesh = dual_contour_octree(&tree, &bvh, &boundaries, 0.0, false, 0.0, Instant::now()).unwrap();
        assert!(!mesh.verts.is_empty());
        assert!(!mesh.tris.is_empty());
        assert!(mesh.manifold, "Dual contouring across forced 2:1 transition on sphere must be 2-manifold");
    }

    #[test]
    fn lava_octree_torus_forced_2to1_transition() {
        let (verts, tris) = make_torus(1.0, 0.4, 40, 20);
        let bvh = MeshBvh::from_arrays(&verts, &tris);
        let bbox = bvh.bounds();
        let boundaries = MeshBoundaries::empty();

        let mut tree = Octree::new(&bbox, 0.08, Some(100_000)).unwrap();
        let md = tree.max_depth;
        let sqrt3 = 3.0f64.sqrt();

        for d in 0..md {
            let leaf_indices: Vec<usize> = (0..tree.nodes.len())
                .filter(|&i| tree.nodes[i].is_leaf() && tree.nodes[i].depth == d)
                .collect();
            if leaf_indices.is_empty() {
                break;
            }
            let h = tree.cell_size(d);
            let circumradius = (sqrt3 * 0.5) * h + 1e-6;

            for &idx in &leaf_indices {
                let n = &tree.nodes[idx];
                let c = tree.cell_center(n.x, n.y, n.z, n.depth);
                let dist = bvh.unsigned_distance_at(c);
                if dist <= circumradius {
                    // Force 2:1 transition across x = 0: x > 0 refines to md, x <= 0 refines to md - 1
                    if c[0] > 0.0 || d + 1 < md {
                        let _ = tree.split_node(idx);
                    }
                }
            }
        }
        tree.enforce_2to1_balance().unwrap();

        let mesh = dual_contour_octree(&tree, &bvh, &boundaries, 0.0, false, 0.0, Instant::now()).unwrap();
        assert!(!mesh.verts.is_empty());
        assert!(!mesh.tris.is_empty());
        assert!(mesh.manifold, "Dual contouring across forced 2:1 transition on torus must be 2-manifold");
    }
}
