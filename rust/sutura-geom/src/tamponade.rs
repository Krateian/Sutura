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

use crate::flap::dp::min_area_triangulation_filtered;
use crate::mincut::BkGraph;
use crate::rbf;
use crate::triangle_intersection;
use crate::winding::MeshBvh;
use faer::linalg::solvers::Solve;
use faer::sparse::linalg::solvers::{Lu, SymbolicLu};
use faer::sparse::{SparseColMat, Triplet};
use faer::Mat;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

/// Boundary loops up to this size are triangulated by the exact O(n^3)
/// minimum-area DP; larger loops use the O(n^2) ear-clipping fallback.
const PLUG_DP_CAP: usize = 300;
/// Plug triangles with an area at or below this (in the mesh's units, squared)
/// are treated as degenerate and cause the loop's plug to be rejected.
const PLUG_MIN_AREA: f64 = 1e-12;
/// A plug triangle whose largest interior angle lies within this many degrees
/// of 180 is a near-collinear sliver and is rejected by the min-area DP. Small
/// enough to leave legitimate fan triangles (whose apex angle is far from 180)
/// untouched, large enough to route the DP around a flat cap's collinear ears.
const PLUG_COLLINEAR_MARGIN_DEG: f64 = 1.0;
/// Boundary loops at or below this vertex count are treated as genuine holes
/// without the min-cut test. A tiny rim cannot separate a meaningful interior
/// from the exterior, and the coarse occupancy grid used by the cut rejects
/// real tiny holes (e.g. artec_copper-key's four 3-vertex rims) as
/// `NoSeparation`.
const TINY_LOOP_MAX: usize = 8;
/// A rim longer than this is split into sub-rims by [`split_long_loops`] before
/// sealing, so a long / curved hole becomes simple enough for the DP cap.
const SPLIT_MAX_VERTS: usize = 64;
/// Minimum vertices each side of a bridge must carry (so a split cannot create
/// a degenerate sub-rim).
const SPLIT_MIN_ARC: usize = 3;
/// Recursion cap for the piecewise split.
const SPLIT_MAX_DEPTH: usize = 16;
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
/// Minimum ring width (small rims keep the historical 3-ring band).
const REF_RING_MAX: usize = 3;
/// The ring width grows with the rim diameter: `rings ~= diameter / (DIV * h)`
/// (one face hop is ~ one median edge `h`), clamped to `[REF_RING_MAX,
/// REF_RING_MAX_CAP]`. A large hole needs a wider band of intact tissue for a
/// stable fit; the cap bounds the scan.
const REF_RING_DIAM_DIV: f64 = 4.0;
const REF_RING_MAX_CAP: usize = 12;
/// Minimum number of distinct intact ring points needed to fit the quadric.
const REF_MIN_POINTS: usize = 8;
/// Ring-face sliver brake: a face whose area is below this fraction of `h^2`
/// is a sliver and does not contribute to the reference.
const REF_SLIVER_AREA_FRAC: f64 = 0.01;
/// A ring whose face normals spread more than this from the mean is too noisy
/// (sharp edge, saddle) and yields `no reference` -- UNLESS the quadric fit is
/// itself smooth (`rms` below `REF_NORMAL_SPREAD_RMS_FRAC * h`), which is the
/// signature of a smooth curved surface (sphere cap, rounded bump) rather than
/// noise.
const REF_NORMAL_SPREAD_DEG: f64 = 60.0;
/// A wide normal spread is tolerated while the fit roughness stays below this
/// fraction of `h`; above it the spread is treated as genuine noise.
const REF_NORMAL_SPREAD_RMS_FRAC: f64 = 0.5;
/// The quadric fit residual (rms over the ring points) must not exceed this
/// multiple of `h`, else the surrounding surface is not locally smooth.
const REF_RMS_FACTOR: f64 = 1.0;
/// Self-consistency brake (no ground truth): the fitted quadric may not
/// extrapolate a sag across the hole radius larger than this multiple of the
/// ring's own height relief plus `h`. A partial rim fragment or a wide ring can
/// otherwise produce a reference whose centre sits well outside the observed
/// tissue, and the plug (which lies ON that reference) becomes a spike that
/// `reference_dev_ok` cannot catch.
const REF_SAG_FACTOR: f64 = 1.25;
/// Laser-ray divergence brake: a tangent ray whose meeting height deviates from
/// the fitted centre by more than this multiple of the ring's own fit roughness
/// (`rms`) counts as being on one side. A saddle sends a *material share* of the
/// rays to both sides and is rejected (`no reference`).
const REF_RAY_SPREAD_FACTOR: f64 = 1.5;
/// Minimum share of the tangent rays that must fall beyond the tolerance on
/// BOTH sides before the reference is rejected as a saddle. A genuine bowl or
/// dome can have one stray ray (a short or overshoot rim neighbour) on the
/// opposite side; a single outlier must not veto a real seal.
const REF_RAY_MIN_SIDE_FRAC: f64 = 0.15;
/// Ridge regularisation of the quadric normal equations (relative to the trace).
const REF_RIDGE: f64 = 1e-6;
// --- RBF fallback (large / curved holes the single quadric cannot represent) --
/// Tikhonov ridge of the thin-plate RBF normal equations (relative to the mean
/// kernel magnitude; `phi(0) = 0` makes the kernel matrix singular without it).
const RBF_RIDGE: f64 = 1e-4;
/// RBF solve size cap: above this many centres the sample set is sub-sampled
/// deterministically (the hand-rolled dense solve is `O(M^3)`).
const RBF_MAX_CENTRES: usize = 600;
/// RBF self-consistency band (no truth): over the hole disk the fitted height
/// may not leave the ring points' own height band by more than this multiple of
/// `(relief + h)`.
const RBF_BAND_K: f64 = 1.0;
/// RBF slope brake (no truth): the max `|grad F|` over the hole disk may not
/// exceed this multiple of the ring's own max height-field slope.
const RBF_GRAD_K: f64 = 2.0;
/// Disk sample resolution of the RBF self-consistency gate (radii x angles).
const RBF_GATE_NR: usize = 4;
const RBF_GATE_NA: usize = 24;
/// The RBF fallback is for LARGE holes only (the brief): below this hole
/// diameter / median-edge ratio the single quadric is kept even when its fit is
/// rejected, so a small flat opening's rim-only plug is unchanged.
const RBF_MIN_HOLE_FACTOR: f64 = 6.0;
/// Trust weight of the hole-rim samples in the ring+rim weighted RBF fallback
/// (the intact-ring samples carry weight 1). A low weight lets the rim pull the
/// extrapolation toward the actual opening without overriding the intact ring.
const RBF_RIM_WEIGHT: f64 = 0.15;
/// Leave-one-out self-consistency band of the saddle RBF fallback. The single
/// quadric is rejected as a saddle when opposite tangent rays predict opposite
/// heights, but a thin-plate RBF can bend both ways and represent it. The RBF is
/// admitted only when dropping each ring sample in turn and predicting it from
/// the rest stays within this band: a genuine saddle's surface is predictable
/// from its own neighbourhood, an over-extrapolated or noisy fit is not. The
/// band is the sampling scale (`0.25 h`): a dropped sample is one edge from its
/// neighbours, so a fit that cannot reproduce it there is not trustworthy over
/// the hole. The band is deliberately expressed against `h`, NOT the quadric fit
/// residual: on a curved hole fragment the quadric `rms` is a measure of the
/// fragment's curvature, not of the sampling budget, and using `2 * rms` as the
/// band let the gate drift to ~1.0 on exactly the curved group rims this test
/// exists to reject (measured on plate_c_dent_large).
const RBF_LOO_H_FRAC: f64 = 0.25;
/// Maximum number of ring samples held out in the LOO gate (deterministic
/// stride); bounds the repeated `O(M^3)` refits of a large ring.
const RBF_LOO_MAX: usize = 24;
/// A rim/tail vertex deeper than this fraction of `h` below the reference is
/// part of the overshoot tail and is lifted onto the reference surface.
const TRIM_EPS_FACTOR: f64 = 0.02;
/// Per-loop cap on lifted tail vertices (a healthy mesh is never touched).
const TRIM_MAX_VERTICES: usize = 64;
/// Above this many patch triangles the local self-intersection check of the
/// skirt strip is skipped (returns "clean") for speed; the patch is a thin
/// collar around one rim, so a large loop is still bounded.
const PATCH_SI_MAX_TRIS: usize = 400;

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

// --- fast generalized winding (Barill et al. 2018, `SUTURA_TAMP_FWN`) -------
/// Accuracy parameter of the fast winding approximation used for the min-cut's
/// bulk queries.  `2` is the paper's reference default: nodes farther than two
/// radii are collapsed to a dipole.  The `tag` decision thresholds (source
/// `> 0.5`, sink `< 0.1`) are re-evaluated exactly within `FWN_REFINE_BAND`.
pub const FWN_BETA: f64 = 2.0;
/// Accuracy used for the cheap second pass on cells near a decision threshold.
/// Much cheaper than the exact sum (far nodes are still aggregated) but orders
/// of magnitude more accurate than `FWN_BETA`.
pub const FWN_REFINE_BETA: f64 = 16.0;
/// Half-width of the band around the `tag` thresholds (`0.1`, `0.5`) inside
/// which the coarse winding is distrusted and sharpened with `FWN_REFINE_BETA`.
pub const FWN_REFINE_BAND: f64 = 0.15;
/// Half-width of the band around a `tag` threshold inside which even the
/// sharpened value is replaced by the exact all-triangle sum, so the min-cut's
/// cell classification is identical to the accurate path.
pub const FWN_TIGHT_BAND: f64 = 0.005;

#[derive(Clone, Debug)]
pub struct TamponadeConfig {
    pub h_min: Option<f64>,
    pub melt_si: bool,
    pub max_nodes: Option<usize>,
    pub time_budget_s: Option<f64>,
    pub openings_gate: Option<bool>,
    /// Use the fast generalized-winding approximation (Barill et al. 2018) for
    /// the min-cut's bulk winding queries, refining exactly near a decision
    /// threshold (`SUTURA_TAMP_FWN`).  Off keeps the accurate BVH winding.
    pub fwn: bool,
}

impl Default for TamponadeConfig {
    fn default() -> Self {
        Self {
            h_min: None,
            melt_si: true,
            max_nodes: None,
            time_budget_s: None,
            openings_gate: None,
            fwn: false,
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
    /// Holes sealed using the thin-plate RBF fallback reference (subset of
    /// `reference_used`).
    pub reference_rbf: usize,
    /// Holes sealed by a skirt plug (cap on the reference + zipper strip to the
    /// projected rim; subset of `reference_used`).
    pub reference_skirt: usize,
    /// Holes sealed by the stent plug (thin-plate active-set inflate).
    pub stent_used: usize,
    /// Stent candidates attempted but not committed (any reason).
    pub stent_rejected: usize,
    /// Per-plug standard numeric metrics (JSON array, see plug-metrics spec).
    pub plug_metrics_json: String,
    /// Rim/tail vertices lifted onto the reference surface (overshoot trim).
    pub trimmed_vertices: usize,
    /// Interior patch vertices placed on the reference surface.
    pub patch_vertices: usize,
    /// Plugs rolled back by the final reload-honest "never worse" safety net.
    pub plugs_rolled_back: usize,
    /// Plugs whose surface pokes outside the original solid (the leak gate).
    pub plugs_leaking: usize,
    /// Plugs rolled back for duplicating an existing (surviving) surface -- a
    /// double wall (the coincidence gate).
    pub plugs_double_wall: usize,
    /// Plugs rolled back for not being edge-connected to the main shell (a
    /// floating component or a closed sub-shell of their own).
    pub plugs_floating: usize,
    /// Reload-welded vertex-connected component count of the tier input.
    pub components_before: usize,
    /// Reload-welded vertex-connected component count of the tier output.
    pub components_after: usize,
    /// Reload-welded closed-shell count of the tier input.
    pub closed_shells_before: usize,
    /// Reload-welded closed-shell count of the tier output.
    pub closed_shells_after: usize,
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

/// Manifold boundary decomposition: half-edge (face-fan) traversal.
///
/// Unlike [`build_boundary_loops`] -- a greedy vertex-marking walker that, at a
/// rim vertex pinched by an attached flap, marks the vertex used and then
/// diverts into the flap, orphaning the rest of the rim -- this pairs each
/// incoming boundary half-edge with its successor across the incident triangle
/// fan. A rim pinched by the dangling tail therefore still yields the true
/// outer rim (e.g. the whole plate rim) plus the tail's own loop, and the outer
/// rim's reference plug covers the whole hole instead of leaving an annulus.
///
/// The walker is deterministic (starts are sorted) and each boundary half-edge
/// belongs to exactly one loop.
pub fn boundary_loops_manifold(tris: &[[u32; 3]]) -> Vec<Vec<u32>> {
    let key = |a: u32, b: u32| if a < b { (a, b) } else { (b, a) };
    let mut count: HashMap<(u32, u32), u32> = HashMap::with_capacity(tris.len() * 2);
    let mut faces: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (fi, t) in tris.iter().enumerate() {
        for &(a, b) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let k = key(a, b);
            *count.entry(k).or_insert(0) += 1;
            faces.entry(k).or_insert_with(Vec::new).push(fi);
        }
    }
    // Directed boundary half-edge -> its single incident face.
    let mut bface: HashMap<(u32, u32), usize> = HashMap::new();
    for (fi, t) in tris.iter().enumerate() {
        for &(a, b) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            if count.get(&key(a, b)) == Some(&1) {
                bface.insert((a, b), fi);
            }
        }
    }
    let third = |fi: usize, b: u32, exclude: u32| -> u32 {
        for &v in &tris[fi] {
            if v != b && v != exclude {
                return v;
            }
        }
        b
    };

    let mut used: HashSet<(u32, u32)> = HashSet::new();
    let mut loops: Vec<Vec<u32>> = Vec::new();
    let mut starts: Vec<(u32, u32)> = bface.keys().cloned().collect();
    starts.sort();
    // Work guard: on a non-manifold / degenerate output the face-fan walk can
    // cycle without ever reaching a boundary half-edge (e.g. a non-manifold edge
    // where >2 faces share an edge, or a face carrying the same edge twice).
    // The budget is PER START: one cycling walk must not consume the budget of
    // every later start (that global budget made the walker return zero loops --
    // `reason="no_holes"` -- on any mesh whose boundaries touch a non-manifold
    // edge). A repeat of the (face, vertex) rotation state bails immediately.
    let limit = tris.len().saturating_mul(8) + 64;
    for h in starts {
        if used.contains(&h) {
            continue;
        }
        let mut loop_v: Vec<u32> = Vec::new();
        let mut cur = h;
        let mut steps = 0usize;
        loop {
            if used.contains(&cur) {
                break;
            }
            used.insert(cur);
            loop_v.push(cur.0);
            // Rotate around the target vertex `b` through the face fan until the
            // next boundary half-edge is found.
            let (a, b) = cur;
            let mut f = bface[&(a, b)];
            let mut c = third(f, b, a);
            let mut nxt: Option<(u32, u32)> = None;
            // A proper fan rotation visits each face once; a repeated
            // (face, current-vertex) state means the walk is cycling.
            let mut fan_seen: HashSet<(usize, u32)> = HashSet::new();
            loop {
                steps += 1;
                if steps > limit || !fan_seen.insert((f, c)) {
                    break;
                }
                let k = key(b, c);
                if count.get(&k) == Some(&1) {
                    nxt = Some(if bface.contains_key(&(b, c)) { (b, c) } else { (c, b) });
                    break;
                }
                let fs = &faces[&k];
                if fs.len() < 2 {
                    break;
                }
                let f2 = if fs[0] == f { fs[1] } else { fs[0] };
                if f2 == f {
                    break;
                }
                c = third(f2, b, c);
                f = f2;
            }
            let Some(n) = nxt else { break };
            cur = n;
            if cur == h {
                break;
            }
        }
        if loop_v.len() >= 3 {
            loops.push(loop_v);
        }
    }
    loops
}

/// Duplicate non-manifold ("pinched") vertices, one copy per face-fan, so a
/// boundary rim pinched at a single vertex separates into independent loops
/// that can each be sealed. Mirrors the full pipeline's
/// `_split_pinched_vertices`: vertices are only duplicated and keep their
/// position (no shape change). This is a preprocessing step for boundary-loop
/// extraction and is opt-in behind `SUTURA_TAMP_MANIFOLD`. Deterministic:
/// vertices are visited in index order and the fan holding the lowest face
/// index keeps the original vertex id.
fn fan_split_pinched_vertices(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
    let nv = verts.len();
    let mut vf: Vec<Vec<usize>> = vec![Vec::new(); nv];
    for (fi, t) in tris.iter().enumerate() {
        for &v in t {
            vf[v as usize].push(fi);
        }
    }
    let mut out_verts: Vec<[f64; 3]> = verts.to_vec();
    let mut out_tris: Vec<[u32; 3]> = tris.to_vec();
    let mut edge_faces: HashMap<u32, Vec<usize>> = HashMap::new();
    for v in 0..nv {
        if vf[v].len() < 2 {
            continue;
        }
        // Faces sharing an edge incident to `v` are connected around `v`.
        edge_faces.clear();
        for &f in &vf[v] {
            let t = tris[f];
            for &(a, b) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                if a as usize == v {
                    edge_faces.entry(b).or_default().push(f);
                } else if b as usize == v {
                    edge_faces.entry(a).or_default().push(f);
                }
            }
        }
        // Union-find over the incident faces.
        let mut root: HashMap<usize, usize> = vf[v].iter().map(|&f| (f, f)).collect();
        fn find(root: &mut HashMap<usize, usize>, x: usize) -> usize {
            let mut r = x;
            while root[&r] != r {
                r = root[&r];
            }
            let mut c = x;
            while root[&c] != r {
                let n = root[&c];
                root.insert(c, r);
                c = n;
            }
            r
        }
        for fl in edge_faces.values() {
            for i in 1..fl.len() {
                let ra = find(&mut root, fl[0]);
                let rb = find(&mut root, fl[i]);
                if ra != rb {
                    root.insert(rb, ra);
                }
            }
        }
        let mut fans: HashMap<usize, Vec<usize>> = HashMap::new();
        for &f in &vf[v] {
            let r = find(&mut root, f);
            fans.entry(r).or_default().push(f);
        }
        if fans.len() < 2 {
            continue;
        }
        // Order fans by their lowest face index so the first one keeps `v`.
        let mut keys: Vec<usize> = fans.keys().cloned().collect();
        keys.sort_by_key(|k| *fans[k].iter().min().unwrap());
        for k in &keys[1..] {
            let nvi = out_verts.len() as u32;
            out_verts.push(verts[v]);
            for &f in &fans[k] {
                for x in out_tris[f].iter_mut() {
                    if *x == v as u32 {
                        *x = nvi;
                    }
                }
            }
        }
    }
    (out_verts, out_tris)
}

