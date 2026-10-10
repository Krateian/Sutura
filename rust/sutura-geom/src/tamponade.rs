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
use crate::winding::MeshBvh;
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
    /// Holes sealed using the thin-plate RBF fallback reference (subset of
    /// `reference_used`).
    pub reference_rbf: usize,
    /// Holes sealed by a skirt plug (cap on the reference + zipper strip to the
    /// projected rim; subset of `reference_used`).
    pub reference_skirt: usize,
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
    // cycle without ever reaching a boundary half-edge (e.g. a face carrying the
    // same edge twice), which would loop forever. Abandon such a start.
    let limit = tris.len().saturating_mul(8) + 64;
    let mut steps = 0usize;
    for h in starts {
        if used.contains(&h) {
            continue;
        }
        let mut loop_v: Vec<u32> = Vec::new();
        let mut cur = h;
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
            loop {
                steps += 1;
                if steps > limit {
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
struct ReferenceSurface {
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
    let mut add_tri = |t: &[u32; 3], edges: &mut HashMap<([u32; 3], [u32; 3]), i32>| {
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

    for (li, loop_v) in loops.iter().enumerate() {
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
        // (greedy) path the group is the loop itself, so this is `rim`.
        let group_rim: Vec<u32> = if manifold_mode {
            loops[loop_outer[li]].clone()
        } else {
            rim.clone()
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
        let mut reference = if manifold_mode {
            loop_parent
                .get(li)
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
                let cand_counts = edge_use_counts(&cand);
                let collides = cand_counts
                    .iter()
                    .any(|(e, &d)| edge_count.get(e).copied().unwrap_or(0) + d > 2);
                if collides {
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
                        if collides {
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
                            out_tris.extend(cand);
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
        if !sealed {
            if let Some(snap) = trim_snapshot {
                out_verts = snap;
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
}