/// Group boundary loops that belong to one geometric hole: loops sharing a
/// boundary vertex, or whose vertices fall in the same `h`-sized grid cell
/// (i.e. come within ~h of each other). For each group the index of the outer
/// rim -- the loop with the largest bounding-box diagonal -- is returned so a
/// single reference surface can be fitted to it. Deterministic: groups are
/// ordered by their lowest loop index.
fn group_boundary_loops(
    loops: &[Vec<u32>],
    verts: &[[f64; 3]],
    h: f64,
) -> Vec<(Vec<usize>, usize)> {
    let n = loops.len();
    if n == 0 {
        return Vec::new();
    }
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            let gp = parent[parent[x]];
            parent[x] = gp;
            x = gp;
        }
        x
    }
    let mut parent: Vec<usize> = (0..n).collect();
    let mut vloop: HashMap<u32, usize> = HashMap::new();
    for (li, l) in loops.iter().enumerate() {
        for &v in l {
            match vloop.get(&v) {
                Some(&lj) => {
                    let a = find(&mut parent, li);
                    let b = find(&mut parent, lj);
                    if a != b {
                        parent[b] = a;
                    }
                }
                None => {
                    vloop.insert(v, li);
                }
            }
        }
    }
    let cell = h.max(1e-12);
    let mut cloop: HashMap<(i64, i64, i64), usize> = HashMap::new();
    for (li, l) in loops.iter().enumerate() {
        let mut cells: HashSet<(i64, i64, i64)> = HashSet::new();
        for &v in l {
            let p = verts[v as usize];
            cells.insert((
                (p[0] / cell).floor() as i64,
                (p[1] / cell).floor() as i64,
                (p[2] / cell).floor() as i64,
            ));
        }
        for c in cells {
            match cloop.get(&c) {
                Some(&lj) => {
                    let a = find(&mut parent, li);
                    let b = find(&mut parent, lj);
                    if a != b {
                        parent[b] = a;
                    }
                }
                None => {
                    cloop.insert(c, li);
                }
            }
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for li in 0..n {
        let r = find(&mut parent, li);
        groups.entry(r).or_default().push(li);
    }
    let mut out: Vec<(Vec<usize>, usize)> = groups
        .into_values()
        .map(|members| {
            let mut best = members[0];
            let mut best_d = -1.0f64;
            for &li in &members {
                let mut bb = BBox::empty();
                for &v in &loops[li] {
                    bb.expand_point(verts[v as usize]);
                }
                let d = bb.diagonal();
                if d > best_d {
                    best_d = d;
                    best = li;
                }
            }
            (members, best)
        })
        .collect();
    out.sort_by_key(|(m, _)| m[0]);
    out
}

/// Piecewise hole filling (Jun 2005): a long / complex rim is split at a
/// "bridge" -- a pair of rim vertices close in space but far apart along the
/// rim whose connecting segment is clear of the existing surface -- into two
/// simpler sub-rims, recursively, until each is short enough for the
/// DP / reference cap. The bridge edge is shared by the two caps with opposite
/// direction, so the seam stays two-manifold. This also removes the collinear
/// runs that make one large rim triangulate to zero-area faces.
fn split_long_loops(
    loops: Vec<Vec<u32>>,
    verts: &[[f64; 3]],
    bvh: &MeshBvh,
) -> (Vec<Vec<u32>>, Vec<usize>) {
    let mut out: Vec<Vec<u32>> = Vec::new();
    let mut parents: Vec<usize> = Vec::new();
    for (pi, l) in loops.into_iter().enumerate() {
        split_one(l, verts, bvh, 0, pi, &mut out, &mut parents);
    }
    (out, parents)
}

#[allow(clippy::too_many_arguments)]
fn split_one(
    rim: Vec<u32>,
    verts: &[[f64; 3]],
    bvh: &MeshBvh,
    depth: usize,
    parent: usize,
    out: &mut Vec<Vec<u32>>,
    parents: &mut Vec<usize>,
) {
    let m = rim.len();
    if m <= SPLIT_MAX_VERTS || depth >= SPLIT_MAX_DEPTH {
        out.push(rim);
        parents.push(parent);
        return;
    }
    // Candidate bridges: a pair far enough apart along the rim on both sides.
    // Jun 2005 selects the pair closest in space relative to the boundary arc
    // between them -- for a round rim that is a near-diameter (a balanced cut),
    // and for a spike/tail it is the short chord across the spike's base. The
    // ratio `arc / dist` is maximised; the expensive surface-clearance test
    // runs on the candidates in descending order until one is clear.
    let mut arc_len: Vec<f64> = vec![0.0; m + 1];
    for i in 0..m {
        let a = verts[rim[i] as usize];
        let b = verts[rim[(i + 1) % m] as usize];
        arc_len[i + 1] =
            arc_len[i] + ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
    }
    let perimeter = arc_len[m];
    let mut cands: Vec<(f64, usize, usize)> = Vec::new();
    for i in 0..m {
        for j in (i + SPLIT_MIN_ARC)..m {
            if m - (j - i) < SPLIT_MIN_ARC {
                continue;
            }
            let a = verts[rim[i] as usize];
            let b = verts[rim[j] as usize];
            let d = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
            if d < 1e-12 {
                continue;
            }
            let arc = arc_len[j] - arc_len[i];
            let short_arc = arc.min(perimeter - arc);
            cands.push((short_arc / d, i, j));
        }
    }
    cands.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut bridge: Option<(usize, usize)> = None;
    for &(_, i, j) in &cands {
        if segment_clear(bvh, verts[rim[i] as usize], verts[rim[j] as usize]) {
            bridge = Some((i, j));
            break;
        }
    }
    let Some((i, j)) = bridge else {
        out.push(rim);
        parents.push(parent);
        return;
    };
    let mut a: Vec<u32> = rim[i..=j].to_vec();
    a.dedup();
    let mut b: Vec<u32> = rim[j..].to_vec();
    b.extend_from_slice(&rim[..=i]);
    b.dedup();
    split_one(a, verts, bvh, depth + 1, parent, out, parents);
    split_one(b, verts, bvh, depth + 1, parent, out, parents);
}

/// True when the open segment `a`->`b` does not cross the existing surface.
fn segment_clear(bvh: &MeshBvh, a: [f64; 3], b: [f64; 3]) -> bool {
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let len = norm3(d);
    if len < 1e-12 {
        return false;
    }
    let dir = [d[0] / len, d[1] / len, d[2] / len];
    let q = bvh.intersect_ray(a, dir, len * (1.0 - 1e-6), 1e-9);
    q.crossings == 0
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

    // The `tag` decision thresholds (source `> 0.5`, sink `< 0.1`); the fast
    // winding path refines exactly inside `FWN_REFINE_BAND` of either.
    const TAG_THRESHOLDS: [f64; 2] = [0.1, 0.5];
    let winding = |pts: &[[f64; 3]]| -> Vec<f64> {
        if config.fwn {
            bvh.winding_points_hybrid_par(
                pts,
                FWN_BETA,
                FWN_REFINE_BETA,
                &TAG_THRESHOLDS,
                FWN_REFINE_BAND,
                FWN_TIGHT_BAND,
            )
        } else {
            bvh.winding_points_par(pts)
        }
    };

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
    // blocked edge, the seeds of the distance-to-surface flood fill. A cell's
    // bits depend only on that cell and its forward neighbours, so the
    // (BVH-ray) wall test for every edge is independent and runs in parallel;
    // the graph edges and the union-find over the traversable edges then run
    // serially in the original row-major order, so the components and the cut
    // are unchanged.
    let wall_bits: Vec<u8> = (0..total)
        .into_par_iter()
        .map(|idx| {
            let iz = idx / (nx * ny);
            let rem = idx % (nx * ny);
            let iy = rem / nx;
            let ix = rem % nx;
            let c = cell_center(ix, iy, iz);
            let mut bits = 0u8;
            if ix + 1 < nx && crosses_wall(bvh, c, cell_center(ix + 1, iy, iz)) {
                bits |= 1;
            }
            if iy + 1 < ny && crosses_wall(bvh, c, cell_center(ix, iy + 1, iz)) {
                bits |= 2;
            }
            if iz + 1 < nz && crosses_wall(bvh, c, cell_center(ix, iy, iz + 1)) {
                bits |= 4;
            }
            bits
        })
        .collect();

    let mut near_surface: Vec<bool> = vec![false; total];
    for iz in 0..nz {
        if deadline_passed(deadline) {
            return CutVerdict::Timeout;
        }
        for iy in 0..ny {
            for ix in 0..nx {
                let idx = cell_idx(ix, iy, iz);
                let bits = wall_bits[idx];
                if bits & 7 != 0 {
                    near_surface[idx] = true;
                    if ix + 1 < nx && bits & 1 != 0 {
                        near_surface[idx + 1] = true;
                    }
                    if iy + 1 < ny && bits & 2 != 0 {
                        near_surface[idx + nx] = true;
                    }
                    if iz + 1 < nz && bits & 4 != 0 {
                        near_surface[idx + nx * ny] = true;
                    }
                }
            }
        }
    }

    for iz in 0..nz {
        if deadline_passed(deadline) {
            return CutVerdict::Timeout;
        }
        for iy in 0..ny {
            for ix in 0..nx {
                let idx = cell_idx(ix, iy, iz);
                let bits = wall_bits[idx];
                if ix + 1 < nx && bits & 1 == 0 {
                    let j = cell_idx(ix + 1, iy, iz);
                    g.add_edge(idx, j, face_cap, face_cap);
                    uf.union(idx, j);
                }
                if iy + 1 < ny && bits & 2 == 0 {
                    let j = cell_idx(ix, iy + 1, iz);
                    g.add_edge(idx, j, face_cap, face_cap);
                    uf.union(idx, j);
                }
                if iz + 1 < nz && bits & 4 == 0 {
                    let j = cell_idx(ix, iy, iz + 1);
                    g.add_edge(idx, j, face_cap, face_cap);
                    uf.union(idx, j);
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
            let ws = winding(&pts);
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
        let ws = winding(&sample_pts);
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
        let mut fallback_cells = 0usize;
        for iz in 0..nz {
            if deadline_passed(deadline) {
                if prof {
                    eprintln!("[tamp]     fallback timeout at slab {} cells={}", iz, fallback_cells);
                }
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
                    // A border cell is forced to sink by `tag` regardless of
                    // its winding, so its exact value need not be computed.
                    if border_of(idx) {
                        cls[idx] = 2;
                        continue;
                    }
                    pts.push(center_of(idx));
                    idxs.push(idx);
                }
            }
            if pts.is_empty() {
                continue;
            }
            let ws2 = winding(&pts);
            fallback_cells += idxs.len();
            for (k, &idx) in idxs.iter().enumerate() {
                cls[idx] = tag(ws2[k], border_of(idx));
            }
        }
        if prof && fallback_cells > 0 {
            eprintln!(
                "[tamp]     fallback cells={} winding_s={:.3}",
                fallback_cells,
                tw0.elapsed().as_secs_f64()
            );
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
    // DP is preferred (min-area, symmetric), but it can fail on a non-convex or
    // collinear rim; under `SUTURA_TAMP_MANIFOLD`, ear clipping is the
    // structural fallback (brief step 2). The default path keeps the historical
    // DP-only behaviour byte-identical.
    let manifold_mode = std::env::var("SUTURA_TAMP_MANIFOLD").is_ok();
    let local = if m <= PLUG_DP_CAP {
        let dp = min_area_triangulation_filtered(
            &pts,
            PLUG_DP_CAP,
            &HashSet::new(),
            PLUG_MIN_AREA,
            PLUG_COLLINEAR_MARGIN_DEG,
        );
        if manifold_mode && dp.len() != m - 2 {
            ear_clip(&pts)
        } else {
            dp
        }
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

/// The fitted height-field model of a [`ReferenceSurface`]: a single quadric
/// (the default, cheap and exact for a small hole) or a thin-plate RBF height
/// field (the fallback for a large / curved hole the quadric extrapolates
/// badly). Both evaluate `h(u, v)` along the surface normal `n`.
#[derive(Clone, Debug)]
enum RefModel {
    /// `h(u,v) = c0 + c1 u + c2 v + c3 u^2 + c4 u v + c5 v^2`.
    Quadric([f64; 6]),
    /// `h(u,v) = a0 + a1 u + a2 v + sum_j w_j phi(|x - c_j|)`.
    Rbf(rbf::TpsFit),
}

/// A fitted smooth "laser" reference surface over a hole, used to seal at the
/// level of the intact surrounding tissue (Arif's "line 2") instead of the rim
/// point closest to the centre ("line 1").
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct ReferenceSurface {
    origin: [f64; 3],
    u: [f64; 3],
    v: [f64; 3],
    n: [f64; 3],
    model: RefModel,
    rms: f64,
    /// Sign of the mean curvature at the centre (concave < 0).
    concave: bool,
}

impl ReferenceSurface {
    fn eval(&self, u: f64, v: f64) -> f64 {
        match &self.model {
            RefModel::Quadric(c) => {
                c[0] + c[1] * u + c[2] * v + c[3] * u * u + c[4] * u * v + c[5] * v * v
            }
            RefModel::Rbf(f) => f.eval(u, v),
        }
    }

    /// Signed distance of `p` along `n` from the reference surface.
    fn signed_height(&self, p: [f64; 3]) -> f64 {
        let d = [p[0] - self.origin[0], p[1] - self.origin[1], p[2] - self.origin[2]];
        dot3(d, self.n) - self.eval(dot3(d, self.u), dot3(d, self.v))
    }

    /// Project `p` onto the reference surface along `n` (an overshoot tail is
    /// lifted onto line 2 without changing its in-plane position).
    pub(crate) fn place(&self, p: [f64; 3]) -> [f64; 3] {
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

    /// The local frame `(origin, u, v, n)` (Flap's projected-patch candidate).
    pub(crate) fn frame(&self) -> ([f64; 3], [f64; 3], [f64; 3], [f64; 3]) {
        (self.origin, self.u, self.v, self.n)
    }

    /// In-plane coordinates `(u, v)` of `p` in the reference frame.
    pub(crate) fn uv(&self, p: [f64; 3]) -> (f64, f64) {
        let d = [p[0] - self.origin[0], p[1] - self.origin[1], p[2] - self.origin[2]];
        (dot3(d, self.u), dot3(d, self.v))
    }
}

/// Vertex->face and edge->face adjacency of the input mesh, built once.
pub(crate) struct Adjacency {
    vert_faces: Vec<Vec<u32>>,
    edge_faces: HashMap<(u32, u32), Vec<u32>>,
    edge_mult: HashMap<(u32, u32), i32>,
}

pub(crate) fn build_adjacency(nverts: usize, tris: &[[u32; 3]]) -> Adjacency {
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
pub(crate) fn build_reference(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    rim: &[u32],
    h: f64,
    adj: &Adjacency,
) -> Option<ReferenceSurface> {
    let dbg = std::env::var("SUTURA_REF_DBG").is_ok();
    if dbg {
        eprintln!("[ref] start m={}", rim.len());
    }
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

    // Ring width grows with the rim diameter (one face hop ~ one median edge
    // `h`): a large hole needs a wider intact band for a stable quadratic fit.
    // Small rims keep the historical 3-ring band; the width is capped so a huge
    // loop cannot scan an unbounded region.
    let mut rim_box = BBox::empty();
    for p in &rim_pts {
        rim_box.expand_point(*p);
    }
    let ring_max = ((rim_box.diagonal() / (REF_RING_DIAM_DIV * h.max(1e-9))).round()
        as usize)
        .clamp(REF_RING_MAX, REF_RING_MAX_CAP);

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
        if d >= ring_max {
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
        if dbg { eprintln!("[ref] m={} no_ring_faces", rim.len()); }
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
    // A large rim needs a proportionally larger sample; a tiny rim keeps the
    // historical floor.
    let min_points = REF_MIN_POINTS.max(rim.len() / 8);
    if points.len() < min_points || w_sum <= 0.0 {
        if dbg { eprintln!("[ref] m={} few_points {} < {}", rim.len(), points.len(), min_points); }
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

    // Fit the quadric first: the normal-spread veto is gated on the fit
    // roughness, so a smooth curved surface (a sphere cap, a rounded bump) with
    // a wide normal spread is accepted while a genuinely noisy ring is not.
    let mut samples: Vec<(f64, f64, f64)> = Vec::with_capacity(points.len());
    for p in &points {
        let d = [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
        samples.push((dot3(d, u), dot3(d, v), dot3(d, n)));
    }
    let (coeffs, rms) = match fit_quadric(&samples) {
        Some(q) => q,
        None => { if dbg { eprintln!("[ref] m={} quadric_fit_none", rim.len()); } return None; }
    };

    // Self-consistency brake (no ground truth): the quadric must not extrapolate
    // a sag across the hole radius far larger than the ring's own height relief.
    // A partial rim fragment or a very wide ring can otherwise yield a reference
    // whose centre sits well outside the observed tissue; the plug then lies ON
    // that reference (so `reference_dev_ok` cannot flag it) and becomes a spike.
    let mut h_lo = f64::INFINITY;
    let mut h_hi = f64::NEG_INFINITY;
    let mut radii: Vec<f64> = Vec::with_capacity(points.len());
    for p in &points {
        let d = [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
        let hh = dot3(d, n);
        h_lo = h_lo.min(hh);
        h_hi = h_hi.max(hh);
        radii.push(dot3(d, u).hypot(dot3(d, v)));
    }
    radii.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let r_med = radii[radii.len() / 2];
    let band = (h_hi - h_lo) + h;
    let sag = (coeffs[3].abs() + coeffs[4].abs() + coeffs[5].abs()) * r_med * r_med;

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

    // Laser consistency veto: each "laser" ray extends the tangent plane at a
    // ring point to the hole axis. For a bowl/dome/cylinder they all agree in
    // sign (the surface continues one way); a saddle makes opposite sides
    // predict opposite heights, so the sign of the residual to the fitted
    // centre height flips -> `no reference`. A saddle is rejected only when a
    // *material share* of the rays lies beyond the tolerance on BOTH sides; a
    // single stray ray must not veto a genuine seal. The tolerance is scaled by
    // the ring's own fit roughness (`rms`), not the global median edge `h`,
    // which is the wrong scale for a rim spanning many `h`.
    let c0 = coeffs[0];
    let tol = REF_RAY_SPREAD_FACTOR * rms.max(1e-9);
    let min_side = ((rays.len() as f64) * REF_RAY_MIN_SIDE_FRAC).ceil() as usize;
    let mut below = 0usize;
    let mut above = 0usize;
    for &w in &rays {
        let r = w - c0;
        if r < -tol {
            below += 1;
        } else if r > tol {
            above += 1;
        }
    }
    let saddle_veto = below >= min_side && above >= min_side;
    if saddle_veto && dbg {
        eprintln!(
            "[ref] m={} saddle_veto below={} above={}",
            rim.len(),
            below,
            above
        );
    }

    // Quadric acceptance. The single quadric is the default: cheap and exact for
    // a small hole. It is rejected by the rms / normal-spread / sag gates when
    // the surrounding tissue is smooth but not a single quadric (a large curved
    // crater whose rim fragments extrapolate badly). In that case fit the
    // thin-plate RBF through the ring points and the laser meeting point, and
    // accept it only if it stays self-consistent over the hole disk.
    let rms_ok = rms <= REF_RMS_FACTOR * h;
    let spread_ok = !(max_dev > REF_NORMAL_SPREAD_DEG && rms > REF_NORMAL_SPREAD_RMS_FRAC * h);
    let sag_ok = sag <= REF_SAG_FACTOR * band;
    let concave = (coeffs[3] + coeffs[5]) < 0.0;
    if std::env::var("SUTURA_REF_DBG").is_ok() {
        eprintln!(
            "[ref] m={} rms={:.4} maxdev={:.1} sag={:.4} band={:.4} rms_ok={} spread_ok={} sag_ok={}",
            rim.len(),
            rms,
            max_dev,
            sag,
            band,
            rms_ok,
            spread_ok,
            sag_ok
        );
    }
    if !saddle_veto && rms_ok && spread_ok && sag_ok {
        return Some(ReferenceSurface {
            origin: c,
            u,
            v,
            n,
            model: RefModel::Quadric(coeffs),
            rms,
            concave,
        });
    }
    let rbf_enabled =
        !matches!(std::env::var("SUTURA_RBF").as_deref(), Ok("0") | Ok("false"));
    // The saddle fallback has its own switch so the A/B can isolate it from the
    // curved-hole RBF fallback below (SUTURA_RBF=0 still disables all of it).
    let saddle_rbf_enabled = !matches!(
        std::env::var("SUTURA_RBF_SADDLE").as_deref(),
        Ok("0") | Ok("false")
    );
    if !rbf_enabled {
        return None;
    }
    if saddle_veto {
        // A saddle is not representable by the single quadric (opposite tangent
        // rays predict opposite heights) but IS representable by the thin-plate
        // RBF, whose field bends both ways. On a saddle the laser meeting-point
        // anchor is meaningless -- that is exactly what this veto detected -- so
        // use only the ring+rim weighted fit (anchored to the real opening) and
        // gate it with the leave-one-out self-consistency test.
        if !saddle_rbf_enabled {
            return None;
        }
        let loo_band = RBF_LOO_H_FRAC * h;
        return build_rbf_reference(
            &samples,
            &point_normals,
            &rim_pts,
            c,
            u,
            v,
            n,
            h_lo,
            h_hi,
            band,
            h,
            &rays,
            true,
            Some(loo_band),
        )
        .map(|(fit, rbf_rms)| ReferenceSurface {
            origin: c,
            u,
            v,
            n,
            model: RefModel::Rbf(fit),
            rms: rbf_rms,
            concave,
        });
    }
    // Ring-only thin-plate RBF (ring samples + the laser meeting-point anchor).
    if let Some((fit, rbf_rms)) = build_rbf_reference(
        &samples,
        &point_normals,
        &rim_pts,
        c,
        u,
        v,
        n,
        h_lo,
        h_hi,
        band,
        h,
        &rays,
        false,
        None,
    ) {
        return Some(ReferenceSurface {
            origin: c,
            u,
            v,
            n,
            model: RefModel::Rbf(fit),
            rms: rbf_rms,
            concave,
        });
    }
    // Ring+rim weighted thin-plate RBF: the rim is included with a low trust
    // weight so the fit is anchored to the real opening as well as the intact
    // ring; a last-resort reference when the ring-only fit is rejected.
    build_rbf_reference(
        &samples,
        &point_normals,
        &rim_pts,
        c,
        u,
        v,
        n,
        h_lo,
        h_hi,
        band,
        h,
        &rays,
        true,
        None,
    )
    .map(|(fit, rbf_rms)| ReferenceSurface {
        origin: c,
        u,
        v,
        n,
        model: RefModel::Rbf(fit),
        rms: rbf_rms,
        concave,
    })
}
/// Fit the thin-plate RBF reference fallback and run the no-truth
/// self-consistency gate. Returns `(fit, ring_rms)` or `None` (-> no reference,
/// the caller keeps the rim-only plug).
///
/// Centres are the intact-ring height samples plus the **laser meeting point**
/// at the hole axis `(u,v) = (0,0)`: the median of the tangent-ray meetings
/// computed above. Anchoring the hole centre to that extrapolated height is what
/// stops a partial rim from sagging, which is the failure the quadric cannot
/// avoid. The gate then bounds the fitted field over the hole disk by the ring's
/// own height band and slope, so an over-extrapolated fit is rejected.
///
/// When `loo_band` is `Some`, the fit must additionally pass a leave-one-out
/// self-consistency test over the ring samples (used by the saddle fallback,
/// where the band/slope gate alone cannot tell a genuine saddle from a wild
/// extrapolation).
#[allow(clippy::too_many_arguments)]
fn build_rbf_reference(
    samples: &[(f64, f64, f64)],
    point_normals: &[[f64; 3]],
    rim_pts: &[[f64; 3]],
    c: [f64; 3],
    u: [f64; 3],
    v: [f64; 3],
    n: [f64; 3],
    h_lo: f64,
    h_hi: f64,
    band: f64,
    h: f64,
    rays: &[f64],
    use_rim: bool,
    loo_band: Option<f64>,
) -> Option<(rbf::TpsFit, f64)> {
    let dbg = std::env::var("SUTURA_REF_DBG").is_ok();
    if samples.len() < REF_MIN_POINTS || (!use_rim && rays.is_empty()) {
        if dbg {
            eprintln!("[rbf] m={} few_samples", rim_pts.len());
        }
        return None;
    }

    // Hole radius = mean in-plane distance of the rim points from the centre.
    let mut r_sum = 0.0;
    for p in rim_pts {
        let d = [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
        r_sum += dot3(d, u).hypot(dot3(d, v));
    }
    let r_hole = r_sum / rim_pts.len().max(1) as f64;
    if !(r_hole > 1e-9) {
        return None;
    }
    // The RBF fallback is for large holes only; a small opening (below the
    // diameter / `h` ratio) keeps the quadric-or-rim-only behaviour unchanged.
    if 2.0 * r_hole < RBF_MIN_HOLE_FACTOR * h {
        if dbg {
            eprintln!(
                "[rbf] m={} small_hole 2r={:.3} < {:.3}",
                rim_pts.len(),
                2.0 * r_hole,
                RBF_MIN_HOLE_FACTOR * h
            );
        }
        return None;
    }

    // The ring's own height band (no truth) and the laser meeting point. On a
    // strongly curved ring the raw tangent-ray extrapolation can sit far outside
    // the observed tissue, so the anchor is clamped into the band the gate allows
    // anyway -- an unclamped anchor would just drag the centre out and be
    // rejected.
    let lo = h_lo - RBF_BAND_K * band;
    let hi = h_hi + RBF_BAND_K * band;
    let fit = if use_rim {
        // Ring samples (weight 1) plus the rim samples (low trust weight), so
        // the fit is anchored to the real opening without overriding the ring.
        let mut pts4: Vec<(f64, f64, f64, f64)> =
            samples.iter().map(|&(su, sv, d)| (su, sv, d, 1.0)).collect();
        for p in rim_pts {
            let d = [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
            let hh = dot3(d, n);
            if hh.is_finite() {
                pts4.push((dot3(d, u), dot3(d, v), hh, RBF_RIM_WEIGHT));
            }
        }
        rbf::TpsFit::fit_weighted(&pts4, RBF_RIDGE, RBF_MAX_CENTRES)?
    } else {
        // Ring samples plus the laser meeting point at the hole axis (the
        // median tangent-ray height, clamped to the ring band).
        let ray_median = rays[rays.len() / 2];
        if !ray_median.is_finite() {
            return None;
        }
        let anchor = ray_median.clamp(h_lo - 0.5 * band, h_hi + 0.5 * band);
        let mut pts = samples.to_vec();
        pts.push((0.0, 0.0, anchor));
        rbf::TpsFit::fit(&pts, RBF_RIDGE, RBF_MAX_CENTRES)?
    };

    // The ring's own max height-field slope, from its point normals:
    // `slope = tan(angle from n) = sqrt(1 - cos^2) / cos`.
    let mut ring_slope = 0.0f64;
    for pn in point_normals {
        let pl = norm3(*pn);
        if pl < 1e-20 {
            continue;
        }
        let cc = (dot3([pn[0] / pl, pn[1] / pl, pn[2] / pl], n)).abs().clamp(0.0, 1.0);
        if cc > 0.05 {
            let s = (1.0 - cc * cc).sqrt() / cc;
            if s > ring_slope {
                ring_slope = s;
            }
        }
    }
    let grad_limit = RBF_GRAD_K * ring_slope.max(1e-9);

    let dtheta = std::f64::consts::TAU / RBF_GATE_NA as f64;
    let dr = r_hole / RBF_GATE_NR as f64;
    let mut max_grad = 0.0f64;
    for ir in 0..=RBF_GATE_NR {
        let r = dr * ir as f64;
        for ia in 0..RBF_GATE_NA {
            let th = ia as f64 * dtheta;
            let value = fit.eval(r * th.cos(), r * th.sin());
            if !value.is_finite() || value < lo || value > hi {
                if dbg {
                    eprintln!("[rbf] m={} band_fail value={:.4} lo={:.4} hi={:.4}", rim_pts.len(), value, lo, hi);
                }
                return None;
            }
            if r > 1e-9 {
                let th2 = (ia as f64 + 1.0) * dtheta;
                let v2 = fit.eval(r * th2.cos(), r * th2.sin());
                max_grad = max_grad.max((v2 - value).abs() / (r * dtheta));
            }
            if ir < RBF_GATE_NR {
                let r2 = dr * (ir as f64 + 1.0);
                let v2r = fit.eval(r2 * th.cos(), r2 * th.sin());
                if (r2 - r).abs() > 1e-9 {
                    max_grad = max_grad.max((v2r - value).abs() / (r2 - r).abs());
                }
            }
        }
    }
    if max_grad > grad_limit {
        if dbg {
            eprintln!("[rbf] m={} grad_fail {:.4} > {:.4}", rim_pts.len(), max_grad, grad_limit);
        }
        return None;
    }

    if let Some(loo) = loo_band {
        if !rbf_loo_ok(samples, rim_pts, c, u, v, n, use_rim, rays, h_lo, h_hi, loo) {
            if dbg {
                eprintln!("[rbf] m={} loo_fail", rim_pts.len());
            }
            return None;
        }
    }

    let mut ss = 0.0;
    for &(su, sv, d) in samples {
        let r = d - fit.eval(su, sv);
        ss += r * r;
    }
    let rbf_rms = (ss / samples.len().max(1) as f64).sqrt();
    Some((fit, rbf_rms))
}

/// Leave-one-out self-consistency of the RBF fallback: drop each ring sample in
/// turn, refit without it, and require the prediction at the held-out sample to
/// stay within `band`. Returns `false` on any refit failure or oversized error.
/// At most `RBF_LOO_MAX` samples are held out (deterministic stride) so the
/// repeated `O(M^3)` refits stay bounded.
#[allow(clippy::too_many_arguments)]
fn rbf_loo_ok(
    samples: &[(f64, f64, f64)],
    rim_pts: &[[f64; 3]],
    c: [f64; 3],
    u: [f64; 3],
    v: [f64; 3],
    n: [f64; 3],
    use_rim: bool,
    rays: &[f64],
    h_lo: f64,
    h_hi: f64,
    band: f64,
) -> bool {
    let m = samples.len();
    if m < 4 {
        return false;
    }
    // The caller's sample order comes from a HashMap walk, so sort the held-out
    // indices by (u, v) first: the LOO outcome must not depend on hash order.
    let mut order: Vec<usize> = (0..m).collect();
    order.sort_by(|&a, &b| {
        samples[a]
            .0
            .partial_cmp(&samples[b].0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                samples[a]
                    .1
                    .partial_cmp(&samples[b].1)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    let stride = ((m + RBF_LOO_MAX - 1) / RBF_LOO_MAX).max(1);
    let mut oi = 0usize;
    while oi < m {
        let i = order[oi];
        let held = samples[i];
        let fit = if use_rim {
            let mut pts: Vec<(f64, f64, f64, f64)> = Vec::with_capacity(m + rim_pts.len());
            for (j, &s) in samples.iter().enumerate() {
                if j == i {
                    continue;
                }
                pts.push((s.0, s.1, s.2, 1.0));
            }
            for p in rim_pts {
                let d = [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
                let hh = dot3(d, n);
                if hh.is_finite() {
                    pts.push((dot3(d, u), dot3(d, v), hh, RBF_RIM_WEIGHT));
                }
            }
            rbf::TpsFit::fit_weighted(&pts, RBF_RIDGE, RBF_MAX_CENTRES)
        } else {
            let mut pts: Vec<(f64, f64, f64)> = Vec::with_capacity(m + 1);
            for (j, &s) in samples.iter().enumerate() {
                if j == i {
                    continue;
                }
                pts.push(s);
            }
            if !rays.is_empty() {
                let ray_median = rays[rays.len() / 2];
                if !ray_median.is_finite() {
                    return false;
                }
                let anchor = ray_median.clamp(h_lo - 0.5 * band, h_hi + 0.5 * band);
                pts.push((0.0, 0.0, anchor));
            }
            rbf::TpsFit::fit(&pts, RBF_RIDGE, RBF_MAX_CENTRES)
        };
        let fit = match fit {
            Some(f) => f,
            None => return false,
        };
        let pred = fit.eval(held.0, held.1);
        if !pred.is_finite() || (pred - held.2).abs() > band {
            if std::env::var("SUTURA_REF_DBG").is_ok() {
                eprintln!(
                    "[rbf] loo_fail err={:.4} band={:.4} n={}",
                    (pred - held.2).abs(),
                    band,
                    m
                );
            }
            return false;
        }
        oi += stride;
    }
    true
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

/// Shape sanity gate for a candidate cap (T12, second safeguard). Rejects a cap
/// whose vertices stray more than half the rim diameter from the parent
/// reference surface (or, with no reference, from the rim's own best-fit plane),
/// and a cap carrying a face whose normal flips more than 90 deg against the
/// ring. The runaway fin is normally prevented by the exact rollback restore;
/// this gate stops such a plug from being committed at all.
fn cap_shape_ok(
    cand: &[[u32; 3]],
    verts: &[[f64; 3]],
    new_base: u32,
    added: &[[f64; 3]],
    rim: &[u32],
    rim_diag: f64,
    refs: Option<&ReferenceSurface>,
) -> bool {
    if cand.is_empty() || rim.len() < 3 {
        return true;
    }
    let limit = 0.5 * rim_diag;
    let ring_pts: Vec<[f64; 3]> = rim.iter().map(|&v| verts[v as usize]).collect();
    let rn = newell_normal(&ring_pts);
    let rnl = norm3(rn);
    if rnl <= 1e-20 {
        return true; // degenerate ring: no reliable plane, let other gates decide
    }
    let rim_normal = [rn[0] / rnl, rn[1] / rnl, rn[2] / rnl];
    let mut centroid = [0.0f64; 3];
    for p in &ring_pts {
        centroid[0] += p[0];
        centroid[1] += p[1];
        centroid[2] += p[2];
    }
    let inv = 1.0 / ring_pts.len() as f64;
    centroid = [centroid[0] * inv, centroid[1] * inv, centroid[2] * inv];

    // Mean cap normal (area-weighted), used to sign-align the ring normal so a
    // consistently-wound but reversed cap is not read as a per-face flip.
    let mut cap_n = [0.0f64; 3];
    for t in cand {
        let a = skirt_pos(t[0], verts, new_base, added);
        let b = skirt_pos(t[1], verts, new_base, added);
        let c = skirt_pos(t[2], verts, new_base, added);
        let n = cross3(
            [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
            [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
        );
        cap_n[0] += n[0];
        cap_n[1] += n[1];
        cap_n[2] += n[2];
    }
    let ref_dir = if dot3(cap_n, rim_normal) < 0.0 {
        [-rim_normal[0], -rim_normal[1], -rim_normal[2]]
    } else {
        rim_normal
    };

    for t in cand {
        let a = skirt_pos(t[0], verts, new_base, added);
        let b = skirt_pos(t[1], verts, new_base, added);
        let c = skirt_pos(t[2], verts, new_base, added);
        let nf = cross3(
            [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
            [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
        );
        if norm3(nf) > 1e-20 && dot3(nf, ref_dir) < 0.0 {
            return false;
        }
        for p in [a, b, c] {
            let d = if let Some(r) = refs {
                r.signed_height(p).abs()
            } else {
                dot3(
                    [p[0] - centroid[0], p[1] - centroid[1], p[2] - centroid[2]],
                    rim_normal,
                )
                .abs()
            };
            if d > limit {
                return false;
            }
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

/// Fan patch from a single interior vertex at `centre` (local index
/// `rim.len()`). Returns `(triangles, centre)`; the orientation that cancels
/// the most rim edges wins. No reference surface is required.
fn fan_from_centre(
    rim: &[u32],
    centre: [f64; 3],
    directed_set: &HashSet<(u32, u32)>,
) -> Option<(Vec<[u32; 3]>, [f64; 3])> {
    let m = rim.len();
    if m < 4 {
        return None;
    }
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

/// Resolve a skirt-patch vertex index: indices below `new_base` are the real
/// rim/input vertices, the rest index into `added`.
#[inline]
fn skirt_pos(idx: u32, verts: &[[f64; 3]], new_base: u32, added: &[[f64; 3]]) -> [f64; 3] {
    if idx < new_base {
        verts[idx as usize]
    } else {
        added[(idx - new_base) as usize]
    }
}

/// Local self-intersection check over a patch: a proper crossing or a coplanar
/// overlap between two patch triangles that share no vertex is a failure.
/// Triangles that share a vertex are skipped (a seam touch is not a crossing).
/// Above `PATCH_SI_MAX_TRIS` the check is skipped for speed (returns `false`).
fn patch_self_intersects(
    verts: &[[f64; 3]],
    new_base: u32,
    added: &[[f64; 3]],
    patch: &[[u32; 3]],
) -> bool {
    use crate::triangle_intersection::{intersect_triangles, Triangle, TriangleIntersection};
    let n = patch.len();
    if n > PATCH_SI_MAX_TRIS {
        return false;
    }
    for i in 0..n {
        for j in (i + 1)..n {
            if patch[i].iter().any(|x| patch[j].contains(x)) {
                continue;
            }
            let a = patch[i];
            let b = patch[j];
            let ta = Triangle::explicit(
                skirt_pos(a[0], verts, new_base, added),
                skirt_pos(a[1], verts, new_base, added),
                skirt_pos(a[2], verts, new_base, added),
            );
            let tb = Triangle::explicit(
                skirt_pos(b[0], verts, new_base, added),
                skirt_pos(b[1], verts, new_base, added),
                skirt_pos(b[2], verts, new_base, added),
            );
            match intersect_triangles(&ta, &tb) {
                TriangleIntersection::ProperSegment { .. }
                | TriangleIntersection::CoplanarOverlap => return true,
                _ => {}
            }
        }
    }
    false
}

/// Skirt plug (R5): the cap lies ON the reference surface, bounded by the rim
/// projected onto the reference along `n`; the real rim is joined to that
/// projected boundary by a one-triangle-wide zipper strip, so the rim itself
/// never has to lie on the reference. A rim vertex already clamped onto the
/// reference (`clamp[i]`, by the overshoot trim) contributes no strip quad at
/// its edges and is used directly as a cap-boundary vertex. Only the cap is
/// held to the reference deviation gate; the strip is free to stand off it.
///
/// Returns `(triangles, added_vertices)` with patch triangles in terms of the
/// global input indices and `new_base + k` for the added cap/collar vertices.
/// `None` when the cap cannot be triangulated, the cap offends the deviation
/// gate, the structure is not a clean disk bounded by the rim, or the strip
/// self-intersects locally.
fn skirt_patch(
    rim: &[u32],
    verts: &[[f64; 3]],
    refs: &ReferenceSurface,
    directed_set: &HashSet<(u32, u32)>,
    clamp: &[bool],
    new_base: u32,
    h: f64,
) -> Option<(Vec<[u32; 3]>, Vec<[f64; 3]>)> {
    let m = rim.len();
    if m < 4 {
        return None;
    }
    let dbg = std::env::var("SUTURA_SKIRT_DBG").is_ok();
    let mut added: Vec<[f64; 3]> = Vec::with_capacity(m);
    let mut ring: Vec<u32> = Vec::with_capacity(m);
    for i in 0..m {
        if clamp.get(i).copied().unwrap_or(false) {
            ring.push(rim[i]);
        } else {
            let q = refs.place(verts[rim[i] as usize]);
            ring.push(new_base + added.len() as u32);
            added.push(q);
        }
    }

    let cap_pts: Vec<[f64; 3]> = (0..m)
        .map(|i| skirt_pos(ring[i], verts, new_base, &added))
        .collect();
    let local = if m <= PLUG_DP_CAP {
        min_area_triangulation_filtered(
            &cap_pts,
            PLUG_DP_CAP,
            &HashSet::new(),
            PLUG_MIN_AREA,
            PLUG_COLLINEAR_MARGIN_DEG,
        )
    } else {
        ear_clip(&cap_pts)
    };
    if local.len() != m - 2 {
        if dbg {
            eprintln!("[skirt] m={} cap_dp_fail local={}", m, local.len());
        }
        return None;
    }

    // Cap deviation gate (the strip may stand off the reference; the cap must
    // not). Cheap per-triangle check because the patch mixes input and added
    // vertices, so a single combined slice is avoided.
    let limit = REF_DEV_FACTOR * h;
    for t in &local {
        let a = cap_pts[t[0] as usize];
        let b = cap_pts[t[1] as usize];
        let c = cap_pts[t[2] as usize];
        let cen = [
            (a[0] + b[0] + c[0]) / 3.0,
            (a[1] + b[1] + c[1]) / 3.0,
            (a[2] + b[2] + c[2]) / 3.0,
        ];
        if refs.signed_height(cen).abs() > limit {
            if dbg {
                eprintln!(
                    "[skirt] m={} cap_dev_fail {:.4} > {:.4}",
                    m,
                    refs.signed_height(cen).abs(),
                    limit
                );
            }
            return None;
        }
    }

    // Strip: for edge (rim_i, rim_{i+1}) the two triangles are T1 = [a, c, b]
    // (outer edge (b, a), which cancels the input boundary) and T2 = [b, c, d]
    // (inner edge (c, d)); either is dropped when it would be degenerate
    // (a == c or b == d, i.e. a clamped/on-reference vertex).
    let mut tris: Vec<[u32; 3]> = Vec::with_capacity(3 * m);
    for i in 0..m {
        let j = (i + 1) % m;
        let (a, b) = (rim[i], rim[j]);
        let (c, d) = (ring[i], ring[j]);
        if c != a {
            tris.push([a, c, b]);
        }
        if d != b {
            tris.push([b, c, d]);
        }
    }
    // Cap, reversed so its boundary (c_{i+1}, c_i) cancels the strip's inner
    // edges (c_i, c_{i+1}).
    for t in &local {
        tris.push([
            ring[t[0] as usize],
            ring[t[2] as usize],
            ring[t[1] as usize],
        ]);
    }

    // Structure: every triangle non-degenerate; the patch boundary (edges used
    // an odd number of times) must be exactly the rim edge set.
    let mut count: HashMap<(u32, u32), i32> = HashMap::with_capacity(tris.len() * 3);
    for t in &tris {
        let a = skirt_pos(t[0], verts, new_base, &added);
        let b = skirt_pos(t[1], verts, new_base, &added);
        let c = skirt_pos(t[2], verts, new_base, &added);
        if a == b || b == c || a == c || tri_area(a, b, c) <= PLUG_MIN_AREA {
            return None;
        }
        for &(x, y) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            *count.entry(ekey2(x, y)).or_insert(0) += 1;
        }
    }
    let mut boundary: HashSet<(u32, u32)> = HashSet::new();
    for (&e, &c) in &count {
        if c % 2 == 1 {
            boundary.insert(e);
        }
    }
    let mut rim_edges: HashSet<(u32, u32)> = HashSet::new();
    for i in 0..m {
        let a = rim[i];
        let b = rim[(i + 1) % m];
        if a != b {
            rim_edges.insert(ekey2(a, b));
        }
    }
    if boundary != rim_edges {
        if dbg {
            eprintln!(
                "[skirt] m={} structure_fail boundary={} rim={}",
                m,
                boundary.len(),
                rim_edges.len()
            );
        }
        return None;
    }

    // Orientation: cancel the most input boundary edges. The construction
    // already reverses the rim; flip the whole patch only if it does not.
    let mut cancels = 0usize;
    let dir: HashSet<(u32, u32)> = tris
        .iter()
        .flat_map(|t| [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])])
        .collect();
    for i in 0..m {
        let a = rim[i];
        let b = rim[(i + 1) % m];
        if a != b && directed_set.contains(&(a, b)) && dir.contains(&(b, a)) {
            cancels += 1;
        }
    }
    if cancels * 2 <= m {
        for t in tris.iter_mut() {
            t.swap(1, 2);
        }
    }

    if patch_self_intersects(verts, new_base, &added, &tris) {
        if dbg {
            eprintln!("[skirt] m={} self_intersects", m);
        }
        return None;
    }
    Some((tris, added))
}

/// Fan patch from a single interior vertex placed ON the reference surface.
/// Returns `(triangles, centre)` where the centre index is `rim.len()`.
fn fan_patch(
    rim: &[u32],
    refs: &ReferenceSurface,
    directed_set: &HashSet<(u32, u32)>,
) -> Option<(Vec<[u32; 3]>, [f64; 3])> {
    fan_from_centre(rim, refs.place(refs.origin), directed_set)
}

/// Fan patch from the rim centroid. The centroid lies on the rim's
/// least-squares plane by construction, so a fan to it is flat over the rim
/// and passes the deviation gate whenever the rim is reasonably planar. It is
/// the fallback for a rim with no non-degenerate rim-only triangulation; a
/// fully collinear rim still yields only degenerate triangles, which the
/// caller's structure gate then rejects.
fn centroid_fan(
    rim: &[u32],
    verts: &[[f64; 3]],
    directed_set: &HashSet<(u32, u32)>,
) -> Option<(Vec<[u32; 3]>, [f64; 3])> {
    let m = rim.len();
    if m < 4 {
        return None;
    }
    let mut c = [0.0f64; 3];
    for &v in rim {
        let p = verts[v as usize];
        c[0] += p[0];
        c[1] += p[1];
        c[2] += p[2];
    }
    let inv = 1.0 / m as f64;
    c = [c[0] * inv, c[1] * inv, c[2] * inv];
    fan_from_centre(rim, c, directed_set)
}

/// Rim-only plug, and when that has no non-degenerate triangulation, a fan to
/// a single interior Steiner vertex at the rim centroid. `new_idx` is the
/// global index the Steiner vertex will receive (the caller appends it only
/// once the plug is accepted). Returns global triangles plus the centre
/// coordinate when the Steiner fallback supplied one.
fn rim_plug_or_steiner(
    rim: &[u32],
    verts: &[[f64; 3]],
    directed_set: &HashSet<(u32, u32)>,
    allow_tie: bool,
    new_idx: u32,
) -> Result<(Vec<[u32; 3]>, Option<[f64; 3]>), &'static str> {
    match triangulate_rim_loop(rim, verts, directed_set, allow_tie) {
        Ok(tris) => Ok((tris, None)),
        Err(why) => {
            let (fan_local, centre) = centroid_fan(rim, verts, directed_set).ok_or(why)?;
            let m = rim.len() as u32;
            let fan: Vec<[u32; 3]> = fan_local
                .iter()
                .map(|t| {
                    let f = |x: u32| if x == m { new_idx } else { rim[x as usize] };
                    [f(t[0]), f(t[1]), f(t[2])]
                })
                .collect();
            Ok((fan, Some(centre)))
        }
    }
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

/// Reload-safe plug gate. STL drops vertex sharing and the reload welds every
/// vertex that rounds to the same float32 position, so a plug whose vertices
/// coincide (with each other, or with an existing output vertex) can create a
/// non-manifold edge even though its in-memory id topology is clean -- e.g. a
/// skirt that projects a dense / degenerate rim onto nearly the same point.
/// Weld the candidate's positions together with every existing face incident to
/// one of them and reject the plug only when it turns an edge that was used by
/// at most two faces into one used by more than two. Pre-existing weld-NM (a
/// damaged input) is preserved, never charged to the plug.
fn plug_reload_ok(
    out_verts: &[[f64; 3]],
    out_tris: &[[u32; 3]],
    new_base: u32,
    added: &[[f64; 3]],
    cand: &[[u32; 3]],
) -> bool {
    let pos = |id: u32| -> [f64; 3] {
        if id < new_base {
            out_verts[id as usize]
        } else {
            added[(id - new_base) as usize]
        }
    };
    let key = |p: [f64; 3]| -> [u32; 3] {
        [
            (p[0] as f32).to_bits(),
            (p[1] as f32).to_bits(),
            (p[2] as f32).to_bits(),
        ]
    };
    let mut keyset: HashSet<[u32; 3]> = HashSet::new();
    for t in cand {
        for &id in t {
            keyset.insert(key(pos(id)));
        }
    }
    let mut base: HashMap<([u32; 3], [u32; 3]), i32> = HashMap::new();
    let add_tri = |t: &[u32; 3], edges: &mut HashMap<([u32; 3], [u32; 3]), i32>| {
        let a = key(pos(t[0]));
        let b = key(pos(t[1]));
        let c = key(pos(t[2]));
        for (x, y) in [(a, b), (b, c), (c, a)] {
            let e = if x <= y { (x, y) } else { (y, x) };
            *edges.entry(e).or_insert(0) += 1;
        }
    };
    for t in out_tris {
        if t.iter().any(|&id| keyset.contains(&key(pos(id)))) {
            add_tri(t, &mut base);
        }
    }
    let mut with_cand = base.clone();
    for t in cand {
        add_tri(t, &mut with_cand);
    }
    for (e, c) in &with_cand {
        if *c > 2 && base.get(e).copied().unwrap_or(0) <= 2 {
            return false;
        }
    }
    true
}

/// Reload-honest topological verdict of an STL save/reload round-trip.
///
/// Mirrors what `repair.weld_reload_equivalent` / `defects.detect` observe on
/// the saved file: float32-quantised positions, exactly-coincident vertices
/// welded, then boundary loops, non-manifold edges, non-manifold (pinched)
/// vertices and degenerate faces counted. This is the engine's final
/// "never worse" metric -- a repaired mesh must not raise any of the four
/// damage counts over the input's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReloadVerdict {
    pub boundary_loops: usize,
    pub boundary_edges: usize,
    pub nm_edges: usize,
    pub nm_vertices: usize,
    pub degenerate_faces: usize,
    /// Vertex-connected face components of the reload-welded mesh.
    pub components: usize,
    /// Components with no boundary edge (closed shells).
    pub closed_shells: usize,
}

impl ReloadVerdict {
    /// True when `self` carries more structural damage than `base`. Boundary
    /// EDGES are included (a plug that opens a hole is a regression on reload);
    /// `boundary_loops` is deliberately excluded because its walk over the
    /// non-manifold boundary graph is order-sensitive (nondeterministic across
    /// processes) while the edge count is order-independent.
    fn worse_than(&self, base: &ReloadVerdict) -> bool {
        self.boundary_edges > base.boundary_edges
            || self.nm_edges > base.nm_edges
            || self.nm_vertices > base.nm_vertices
            || self.degenerate_faces > base.degenerate_faces
            || self.components > base.components
    }
}

/// Canonical undirected edge key.
#[inline]
fn reload_edge_key(a: u32, b: u32) -> (u32, u32) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Quantised float32 position key used for the exact-weld hash. `-0.0` is
/// folded onto `+0.0` so the two share a bucket, matching the numpy/STL
/// round-trip semantics.
#[inline]
fn f32_position_key(p: [f64; 3]) -> [u32; 3] {
    let q = |x: f64| {
        let f = x as f32;
        let f = if f == 0.0 { 0.0 } else { f };
        f.to_bits()
    };
    [q(p[0]), q(p[1]), q(p[2])]
}

/// Mimic an STL save + reload and count the resulting structure. Hash based
/// and O(V + F). Input positions are float32-quantised and welded on exact
/// coincidence; faces that collapse to a repeated index are dropped and
/// counted as degenerate (float32 rounding can create them).
pub fn reload_verdict(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> ReloadVerdict {
    let mut verdict = ReloadVerdict::default();
    if verts.is_empty() || tris.is_empty() {
        return verdict;
    }

    let nv = verts.len();
    let mut weld: HashMap<[u32; 3], u32> = HashMap::with_capacity(nv);
    let mut remap: Vec<u32> = Vec::with_capacity(nv);
    for p in verts {
        let next = weld.len() as u32;
        let id = *weld.entry(f32_position_key(*p)).or_insert(next);
        remap.push(id);
    }

    let mut wtris: Vec<[u32; 3]> = Vec::with_capacity(tris.len());
    for t in tris {
        let (a, b, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
        if a >= nv || b >= nv || c >= nv {
            continue;
        }
        let (wa, wb, wc) = (remap[a], remap[b], remap[c]);
        if wa == wb || wb == wc || wa == wc {
            verdict.degenerate_faces += 1;
            continue;
        }
        wtris.push([wa, wb, wc]);
    }

    let mut edges: HashMap<(u32, u32), u32> = HashMap::with_capacity(wtris.len() * 3);
    for t in &wtris {
        for (x, y) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            *edges.entry(reload_edge_key(x, y)).or_insert(0) += 1;
        }
    }
    let mut boundary_adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&(a, b), &count) in &edges {
        if count == 1 {
            verdict.boundary_edges += 1;
            boundary_adj.entry(a).or_default().push(b);
            boundary_adj.entry(b).or_default().push(a);
        } else if count > 2 {
            verdict.nm_edges += 1;
        }
    }
    verdict.boundary_loops = reload_count_boundary_loops(&boundary_adj);

    let mut vfaces: HashMap<u32, Vec<u32>> = HashMap::new();
    for (fi, t) in wtris.iter().enumerate() {
        for &v in t {
            vfaces.entry(v).or_default().push(fi as u32);
        }
    }
    for (&v, faces) in &vfaces {
        if faces.len() >= 2 && !reload_faces_single_fan(v, faces, &wtris) {
            verdict.nm_vertices += 1;
        }
    }

    // Vertex-connected face components and closed-shell count. A component is a
    // closed shell when none of its faces has a boundary (once-used) edge.
    let nf = wtris.len();
    if nf > 0 {
        let mut parent: Vec<u32> = (0..nf as u32).collect();
        fn find(p: &mut [u32], mut x: u32) -> u32 {
            while p[x as usize] != x {
                p[x as usize] = p[p[x as usize] as usize];
                x = p[x as usize];
            }
            x
        }
        for faces in vfaces.values() {
            for k in 1..faces.len() {
                let a = find(&mut parent, faces[0]);
                let b = find(&mut parent, faces[k]);
                if a != b {
                    parent[b as usize] = a;
                }
            }
        }
        let mut has_boundary: HashMap<u32, bool> = HashMap::new();
        for (fi, t) in wtris.iter().enumerate() {
            let root = find(&mut parent, fi as u32);
            let e = has_boundary.entry(root).or_insert(false);
            for (x, y) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                if edges.get(&reload_edge_key(x, y)).copied().unwrap_or(0) == 1 {
                    *e = true;
                }
            }
        }
        let mut roots: HashSet<u32> = HashSet::new();
        for fi in 0..nf {
            roots.insert(find(&mut parent, fi as u32));
        }
        verdict.components = roots.len();
        verdict.closed_shells = roots
            .iter()
            .filter(|r| !has_boundary.get(r).copied().unwrap_or(false))
            .count();
    }

    verdict
}

/// Count the boundary loops of a boundary-adjacency graph by walking every
/// undirected boundary edge once. Keys and neighbour lists are walked in sorted
/// order so the result is independent of `HashMap` iteration order (the graph
/// can be non-manifold, where the walk order changes the loop count).
fn reload_count_boundary_loops(adj: &HashMap<u32, Vec<u32>>) -> usize {
    let mut used: HashSet<(u32, u32)> = HashSet::new();
    let mut loops = 0usize;
    let mut keys: Vec<u32> = adj.keys().copied().collect();
    keys.sort_unstable();
    for start in keys {
        let mut nbrs = adj[&start].clone();
        nbrs.sort_unstable();
        for nb in nbrs {
            if !used.insert(reload_edge_key(start, nb)) {
                continue;
            }
            loops += 1;
            let mut prev = start;
            let mut cur = nb;
            while cur != start {
                let mut next: Option<u32> = None;
                if let Some(cn) = adj.get(&cur) {
                    let mut cs = cn.clone();
                    cs.sort_unstable();
                    for &m in &cs {
                        if m == prev {
                            continue;
                        }
                        if !used.contains(&reload_edge_key(cur, m)) {
                            next = Some(m);
                            break;
                        }
                    }
                }
                match next {
                    Some(m) => {
                        used.insert(reload_edge_key(cur, m));
                        prev = cur;
                        cur = m;
                    }
                    None => break,
                }
            }
        }
    }
    loops
}

/// Whether the faces incident to vertex `v` form one connected fan: two
/// incident faces connect when they share the edge `(v, w)`. Two or more
/// fans mean a pinched (non-manifold) vertex.
fn reload_faces_single_fan(v: u32, faces: &[u32], wtris: &[[u32; 3]]) -> bool {
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    let n = faces.len();
    let mut parent: Vec<usize> = (0..n).collect();
    let mut edge_faces: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, &fi) in faces.iter().enumerate() {
        for &w in &wtris[fi as usize] {
            if w != v {
                edge_faces.entry(w).or_default().push(i);
            }
        }
    }
    for fl in edge_faces.values() {
        for k in 1..fl.len() {
            let a = find(&mut parent, fl[0]);
            let b = find(&mut parent, fl[k]);
            if a != b {
                parent[b] = a;
            }
        }
    }
    (0..n).filter(|&i| find(&mut parent, i) == i).count() == 1
}

/// One accepted plug's full footprint in the output arrays, used to attribute a
/// reload regression to individual plugs (per-plug rollback) AND to restore the
/// touched region EXACTLY on rollback.
///
/// A plug is not only the triangles it appends: committing it may also have
/// moved existing (original) vertices in place (the overshoot trim lifts a tail
/// rim vertex onto the reference). Rolling back only the appended triangles
/// therefore left the moved rim ring behind -- a jagged strip / a runaway fin
/// bulging out of the surface. The footprint records every moved vertex with
/// its pre-move position so a rollback restores the region to the input state.
#[derive(Clone, Debug, Default)]
struct PlugRegion {
    /// Added triangles live in `out_tris[tri_start..tri_end]`.
    tri_start: usize,
    tri_end: usize,
    reference: bool,
    steiner: bool,
    /// Vertices this plug moved in place, each with its original position
    /// (captured before the move). Restored on rollback.
    moved: Vec<(u32, [f64; 3])>,
    /// The hole aperture rim (original vertex indices): the GROUP/parent rim
    /// the reference was fitted on, not the sealed sub-rim. Used by the leak
    /// gate's aperture-footprint test so a reference fill that bulges over the
    /// whole hole is not mistaken for a leak.
    rim: Vec<u32>,
    /// True when this region was produced by the stent plug candidate.
    stent: bool,
    /// Diagnostic method tag for the per-plug metrics report.
    method: &'static str,
}

/// Reload verdict of the output with one plug region removed (vertices kept,
/// so a trim is not undone). Used only to attribute a regression.
fn reload_verdict_without(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    region: &PlugRegion,
) -> ReloadVerdict {
    let mut filtered: Vec<[u32; 3]> =
        Vec::with_capacity(tris.len() - (region.tri_end - region.tri_start));
    filtered.extend_from_slice(&tris[..region.tri_start]);
    filtered.extend_from_slice(&tris[region.tri_end..]);
    reload_verdict(verts, &filtered)
}

/// Reload verdict of the output with the flagged regions removed. Used to
/// project the fixed point of a multi-region rollback before applying it.
fn reload_verdict_dropping(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    regions: &[PlugRegion],
    drop: &[bool],
) -> ReloadVerdict {
    let mut mask = vec![true; tris.len()];
    for (r, &d) in regions.iter().zip(drop) {
        if d {
            for slot in mask.iter_mut().take(r.tri_end).skip(r.tri_start) {
                *slot = false;
            }
        }
    }
    let filtered: Vec<[u32; 3]> = tris
        .iter()
        .enumerate()
        .filter(|(i, _)| mask[*i])
        .map(|(_, t)| *t)
        .collect();
    reload_verdict(verts, &filtered)
}

/// Drop the flagged regions from the output, restore every vertex move they
/// made, and compact the vertex array to the referenced vertices. Restoring the
/// moved vertices is what makes a rolled-back plug leave the region EXACTLY as
/// the input (the append-only triangle range alone left the trim/snap ring
/// behind). Regions are walked in reverse so that, if two loops ever touched the
/// same vertex, the earliest region's recorded original position wins.
fn apply_region_rollback(
    out_verts: &mut Vec<[f64; 3]>,
    out_tris: &mut Vec<[u32; 3]>,
    regions: &[PlugRegion],
    drop: &[bool],
) {
    let nv = out_verts.len();
    for (r, &d) in regions.iter().zip(drop).rev() {
        if d {
            for &(v, orig) in &r.moved {
                if (v as usize) < nv {
                    out_verts[v as usize] = orig;
                }
            }
        }
    }
    let mut keep = vec![true; out_tris.len()];
    for (r, &d) in regions.iter().zip(drop) {
        if d {
            for slot in keep.iter_mut().take(r.tri_end).skip(r.tri_start) {
                *slot = false;
            }
        }
    }
    let mut remap: Vec<Option<u32>> = vec![None; nv];
    let mut new_verts: Vec<[f64; 3]> = Vec::new();
    let mut new_tris: Vec<[u32; 3]> = Vec::with_capacity(out_tris.len());
    for (i, t) in out_tris.iter().enumerate() {
        if !keep[i] {
            continue;
        }
        if t.iter().any(|&v| v as usize >= nv) {
            continue;
        }
        let mut nt = [0u32; 3];
        for k in 0..3 {
            let vi = t[k] as usize;
            nt[k] = match remap[vi] {
                Some(id) => id,
                None => {
                    let id = new_verts.len() as u32;
                    remap[vi] = Some(id);
                    new_verts.push(out_verts[vi]);
                    id
                }
            };
        }
        new_tris.push(nt);
    }
    *out_verts = new_verts;
    *out_tris = new_tris;
}

/// AABB of one triangle in `verts`.
fn tri_bbox(verts: &[[f64; 3]], t: &[u32; 3]) -> BBox {
    let mut b = BBox::empty();
    for &v in t {
        b.expand_point(verts[v as usize]);
    }
    b
}

/// Uniform-grid AABB index over a face array. Built once per safety-net pass so
/// the per-plug local self-intersection scan touches only the faces near that
/// plug (O(local)) instead of rescanning the whole mesh for every plug. Mirrors
/// the cell choice of `arrangement::broad_phase_candidates`.
struct FaceGrid {
    cell: f64,
    map: HashMap<(i64, i64, i64), Vec<u32>>,
}

impl FaceGrid {
    fn build(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> Self {
        let boxes: Vec<BBox> = tris.iter().map(|t| tri_bbox(verts, t)).collect();
        let mut diags: Vec<f64> = boxes
            .iter()
            .map(|b| {
                let dx = b.max[0] - b.min[0];
                let dy = b.max[1] - b.min[1];
                let dz = b.max[2] - b.min[2];
                (dx * dx + dy * dy + dz * dz).sqrt()
            })
            .collect();
        let mut cell = 1.0f64;
        if !diags.is_empty() {
            diags.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            cell = diags[diags.len() / 2] * 1.5;
        }
        if !(cell > 1e-9) {
            cell = 1e-9;
        }
        let mut map: HashMap<(i64, i64, i64), Vec<u32>> = HashMap::new();
        for (i, b) in boxes.iter().enumerate() {
            let c0 = [
                (b.min[0] / cell).floor() as i64,
                (b.min[1] / cell).floor() as i64,
                (b.min[2] / cell).floor() as i64,
            ];
            let c1 = [
                (b.max[0] / cell).floor() as i64,
                (b.max[1] / cell).floor() as i64,
                (b.max[2] / cell).floor() as i64,
            ];
            for x in c0[0]..=c1[0] {
                for y in c0[1]..=c1[1] {
                    for z in c0[2]..=c1[2] {
                        map.entry((x, y, z)).or_default().push(i as u32);
                    }
                }
            }
        }
        Self { cell, map }
    }

    /// Every face whose AABB overlaps `query`, sorted and deduplicated.
    fn query(&self, query: &BBox) -> Vec<u32> {
        let c0 = [
            (query.min[0] / self.cell).floor() as i64,
            (query.min[1] / self.cell).floor() as i64,
            (query.min[2] / self.cell).floor() as i64,
        ];
        let c1 = [
            (query.max[0] / self.cell).floor() as i64,
            (query.max[1] / self.cell).floor() as i64,
            (query.max[2] / self.cell).floor() as i64,
        ];
        let mut out: Vec<u32> = Vec::new();
        for x in c0[0]..=c1[0] {
            for y in c0[1]..=c1[1] {
                for z in c0[2]..=c1[2] {
                    if let Some(v) = self.map.get(&(x, y, z)) {
                        out.extend_from_slice(v);
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Proper, non-adjacent triangle crossings among a local candidate face set
/// where at least one face belongs to `subset`. Mirrors
/// `self_intersections_subset_core` (pairs sharing a vertex index are
/// adjacent and ignored; only a proper interior segment counts) but restricted
/// to the faces near one plug.
fn local_si_subset(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    cand: &[u32],
    subset: &HashSet<u32>,
) -> i64 {
    let n = cand.len();
    if n < 2 {
        return 0;
    }
    let tri: Vec<triangle_intersection::Triangle> = cand
        .iter()
        .map(|&f| {
            let t = tris[f as usize];
            triangle_intersection::Triangle::explicit(
                verts[t[0] as usize],
                verts[t[1] as usize],
                verts[t[2] as usize],
            )
        })
        .collect();
    let mut count = 0i64;
    for i in 0..n {
        let fi = cand[i];
        let si = subset.contains(&fi);
        for j in (i + 1)..n {
            let fj = cand[j];
            if !si && !subset.contains(&fj) {
                continue;
            }
            let a = tris[fi as usize];
            let b = tris[fj as usize];
            if a.iter().any(|x| b.contains(x)) {
                continue;
            }
            if let triangle_intersection::TriangleIntersection::ProperSegment { .. } =
                triangle_intersection::intersect_triangles(&tri[i], &tri[j])
            {
                count += 1;
            }
        }
    }
    count
}

/// Local self-intersection delta of one plug: how many proper crossings the
/// plug's new faces make with any face within its AABB expanded by `2h`. The
/// plug's faces did not exist in the base mesh, so any positive value is a rise
/// the reload verdict cannot see (a plug poking through a nearby sheet) and the
/// plug is rolled back.
///
/// Superseded by the leak gate (`plug_leak`) in the adoption decision; kept for
/// the unit tests and as the local-SI metric.
#[allow(dead_code)]
fn plug_local_si(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    grid: &FaceGrid,
    region: &PlugRegion,
    h: f64,
) -> i64 {
    let mut b = BBox::empty();
    let mut subset: HashSet<u32> = HashSet::new();
    for f in region.tri_start..region.tri_end {
        if f >= tris.len() {
            return 0;
        }
        let t = tris[f];
        for &v in &t {
            if (v as usize) >= verts.len() {
                return 0;
            }
            b.expand_point(verts[v as usize]);
        }
        subset.insert(f as u32);
    }
    if subset.is_empty() {
        return 0;
    }
    b.pad(2.0 * h);
    let cand = grid.query(&b);
    local_si_subset(verts, tris, &cand, &subset)
}

/// Per-plug local self-intersection rollback mask: `true` for every plug whose
/// new faces create a proper crossing with nearby tissue. One AABB grid is
/// built for all plugs, so the scan is O(faces) once plus O(local) per plug.
///
/// No longer part of the adoption decision: self-intersections are report-only
/// (slicers union overlapping volumes), replaced by the leak gate below. Kept
/// for the unit tests and as the local-SI metric.
#[allow(dead_code)]
fn si_rollback_mask(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    regions: &[PlugRegion],
    h: f64,
) -> Vec<bool> {
    if regions.is_empty() {
        return Vec::new();
    }
    let grid = FaceGrid::build(verts, tris);
    regions
        .iter()
        .map(|region| plug_local_si(verts, tris, &grid, region, h) > 0)
        .collect()
}

/// Tolerance of the plug leak gate: a plug sample that lies in empty space is
/// treated as touching the original surface (accepted) when it is within this
/// distance of it. Mirrors the CLI rule `clamp(diag/10000, 0.0125, 0.05)` mm.
fn leak_tol(model_diag: f64) -> f64 {
    (model_diag / 10_000.0).clamp(0.0125, 0.05)
}

/// True when `p` lies over the hole aperture: its projection onto the rim's
/// best-fit plane is inside the rim polygon or within `tol` of its boundary.
/// A reference/Steiner plug that fills a crater or bump sits over the aperture
/// and is therefore not a leak, even though the missing region is outside the
/// original solid.
fn over_aperture(p: [f64; 3], rim_pts: &[[f64; 3]], tol: f64) -> bool {
    let m = rim_pts.len();
    if m < 3 {
        return true; // degenerate rim: never reject on footprint
    }
    // Newell normal of the ordered loop (robust for a planar-ish rim).
    let mut nrm = [0.0f64; 3];
    for i in 0..m {
        let a = rim_pts[i];
        let b = rim_pts[(i + 1) % m];
        nrm[0] += (a[1] - b[1]) * (a[2] + b[2]);
        nrm[1] += (a[2] - b[2]) * (a[0] + b[0]);
        nrm[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    let nl = (nrm[0] * nrm[0] + nrm[1] * nrm[1] + nrm[2] * nrm[2]).sqrt();
    if nl < 1e-12 {
        return true;
    }
    let nrm = [nrm[0] / nl, nrm[1] / nl, nrm[2] / nl];
    // Orthonormal in-plane basis (u, v).
    let mut u = if nrm[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let un = u[0] * nrm[0] + u[1] * nrm[1] + u[2] * nrm[2];
    u = [u[0] - un * nrm[0], u[1] - un * nrm[1], u[2] - un * nrm[2]];
    let ul = (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt();
    if ul < 1e-12 {
        return true;
    }
    u = [u[0] / ul, u[1] / ul, u[2] / ul];
    let v = [
        nrm[1] * u[2] - nrm[2] * u[1],
        nrm[2] * u[0] - nrm[0] * u[2],
        nrm[0] * u[1] - nrm[1] * u[0],
    ];
    let mut c = [0.0f64; 3];
    for q in rim_pts {
        c[0] += q[0];
        c[1] += q[1];
        c[2] += q[2];
    }
    let inv = 1.0 / m as f64;
    let c = [c[0] * inv, c[1] * inv, c[2] * inv];
    let to2d = |q: [f64; 3]| -> [f64; 2] {
        let d = [q[0] - c[0], q[1] - c[1], q[2] - c[2]];
        [
            d[0] * u[0] + d[1] * u[1] + d[2] * u[2],
            d[0] * v[0] + d[1] * v[1] + d[2] * v[2],
        ]
    };
    let poly: Vec<[f64; 2]> = rim_pts.iter().map(|q| to2d(*q)).collect();
    let pp = to2d(p);
    // Ray-casting point-in-polygon.
    let mut inside = false;
    let mut j = m - 1;
    for i in 0..m {
        let (xi, yi) = (poly[i][0], poly[i][1]);
        let (xj, yj) = (poly[j][0], poly[j][1]);
        if (yi > pp[1]) != (yj > pp[1])
            && pp[0] < (xj - xi) * (pp[1] - yi) / (yj - yi) + xi
        {
            inside = !inside;
        }
        j = i;
    }
    if inside {
        return true;
    }
    // Distance to the polygon boundary (in-plane), tolerant to `tol`.
    let mut best = f64::INFINITY;
    for i in 0..m {
        let a = poly[i];
        let b = poly[(i + 1) % m];
        let abx = b[0] - a[0];
        let aby = b[1] - a[1];
        let apx = pp[0] - a[0];
        let apy = pp[1] - a[1];
        let denom = abx * abx + aby * aby;
        let t = if denom > 1e-18 {
            ((apx * abx + apy * aby) / denom).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let dx = apx - t * abx;
        let dy = apy - t * aby;
        let d = (dx * dx + dy * dy).sqrt();
        if d < best {
            best = d;
        }
    }
    best <= tol
}

/// True when the segment `a`->`b` crosses the surface away from both ends (a
/// crossing within `tol` of either endpoint is a grazing hit at the rim or the
/// sample itself and is ignored).
fn plug_pokes_wall(bvh: &MeshBvh, a: [f64; 3], b: [f64; 3], tol: f64) -> bool {
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if len <= 2.0 * tol {
        return false;
    }
    let dir = [d[0] / len, d[1] / len, d[2] / len];
    let start = [a[0] + dir[0] * tol, a[1] + dir[1] * tol, a[2] + dir[2] * tol];
    let max_t = len - 2.0 * tol;
    bvh.intersect_ray(start, dir, max_t, 1e-9).crossings > 0
}

/// Leak test for one plug. Only the plug's NEW geometry is sampled: the
/// vertices it appends (index >= `orig_nv`) and the original rim vertices it
/// relocated (`region.moved`). A cap built purely from the existing rim cannot
/// protrude, so it is never a leak. A sample is a leak when it lies in empty
/// space (`w < 0.5`), is NOT over the hole aperture, and is farther than `tol`
/// from the nearest original surface -- i.e. the plug pokes out through a wall
/// into open space (material a slicer would print outside the model). Plug
/// faces inside the solid, over the aperture, or touching existing geometry
/// are accepted; self-intersections are ignored (slicers union overlapping
/// volumes), so this replaces the former local-SI rejection.
fn plug_leak(
    orig_bvh: &MeshBvh,
    orig_verts: &[[f64; 3]],
    new_verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    region: &PlugRegion,
    tol: f64,
    orig_nv: usize,
) -> bool {
    if region.moved.is_empty() && region.tri_start >= tris.len() {
        return false;
    }
    let rim_pts: Vec<[f64; 3]> = region
        .rim
        .iter()
        .filter_map(|&v| orig_verts.get(v as usize).copied())
        .collect();
    let moved: HashSet<u32> = region.moved.iter().map(|(v, _)| *v).collect();
    for f in region.tri_start..region.tri_end {
        if f >= tris.len() {
            break;
        }
        let t = tris[f];
        for &v in &t {
            let vi = v as usize;
            // Only added vertices and relocated originals can protrude.
            if vi < orig_nv && !moved.contains(&v) {
                continue;
            }
            let p = match new_verts.get(vi) {
                Some(p) => *p,
                None => continue,
            };
            if orig_bvh.winding_at(p) >= 0.5 {
                continue; // inside the solid
            }
            if over_aperture(p, &rim_pts, tol) {
                continue; // bridging the hole
            }
            let (_q, d, _i) = orig_bvh.closest_point(p);
            if d <= tol {
                continue; // touching/grazing the original surface
            }
            // Outside the solid, not over the aperture and not touching it: a
            // genuine leak only when the plug reaches it by crossing a wall
            // from the hole rim (a bridge over the opening stays clear).
            let mut best = f64::INFINITY;
            let mut nearest: Option<[f64; 3]> = None;
            for q in &rim_pts {
                let dx = p[0] - q[0];
                let dy = p[1] - q[1];
                let dz = p[2] - q[2];
                let d2 = dx * dx + dy * dy + dz * dz;
                if d2 < best {
                    best = d2;
                    nearest = Some(*q);
                }
            }
            if let Some(r) = nearest {
                if plug_pokes_wall(orig_bvh, r, p, tol) {
                    return true;
                }
            }
        }
    }
    false
}

/// Per-plug leak mask: `true` for every plug region whose new surface pokes
/// outside the original solid. The winding BVH is built once over the tier
/// input and shared across regions.
fn plug_leak_mask(
    orig_bvh: &MeshBvh,
    orig_verts: &[[f64; 3]],
    new_verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    regions: &[PlugRegion],
    tol: f64,
    orig_nv: usize,
) -> Vec<bool> {
    regions
        .iter()
        .map(|region| {
            plug_leak(orig_bvh, orig_verts, new_verts, tris, region, tol, orig_nv)
        })
        .collect()
}

/// Normal of triangle `(a, b, c)`, zero for a degenerate face.
fn tri_normal(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> [f64; 3] {
    [
        (b[1] - a[1]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[1] - a[1]),
        (b[2] - a[2]) * (c[0] - a[0]) - (b[0] - a[0]) * (c[2] - a[2]),
        (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]),
    ]
}

/// Cosine of the coincidence angle: a plug face whose normal is within this of
/// an original face normal (same or opposite orientation) and whose centroid
/// lies within `tol` of it duplicates that surface.
const DOUBLE_WALL_COS: f64 = 0.9396926207859084; // cos(20 deg)
/// Reject a plug when more than this fraction of its new-face area coincides
/// with an existing (surviving) surface with the OPPOSITE orientation (normal
/// angle > 160 deg). Same-orientation coincidence is harmless for FDM (a
/// positive-fill slicer unions it); opposite coincidence can cancel material
/// in even-odd slicing.
const DOUBLE_WALL_AREA_FRAC: f64 = 0.20;

/// Coincidence breakdown of one plug against the surviving original surface:
/// `(total_area, opposite_area, same_area)`. A face counts as coincident when
/// its centroid lies within `tol` of an original face, projects inside it and
/// its normal is within 20 deg of the original (same) or within 20 deg of
/// anti-parallel (opposite).
fn plug_coincidence(
    orig_bvh: &MeshBvh,
    orig_verts: &[[f64; 3]],
    orig_tris: &[[u32; 3]],
    new_verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    region: &PlugRegion,
    tol: f64,
) -> (f64, f64, f64) {
    let mut total_area = 0.0f64;
    let mut opposite_area = 0.0f64;
    let mut same_area = 0.0f64;
    for f in region.tri_start..region.tri_end {
        if f >= tris.len() {
            break;
        }
        let t = tris[f];
        if t.iter().any(|&v| v as usize >= new_verts.len()) {
            continue;
        }
        let a = new_verts[t[0] as usize];
        let b = new_verts[t[1] as usize];
        let c = new_verts[t[2] as usize];
        let n = tri_normal(a, b, c);
        let nl = norm3(n);
        total_area += 0.5 * nl;
        if nl < 1e-12 {
            continue;
        }
        let centroid = [(a[0] + b[0] + c[0]) / 3.0, (a[1] + b[1] + c[1]) / 3.0,
                        (a[2] + b[2] + c[2]) / 3.0];
        let (_q, d, fi) = orig_bvh.closest_point(centroid);
        if d >= tol || fi == usize::MAX {
            continue;
        }
        let ot = orig_tris[fi];
        if ot.iter().any(|&v| v as usize >= orig_verts.len()) {
            continue;
        }
        let oa = orig_verts[ot[0] as usize];
        let ob = orig_verts[ot[1] as usize];
        let oc = orig_verts[ot[2] as usize];
        let on = tri_normal(oa, ob, oc);
        let onl = norm3(on);
        if onl < 1e-12 {
            continue;
        }
        // Perpendicular distance from the centroid to the ORIGINAL face plane.
        let w = [centroid[0] - oa[0], centroid[1] - oa[1], centroid[2] - oa[2]];
        let h = (w[0] * on[0] + w[1] * on[1] + w[2] * on[2]) / onl;
        if h.abs() >= tol {
            continue;
        }
        // The centroid must project INSIDE the original triangle: an adjacent
        // coplanar rim face (sharing an edge) projects outside and is not
        // coincident.
        let proj = [centroid[0] - h * on[0] / onl, centroid[1] - h * on[1] / onl,
                    centroid[2] - h * on[2] / onl];
        let v0 = [ob[0] - oa[0], ob[1] - oa[1], ob[2] - oa[2]];
        let v1 = [oc[0] - oa[0], oc[1] - oa[1], oc[2] - oa[2]];
        let v2 = [proj[0] - oa[0], proj[1] - oa[1], proj[2] - oa[2]];
        let dot = |p: [f64; 3], q: [f64; 3]| p[0] * q[0] + p[1] * q[1] + p[2] * q[2];
        let d00 = dot(v0, v0);
        let d01 = dot(v0, v1);
        let d11 = dot(v1, v1);
        let d20 = dot(v2, v0);
        let d21 = dot(v2, v1);
        let denom = d00 * d11 - d01 * d01;
        if denom.abs() < 1e-18 {
            continue;
        }
        let u = (d11 * d20 - d01 * d21) / denom;
        let vv = (d00 * d21 - d01 * d20) / denom;
        let inside = u >= -1e-4 && vv >= -1e-4 && u + vv <= 1.0 + 1e-4;
        if !inside {
            continue;
        }
        let cos = (n[0] * on[0] + n[1] * on[1] + n[2] * on[2]) / (nl * onl);
        if cos >= DOUBLE_WALL_COS {
            same_area += 0.5 * nl;
        } else if cos <= -DOUBLE_WALL_COS {
            opposite_area += 0.5 * nl;
        }
    }
    (total_area, opposite_area, same_area)
}

/// A plug is a double wall when more than `DOUBLE_WALL_AREA_FRAC` of its new
/// face area coincides with a surviving original face with the OPPOSITE
/// orientation (normal angle > 160 deg). Such a plug can cancel material in an
/// even-odd slicer; the WHOLE plug is rolled back (dropping individual faces
/// would reopen the hole). Same-orientation coincidence is accepted.
fn plug_is_double_wall(
    orig_bvh: &MeshBvh,
    orig_verts: &[[f64; 3]],
    orig_tris: &[[u32; 3]],
    new_verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    region: &PlugRegion,
    tol: f64,
) -> bool {
    let (total, opposite, _same) =
        plug_coincidence(orig_bvh, orig_verts, orig_tris, new_verts, tris, region, tol);
    total > 0.0 && opposite > DOUBLE_WALL_AREA_FRAC * total
}

/// Per-plug double-wall mask (see `plug_is_double_wall`).
fn plug_double_wall_mask(
    orig_bvh: &MeshBvh,
    orig_verts: &[[f64; 3]],
    orig_tris: &[[u32; 3]],
    new_verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    regions: &[PlugRegion],
    tol: f64,
) -> Vec<bool> {
    regions
        .iter()
        .map(|region| {
            plug_is_double_wall(orig_bvh, orig_verts, orig_tris, new_verts, tris,
                                region, tol)
        })
        .collect()
}

/// True when the plug region is edge-connected to the rest of the output: it
/// must have at least one region-boundary edge (an edge used once within the
/// region, i.e. the plug is NOT a closed sub-shell of its own) AND that edge
/// must be shared with a face outside the region. A plug that fails either test
/// is a floating / self-contained shell a slicer may read as a separate part.
fn plug_edges_connected(
    tris: &[[u32; 3]],
    region: &PlugRegion,
    out_edge_count: &HashMap<(u32, u32), i32>,
) -> bool {
    let mut region_use: HashMap<(u32, u32), i32> = HashMap::new();
    for f in region.tri_start..region.tri_end {
        if f >= tris.len() {
            break;
        }
        let t = tris[f];
        for (x, y) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            *region_use.entry(reload_edge_key(x, y)).or_insert(0) += 1;
        }
    }
    if region_use.is_empty() {
        return false;
    }
    let mut connected = false;
    for (e, &ru) in &region_use {
        if ru == 1 && out_edge_count.get(e).copied().unwrap_or(0) > ru {
            connected = true;
            break;
        }
    }
    connected
}

/// Standard per-plug numeric metrics as a JSON array (see the plug-metrics
/// spec). Computed on the pre-rollback output arrays; `rolled[ri]` marks a
/// region the adoption gates dropped (skipped from the report). `tol` is the
/// leak/deviation tolerance in mesh units.
#[allow(clippy::too_many_arguments)]
fn build_plug_metrics_json(
    orig_bvh: &MeshBvh,
    orig_verts: &[[f64; 3]],
    orig_tris: &[[u32; 3]],
    new_verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    regions: &[PlugRegion],
    rolled: &[bool],
    leak: &[bool],
    dw: &[bool],
    floating: &[bool],
    tol: f64,
) -> String {
    let mut out = String::from("[");
    for (ri, region) in regions.iter().enumerate() {
        if rolled.get(ri).copied().unwrap_or(false) {
            continue;
        }
        let (total, opposite, same) =
            plug_coincidence(orig_bvh, orig_verts, orig_tris, new_verts, tris, region, tol);
        let coincident_frac = if total > 0.0 { (opposite + same) / total } else { 0.0 };
        let mut area = 0.0f64;
        let mut min_edge = f64::INFINITY;
        let mut max_edge = 0.0f64;
        for f in region.tri_start..region.tri_end {
            if f >= tris.len() {
                break;
            }
            let t = tris[f];
            if t.iter().any(|&v| v as usize >= new_verts.len()) {
                continue;
            }
            let a = new_verts[t[0] as usize];
            let b = new_verts[t[1] as usize];
            let c = new_verts[t[2] as usize];
            area += tri_area(a, b, c);
            for (p, q) in [(a, b), (b, c), (c, a)] {
                let e = norm3([q[0] - p[0], q[1] - p[1], q[2] - p[2]]);
                if e < min_edge {
                    min_edge = e;
                }
                if e > max_edge {
                    max_edge = e;
                }
            }
        }
        let min_edge = if min_edge.is_finite() { min_edge } else { 0.0 };
        let max_aspect = if min_edge > 1e-12 { max_edge / min_edge } else { 0.0 };
        if out.len() > 1 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"m\":{},\"method\":\"{}\",\"stent\":{},\"n_faces\":{},\"area_mm2\":{:.6},\"coincident_frac\":{:.4},\"leak\":{},\"double_wall\":{},\"connected\":{},\"min_edge_mm\":{:.6},\"max_aspect\":{:.3},\"tol\":{:.6}}}",
            region.rim.len(),
            region.method,
            region.stent,
            region.tri_end - region.tri_start,
            area,
            coincident_frac,
            leak.get(ri).copied().unwrap_or(false),
            dw.get(ri).copied().unwrap_or(false),
            !floating.get(ri).copied().unwrap_or(false),
            min_edge,
            max_aspect,
            tol,
        ));
    }
    out.push(']');
    out
}

// --- Stent plug (thin-plate active-set inflate, Method #17 candidate) ----
/// Opt-out switch for the stent plug candidate. Unset -> enabled; `0`/`false`
/// -> disabled and the tier is a byte-exact no-op.
pub fn stent_plug_enabled() -> bool {
    match std::env::var("SUTURA_TAMP_STENT") {
        Ok(v) => v != "0" && !v.eq_ignore_ascii_case("false"),
        Err(_) => true,
    }
}

/// Dimensionless inflation pressure scale (multiplied by the median edge
/// length). The active set freezes a free vertex the instant it reaches the
/// shell and the collision clamp bounds every step, so an over-large value is
/// contained; a few edges reaches a crater wall within the iteration cap.
const STENT_PRESSURE: f64 = 6.0;
/// Active-set iterations for one stent candidate.
const STENT_MAX_ITERS: usize = 6;
/// Per-loop wall-clock cap for the fallback pass, in milliseconds.
const STENT_LOOP_BUDGET_MS: u64 = 500;

/// Prune back-and-forth antenna spur pairs where `dist(pts[i], pts[i+2]) < 1e-4`.
/// Operates on a LOCAL rim copy only: the caller must never pass the rim that
/// feeds the reference fit / other tiers.
fn prune_spur_slits(rim: &mut Vec<u32>, verts: &[[f64; 3]]) {
    loop {
        let n = rim.len();
        if n < 3 {
            break;
        }
        let mut pruned = false;
        for i in 0..n {
            let v0 = rim[i] as usize;
            let v2 = rim[(i + 2) % n] as usize;
            let p0 = verts[v0];
            let p2 = verts[v2];
            let d2 = (p2[0] - p0[0]).powi(2) + (p2[1] - p0[1]).powi(2) + (p2[2] - p0[2]).powi(2);
            if d2 < 1e-8 {
                let idx1 = (i + 1) % n;
                let idx2 = (i + 2) % n;
                if idx1 < idx2 {
                    rim.remove(idx2);
                    rim.remove(idx1);
                } else {
                    rim.remove(idx1);
                    rim.remove(idx2);
                }
                pruned = true;
                break;
            }
        }
        if !pruned {
            break;
        }
    }
}

/// Outward prior from the rim's incident boundary face normals (outward for a
/// consistently oriented shell). Zero vector when unavailable.
fn outward_prior(edge_normals: &HashMap<(u32, u32), [f64; 3]>, rim: &[u32]) -> [f64; 3] {
    let mut s = [0.0f64; 3];
    let m = rim.len();
    for i in 0..m {
        if let Some(n) = edge_normals.get(&ekey2(rim[i], rim[(i + 1) % m])) {
            s[0] += n[0];
            s[1] += n[1];
            s[2] += n[2];
        }
    }
    let l = norm3(s);
    if l > 1e-9 {
        [s[0] / l, s[1] / l, s[2] / l]
    } else {
        [0.0, 0.0, 0.0]
    }
}

/// Thin-plate (biharmonic `L^2`) active-set obstacle solve with a constant
/// outward pressure on the free vertices. Rim vertices (`0..m`) and any free
/// vertex that has reached the shell (within `tol`) are Dirichlet (frozen); a
/// Newton step that would cross the shell is clamped to the crossing.
fn solve_thin_plate_active_set(
    p: &mut Vec<[f64; 3]>,
    faces: &[[u32; 3]],
    m: usize,
    bvh: &MeshBvh,
    h: f64,
    tol: f64,
    n_out: [f64; 3],
    max_iters: usize,
    deadline: Option<Instant>,
) {
    let n = p.len();
    if n <= m {
        return;
    }
    let mut active = vec![false; n];
    for i in 0..m {
        active[i] = true;
    }

    let mut nb: Vec<Vec<u32>> = vec![Vec::new(); n];
    for f in faces {
        for &(u, w) in &[(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
            nb[u as usize].push(w);
            nb[w as usize].push(u);
        }
    }
    for v in nb.iter_mut() {
        v.sort_unstable();
        v.dedup();
    }

    for _iter in 0..max_iters {
        if deadline_passed(deadline) {
            break;
        }
        for i in m..n {
            if !active[i] {
                let (_q, dist, _) = bvh.closest_point(p[i]);
                if dist <= tol {
                    active[i] = true;
                }
            }
        }
        let free: Vec<usize> = (m..n).filter(|&i| !active[i]).collect();
        if free.is_empty() {
            break;
        }
        let n_free = free.len();
        let mut free_map = vec![usize::MAX; n];
        for (k, &idx) in free.iter().enumerate() {
            free_map[idx] = k;
        }

        let mut v_normals = vec![[0.0f64; 3]; n];
        for f in faces {
            let a = p[f[0] as usize];
            let b = p[f[1] as usize];
            let c = p[f[2] as usize];
            let fnorm = cross3(
                [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
                [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
            );
            for &v in f {
                v_normals[v as usize][0] += fnorm[0];
                v_normals[v as usize][1] += fnorm[1];
                v_normals[v as usize][2] += fnorm[2];
            }
        }
        for &r in &free {
            let nl = norm3(v_normals[r]);
            v_normals[r] = if nl > 1e-12 {
                [v_normals[r][0] / nl, v_normals[r][1] / nl, v_normals[r][2] / nl]
            } else {
                n_out
            };
            if dot3(v_normals[r], n_out) < 0.0 {
                v_normals[r] = [-v_normals[r][0], -v_normals[r][1], -v_normals[r][2]];
            }
        }

        let mut lower: HashMap<(usize, usize), f64> = HashMap::new();
        let mut rhs = vec![[0.0f64; 3]; n_free];
        let mut mrow: Vec<(u32, f64)> = Vec::new();
        let p_force = STENT_PRESSURE * h;

        for &r in &free {
            let row_local = free_map[r];
            mrow.clear();
            for a in std::iter::once(r as u32).chain(nb[r].iter().copied()) {
                let va = if a as usize == r {
                    nb[r].len() as f64
                } else {
                    -1.0
                };
                if va == 0.0 {
                    continue;
                }
                mrow.push((a, va * nb[a as usize].len() as f64));
                for &b in &nb[a as usize] {
                    mrow.push((b, -va));
                }
            }
            mrow.sort_by_key(|&(c, _)| c);
            let mut merged: Vec<(u32, f64)> = Vec::with_capacity(mrow.len());
            for &(c, val) in &mrow {
                if let Some(last) = merged.last_mut() {
                    if last.0 == c {
                        last.1 += val;
                        continue;
                    }
                }
                merged.push((c, val));
            }

            for &(c, val) in &merged {
                let c_usize = c as usize;
                if active[c_usize] {
                    rhs[row_local][0] -= val * p[c_usize][0];
                    rhs[row_local][1] -= val * p[c_usize][1];
                    rhs[row_local][2] -= val * p[c_usize][2];
                } else {
                    let col_local = free_map[c_usize];
                    if row_local >= col_local {
                        *lower.entry((row_local, col_local)).or_insert(0.0) += val;
                    }
                }
            }
            *lower.entry((row_local, row_local)).or_insert(0.0) += 1e-9;
            rhs[row_local][0] += p_force * v_normals[r][0];
            rhs[row_local][1] += p_force * v_normals[r][1];
            rhs[row_local][2] += p_force * v_normals[r][2];
        }

        let mut trips: Vec<Triplet<usize, usize, f64>> = Vec::with_capacity(lower.len() * 2);
        for (&(hi, lo), &v) in &lower {
            trips.push(Triplet::new(hi, lo, v));
            if hi != lo {
                trips.push(Triplet::new(lo, hi, v));
            }
        }
        trips.sort_by(|a, b| a.row.cmp(&b.row).then(a.col.cmp(&b.col)));

        let mat = match SparseColMat::<usize, f64>::try_new_from_triplets(n_free, n_free, &trips) {
            Ok(mm) => mm,
            Err(_) => break,
        };
        let sym = match SymbolicLu::try_new(mat.symbolic()) {
            Ok(s) => s,
            Err(_) => break,
        };
        let lu = match Lu::try_new_with_symbolic(sym, mat.as_ref()) {
            Ok(l) => l,
            Err(_) => break,
        };
        let mut b_mat = Mat::<f64>::zeros(n_free, 3);
        for i in 0..n_free {
            b_mat[(i, 0)] = rhs[i][0];
            b_mat[(i, 1)] = rhs[i][1];
            b_mat[(i, 2)] = rhs[i][2];
        }
        lu.solve_in_place(&mut b_mat);

        let mut max_move = 0.0f64;
        for (k, &r) in free.iter().enumerate() {
            let p_target = [b_mat[(k, 0)], b_mat[(k, 1)], b_mat[(k, 2)]];
            let p_old = p[r];
            let step = [p_target[0] - p_old[0], p_target[1] - p_old[1], p_target[2] - p_old[2]];
            let slen = norm3(step);
            if slen > 1e-12 {
                let sdir = [step[0] / slen, step[1] / slen, step[2] / slen];
                let hit = bvh.intersect_ray(p_old, sdir, slen, 1e-6);
                if hit.crossings > 0 && hit.nearest_t < slen {
                    let t_hit = hit.nearest_t.max(0.0);
                    p[r] = [
                        p_old[0] + sdir[0] * t_hit,
                        p_old[1] + sdir[1] * t_hit,
                        p_old[2] + sdir[2] * t_hit,
                    ];
                    let (_q, d, _) = bvh.closest_point(p[r]);
                    if d <= tol {
                        active[r] = true;
                    }
                } else {
                    let (_q, d, _) = bvh.closest_point(p_target);
                    p[r] = p_target;
                    if d <= tol {
                        active[r] = true;
                    }
                }
                let actual = norm3([p[r][0] - p_old[0], p[r][1] - p_old[1], p[r][2] - p_old[2]]);
                if actual > max_move {
                    max_move = actual;
                }
            }
        }
        if max_move < 1e-4 * h {
            break;
        }
    }
}

/// Runaway guard for a stent patch: no added vertex may stray beyond
/// `1.5 * rim_diag` from the rim centroid. Curved-shell conformance is expected,
/// so no planarity / planar-normal test is applied.
fn stent_shape_ok(
    cand: &[[u32; 3]],
    verts: &[[f64; 3]],
    added: &[[f64; 3]],
    rim: &[u32],
    rim_diag: f64,
) -> bool {
    if cand.is_empty() || rim.len() < 3 {
        return true;
    }
    let mut centroid = [0.0f64; 3];
    for &v in rim {
        let p = verts[v as usize];
        centroid[0] += p[0];
        centroid[1] += p[1];
        centroid[2] += p[2];
    }
    let inv = 1.0 / rim.len() as f64;
    centroid = [centroid[0] * inv, centroid[1] * inv, centroid[2] * inv];
    let limit = 1.5 * rim_diag;
    for p in added {
        let d = norm3([p[0] - centroid[0], p[1] - centroid[1], p[2] - centroid[2]]);
        if !d.is_finite() || d > limit {
            return false;
        }
    }
    true
}

/// Stent plug candidate: stiff active-set thin-plate inflate from INSIDE the
/// solid. `rim` must already be a local (possibly pruned) copy. Returns
/// `(cand_tris, added_verts)`; `cand` is in global vertex indices.
#[allow(clippy::too_many_arguments)]
fn stent_plug(
    rim: &[u32],
    verts: &[[f64; 3]],
    directed_set: &HashSet<(u32, u32)>,
    edge_count: &HashMap<(u32, u32), i32>,
    bvh: &MeshBvh,
    h: f64,
    tol: f64,
    new_base: u32,
    n_prior: [f64; 3],
    deadline: Option<Instant>,
) -> Option<(Vec<[u32; 3]>, Vec<[f64; 3]>)> {
    let m = rim.len();
    if m < 3 {
        return None;
    }
    let pts: Vec<[f64; 3]> = rim.iter().map(|&v| verts[v as usize]).collect();
    let mut c = [0.0f64; 3];
    for p in &pts {
        c[0] += p[0];
        c[1] += p[1];
        c[2] += p[2];
    }
    let inv_m = 1.0 / m as f64;
    c = [c[0] * inv_m, c[1] * inv_m, c[2] * inv_m];

    let mut rim_extent = 0.0f64;
    for p in &pts {
        let d = norm3([p[0] - c[0], p[1] - c[1], p[2] - c[2]]);
        if d > rim_extent {
            rim_extent = d;
        }
    }
    if rim_extent < 1e-12 {
        return None;
    }

    let rn = newell_normal(&pts);
    let rnl = norm3(rn);
    let n_newell = if rnl > 1e-12 {
        [rn[0] / rnl, rn[1] / rnl, rn[2] / rnl]
    } else {
        [0.0, 0.0, 1.0]
    };

    let eps_test = 0.3 * rim_extent.min(h).max(tol);
    let w_pos = bvh.winding_at([
        c[0] + eps_test * n_newell[0],
        c[1] + eps_test * n_newell[1],
        c[2] + eps_test * n_newell[2],
    ]);
    let w_neg = bvh.winding_at([
        c[0] - eps_test * n_newell[0],
        c[1] - eps_test * n_newell[1],
        c[2] - eps_test * n_newell[2],
    ]);
    let w_diff = (w_pos - w_neg).abs();
    // Robust outward direction: trust the winding when unambiguous; near an open
    // aperture both sides read ~0.5, so fall back to the mesh-derived outward
    // prior (the side the solid lies on), and finally to the Newell normal.
    let n_out = if w_diff > 0.2 {
        if w_pos >= w_neg {
            [-n_newell[0], -n_newell[1], -n_newell[2]]
        } else {
            n_newell
        }
    } else if norm3(n_prior) > 1e-9 {
        n_prior
    } else {
        n_newell
    };

    // Seed: min-area DP when clean, else a centroid fan (never splits the rim).
    let mut seed_pts: Vec<[f64; 3]> = pts.clone();
    let mut seed_local_tris: Vec<[u32; 3]> = Vec::new();
    if m <= PLUG_DP_CAP {
        let dp = min_area_triangulation_filtered(
            &pts,
            PLUG_DP_CAP,
            &HashSet::new(),
            PLUG_MIN_AREA,
            PLUG_COLLINEAR_MARGIN_DEG,
        );
        if dp.len() == m - 2 {
            let mut collides = false;
            for t in &dp {
                for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                    let diff = (a as i64 - b as i64).abs();
                    let is_rim = diff == 1 || diff == (m as i64 - 1);
                    if !is_rim {
                        let e = ekey2(rim[a as usize], rim[b as usize]);
                        if edge_count.get(&e).copied().unwrap_or(0) >= 2 {
                            collides = true;
                            break;
                        }
                    }
                }
                if collides {
                    break;
                }
            }
            if !collides {
                seed_local_tris = dp;
            }
        }
    }
    if seed_local_tris.is_empty() {
        let c_idx = seed_pts.len() as u32;
        seed_pts.push(c);
        for i in 0..m {
            let next = ((i + 1) % m) as u32;
            seed_local_tris.push([i as u32, next, c_idx]);
        }
    } else {
        // One centroid split per large all-rim seed triangle (front refinement).
        let mut initial_faces = Vec::with_capacity(seed_local_tris.len() + 8);
        for f in seed_local_tris {
            let all_rim = (f[0] as usize) < m && (f[1] as usize) < m && (f[2] as usize) < m;
            let pa = seed_pts[f[0] as usize];
            let pb = seed_pts[f[1] as usize];
            let pc = seed_pts[f[2] as usize];
            if all_rim && tri_area(pa, pb, pc) > 0.5 * h * h && seed_pts.len() < 400 {
                let c_idx = seed_pts.len() as u32;
                seed_pts.push([
                    (pa[0] + pb[0] + pc[0]) / 3.0,
                    (pa[1] + pb[1] + pc[1]) / 3.0,
                    (pa[2] + pb[2] + pc[2]) / 3.0,
                ]);
                initial_faces.push([f[0], f[1], c_idx]);
                initial_faces.push([f[1], f[2], c_idx]);
                initial_faces.push([f[2], f[0], c_idx]);
            } else {
                initial_faces.push(f);
            }
        }
        seed_local_tris = initial_faces;
    }

    // Orientation that cancels the rim boundary edges.
    let base_cancels = cancellation_count_local(&seed_local_tris, rim, directed_set);
    let flipped: Vec<[u32; 3]> = seed_local_tris.iter().map(|t| [t[0], t[2], t[1]]).collect();
    let flip_cancels = cancellation_count_local(&flipped, rim, directed_set);
    if flip_cancels > base_cancels {
        seed_local_tris = flipped;
    }

    solve_thin_plate_active_set(
        &mut seed_pts,
        &seed_local_tris,
        m,
        bvh,
        h,
        tol,
        n_out,
        STENT_MAX_ITERS,
        deadline,
    );

    let added: Vec<[f64; 3]> = seed_pts[m..].to_vec();
    let cand: Vec<[u32; 3]> = seed_local_tris
        .iter()
        .map(|t| {
            let f = |x: u32| -> u32 {
                if (x as usize) < m {
                    rim[x as usize]
                } else {
                    new_base + (x as usize - m) as u32
                }
            };
            [f(t[0]), f(t[1]), f(t[2])]
        })
        .collect();

    let mut vy = verts.to_vec();
    vy.extend_from_slice(&added);
    if !plug_structure_ok(&cand, rim, &vy) {
        return None;
    }
    if cancellation_count(&cand, rim, directed_set) == 0 {
        return None;
    }
    Some((cand, added))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum LoopVerdict {
    IntentionalOpening { score: f64, reason: &'static str },
    Uncertain,
    DamageHole,
}

fn classify_boundary_loop(
    verts: &[[f64; 3]],
    rim: &[u32],
    edge_normals: &HashMap<(u32, u32), [f64; 3]>,
    h: f64,
    model_diag: f64,
    bvh: &MeshBvh,
) -> LoopVerdict {
    let m = rim.len();
    if m <= TINY_LOOP_MAX {
        return LoopVerdict::DamageHole;
    }

    // 1. Centroid
    let mut c = [0.0, 0.0, 0.0];
    for &v in rim {
        let p = verts[v as usize];
        c[0] += p[0];
        c[1] += p[1];
        c[2] += p[2];
    }
    let inv_m = 1.0 / m as f64;
    c = [c[0] * inv_m, c[1] * inv_m, c[2] * inv_m];

    // 2. Loop diameter & edge length stats
    let mut loop_diam: f64 = 0.0;
    let mut edge_lens = Vec::with_capacity(m);
    for i in 0..m {
        let p0 = verts[rim[i] as usize];
        let p1 = verts[rim[(i + 1) % m] as usize];
        let d = ((p1[0] - p0[0]).powi(2) + (p1[1] - p0[1]).powi(2) + (p1[2] - p0[2]).powi(2)).sqrt();
        edge_lens.push(d);
        for j in (i + 1)..m {
            let pj = verts[rim[j] as usize];
            let dist = ((pj[0] - p0[0]).powi(2) + (pj[1] - p0[1]).powi(2) + (pj[2] - p0[2]).powi(2)).sqrt();
            if dist > loop_diam {
                loop_diam = dist;
            }
        }
    }
    let sum_l: f64 = edge_lens.iter().sum();
    let mean_l = sum_l / m as f64;
    let var_l: f64 = edge_lens.iter().map(|&l| (l - mean_l).powi(2)).sum::<f64>() / m as f64;
    let std_l = var_l.sqrt();
    let cv_l = std_l / mean_l.max(1e-12);

    // 3. Best-fit plane via Newell normal
    let mut newell = [0.0, 0.0, 0.0];
    for i in 0..m {
        let p0 = verts[rim[i] as usize];
        let p1 = verts[rim[(i + 1) % m] as usize];
        newell[0] += (p0[1] - p1[1]) * (p0[2] + p1[2]);
        newell[1] += (p0[2] - p1[2]) * (p0[0] + p1[0]);
        newell[2] += (p0[0] - p1[0]) * (p0[1] + p1[1]);
    }
    let newell_len = (newell[0].powi(2) + newell[1].powi(2) + newell[2].powi(2)).sqrt();
    let plane_norm = if newell_len > 1e-12 {
        [newell[0] / newell_len, newell[1] / newell_len, newell[2] / newell_len]
    } else {
        [0.0, 0.0, 1.0]
    };

    // Planar residual
    let mut max_dev_plane: f64 = 0.0;
    for &v in rim {
        let p = verts[v as usize];
        let dev = ((p[0] - c[0]) * plane_norm[0] + (p[1] - c[1]) * plane_norm[1] + (p[2] - c[2]) * plane_norm[2]).abs();
        if dev > max_dev_plane {
            max_dev_plane = dev;
        }
    }
    let delta_plane = max_dev_plane / loop_diam.max(1e-12);

    // Check whether adjacent faces lie in the rim plane:
    // A hole cut into a continuous surface (flat plate, curved wall, sphere) has adjacent faces
    // whose normals align with the rim normal (mean dot >= 0.65).
    // An intentional opening terminating a wall (cup mouth, pipe end) has adjacent wall faces
    // that are orthogonal to the rim plane (mean dot ~ 0).
    let mut dot_sum = 0.0;
    let mut dot_count = 0;
    for i in 0..m {
        let a = rim[i];
        let b = rim[(i + 1) % m];
        let key = if a < b { (a, b) } else { (b, a) };
        if let Some(&fnorm) = edge_normals.get(&key) {
            let dot = (fnorm[0] * plane_norm[0] + fnorm[1] * plane_norm[1] + fnorm[2] * plane_norm[2]).abs();
            dot_sum += dot;
            dot_count += 1;
        }
    }
    let mean_face_dot = if dot_count > 0 { dot_sum / dot_count as f64 } else { 0.0 };
    if mean_face_dot >= 0.45 {
        return LoopVerdict::DamageHole;
    }

    // 4. Maximum turning angle along loop vertices
    let mut max_turn_rad: f64 = 0.0;
    for i in 0..m {
        let p_prev = verts[rim[(i + m - 1) % m] as usize];
        let p_curr = verts[rim[i] as usize];
        let p_next = verts[rim[(i + 1) % m] as usize];
        let e_in = [p_curr[0] - p_prev[0], p_curr[1] - p_prev[1], p_curr[2] - p_prev[2]];
        let e_out = [p_next[0] - p_curr[0], p_next[1] - p_curr[1], p_next[2] - p_curr[2]];
        let l_in = (e_in[0].powi(2) + e_in[1].powi(2) + e_in[2].powi(2)).sqrt();
        let l_out = (e_out[0].powi(2) + e_out[1].powi(2) + e_out[2].powi(2)).sqrt();
        if l_in > 1e-12 && l_out > 1e-12 {
            let cos_th = (e_in[0] * e_out[0] + e_in[1] * e_out[1] + e_in[2] * e_out[2]) / (l_in * l_out);
            let th = cos_th.clamp(-1.0, 1.0).acos();
            if th > max_turn_rad {
                max_turn_rad = th;
            }
        }
    }

    // 5. Ray casting along rim-plane normal for local wall clearance & open bore detection
    let t_max = model_diag * 2.0;
    let merge_eps = 1e-4;
    let ray_pos = bvh.intersect_ray(c, plane_norm, t_max, merge_eps);
    let ray_neg = bvh.intersect_ray(
        c,
        [-plane_norm[0], -plane_norm[1], -plane_norm[2]],
        t_max,
        merge_eps,
    );
    let dist_pos = if ray_pos.crossings > 0 { ray_pos.nearest_t } else { f64::INFINITY };
    let dist_neg = if ray_neg.crossings > 0 { ray_neg.nearest_t } else { f64::INFINITY };

    // Adaptive probe placement: along rim-plane normal at +/- 1.5 wall thickness (or 2h)
    let delta_pos = (1.5 * h)
        .min(0.4 * dist_pos)
        .min(0.3 * loop_diam)
        .max(1e-4);
    let delta_neg = (1.5 * h)
        .min(0.4 * dist_neg)
        .min(0.3 * loop_diam)
        .max(1e-4);

    let p_plus = [
        c[0] + delta_pos * plane_norm[0],
        c[1] + delta_pos * plane_norm[1],
        c[2] + delta_pos * plane_norm[2],
    ];
    let p_minus = [
        c[0] - delta_neg * plane_norm[0],
        c[1] - delta_neg * plane_norm[1],
        c[2] - delta_neg * plane_norm[2],
    ];

    let w_plus = bvh.winding_at(p_plus).abs();
    let w_minus = bvh.winding_at(p_minus).abs();
    let delta_w = (w_plus - w_minus).abs();
    let w_max = w_plus.max(w_minus);
    let rho_scale = loop_diam / model_diag.max(1e-12);

    // 6. Decision & Veto Logic
    // Veto 1: Pure exterior void (both probes in exterior void, regular rim, major structural feature)
    if w_plus < 0.25 && w_minus < 0.25 && cv_l <= 0.45 && rho_scale >= 0.10 {
        return LoopVerdict::IntentionalOpening {
            score: 1.0,
            reason: "both_exterior_void",
        };
    }

    // Veto 3: Definite damage hole on vessel (high winding jump + non-planar or ragged)
    if delta_w >= 0.50 && (delta_plane >= 0.04 || cv_l >= 0.45) {
        return LoopVerdict::DamageHole;
    }

    // Missing face on solid polyhedron (box with deleted face, prism missing end):
    // Small vertex count or sharp polygon corners with solid interior transition
    if delta_w >= 0.50 && (m <= 5 || max_turn_rad >= 60.0_f64.to_radians()) {
        return LoopVerdict::DamageHole;
    }

    // Veto 2 Check: Smooth circular/curved CAD mouth
    let is_geometric_cad_rim = rho_scale >= 0.10
        && delta_plane <= 0.03
        && cv_l <= 0.35
        && m >= 8
        && max_turn_rad <= 45.0_f64.to_radians();

    if is_geometric_cad_rim {
        if delta_w < 0.35 || w_max < 0.35 {
            return LoopVerdict::IntentionalOpening {
                score: 0.95,
                reason: "planar_cad_mouth",
            };
        } else if ray_pos.crossings == 0 && ray_neg.crossings == 0 {
            return LoopVerdict::IntentionalOpening {
                score: 0.95,
                reason: "open_through_bore",
            };
        } else {
            // Ambiguous conflict between CAD mouth geometry and cavity winding:
            // e.g. Thin-shell cup whose hollow bowl has high winding, but rim is smooth CAD circle.
            // Never-worse principle: do not seal.
            return LoopVerdict::Uncertain;
        }
    }

    // Default: damage hole to seal
    LoopVerdict::DamageHole
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

    // Loop-extraction experiments (half-edge decomposition, loop grouping,
    // piecewise split, pinched-vertex fan split) are opt-in behind
    // `SUTURA_TAMP_MANIFOLD`; the default is the historical greedy walker, so
    // the tier output stays byte-identical unless the flag is set.
    let manifold_mode = std::env::var("SUTURA_TAMP_MANIFOLD").is_ok();
    // Keep the pre-fan-split arrays for the per-region greedy fallback below.
    let orig_verts: &[[f64; 3]] = verts;
    let orig_tris: &[[u32; 3]] = tris;
    // Pinched-vertex fan split: separate boundary rims that meet at one vertex
    // before loop extraction (loop-extraction preprocessing only).
    let split_pair = if manifold_mode {
        Some(fan_split_pinched_vertices(verts, tris))
    } else {
        None
    };
    let (verts, tris): (&[[f64; 3]], &[[u32; 3]]) = match &split_pair {
        Some((v, t)) => (v.as_slice(), t.as_slice()),
        None => (verts, tris),
    };
    let directed = directed_boundary_edges(tris);
    let mut loops = if directed.is_empty() {
        Vec::new()
    } else if manifold_mode {
        boundary_loops_manifold(tris)
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
    // surface is available (lead decision, 10 Oct). The manifold path
    // re-derives this after splitting/grouping (below).
    let mut single_shell =
        !manifold_mode && loops.len() == 1 && count_shell_components(verts.len(), tris) == 1;

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

    // Per-loop group-outer rim (default: the loop itself). In the manifold path
    // a long / complex rim is first split into simple sub-rims (piecewise hole
    // filling, Jun 2005), then fragmented loops that belong to one geometric
    // hole are grouped; every loop of a group seals against the group's OUTER
    // rim so a single reference surface spans the whole hole rather than the
    // fragment.
    let mut loop_outer: Vec<usize> = (0..loops.len()).collect();
    // The loops as they are BEFORE the piecewise split (one entry per "parent
    // hole"); every sub-rim carries the index of its parent so all of them can
    // share ONE reference fitted on the intact ring of the unsplit hole.
    let orig_loops: Vec<Vec<u32>> = if manifold_mode { loops.clone() } else { Vec::new() };
    let mut loop_parent: Vec<usize> = (0..loops.len()).collect();
    // Greedy sub-loops per manifold region (outer-loop index), populated only
    // in manifold mode for the per-region fallback.
    let mut greedy_by_region: HashMap<usize, Vec<Vec<u32>>> = HashMap::new();
    if manifold_mode {
        if loops.iter().any(|l| l.len() > SPLIT_MAX_VERTS) {
            let (split, parents) = split_long_loops(loops, verts, &bvh);
            loops = split;
            loop_parent = parents;
            loop_outer = (0..loops.len()).collect();
        }
        let loop_groups = group_boundary_loops(&loops, verts, h);
        for (members, outer) in &loop_groups {
            for &li in members {
                loop_outer[li] = *outer;
            }
        }
        single_shell =
            loop_groups.len() == 1 && count_shell_components(verts.len(), tris) == 1;
        // Per-region greedy fallback (T10). The manifold walker can present a
        // whole damaged region as ONE long rim (e.g. plate_c_dent_large: a
        // single m=86 rim) while the historical greedy walker -- run on the
        // ORIGINAL input, before the pinched-vertex fan split -- splits the
        // same region into sub-loops, some of which seal. If EVERY manifold
        // loop of a region fails its candidates, the region is retried with
        // its greedy sub-loops, sharing the region's parent reference. A region
        // that already sealed is never mixed with the greedy decomposition.
        let region_positions: HashMap<usize, HashSet<[u32; 3]>> = {
            let mut m: HashMap<usize, HashSet<[u32; 3]>> = HashMap::new();
            for (li, l) in loops.iter().enumerate() {
                let e = m.entry(loop_outer[li]).or_default();
                for &v in l {
                    e.insert(f32_position_key(verts[v as usize]));
                }
            }
            m
        };
        let d_orig = directed_boundary_edges(orig_tris);
        for gl in build_boundary_loops(&d_orig) {
            let gl_pos: HashSet<[u32; 3]> = gl
                .iter()
                .map(|&v| f32_position_key(orig_verts[v as usize]))
                .collect();
            let mut best: Option<(usize, usize)> = None;
            for (&region, pos) in &region_positions {
                let ov = gl_pos.iter().filter(|p| pos.contains(*p)).count();
                if ov > 0 && best.map_or(true, |(b, _)| ov > b) {
                    best = Some((ov, region));
                }
            }
            if let Some((_, region)) = best {
                greedy_by_region.entry(region).or_default().push(gl);
            }
        }
    }

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
    // Stent / leak-gate tolerance: `clamp(diag/10000, 0.0125, 0.05)`.
    let tol = leak_tol(model_diag);

    let mut out_verts = verts.to_vec();
    let mut out_tris = tris.to_vec();
    let mut plug_count = 0usize;
    let mut plug_rejected_count = 0usize;
    let mut plug_inconclusive_count = 0usize;
    let mut plug_rejected_size_count = 0usize;
    let mut plug_rejected_dev_count = 0usize;
    let mut plug_face_count = 0usize;
    // One entry per accepted plug, for the final reload-honest safety net.
    let mut plug_regions: Vec<PlugRegion> = Vec::new();

    // Input adjacency for the T3 reference-surface fit (vertex/edge -> faces).
    let adj = if loops.is_empty() { None } else { Some(build_adjacency(verts.len(), tris)) };

    // One reference per parent hole (manifold path): fit ONCE on the intact ring
    // of the ORIGINAL unsplit loop and share it with every sub-rim of that hole.
    // A sub-rim never fits its own reference -- its ring is cut by the split, so
    // the fit would extrapolate from a fragment.
    let parent_refs: Vec<Option<ReferenceSurface>> = if manifold_mode {
        orig_loops
            .iter()
            .map(|l| {
                let mut rim = l.clone();
                rim.dedup();
                if rim.len() >= 2 && rim.first() == rim.last() {
                    rim.pop();
                }
                if rim.len() < 3 {
                    return None;
                }
                adj.as_ref()
                    .and_then(|a| build_reference(verts, tris, &rim, h, a))
            })
            .collect()
    } else {
        Vec::new()
    };

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

    // Work queue: one entry per rim to try, `(rim, parent-reference hint,
    // region key)`. In manifold mode the manifold loops are queued first; once
    // they drain, the greedy sub-loops of any region that sealed nothing are
    // appended (per-region best-of, T10). A region that already sealed is never
    // re-tried with the greedy decomposition, so the two decompositions are
    // never mixed on one region.
    let mut bnd_edge_normals: HashMap<(u32, u32), [f64; 3]> =
        HashMap::with_capacity(directed.len());
    for t in tris {
        let v0 = verts[t[0] as usize];
        let v1 = verts[t[1] as usize];
        let v2 = verts[t[2] as usize];
        let e1 = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
        let e2 = [v2[0] - v0[0], v2[1] - v0[1], v2[2] - v0[2]];
        let n = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        let n_len = (n[0].powi(2) + n[1].powi(2) + n[2].powi(2)).sqrt();
        if n_len > 1e-12 {
            let un = [n[0] / n_len, n[1] / n_len, n[2] / n_len];
            for &(a, b) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                let key = if a < b { (a, b) } else { (b, a) };
                bnd_edge_normals.entry(key).or_insert(un);
            }
        }
    }

    let mut queue: VecDeque<(Vec<u32>, Option<Vec<u32>>, usize)> = VecDeque::new();
    if manifold_mode {
        for (li, loop_v) in loops.iter().enumerate() {
            let outer = loop_outer[li];
            queue.push_back((loop_v.clone(), Some(loops[outer].clone()), outer));
        }
    } else {
        for (li, loop_v) in loops.iter().enumerate() {
            queue.push_back((loop_v.clone(), None, li));
        }
    }
    let mut region_sealed: HashSet<usize> = HashSet::new();
    // Regions a keep-open decision deliberately left unsealed. They must NEVER be
    // revisited (by a greedy sub-loop injection or the stent fallback): an
    // intentional opening, an uncertain loop, a no-separation cut, a loop that
    // exhausted its own allowance, or an oversized rim.
    let mut kept_open: HashSet<usize> = HashSet::new();
    let mut greedy_injected: HashSet<usize> = HashSet::new();

    while let Some((loop_v, group_hint, region_key)) = queue.pop_front() {
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

        // The group's outer rim drives the reference fit so a fragment of a
        // pinched hole seals at the level of the whole hole. In the default
        // (greedy) path the group is the loop itself, so this is `rim`. In
        // manifold mode `group_hint` carries the region's outer rim; the
        // per-region greedy fallback shares that same parent reference.
        let group_rim: Vec<u32> = match group_hint {
            Some(g) => g,
            None => rim.clone(),
        };

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
            kept_open.insert(region_key);
            continue;
        }

        let openings_gate_enabled = config.openings_gate.unwrap_or_else(|| {
            std::env::var("SUTURA_TAMP_OPENINGS")
                .map(|v| v == "1")
                .unwrap_or(false)
        });
        if openings_gate_enabled {
            match classify_boundary_loop(verts, &rim, &bnd_edge_normals, h, model_diag, &bvh) {
                LoopVerdict::IntentionalOpening { score, reason } => {
                    info.loop_diag.push(format!(
                        "m={} rejected:intentional_opening(S={:.2},{})",
                        rim.len(), score, reason
                    ));
                    plug_rejected_count += 1;
                    kept_open.insert(region_key);
                    continue;
                }
                LoopVerdict::Uncertain => {
                    info.loop_diag.push(format!("m={} kept_open:uncertain", rim.len()));
                    plug_rejected_count += 1;
                    kept_open.insert(region_key);
                    continue;
                }
                LoopVerdict::DamageHole => {}
            }
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
        let mut reference = if manifold_mode {
            loop_parent
                .get(region_key)
                .and_then(|&p| parent_refs.get(p).cloned().flatten())
        } else if single_shell {
            adj.as_ref()
                .and_then(|a| build_reference(verts, tris, &group_rim, h, a))
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
                    kept_open.insert(region_key);
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
                    kept_open.insert(region_key);
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
        // plug. Already built above on the single-shell fast path, and on the
        // manifold path it comes from the parent-hole cache (never re-fit).
        if reference.is_none() && !single_shell && !manifold_mode {
            reference = adj
                .as_ref()
                .and_then(|a| build_reference(verts, tris, &group_rim, h, a));
        }
        // The overshoot trim below mutates `out_verts` in place. Snapshot it so
        // a loop whose plug is ultimately rejected leaves the input vertices
        // byte-identical -- otherwise a rejected reference still lifts rim
        // vertices onto an implausible surface and spikes the output.
        let trim_snapshot = if reference.is_some() {
            Some(out_verts.clone())
        } else {
            None
        };
        let mut sealed = false;
        // Every in-place vertex move this loop makes (the overshoot trim), with
        // the position each vertex held before the move. Carried into whichever
        // plug commits, so a later rollback can restore the region exactly.
        let mut loop_moved: Vec<(u32, [f64; 3])> = Vec::new();
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
                    if moved.insert(v) {
                        loop_moved.push((v, p));
                        trimmed += 1;
                    }
                    out_verts[v as usize] = refs.place(p);
                }
            }
            info.trimmed_vertices += trimmed;

            // Candidate 1: rim-only DP on the (possibly trimmed) rim.
            let mut added: Vec<[f64; 3]> = Vec::new();
            let mut is_skirt = false;
            let mut cand: Option<Vec<[u32; 3]>> = None;
            // A candidate was produced but failed the reference deviation gate
            // (reported only when no later candidate seals the loop).
            let mut rejected_dev = false;
            if let Ok(dp) = triangulate_rim_loop(&rim, &out_verts, &directed_set, tiny) {
                if reference_dev_ok(&dp, refs, &out_verts, h) {
                    cand = Some(dp);
                } else {
                    rejected_dev = true;
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
                    let mut verts_ext = out_verts.clone();
                    verts_ext.push(centre);
                    if reference_dev_ok(&fan, refs, &verts_ext, h) {
                        added.push(centre);
                        cand = Some(fan);
                    } else {
                        rejected_dev = true;
                    }
                }
            }
            // Candidate 3: skirt plug - a cap ON the reference bounded by the
            // projected rim, joined to the real rim by a one-triangle-wide
            // zipper strip, so the rim need not lie on the reference. The
            // clamped tail vertices were already lifted above; they carry no
            // strip quad. The cap deviation gate runs inside `skirt_patch`.
            if cand.is_none() {
                let clamp_mask: Vec<bool> =
                    rim.iter().map(|v| moved.contains(v)).collect();
                let new_base = out_verts.len() as u32;
                if let Some((patch, add)) =
                    skirt_patch(&rim, &out_verts, refs, &directed_set, &clamp_mask, new_base, h)
                {
                    added = add;
                    cand = Some(patch);
                    is_skirt = true;
                }
            }

            if let Some(cand) = cand {
                let cap_counts = edge_use_counts(&cand);
                let collides = cap_counts
                    .iter()
                    .any(|(e, &d)| edge_count.get(e).copied().unwrap_or(0) + d > 2);
                if !cap_shape_ok(
                    &cand,
                    &out_verts,
                    out_verts.len() as u32,
                    &added,
                    &rim,
                    rim_diag,
                    Some(refs),
                ) {
                    info.loop_diag
                        .push(format!("m={} rejected:shape", rim.len()));
                    plug_rejected_count += 1;
                    info.reference_fallback += 1;
                } else if collides {
                    info.loop_diag
                        .push(format!("m={} rejected:edge_collision", rim.len()));
                    plug_rejected_count += 1;
                    info.reference_fallback += 1;
                } else if !plug_reload_ok(
                    &out_verts,
                    &out_tris,
                    out_verts.len() as u32,
                    &added,
                    &cand,
                ) {
                    // The plug is clean by id but its vertices coincide by
                    // float32 position, so the STL reload would weld it into a
                    // non-manifold edge. Leave the hole for a later tier.
                    info.loop_diag
                        .push(format!("m={} rejected:reload_nm", rim.len()));
                    plug_rejected_count += 1;
                    info.reference_fallback += 1;
                } else {
                    for (e, &d) in &cap_counts {
                        *edge_count.entry(*e).or_insert(0) += d;
                    }
                    let n_added = added.len();
                    out_verts.extend_from_slice(&added);
                    info.patch_vertices += n_added;
                    plug_face_count += cand.len();
                    let tri_start = out_tris.len();
                    out_tris.extend(cand);
                    plug_regions.push(PlugRegion {
                        tri_start,
                        tri_end: out_tris.len(),
                        reference: true,
                        steiner: n_added > 0,
                        moved: loop_moved.clone(),
                        rim: group_rim.clone(),
                        stent: false,
                        method: if is_skirt { "skirt" } else { "reference" },
                    });
                    plug_count += 1;
                    info.reference_used += 1;
                    let is_rbf = matches!(refs.model, RefModel::Rbf(_));
                    if is_rbf {
                        info.reference_rbf += 1;
                    }
                    sealed = true;
                    if is_skirt {
                        info.reference_skirt += 1;
                    }
                    info.loop_diag.push(format!(
                        "m={} sealed:{}",
                        rim.len(),
                        if is_skirt {
                            "skirt"
                        } else if is_rbf {
                            if n_added > 0 { "rbf:fan" } else { "rbf" }
                        } else if n_added > 0 {
                            "ref:fan"
                        } else {
                            "ref"
                        }
                    ));
                }
            } else if rejected_dev {
                info.loop_diag
                    .push(format!("m={} rejected:deviation", rim.len()));
                plug_rejected_dev_count += 1;
                info.reference_fallback += 1;
            } else {
                info.reference_fallback += 1;
            }
        } else {
            info.reference_fallback += 1;
        }

        // Fallback: rim-only plug (byte-identical to the pre-T3 path), and when
        // the rim has no non-degenerate triangulation, a fan to a single
        // interior Steiner vertex placed at the rim centroid.
        if !sealed {
            let new_idx = out_verts.len() as u32;
            let tri = rim_plug_or_steiner(&rim, verts, &directed_set, tiny, new_idx);
            match tri {
                Ok((cand, centre)) => {
                    // The rim-only plug is judged against the original input
                    // positions (unchanged). The Steiner fan is judged against
                    // the output positions plus its own appended centre, since
                    // the reference trim may have moved rim vertices.
                    let (struct_ok, dev_ok) = if let Some(c) = centre {
                        let mut vy = out_verts.clone();
                        vy.push(c);
                        (
                            plug_structure_ok(&cand, &rim, &vy),
                            plug_deviation_ok(&cand, &rim, &vy, h),
                        )
                    } else {
                        (
                            plug_structure_ok(&cand, &rim, verts),
                            plug_deviation_ok(&cand, &rim, verts, h),
                        )
                    };
                    if !struct_ok {
                        info.loop_diag
                            .push(format!("m={} rejected:degenerate", rim.len()));
                        plug_rejected_count += 1;
                    } else if !dev_ok {
                        info.loop_diag
                            .push(format!("m={} rejected:deviation", rim.len()));
                        plug_rejected_dev_count += 1;
                    } else {
                        let added_local: Vec<[f64; 3]> =
                            centre.map(|c| vec![c]).unwrap_or_default();
                        let cand_counts = edge_use_counts(&cand);
                        let collides = cand_counts
                            .iter()
                            .any(|(e, &d)| edge_count.get(e).copied().unwrap_or(0) + d > 2);
                        if !cap_shape_ok(
                            &cand,
                            &out_verts,
                            out_verts.len() as u32,
                            &added_local,
                            &rim,
                            rim_diag,
                            reference.as_ref(),
                        ) {
                            info.loop_diag
                                .push(format!("m={} rejected:shape", rim.len()));
                            plug_rejected_count += 1;
                        } else if collides {
                            info.loop_diag
                                .push(format!("m={} rejected:edge_collision", rim.len()));
                            plug_rejected_count += 1;
                        } else if !plug_reload_ok(
                            &out_verts,
                            &out_tris,
                            out_verts.len() as u32,
                            &added_local,
                            &cand,
                        ) {
                            info.loop_diag
                                .push(format!("m={} rejected:reload_nm", rim.len()));
                            plug_rejected_count += 1;
                        } else {
                            for (e, &d) in &cand_counts {
                                *edge_count.entry(*e).or_insert(0) += d;
                            }
                            if let Some(c) = centre {
                                out_verts.push(c);
                                info.patch_vertices += 1;
                            }
                            plug_face_count += cand.len();
                            let tri_start = out_tris.len();
                            out_tris.extend(cand);
                            plug_regions.push(PlugRegion {
                                tri_start,
                                tri_end: out_tris.len(),
                                reference: false,
                                steiner: centre.is_some(),
                                moved: loop_moved.clone(),
                                rim: group_rim.clone(),
                                stent: false,
                                method: if centre.is_some() { "steiner" } else { "rim" },
                            });
                            plug_count += 1;
                            info.loop_diag.push(format!(
                                "m={} sealed{}{}",
                                rim.len(),
                                if reference.is_some() {
                                    ":fallback"
                                } else if tiny {
                                    ":tiny"
                                } else {
                                    ""
                                },
                                if centre.is_some() { ":steiner" } else { "" }
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
        // Stent candidate (last resort for a damage hole): thin-plate
        // active-set inflate from inside. The rim is pruned on a LOCAL copy only.
        // The shared end-of-tier gates (leak / double-wall / connectivity) and
        // the reload verdict judge the committed region.
        if !sealed && stent_plug_enabled() {
            let mut rim_b = rim.clone();
            prune_spur_slits(&mut rim_b, &out_verts);
            if rim_b.len() >= 3 {
                let n_prior = outward_prior(&bnd_edge_normals, &rim_b);
                let new_base = out_verts.len() as u32;
                let loop_deadline = match deadline {
                    Some(global) => {
                        let local = Instant::now()
                            + Duration::from_millis(STENT_LOOP_BUDGET_MS);
                        Some(if local < global { local } else { global })
                    }
                    None => {
                        Some(Instant::now() + Duration::from_millis(STENT_LOOP_BUDGET_MS))
                    }
                };
                match stent_plug(
                    &rim_b,
                    &out_verts,
                    &directed_set,
                    &edge_count,
                    &bvh,
                    h,
                    tol,
                    new_base,
                    n_prior,
                    loop_deadline,
                ) {
                    Some((cand, added)) => {
                        let cand_counts = edge_use_counts(&cand);
                        let collides = cand_counts
                            .iter()
                            .any(|(e, &d)| edge_count.get(e).copied().unwrap_or(0) + d > 2);
                        if collides {
                            info.loop_diag
                                .push(format!("m={} rejected:edge_collision", rim_b.len()));
                            info.stent_rejected += 1;
                        } else if !stent_shape_ok(&cand, &out_verts, &added, &rim_b, rim_diag) {
                            info.loop_diag
                                .push(format!("m={} rejected:shape", rim_b.len()));
                            info.stent_rejected += 1;
                        } else if !plug_reload_ok(&out_verts, &out_tris, new_base, &added, &cand) {
                            info.loop_diag
                                .push(format!("m={} rejected:reload_nm", rim_b.len()));
                            info.stent_rejected += 1;
                        } else {
                            for (e, &d) in &cand_counts {
                                *edge_count.entry(*e).or_insert(0) += d;
                            }
                            let n_added = added.len();
                            out_verts.extend_from_slice(&added);
                            info.patch_vertices += n_added;
                            plug_face_count += cand.len();
                            let tri_start = out_tris.len();
                            out_tris.extend(cand);
                            plug_regions.push(PlugRegion {
                                tri_start,
                                tri_end: out_tris.len(),
                                reference: false,
                                steiner: n_added > 0,
                                moved: loop_moved.clone(),
                                rim: group_rim.clone(),
                                stent: true,
                                method: "stent",
                            });
                            plug_count += 1;
                            info.stent_used += 1;
                            sealed = true;
                            info.loop_diag.push(format!("m={} sealed:stent", rim_b.len()));
                        }
                    }
                    None => {
                        info.stent_rejected += 1;
                    }
                }
            }
        }
        if !sealed {
            if let Some(snap) = trim_snapshot {
                out_verts = snap;
            }
        }
        if sealed {
            region_sealed.insert(region_key);
        }
        let plug_dt = tp0.elapsed().as_secs_f64();
        plug_s += plug_dt;
        if profile {
            eprintln!("[tamp]   loop m={} plug_s={:.3}", rim.len(), plug_dt);
        }

        // Once the manifold queue drains, retry every region that sealed
        // nothing with its greedy sub-loops (shared parent reference). Each
        // region is injected at most once; a region that sealed is skipped.
        if queue.is_empty() {
            for (region, gls) in &greedy_by_region {
                if !region_sealed.contains(region) && greedy_injected.insert(*region) {
                    for gl in gls {
                        queue.push_back((gl.clone(), Some(loops[*region].clone()), *region));
                    }
                }
            }
        }
    }

    // Stent fallback pass for loops no candidate could seal. Largest rejected
    // rim first; each loop bounded by its own wall-clock slice within the tier
    // deadline. Loops a keep-open decision left open are NEVER touched.
    if stent_plug_enabled() {
        let mut stent_candidates: Vec<(usize, Vec<u32>)> = Vec::new();
        for (li, loop_v) in loops.iter().enumerate() {
            let key = if manifold_mode { loop_outer[li] } else { li };
            if !region_sealed.contains(&key) && !kept_open.contains(&key) {
                stent_candidates.push((key, loop_v.clone()));
            }
        }
        stent_candidates.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

        for (region_key, loop_v) in stent_candidates {
            if deadline_passed(deadline) {
                info.reason = "timeout exceeded".to_string();
                break;
            }
            if region_sealed.contains(&region_key) || kept_open.contains(&region_key) {
                continue;
            }
            let mut rim: Vec<u32> = loop_v.clone();
            rim.dedup();
            if rim.len() >= 2 && rim.first() == rim.last() {
                rim.pop();
            }
            if rim.len() < 3 {
                continue;
            }
            prune_spur_slits(&mut rim, &out_verts);
            if rim.len() < 3 {
                continue;
            }

            let mut rim_box = BBox::empty();
            for &v in &rim {
                rim_box.expand_point(out_verts[v as usize]);
            }
            let rim_diag = rim_box.diagonal();
            if rim_diag > PLUG_MAX_LOOP_DIAM_FACTOR * h {
                continue;
            }

            let n_prior = outward_prior(&bnd_edge_normals, &rim);
            let new_base = out_verts.len() as u32;
            let loop_deadline = match deadline {
                Some(global) => {
                    let local = Instant::now() + Duration::from_millis(STENT_LOOP_BUDGET_MS);
                    Some(if local < global { local } else { global })
                }
                None => Some(Instant::now() + Duration::from_millis(STENT_LOOP_BUDGET_MS)),
            };
            let stent_res = stent_plug(
                &rim,
                &out_verts,
                &directed_set,
                &edge_count,
                &bvh,
                h,
                tol,
                new_base,
                n_prior,
                loop_deadline,
            );
            if deadline_passed(deadline) {
                // The global tier budget is gone; do not commit further plugs.
                info.reason = "timeout exceeded".to_string();
                break;
            }

            if let Some((cand, added)) = stent_res {
                let cand_counts = edge_use_counts(&cand);
                let collides = cand_counts
                    .iter()
                    .any(|(e, &d)| edge_count.get(e).copied().unwrap_or(0) + d > 2);
                if collides {
                    info.loop_diag
                        .push(format!("m={} rejected:edge_collision", rim.len()));
                    info.stent_rejected += 1;
                } else if !stent_shape_ok(&cand, &out_verts, &added, &rim, rim_diag) {
                    info.loop_diag
                        .push(format!("m={} rejected:shape", rim.len()));
                    info.stent_rejected += 1;
                } else if !plug_reload_ok(&out_verts, &out_tris, new_base, &added, &cand) {
                    info.loop_diag
                        .push(format!("m={} rejected:reload_nm", rim.len()));
                    info.stent_rejected += 1;
                } else {
                    for (e, &d) in &cand_counts {
                        *edge_count.entry(*e).or_insert(0) += d;
                    }
                    let n_added = added.len();
                    out_verts.extend_from_slice(&added);
                    info.patch_vertices += n_added;
                    plug_face_count += cand.len();
                    let tri_start = out_tris.len();
                    out_tris.extend(cand);
                    plug_regions.push(PlugRegion {
                        tri_start,
                        tri_end: out_tris.len(),
                        reference: false,
                        steiner: n_added > 0,
                        moved: Vec::new(),
                        rim: rim.clone(),
                        stent: true,
                        method: "stent",
                    });
                    plug_count += 1;
                    info.stent_used += 1;
                    region_sealed.insert(region_key);
                    info.loop_diag.push(format!("m={} sealed:stent", rim.len()));
                }
            } else {
                info.stent_rejected += 1;
            }
        }
    }

    // Final reload-honest "never worse" safety net. Mimic an STL save/reload
    // (float32 round + exact position weld) on the sealed output; if a plug
    // raised the non-manifold edge / non-manifold vertex / degenerate face
    // count over the input's, roll it back per plug. A plug that accounts for
    // the regression is dropped; a good plug survives a bad one. When the
    // regression is not attributable to any single plug (or survives the
    // per-plug fix), every plug is rolled back -- restoring the input
    // byte-for-byte, so the output can never be worse than the input.
    //
    // A reload regression is not the only way a plug can damage the mesh: a
    // plug can poke through a nearby sheet. Self-intersections are NOT repaired
    // here (slicers union overlapping volumes, so an interior crossing prints
    // fine); what matters is material OUTSIDE the solid. The leak gate
    // (`plug_leak_mask`) samples each plug against the ORIGINAL input's
    // generalized winding number and drops a plug only when a sample lies in
    // empty space farther than `leak_tol` from the nearest original surface
    // (a real protrusion). SI stays report-only.
    let mut plugs_rolled_back = 0usize;
    if !plug_regions.is_empty() {
        let base = reload_verdict(verts, tris);
        let full = reload_verdict(&out_verts, &out_tris);
        let reload_regressed = full.worse_than(&base);
        let tol = leak_tol(model_diag);
        let leak_drop = plug_leak_mask(
            &bvh, verts, &out_verts, &out_tris, &plug_regions, tol, verts.len());
        info.plugs_leaking = leak_drop.iter().filter(|&&d| d).count();
        let dw_drop = plug_double_wall_mask(
            &bvh, verts, tris, &out_verts, &out_tris, &plug_regions, tol);
        info.plugs_double_wall = dw_drop.iter().filter(|&&d| d).count();
        // Connectivity gate: every plug must be edge-connected to the shell
        // through its loop (not a floating component or a closed sub-shell).
        let float_drop: Vec<bool> = plug_regions
            .iter()
            .map(|r| !plug_edges_connected(&out_tris, r, &edge_count))
            .collect();
        info.plugs_floating = float_drop.iter().filter(|&&d| d).count();
        info.components_before = base.components;
        info.components_after = full.components;
        info.closed_shells_before = base.closed_shells;
        info.closed_shells_after = full.closed_shells;
        // Stent no-gain guard: a stent that does not STRICTLY reduce the
        // reload boundary-edge count does not seal its hole (e.g. a sub-rim of a
        // piecewise-split hole); keeping it only adds a visible membrane fin.
        // A genuinely sealing stent makes the tier-without-it worse (the loop
        // reopens), so only a no-gain region is dropped here.
        let mut stent_no_gain = vec![false; plug_regions.len()];
        for (ri, region) in plug_regions.iter().enumerate() {
            if region.stent {
                let without = reload_verdict_without(&out_verts, &out_tris, region);
                if without.boundary_loops <= full.boundary_loops {
                    stent_no_gain[ri] = true;
                }
            }
        }
        let mut rolled = vec![false; plug_regions.len()];
        if reload_regressed
            || leak_drop.iter().any(|&d| d)
            || dw_drop.iter().any(|&d| d)
            || float_drop.iter().any(|&d| d)
            || stent_no_gain.iter().any(|&d| d)
        {
            let mut drop = vec![false; plug_regions.len()];
            if reload_regressed {
                for (ri, region) in plug_regions.iter().enumerate() {
                    let without = reload_verdict_without(&out_verts, &out_tris, region);
                    if (without.boundary_edges < full.boundary_edges
                        || without.nm_edges < full.nm_edges
                        || without.nm_vertices < full.nm_vertices
                        || without.degenerate_faces < full.degenerate_faces
                        || without.components < full.components)
                        && !without.worse_than(&base)
                    {
                        drop[ri] = true;
                    }
                }
            }
            for ri in 0..plug_regions.len() {
                drop[ri] |= leak_drop[ri];
                drop[ri] |= dw_drop[ri];
                drop[ri] |= float_drop[ri];
                drop[ri] |= stent_no_gain[ri];
            }
            let any = drop.iter().any(|&d| d);
            // When every plug region is dropped the tier must be a byte-exact
            // no-op: the per-region path would still compact/re-index vertices
            // (and could leave an overshoot trim in place), so take the full
            // restore branch. The STL output was already geometry-identical;
            // this makes the in-memory arrays identical too.
            let all_dropped = drop.iter().all(|&d| d);
            let give_up = all_dropped || if any {
                // A per-plug fix whose fixed point still regresses is treated
                // as un-attributeable and falls back to a full rollback.
                reload_verdict_dropping(&out_verts, &out_tris, &plug_regions, &drop)
                    .worse_than(&base)
            } else {
                true
            };
            for (ri, region) in plug_regions.iter().enumerate() {
                rolled[ri] = give_up || drop[ri];
                if !rolled[ri] {
                    continue;
                }
                plug_face_count -= region.tri_end - region.tri_start;
                plug_count -= 1;
                if region.reference {
                    info.reference_used = info.reference_used.saturating_sub(1);
                }
                if region.steiner {
                    info.patch_vertices = info.patch_vertices.saturating_sub(1);
                }
                if region.stent {
                    info.stent_used = info.stent_used.saturating_sub(1);
                }
                plugs_rolled_back += 1;
            }
            info.plug_metrics_json = build_plug_metrics_json(
                &bvh, verts, tris, &out_verts, &out_tris, &plug_regions, &rolled,
                &leak_drop, &dw_drop, &float_drop, tol,
            );
            if give_up {
                // Restore the exact input topology and vertices (also undoes
                // any overshoot trim), guaranteeing no regression.
                out_verts = verts.to_vec();
                out_tris = tris.to_vec();
                plug_count = 0;
                plug_face_count = 0;
                info.reference_used = 0;
                info.patch_vertices = 0;
                info.trimmed_vertices = 0;
                plugs_rolled_back = plug_regions.len();
            } else {
                apply_region_rollback(&mut out_verts, &mut out_tris, &plug_regions, &drop);
            }
            let leak_fired = leak_drop.iter().filter(|&&d| d).count();
            let dw_fired = dw_drop.iter().filter(|&&d| d).count();
            let float_fired = float_drop.iter().filter(|&&d| d).count();
            info.loop_diag.push(format!(
                "rolled_back {} plug(s) ({} leaking, {} double-wall, {} floating)",
                plugs_rolled_back, leak_fired, dw_fired, float_fired
            ));
        } else {
            // No adoption gate fired: every plug survives, report them all.
            info.plug_metrics_json = build_plug_metrics_json(
                &bvh, verts, tris, &out_verts, &out_tris, &plug_regions, &rolled,
                &leak_drop, &dw_drop, &float_drop, tol,
            );
        }
    }
    info.plugs_rolled_back = plugs_rolled_back;

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
        tris.push([4, 5, 6]);
        tris.push([4, 6, 7]);
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

    #[test]
    fn test_fwn_winding_matches_accurate_cut() {
        // The fast generalized-winding path (SUTURA_TAMP_FWN) must yield the
        // same cut verdict as the accurate BVH winding on the fixtures, i.e.
        // the near-threshold exact refine makes the occupancy grid identical.
        let (cube_v, cube_t) = unit_cube_open_top();
        let (solid_v, solid_t) = unit_solid_cube();
        let (sphere_v, sphere_t) = unit_sphere();
        let fixtures: Vec<(Vec<[f64; 3]>, Vec<[u32; 3]>, Vec<u32>)> = vec![
            (cube_v, cube_t, vec![4, 5, 6, 7]),
            (solid_v, solid_t, vec![4, 5, 6, 7]),
            (sphere_v, sphere_t, vec![0, 1, 2, 3, 4, 5]),
        ];
        let accurate = TamponadeConfig::default();
        let mut fast = TamponadeConfig::default();
        fast.fwn = true;
        for (verts, tris, rim) in fixtures {
            let idx: Vec<[usize; 3]> = tris
                .iter()
                .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
                .collect();
            let bvh = MeshBvh::from_arrays(&verts, &idx);
            for &h in &[0.05_f64, 0.2_f64] {
                let a = throat_separates(&verts, &rim, h, 1.0, &bvh, &accurate, false, None);
                let f = throat_separates(&verts, &rim, h, 1.0, &bvh, &fast, false, None);
                assert_eq!(a, f, "fwn vs accurate verdict differ (h={}, rim={:?})", h, rim);
            }
        }
    }

    // ---- T3 reference-surface ("laser") tests ----------------------------

    /// A curved (or flat) `n x n` grid over [-1,1]^2 with a circular central
    /// hole, `z = a (x^2 + y^2)` (`a > 0` concave bowl, `a = 0` flat).
    fn grid_with_central_hole(n: usize, a: f64) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        grid_with_central_hole_fn(n, 0.32, move |x, y| a * (x * x + y * y))
    }

    /// The same grid with an arbitrary height function and hole radius.
    fn grid_with_central_hole_fn(
        n: usize,
        hole_r: f64,
        f: impl Fn(f64, f64) -> f64,
    ) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let step = 2.0 / n as f64;
        let mut verts = Vec::new();
        for j in 0..=n {
            for i in 0..=n {
                let x = -1.0 + i as f64 * step;
                let y = -1.0 + j as f64 * step;
                verts.push([x, y, f(x, y)]);
            }
        }
        let idx = |i: usize, j: usize| (j * (n + 1) + i) as u32;
        let mut tris = Vec::new();
        for j in 0..n {
            for i in 0..n {
                let cx = -1.0 + (i as f64 + 0.5) * step;
                let cy = -1.0 + (j as f64 + 0.5) * step;
                if (cx * cx + cy * cy).sqrt() < hole_r {
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
    fn test_plug_reload_gate_rejects_weld_non_manifold() {
        // A candidate that is clean by vertex id but adds a vertex coincident
        // (float32) with an existing one can weld a shared edge to three faces.
        // Square 0-1-2-3 (diagonal 0-2): edge 0-2 is already used twice; a
        // candidate triangle (1, new, 2) with `new` at vertex 0's position welds
        // edge 0-2 a third time.
        let out_verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let out_tris = vec![[0u32, 1, 2], [0, 2, 3]];
        let coincident = vec![[0.0, 0.0, 0.0]];
        let cand = vec![[1u32, 4, 2]];
        assert!(
            !plug_reload_ok(&out_verts, &out_tris, 4, &coincident, &cand),
            "a weld-coincident candidate must be rejected"
        );
        // The same candidate with a distinct new position is reload-clean.
        let distinct = vec![[0.5, 0.5, 1.0]];
        assert!(
            plug_reload_ok(&out_verts, &out_tris, 4, &distinct, &cand),
            "a distinct candidate must pass"
        );
    }

    #[test]
    fn test_split_long_loops_carries_parent_ids() {
        // "One reference per parent hole": every sub-rim produced by the
        // piecewise split must carry the index of the original loop it came
        // from, so all its pieces share one reference. A long circle rim splits;
        // a short triangle loop is left whole.
        let n = 80usize;
        let mut verts: Vec<[f64; 3]> = (0..n)
            .map(|i| {
                let a = i as f64 / n as f64 * std::f64::consts::TAU;
                [a.cos(), a.sin(), 0.0]
            })
            .collect();
        verts.push([10.0, 0.0, 0.0]);
        verts.push([10.0, 1.0, 0.0]);
        verts.push([10.0, 0.0, 1.0]);
        let tris: Vec<[u32; 3]> = vec![[n as u32, n as u32 + 1, n as u32 + 2]];
        let tris_usize: Vec<[usize; 3]> =
            tris.iter().map(|t| [t[0] as usize, t[1] as usize, t[2] as usize]).collect();
        let bvh = MeshBvh::from_arrays(&verts, &tris_usize);
        let long: Vec<u32> = (0..n as u32).collect();
        let short: Vec<u32> = vec![n as u32, n as u32 + 1, n as u32 + 2];
        let (split, parents) = split_long_loops(vec![long, short], &verts, &bvh);
        assert!(split.len() >= 3, "the long rim must split: {}", split.len());
        assert_eq!(*parents.last().unwrap(), 1, "the short loop keeps parent 1");
        assert!(
            parents[..parents.len() - 1].iter().all(|&p| p == 0),
            "all sub-rims of the long loop carry parent 0: {parents:?}"
        );
    }

    #[test]
    fn test_curved_big_hole_gets_reference() {
        // R2: a big hole on a curved surface needs a wider intact ring, and the
        // ring's normals spread widely. The ring must grow with the rim and the
        // normal-spread veto must be gated on the quadric rms, so the smooth
        // curved surface keeps its reference instead of being rejected as
        // "noisy". A paraboloid is locally an exact quadric (rms ~ 0) but its
        // normals tilt strongly over a wide ring.
        let (verts, tris) = grid_with_central_hole(32, 1.0);
        let adj = build_adjacency(verts.len(), &tris);
        let loops = build_boundary_loops(&directed_boundary_edges(&tris));
        let rim = loops
            .iter()
            .filter(|l| l.len() < 60)
            .max_by_key(|l| l.len())
            .expect("central hole rim");
        let refs = build_reference(&verts, &tris, rim, 0.02, &adj);
        assert!(refs.is_some(), "curved big hole must keep a reference");
    }

    #[test]
    fn test_saddle_rbf_reference_passes_loo() {
        // A genuine saddle z = xy: the quadric fit is exact on the ring, but the
        // tangent-ray split vetoes it. The R6 design routes the saddle veto into
        // the thin-plate RBF fallback, gated by a leave-one-out self-consistency
        // test, so a real smooth saddle keeps a reference (the RBF can bend both
        // ways). h is the grid step, matching the sampling scale.
        let (verts, tris) = grid_with_central_hole_fn(40, 0.5, |x, y| x * y);
        let adj = build_adjacency(verts.len(), &tris);
        let loops = build_boundary_loops(&directed_boundary_edges(&tris));
        let rim = loops
            .iter()
            .filter(|l| l.len() < 120)
            .max_by_key(|l| l.len())
            .expect("central hole rim");
        let refs = build_reference(&verts, &tris, rim, 0.05, &adj)
            .expect("saddle must get an RBF reference");
        assert!(
            matches!(refs.model, RefModel::Rbf(_)),
            "saddle reference must use the RBF model"
        );
    }

    #[test]
    fn test_rbf_big_curved_hole_gets_reference() {
        // R4: on a strongly curved surface that is NOT a single quadric, the
        // quadric fit is rejected by the rms gate (it would over-extrapolate a
        // large hole). The thin-plate RBF fallback must still produce a
        // reference, and its centre must stay inside the observed rim band
        // instead of spiking. Before R4 this loop returned `None`.
        let (verts, tris) = grid_with_central_hole_fn(40, 0.5, |x, y| {
            let r2 = x * x + y * y;
            2.0 * r2 + 3.0 * r2 * r2
        });
        let adj = build_adjacency(verts.len(), &tris);
        let loops = build_boundary_loops(&directed_boundary_edges(&tris));
        let rim = loops
            .iter()
            .filter(|l| l.len() < 120)
            .max_by_key(|l| l.len())
            .expect("central hole rim");
        let h = 0.05;
        let refs = build_reference(&verts, &tris, rim, h, &adj)
            .expect("RBF fallback must keep a reference on a large curved hole");
        let mut h_lo = f64::INFINITY;
        let mut h_hi = f64::NEG_INFINITY;
        for &v in rim {
            let z = verts[v as usize][2];
            h_lo = h_lo.min(z);
            h_hi = h_hi.max(z);
        }
        let centre = refs.place(refs.origin);
        // The centre may be genuinely below the rim (a curved crater continues
        // downward), so allow the local relief plus a margin -- but a wild
        // over-extrapolation (the failure R4 exists to prevent) is far larger.
        let slack = 2.0 * (h_hi - h_lo) + 1.0;
        assert!(
            centre[2] <= h_hi + slack && centre[2] >= h_lo - slack,
            "RBF centre z={:.3} far outside the rim band [{:.3}, {:.3}]",
            centre[2],
            h_lo,
            h_hi
        );
    }

    fn flat_reference() -> ReferenceSurface {
        ReferenceSurface {
            origin: [0.0, 0.0, 0.0],
            u: [1.0, 0.0, 0.0],
            v: [0.0, 1.0, 0.0],
            n: [0.0, 0.0, 1.0],
            model: RefModel::Quadric([0.0; 6]),
            rms: 0.0,
            concave: false,
        }
    }

    #[test]
    fn test_skirt_seals_rim_below_reference() {
        // A flat reference at z = 0 and a square rim lowered to z = -0.3: the
        // skirt must build a cap ON the reference (new vertices at z = 0) and a
        // strip down to the real rim, leaving the rim in place.
        let s = 0.5;
        let verts = vec![
            [-s, -s, -0.3],
            [s, -s, -0.3],
            [s, s, -0.3],
            [-s, s, -0.3],
        ];
        let rim: Vec<u32> = (0..4).collect();
        let directed: HashSet<(u32, u32)> =
            [(0u32, 1u32), (1, 2), (2, 3), (3, 0)].into_iter().collect();
        let clamp = vec![false; 4];
        let (tris, added) =
            skirt_patch(&rim, &verts, &flat_reference(), &directed, &clamp, 4, 0.1)
                .expect("skirt must build");
        assert_eq!(added.len(), 4, "one projected vertex per rim vertex");
        for q in &added {
            assert!(q[2].abs() < 1e-9, "projected vertex not on the reference");
        }
        // The rim itself is untouched.
        for i in 0..4 {
            assert!((verts[i][2] + 0.3).abs() < 1e-12);
        }
        // The patch boundary is exactly the rim edge set.
        let counts = edge_use_counts(&tris);
        let boundary: HashSet<(u32, u32)> = counts
            .iter()
            .filter(|(_, &c)| c % 2 == 1)
            .map(|(e, _)| *e)
            .collect();
        let rim_edges: HashSet<(u32, u32)> =
            [(0u32, 1u32), (1, 2), (2, 3), (3, 0)]
                .into_iter()
                .map(|(a, b)| ekey2(a, b))
                .collect();
        assert_eq!(boundary, rim_edges);
    }

    #[test]
    fn test_skirt_clamped_rim_omits_strip() {
        // Every rim vertex already clamped onto the reference: no strip, just
        // the cap over the rim (the "clamped instead of stitched" case).
        let s = 0.5;
        let verts = vec![
            [-s, -s, 0.0],
            [s, -s, 0.0],
            [s, s, 0.0],
            [-s, s, 0.0],
        ];
        let rim: Vec<u32> = (0..4).collect();
        let directed: HashSet<(u32, u32)> =
            [(0u32, 1u32), (1, 2), (2, 3), (3, 0)].into_iter().collect();
        let clamp = vec![true; 4];
        let (tris, added) =
            skirt_patch(&rim, &verts, &flat_reference(), &directed, &clamp, 4, 0.1)
                .expect("skirt must build");
        assert!(added.is_empty(), "a fully clamped rim adds no vertices");
        assert_eq!(tris.len(), 2, "cap only, no strip quads");
}

    fn planar_hexagon() -> (Vec<u32>, Vec<[f64; 3]>, HashSet<(u32, u32)>) {
        let rim: Vec<u32> = (0..6).collect();
        let verts = vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [3.0, 1.0, 0.0],
            [2.0, 2.0, 0.0],
            [0.0, 2.0, 0.0],
            [-1.0, 1.0, 0.0],
        ];
        let directed: HashSet<(u32, u32)> = rim
            .iter()
            .enumerate()
            .map(|(i, _)| (i as u32, ((i + 1) % rim.len()) as u32))
            .collect();
        (rim, verts, directed)
    }

    #[test]
    fn centroid_fan_is_valid_for_a_planar_rim() {
        let (rim, verts, directed) = planar_hexagon();
        let (fan_local, centre) = centroid_fan(&rim, &verts, &directed).expect("fan");
        let m = rim.len() as u32;
        let new_idx = verts.len() as u32;
        let remap = |x: u32| if x == m { new_idx } else { rim[x as usize] };
        let fan: Vec<[u32; 3]> = fan_local
            .iter()
            .map(|t| [remap(t[0]), remap(t[1]), remap(t[2])])
            .collect();
        let mut vy = verts.clone();
        vy.push(centre);
        assert_eq!(fan.len(), rim.len());
        assert!(plug_structure_ok(&fan, &rim, &vy));
        assert!(cancellation_count_local(&fan_local, &rim, &directed) * 2 > rim.len());
    }

    #[test]
    fn centroid_fan_of_a_collinear_rim_is_still_degenerate() {
        let rim: Vec<u32> = (0..5).collect();
        let verts: Vec<[f64; 3]> = (0..5).map(|i| [i as f64, 0.0, 0.0]).collect();
        let directed: HashSet<(u32, u32)> =
            (0..5).map(|i| (i as u32, ((i + 1) % 5) as u32)).collect();
        let (fan_local, centre) = centroid_fan(&rim, &verts, &directed).expect("fan built");
        let m = 5u32;
        let new_idx = 5u32;
        let remap = |x: u32| if x == m { new_idx } else { rim[x as usize] };
        let fan: Vec<[u32; 3]> = fan_local
            .iter()
            .map(|t| [remap(t[0]), remap(t[1]), remap(t[2])])
            .collect();
        let mut vy = verts.clone();
        vy.push(centre);
        assert!(!plug_structure_ok(&fan, &rim, &vy));
    }

    #[test]
    fn rim_plug_or_steiner_prefers_the_rim_only_plug() {
        let (rim, verts, directed) = planar_hexagon();
        let (tris, centre) =
            rim_plug_or_steiner(&rim, &verts, &directed, false, verts.len() as u32).expect("plug");
        assert!(centre.is_none());
        assert_eq!(tris.len(), rim.len() - 2);
        assert!(plug_structure_ok(&tris, &rim, &verts));
    }

    #[test]
    fn open_cube_with_a_collinear_rim_vertex_still_seals_rim_only() {
        // A rectangle-with-midpoint rim: the unfiltered min-area DP would pick
        // the zero-area ear and reject the plug as degenerate. The filtered DP
        // must find the non-degenerate triangulation and seal with no Steiner
        // vertex.
        let (mut verts, mut tris) = unit_cube_open_top();
        verts.push([0.5, 0.0, 1.0]); // v8: midpoint of top edge 4-5
        let split = [0u32, 5, 4];
        let pos = tris.iter().position(|t| *t == split).expect("triangle 0,5,4");
        tris.splice(pos..pos + 1, [[0, 5, 8], [0, 8, 4]]);
        let config = TamponadeConfig { h_min: Some(0.5), ..Default::default() };
        let (out_verts, out_tris, info) = tamponade_repair(&verts, &tris, &config);
        assert_eq!(info.reason, "ok", "{info:?}");
        assert_eq!(info.plugs, 1);
        assert_eq!(boundary_edge_count(&out_tris), 0);
        // Rim-only: no Steiner vertex was appended.
        assert_eq!(out_verts.len(), verts.len());
        assert!(info.patch_vertices == 0, "{info:?}");
    }

    // ---- T8 reload-honest "never worse" safety net -----------------------

    #[test]
    fn reload_verdict_flags_float32_coincident_vertex_nm() {
        // Three triangles share the edge (0,1) and their third vertex is
        // three near-identical positions that all round to the SAME float32
        // coordinate: after the reload weld the edge (0,1) carries three
        // faces, i.e. a non-manifold edge. This is exactly the damage the
        // safety net must catch (float32 rounding manufacturing a weld).
        let verts: Vec<[f64; 3]> = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 1.0 + 1e-8, 0.0],
            [0.0, 1.0 - 1e-8, 0.0],
        ];
        let tris: Vec<[u32; 3]> = vec![[0, 1, 2], [0, 1, 3], [0, 1, 4]];
        let base = reload_verdict(&verts, &tris[..2]);
        let full = reload_verdict(&verts, &tris);
        assert!(full.worse_than(&base), "NM not detected: {full:?} vs {base:?}");
        assert!(full.nm_edges > 0, "{full:?}");
    }

    #[test]
    fn reload_safety_net_rolls_back_coincident_vertex_nm() {
        let verts: Vec<[f64; 3]> = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 1.0 + 1e-8, 0.0],
            [0.0, 1.0 - 1e-8, 0.0],
        ];
        let tris: Vec<[u32; 3]> = vec![[0, 1, 2], [0, 1, 3], [0, 1, 4]];
        let base = reload_verdict(&verts, &tris[..2]);
        let full = reload_verdict(&verts, &tris);
        // The third triangle is the "plug" region.
        let region = PlugRegion { tri_start: 2, tri_end: 3, reference: false, steiner: false, moved: Vec::new(), rim: Vec::new(), stent: false, method: "rim" };
        let without = reload_verdict_without(&verts, &tris, &region);
        assert!(
            without.nm_edges < full.nm_edges && !without.worse_than(&base),
            "removing the bad plug must clear the regression: {without:?}"
        );
        // Applying the rollback drops the region and compacts the vertices.
        let mut ov = verts.clone();
        let mut ot = tris.clone();
        apply_region_rollback(&mut ov, &mut ot, &[region], &[true]);
        assert_eq!(ot.len(), 2);
        assert!(!reload_verdict(&ov, &ot).worse_than(&base));
    }

    #[test]
    fn reload_safety_net_leaves_a_clean_seal_alone() {
        // A correct rim-only cap must not regress the reload verdict, so the
        // full engine leaves it in place and reports zero rollbacks.
        let (verts, tris) = unit_cube_open_top();
        let mut out_t = tris.clone();
        out_t.push([4, 6, 5]);
        out_t.push([4, 7, 6]);
        let base = reload_verdict(&verts, &tris);
        let full = reload_verdict(&verts, &out_t);
        assert!(!full.worse_than(&base), "clean cap regressed: {full:?} vs {base:?}");
        assert_eq!(full.nm_edges, 0);
        assert_eq!(full.nm_vertices, 0);
        assert_eq!(full.degenerate_faces, 0);
        assert_eq!(full.boundary_edges, 0);

        let config = TamponadeConfig { h_min: Some(0.25), ..Default::default() };
        let (_ov, _ot, info) = tamponade_repair(&verts, &tris, &config);
        assert_eq!(info.plugs, 1, "{info:?}");
        assert_eq!(info.plugs_rolled_back, 0, "{info:?}");
    }

    // ---- T9 local self-intersection per-plug rollback --------------------

    /// A horizontal sheet (z = 0, two triangles) plus one plug face. The plug
    /// either pokes straight through the sheet or floats well above it.
    fn sheet_with_plug(poking: bool) -> (Vec<[f64; 3]>, Vec<[u32; 3]>, PlugRegion) {
        let mut verts: Vec<[f64; 3]> = vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            [2.0, 2.0, 0.0],
        ];
        let mut tris: Vec<[u32; 3]> = vec![[0, 1, 2], [1, 3, 2]];
        if poking {
            // Spans z in [-1, 1] at y = 0.5: it crosses the sheet interior.
            verts.push([0.5, 0.5, -1.0]);
            verts.push([1.5, 0.5, -1.0]);
            verts.push([1.0, 0.5, 1.0]);
        } else {
            // Floats at z in [2, 3], far above the sheet.
            verts.push([0.5, 0.5, 2.0]);
            verts.push([1.5, 0.5, 2.0]);
            verts.push([1.0, 0.5, 3.0]);
        }
        let tri_start = tris.len();
        tris.push([4, 5, 6]);
        let region = PlugRegion {
            tri_start,
            tri_end: tris.len(),
            reference: false,
            steiner: false,
            moved: Vec::new(),
            rim: Vec::new(),
            stent: false,
            method: "rim",
        };
        (verts, tris, region)
    }

    #[test]
    fn local_si_flags_a_plug_that_pokes_through_a_sheet() {
        let h = 0.1;
        let (pv, pt, pregion) = sheet_with_plug(true);
        let mask = si_rollback_mask(&pv, &pt, &[pregion], h);
        assert_eq!(mask, vec![true], "poking plug not flagged");

        let (cv, ct, cregion) = sheet_with_plug(false);
        let mask = si_rollback_mask(&cv, &ct, &[cregion], h);
        assert_eq!(mask, vec![false], "clean plug wrongly flagged");
    }

    #[test]
    fn local_si_rollback_drops_only_the_poking_plug() {
        // One poking and one clean plug in the same output; only the poking one
        // may be rolled back, leaving the clean plug and the sheet intact.
        let h = 0.1;
        let (mut verts, mut tris, poking) = sheet_with_plug(true);
        verts.push([0.5, 0.5, 2.0]);
        verts.push([1.5, 0.5, 2.0]);
        verts.push([1.0, 0.5, 3.0]);
        let clean_start = tris.len();
        tris.push([7, 8, 9]);
        let clean = PlugRegion {
            tri_start: clean_start,
            tri_end: tris.len(),
            reference: false,
            steiner: false,
            moved: Vec::new(),
            rim: Vec::new(),
            stent: false,
            method: "rim",
        };

        let regions = [poking, clean];
        let mask = si_rollback_mask(&verts, &tris, &regions, h);
        assert_eq!(mask, vec![true, false], "wrong plug attributed");

        let mut ov = verts.clone();
        let mut ot = tris.clone();
        apply_region_rollback(&mut ov, &mut ot, &regions, &mask);
        // The sheet (2) plus the clean plug (1) survive, the poking face is gone.
        assert_eq!(ot.len(), 3, "rollback removed the wrong faces");
        // The poking plug reached z = -1; after the rollback no vertex is below
        // the sheet plane, i.e. the poking geometry was dropped while the clean
        // plug above the sheet stayed.
        assert!(
            ov.iter().all(|v| v[2] >= -1e-9),
            "poking plug survived: {ov:?}"
        );
        assert!(
            ov.iter().any(|v| v[2] >= 2.0),
            "clean plug was dropped: {ov:?}"
        );
    }

    // ---- T12 exact footprint restore + cap shape gate --------------------

    #[test]
    fn rollback_restores_a_moved_original_vertex() {
        // A plug that moved an ORIGINAL vertex in place (the overshoot trim)
        // must restore that vertex's pre-move position on rollback, not only
        // drop its appended faces. Face 0 stays and references vertex 3, so the
        // restored position survives the compaction and is observable.
        let verts: Vec<[f64; 3]> = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.5, 0.5, 5.0], // original vertex, moved to z = 5
            [1.0, 1.0, 1.0], // added plug vertex
        ];
        let tris: Vec<[u32; 3]> = vec![[0, 3, 2], [0, 1, 4]];
        let region = PlugRegion {
            tri_start: 1,
            tri_end: 2,
            reference: true,
            steiner: true,
            moved: vec![(3, [0.5, 0.5, 0.05])],
            rim: Vec::new(),
            stent: false,
            method: "reference",
        };
        let mut ov = verts.clone();
        let mut ot = tris.clone();
        apply_region_rollback(&mut ov, &mut ot, &[region], &[true]);
        assert_eq!(ot.len(), 1, "the plug face must be dropped");
        assert!(
            ov.iter().all(|v| (v[2] - 5.0).abs() > 1e-9),
            "the moved vertex still carries its displaced position: {ov:?}"
        );
        assert!(
            ov.iter().any(|v| (v[2] - 0.05).abs() < 1e-9),
            "restored position missing: {ov:?}"
        );
    }

    // ---- leak gate (replaces the local-SI adoption rejection) ------------

    #[test]
    fn leak_gate_flags_only_a_plug_outside_the_solid() {
        // Closed unit cube; a plug triangle either sits inside the solid, is a
        // cap over the top opening (over the aperture), or floats laterally in
        // open space.
        let verts: Vec<[f64; 3]> = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        let tris: Vec<[u32; 3]> = vec![
            [0, 2, 1], [0, 3, 2],
            [4, 5, 6], [4, 6, 7],
            [0, 1, 5], [0, 5, 4],
            [1, 2, 6], [1, 6, 5],
            [2, 3, 7], [2, 7, 6],
            [3, 0, 4], [3, 4, 7],
        ];
        let idx: Vec<[usize; 3]> =
            tris.iter().map(|t| [t[0] as usize, t[1] as usize, t[2] as usize]).collect();
        let bvh = MeshBvh::from_arrays(&verts, &idx);
        let tol = leak_tol(2.0); // small cube -> 0.0125 floor
        assert!(tol >= 0.0125);
        let rim = vec![4u32, 5, 6, 7]; // top opening

        let probe = |pt: [f64; 3]| -> bool {
            let mut nv = verts.clone();
            nv.push(pt);
            nv.push([pt[0] + 0.05, pt[1], pt[2]]);
            nv.push([pt[0], pt[1] + 0.05, pt[2]]);
            let t = vec![[8u32, 9, 10]];
            let region = PlugRegion {
                tri_start: 0,
                tri_end: 1,
                reference: false,
                steiner: true,
                moved: Vec::new(),
                rim: rim.clone(),
                stent: false,
                method: "steiner",
            };
            plug_leak(&bvh, &verts, &nv, &t, &region, tol, 8)
        };

        assert!(!probe([0.5, 0.5, 0.5]), "an interior plug must not leak");
        assert!(
            !probe([0.5, 0.5, 1.5]),
            "a cap over the aperture must not leak"
        );
        // A true leak reaches a point by CROSSING a wall, which `plug_pokes_wall`
        // detects; a point merely outside the hole is not a leak on its own.
        assert!(
            plug_pokes_wall(&bvh, [0.5, 0.5, 1.0], [0.5, 0.5, -1.0], tol),
            "a segment through the solid must be a wall crossing"
        );
        assert!(
            !plug_pokes_wall(&bvh, [0.5, 0.5, 1.0], [0.5, 0.5, 2.0], tol),
            "a segment clear of the solid must not cross a wall"
        );
    }

    #[test]
    fn double_wall_gate_flags_a_plug_over_existing_surface() {
        let verts: Vec<[f64; 3]> = vec![
            [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [1.0, 1.0, 1.0], [0.0, 1.0, 1.0],
        ];
        let tris: Vec<[u32; 3]> = vec![
            [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7], [0, 1, 5], [0, 5, 4],
            [1, 2, 6], [1, 6, 5], [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7],
        ];
        let idx: Vec<[usize; 3]> =
            tris.iter().map(|t| [t[0] as usize, t[1] as usize, t[2] as usize]).collect();
        let bvh = MeshBvh::from_arrays(&verts, &idx);
        let tol = leak_tol(2.0);

        // A plug face coincident with the existing top face but with the SAME
        // orientation is harmless (accepted).
        let mut dup = verts.clone();
        dup.push([0.0, 0.0, 1.0]);
        dup.push([1.0, 0.0, 1.0]);
        dup.push([1.0, 1.0, 1.0]);
        let dup_tris = vec![[8u32, 9, 10]];
        let region = PlugRegion {
            tri_start: 0, tri_end: 1, reference: false, steiner: true,
            moved: Vec::new(), rim: Vec::new(), stent: false, method: "steiner",
        };
        assert!(
            !plug_is_double_wall(&bvh, &verts, &tris, &dup, &dup_tris, &region, tol),
            "a same-orientation coincident face must be accepted"
        );

        // The same face with OPPOSITE winding (anti-parallel) is a double wall.
        let opp_tris = vec![[10u32, 9, 8]];
        assert!(
            plug_is_double_wall(&bvh, &verts, &tris, &dup, &opp_tris, &region, tol),
            "an opposite-orientation coincident face must be a double wall"
        );

        // A plug face floating clear of the surface is not a double wall.
        let mut free = verts.clone();
        free.push([0.0, 0.0, 5.0]);
        free.push([1.0, 0.0, 5.0]);
        free.push([1.0, 1.0, 5.0]);
        let free_tris = vec![[8u32, 9, 10]];
        assert!(
            !plug_is_double_wall(&bvh, &verts, &tris, &free, &free_tris, &region, tol),
            "a floating plug face must not be a double wall"
        );
    }

    #[test]
    fn cap_shape_gate_rejects_a_runaway_fin() {
        let verts: Vec<[f64; 3]> = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.5, 0.5, 10.0], // apex poking 10 above a rim_diag of ~1.41
        ];
        let rim = vec![0u32, 1, 2, 3];
        let rim_diag = 2.0f64.sqrt();
        // Flat fan: acceptable.
        let flat: Vec<[u32; 3]> = vec![[0, 1, 2], [0, 2, 3]];
        assert!(
            cap_shape_ok(&flat, &verts, verts.len() as u32, &[], &rim, rim_diag, None),
            "flat cap wrongly rejected"
        );
        // Fan to a far apex: the fin. Must be rejected.
        let fin: Vec<[u32; 3]> = vec![[0, 1, 4], [1, 2, 4], [2, 3, 4], [3, 0, 4]];
        assert!(
            !cap_shape_ok(&fin, &verts, verts.len() as u32, &[], &rim, rim_diag, None),
            "runaway fin accepted"
        );
    }

    #[test]
    fn cap_shape_gate_rejects_a_flipped_face() {
        let verts: Vec<[f64; 3]> = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let rim = vec![0u32, 1, 2, 3];
        let rim_diag = 2.0f64.sqrt();
        // A flat cap plus one face wound backwards: mean normal still +z, so the
        // reversed face reads as a >90 deg flip against the ring.
        let folded: Vec<[u32; 3]> = vec![[0, 1, 2], [0, 2, 3], [0, 2, 1]];
        assert!(
            !cap_shape_ok(&folded, &verts, verts.len() as u32, &[], &rim, rim_diag, None),
            "flipped cap face accepted"
        );
    }

    #[test]
    fn test_concentric_nested_cubes_dissolves_inner() {
        let (mut verts, mut tris) = unit_solid_cube();
        // Inner cube: scaled down to 0.25 and centered at [0.5, 0.5, 0.5]
        let offset = verts.len() as u32;
        for i in 0..8 {
            let v = verts[i];
            verts.push([v[0] * 0.25 + 0.375, v[1] * 0.25 + 0.375, v[2] * 0.25 + 0.375]);
        }
        for i in 0..12 {
            let t = tris[i];
            tris.push([t[0] + offset, t[1] + offset, t[2] + offset]);
        }
        assert_eq!(tris.len(), 24);
        let (_out_verts, out_tris, info) = crate::shell::shell_analyze_and_filter(
            &verts,
            &tris,
            &crate::shell::ShellConfig::default(),
        );
        assert_eq!(info.reason, "ok");
        assert_eq!(out_tris.len(), 12);
        assert_eq!(boundary_edge_count(&out_tris), 0);
    }

    fn read_stl_u32(path: &str) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let data = std::fs::read(path).expect("read STL");
        let n = u32::from_le_bytes(data[80..84].try_into().unwrap()) as usize;
        let mut index: HashMap<[u32; 3], u32> = HashMap::new();
        let mut verts = Vec::new();
        let mut tris = Vec::new();
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
        (verts, tris)
    }

    #[test]
    fn test_a6_case1_open_cup_stays_open() {
        let path = "/tmp/v074/agy/a6-fixtures/case1_open_cup.stl";
        let (v, t) = read_stl_u32(path);
        let config = TamponadeConfig {
            openings_gate: Some(true),
            ..Default::default()
        };
        let (_ov, _ot, info) = tamponade_repair(&v, &t, &config);
        assert_eq!(info.plugs, 0, "Open cup mouth loops must not be plugged");
        assert_eq!(info.plugs_rejected, 2, "Both mouth loops must be rejected");
        assert!(info.loop_diag.iter().all(|d| d.contains("rejected:intentional_opening")));
    }

    #[test]
    fn test_a6_case2_pipe_stays_open() {
        let path = "/tmp/v074/agy/a6-fixtures/case2_pipe.stl";
        let (v, t) = read_stl_u32(path);
        let config = TamponadeConfig {
            openings_gate: Some(true),
            ..Default::default()
        };
        let (_ov, _ot, info) = tamponade_repair(&v, &t, &config);
        assert_eq!(info.plugs, 0, "Pipe ends must not be plugged");
        assert_eq!(info.plugs_rejected, 2, "Both pipe ends must be rejected");
        assert!(info.loop_diag.iter().all(|d| d.contains("rejected:intentional_opening") || d.contains("kept_open:uncertain")));
    }

    #[test]
    fn test_a6_case3_broken_cup_seals_puncture_only() {
        let path = "/tmp/v074/agy/a6-fixtures/case3_broken_cup.stl";
        let (v, t) = read_stl_u32(path);
        let config = TamponadeConfig {
            openings_gate: Some(true),
            ..Default::default()
        };
        let (_ov, _ot, info) = tamponade_repair(&v, &t, &config);
        assert_eq!(info.plugs, 1, "Puncture must be plugged while mouth stays open");
        assert_eq!(info.plugs_rejected, 2, "Both mouth loops must be rejected");
        assert!(info.loop_diag.iter().any(|d| d.contains("sealed")));
    }

    #[test]
    fn test_a6_case4_cracked_sphere_seals() {
        let path = "/tmp/v074/agy/a6-fixtures/case4_cracked_sphere.stl";
        let (v, t) = read_stl_u32(path);
        let config = TamponadeConfig {
            openings_gate: Some(true),
            ..Default::default()
        };
        let (_ov, _ot, info) = tamponade_repair(&v, &t, &config);
        assert_eq!(info.plugs, 1, "Cracked sphere must be sealed");
    }

    #[test]
    fn test_a6_case5_box_missing_face_seals() {
        let path = "/tmp/v074/agy/a6-fixtures/case5_box_missing_face.stl";
        let (v, t) = read_stl_u32(path);
        let config = TamponadeConfig {
            openings_gate: Some(true),
            ..Default::default()
        };
        let (_ov, _ot, info) = tamponade_repair(&v, &t, &config);
        assert_eq!(info.plugs, 1, "Box missing face must be sealed (not treated as intentional opening)");
    }

    #[test]
    fn test_a6_case6_thin_cup_stays_open() {
        let path = "/tmp/v074/agy/a6-fixtures/case6_thin_cup.stl";
        let (v, t) = read_stl_u32(path);
        let config = TamponadeConfig {
            openings_gate: Some(true),
            ..Default::default()
        };
        let (_ov, _ot, info) = tamponade_repair(&v, &t, &config);
        assert_eq!(info.plugs, 0, "Thin cup mouth must stay open with gate ON");
        assert_eq!(info.plugs_rejected, 1, "Mouth loop must be rejected");

        // Baseline verification: with gate OFF, thin cup mouth gets sealed as before
        let config_off = TamponadeConfig {
            openings_gate: Some(false),
            ..Default::default()
        };
        let (_ov2, _ot2, info2) = tamponade_repair(&v, &t, &config_off);
        assert_eq!(info2.plugs, 1, "Thin cup mouth must seal with gate OFF (default unchanged)");
    }

    // ---- Stent plug candidate -------------------------------------------

    #[test]
    fn test_stent_flat_hole_seals() {
        let (verts, tris) = unit_cube_open_top();
        let rim: Vec<u32> = vec![4, 5, 6, 7];
        let directed_set: HashSet<(u32, u32)> =
            [(4, 5), (5, 6), (6, 7), (7, 4)].into_iter().collect();
        let edge_count: HashMap<(u32, u32), i32> = HashMap::new();
        let idx: Vec<[usize; 3]> = tris
            .iter()
            .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
            .collect();
        let bvh = MeshBvh::from_arrays(&verts, &idx);
        let plug = stent_plug(
            &rim, &verts, &directed_set, &edge_count, &bvh, 0.5, 0.05,
            verts.len() as u32, [0.0, 0.0, 1.0], None,
        );
        assert!(plug.is_some(), "stent must produce a candidate for a flat square hole");
        let (cand, _added) = plug.unwrap();
        assert!(!cand.is_empty());
    }

    #[test]
    fn test_stent_crater_fixture_used() {
        let path = "/tmp/v074/go/fixtures-v2/plate_a_dent.stl";
        let (v, t) = read_stl_u32(path);
        let config = TamponadeConfig {
            h_min: Some(0.5),
            ..Default::default()
        };
        let (_ov, _ot, info) = tamponade_repair(&v, &t, &config);
        assert!(info.stent_used >= 1, "stent must seal the crater plate_a_dent");
        assert!(info.loop_diag.iter().any(|d| d.contains("sealed:stent")));
    }

    #[test]
    fn test_stent_conical_tip_within_tol() {
        // Shallow conical tip (depth 0.02 < tol) inside a ring rim must be sealed.
        let mut verts: Vec<[f64; 3]> = Vec::new();
        let m = 6usize;
        for i in 0..m {
            let a = (i as f64) * std::f64::consts::PI * 2.0 / (m as f64);
            verts.push([a.cos(), a.sin(), 0.0]);
        }
        let tip = verts.len() as u32;
        verts.push([0.0, 0.0, -0.02]);
        let mut tris: Vec<[u32; 3]> = Vec::new();
        for i in 0..m {
            let next = ((i + 1) % m) as u32;
            tris.push([i as u32, next, tip]);
        }
        let bottom = verts.len() as u32;
        verts.push([0.0, 0.0, -0.1]);
        for i in 0..m {
            let next = ((i + 1) % m) as u32;
            tris.push([next, i as u32, bottom]);
        }
        let rim: Vec<u32> = (0..m as u32).collect();
        let directed_set: HashSet<(u32, u32)> =
            (0..m).map(|i| (i as u32, ((i + 1) % m) as u32)).collect();
        let edge_count: HashMap<(u32, u32), i32> = HashMap::new();
        let idx: Vec<[usize; 3]> = tris
            .iter()
            .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
            .collect();
        let bvh = MeshBvh::from_arrays(&verts, &idx);
        let plug = stent_plug(
            &rim, &verts, &directed_set, &edge_count, &bvh, 0.2, 0.05,
            verts.len() as u32, [0.0, 0.0, 1.0], None,
        );
        assert!(plug.is_some(), "a shallow conical tip within tol must be sealed by the stent");
    }

    #[test]
    fn test_stent_kept_open_cup_stays_open() {
        // An intentional opening must never be revisited by the stent.
        let path = "/tmp/v074/agy/a6-fixtures/case1_open_cup.stl";
        let (v, t) = read_stl_u32(path);
        let config = TamponadeConfig {
            openings_gate: Some(true),
            ..Default::default()
        };
        let (_ov, _ot, info) = tamponade_repair(&v, &t, &config);
        assert_eq!(info.plugs, 0, "stent must not seal an intentional opening");
        assert_eq!(info.stent_used, 0, "no stent may be adopted on a kept-open cup");
    }

    #[test]
    fn test_stent_metrics_json_reports_stent() {
        // The per-plug metrics report is a valid JSON array and tags an adopted
        // stent with method "stent" (the standard numeric plug report).
        let path = "/tmp/v074/go/fixtures-v2/plate_a_dent.stl";
        let (v, t) = read_stl_u32(path);
        let config = TamponadeConfig {
            h_min: Some(0.5),
            ..Default::default()
        };
        let (_ov, _ot, info) = tamponade_repair(&v, &t, &config);
        let mj = &info.plug_metrics_json;
        assert!(
            mj.starts_with('[') && mj.ends_with(']'),
            "metrics must be a JSON array: {mj}"
        );
        assert!(
            mj.contains("\"method\":\"stent\""),
            "an adopted stent must appear in the metrics report: {mj}"
        );
        assert!(mj.contains("\"tol\""), "metrics must carry the tolerance");
    }
}
