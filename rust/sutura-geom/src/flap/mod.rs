//! Flap: surface-based boundary-loop hole filling (Rust port of the validated
//! Python oracle `flap_fill_python` in `sutura/sutura_engine/flap.py`).
//!
//! Every output triangle is either an input triangle (kept verbatim) or a flap
//! face built from a hole's own boundary vertices plus new interior points.
//! No input face is deleted or moved.  The algorithm is Liepa 2003's
//! minimum-area dynamic programme, conforming density refinement and a discrete
//! biharmonic (thin-plate) fairing with a C1 ghost boundary.

pub mod binding;
pub mod dp;
pub mod fair;
pub mod guard;
pub mod refine;
pub mod topology;

pub use binding::flap_fill_py;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// A 3-D point.
pub type V3 = [f64; 3];
/// A triangle with vertex indices.
pub type T3 = [u32; 3];

const FAIR_SMALL: f64 = 3.0;
const FAIR_LARGE: f64 = 3.0;
const FAIR_RAMP: f64 = 30.0;
const ZERO_VOL_FRAC: f64 = 1e-3;

#[inline]
pub fn ekey(a: u32, b: u32) -> (u32, u32) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Packed undirected edge key `(min << 32) | max`.
#[inline]
pub fn encoded(a: u32, b: u32) -> u64 {
    let (x, y) = ekey(a, b);
    ((x as u64) << 32) | (y as u64)
}

#[inline]
pub fn tkey(a: u32, b: u32) -> (u32, u32) {
    ekey(a, b)
}

/// Disjoint-set union over `n` elements.
pub struct Dsu {
    parent: Vec<u32>,
}

impl Dsu {
    pub fn new(n: usize) -> Self {
        Dsu {
            parent: (0..n as u32).collect(),
        }
    }
    pub fn find(&mut self, mut x: u32) -> u32 {
        while self.parent[x as usize] != x {
            self.parent[x as usize] = self.parent[self.parent[x as usize] as usize];
            x = self.parent[x as usize];
        }
        x
    }
    pub fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra as usize] = rb;
        }
    }
}

pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn norm(a: V3) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
pub fn median(mut xs: Vec<f64>) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = xs.len();
    if n % 2 == 1 {
        xs[n / 2]
    } else {
        0.5 * (xs[n / 2 - 1] + xs[n / 2])
    }
}

/// Half-edges in block-stacked order `[all (0,1), all (1,2), all (2,0)]`.
pub(crate) fn face_half_edges(tris: &[T3]) -> Vec<(u32, u32, u32)> {
    let f = tris.len();
    let mut out = Vec::with_capacity(f * 3);
    for local in 0..3u32 {
        for (fi, t) in tris.iter().enumerate() {
            out.push((fi as u32, t[local as usize], t[((local + 1) % 3) as usize]));
        }
    }
    out
}

/// `edge -> incident faces` for a triangle array.
pub fn edge_faces(tris: &[T3]) -> HashMap<(u32, u32), Vec<u32>> {
    let mut m: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for (fi, t) in tris.iter().enumerate() {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            m.entry(tkey(a, b)).or_default().push(fi as u32);
        }
    }
    m
}

fn fair_fraction(m: usize) -> f64 {
    FAIR_LARGE + (FAIR_SMALL - FAIR_LARGE) * (-((m as f64) - 3.0) / FAIR_RAMP).exp()
}

fn vertex_adjacency(t: &[T3], nv: usize) -> Vec<Vec<u32>> {
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); nv];
    for f in t {
        adj[f[0] as usize].push(f[1]);
        adj[f[1] as usize].push(f[2]);
        adj[f[2] as usize].push(f[0]);
    }
    adj
}

fn vertex_normals(v: &[V3], t: &[T3]) -> Vec<V3> {
    let mut vn = vec![[0.0f64; 3]; v.len()];
    for f in t {
        let (a, b, c) = (v[f[0] as usize], v[f[1] as usize], v[f[2] as usize]);
        let fnv = cross(sub(b, a), sub(c, a));
        for &idx in f {
            vn[idx as usize][0] += fnv[0];
            vn[idx as usize][1] += fnv[1];
            vn[idx as usize][2] += fnv[2];
        }
    }
    for x in vn.iter_mut() {
        let n = norm(*x);
        let n = if n == 0.0 { 1.0 } else { n };
        x[0] /= n;
        x[1] /= n;
        x[2] /= n;
    }
    vn
}

fn percentile(mut v: Vec<f64>, q: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    let pos = (q / 100.0) * (n as f64 - 1.0);
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        v[lo]
    } else {
        v[lo] + (v[hi] - v[lo]) * (pos - lo as f64)
    }
}

/// Per-loop safety: reject a candidate patch that (a) makes any of its own
/// faces properly self-intersect or (b) leaves a RIM edge that is not shared by
/// exactly two faces after an exact float32 reload-weld (a split rim edge /
/// T-junction, a doubled seam, a crack).  Mirror of the Python `patch_safety`.
///
/// The self-intersection scan is LOCAL: only the original faces whose bounding
/// box overlaps the patch's (plus the patch against itself) are tested, so it
/// runs on a large mesh too (ornate-frame ~400k faces) instead of being skipped.
fn patch_safety(
    base_v: &[V3],
    base_t: &[T3],
    add_v: &[V3],
    patch: &[T3],
    rim_edges: &[(u32, u32)],
) -> (bool, &'static str) {
    if patch.is_empty() {
        return (true, "");
    }
    let mut cand_v: Vec<V3> = Vec::with_capacity(base_v.len() + add_v.len());
    cand_v.extend_from_slice(base_v);
    cand_v.extend_from_slice(add_v);

    // Patch bounding box + margin.
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for t in patch {
        for &v in t {
            let p = cand_v[v as usize];
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
    let diag = norm([hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]]);
    let margin = diag.max(1e-9) * 0.5;
    let inb = |p: V3| (0..3).all(|k| p[k] >= lo[k] - margin && p[k] <= hi[k] + margin);

    // Local mesh: near original faces + the patch faces, remapped compactly.
    let mut lmap: HashMap<u32, u32> = HashMap::new();
    let mut lv: Vec<V3> = Vec::new();
    let mut lt: Vec<[usize; 3]> = Vec::new();
    let mut remap = |vi: u32, lmap: &mut HashMap<u32, u32>, lv: &mut Vec<V3>| -> usize {
        if let Some(&x) = lmap.get(&vi) {
            return x as usize;
        }
        lv.push(cand_v[vi as usize]);
        let x = (lv.len() - 1) as u32;
        lmap.insert(vi, x);
        x as usize
    };
    for t in base_t {
        if inb(cand_v[t[0] as usize]) || inb(cand_v[t[1] as usize]) || inb(cand_v[t[2] as usize])
        {
            let a = remap(t[0], &mut lmap, &mut lv);
            let b = remap(t[1], &mut lmap, &mut lv);
            let c = remap(t[2], &mut lmap, &mut lv);
            lt.push([a, b, c]);
        }
    }
    let nbase = lt.len();
    for t in patch {
        let a = remap(t[0], &mut lmap, &mut lv);
        let b = remap(t[1], &mut lmap, &mut lv);
        let c = remap(t[2], &mut lmap, &mut lv);
        lt.push([a, b, c]);
    }

    // (a) exact self-intersection involving a patch face, on the local mesh.
    {
        let mut mask = vec![false; lt.len()];
        for m in mask.iter_mut().skip(nbase) {
            *m = true;
        }
        if crate::self_intersections_subset_core(&lv, &lt, &mask) > 0 {
            return (false, "si");
        }
    }

    // (b) reload-weld seam: every rim edge must be shared by exactly two faces
    // after an exact float32 weld (the original face + the patch face).
    let mut remap32: HashMap<[u32; 3], u32> = HashMap::with_capacity(lv.len());
    let mut rmap = vec![0u32; lv.len()];
    let mut coincident = false;
    for (i, p) in lv.iter().enumerate() {
        let key = [
            (p[0] as f32).to_bits(),
            (p[1] as f32).to_bits(),
            (p[2] as f32).to_bits(),
        ];
        let e = remap32.entry(key).or_insert(i as u32);
        rmap[i] = *e;
        if (*e as usize) != i {
            coincident = true;
        }
    }
    // Distinct patch vertices that coincide in float32 would be nudged apart by
    // `separate_coincident_stl` after the loop, which can itself introduce a
    // self-intersection (measured: the unfaired plate_a_dent patch reloads
    // clean in isolation but the pipeline reaches SI 8).  Reject them here.
    if coincident {
        return (false, "coincident");
    }
    let mut counts: HashMap<(u32, u32), u32> = HashMap::with_capacity(lt.len() * 3);
    for t in &lt {
        let (a, b, c) = (rmap[t[0]], rmap[t[1]], rmap[t[2]]);
        for (u, w) in [(a, b), (b, c), (c, a)] {
            *counts.entry(ekey(u, w)).or_insert(0) += 1;
        }
    }
    for &(ra, rb) in rim_edges {
        let la = *lmap.get(&ra).unwrap_or(&u32::MAX);
        let lb = *lmap.get(&rb).unwrap_or(&u32::MAX);
        if la == u32::MAX || lb == u32::MAX {
            return (false, "reload_nm");
        }
        let (u, w) = (rmap[la as usize], rmap[lb as usize]);
        if counts.get(&ekey(u, w)).copied().unwrap_or(0) != 2 {
            return (false, "reload_nm");
        }
    }
    (true, "")
}

/// Wrap a 2-D point for the `robust` exact predicates.
#[inline]
fn coord2(p: [f64; 2]) -> robust::Coord<f64> {
    robust::Coord { x: p[0], y: p[1] }
}

/// Signed area (x2) of a 2-D polygon.
fn poly_area2(poly: &[[f64; 2]]) -> f64 {
    let n = poly.len();
    let mut a = 0.0;
    for i in 0..n {
        let j = (i + 1) % n;
        a += poly[i][0] * poly[j][1] - poly[j][0] * poly[i][1];
    }
    a
}

/// True when point `p` lies on segment `a`-`b` (assumed collinear).
fn on_seg(a: [f64; 2], b: [f64; 2], p: [f64; 2]) -> bool {
    p[0] >= a[0].min(b[0]) - 1e-12
        && p[0] <= a[0].max(b[0]) + 1e-12
        && p[1] >= a[1].min(b[1]) - 1e-12
        && p[1] <= a[1].max(b[1]) + 1e-12
}

/// Any shared point of segments `a`-`b` and `c`-`d` (exact `orient2d`).
fn seg_intersect(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> bool {
    let o1 = robust::orient2d(coord2(a), coord2(b), coord2(c));
    let o2 = robust::orient2d(coord2(a), coord2(b), coord2(d));
    let o3 = robust::orient2d(coord2(c), coord2(d), coord2(a));
    let o4 = robust::orient2d(coord2(c), coord2(d), coord2(b));
    if ((o1 > 0.0) != (o2 > 0.0)) && o1 != 0.0 && o2 != 0.0 && ((o3 > 0.0) != (o4 > 0.0)) && o3 != 0.0
        && o4 != 0.0
    {
        return true;
    }
    if o1 == 0.0 && on_seg(a, b, c) {
        return true;
    }
    if o2 == 0.0 && on_seg(a, b, d) {
        return true;
    }
    if o3 == 0.0 && on_seg(c, d, a) {
        return true;
    }
    if o4 == 0.0 && on_seg(c, d, b) {
        return true;
    }
    false
}

/// True when the closed polygon has no self-crossing (non-adjacent edges must
/// not share any point).
fn polygon_simple(poly: &[[f64; 2]]) -> bool {
    let n = poly.len();
    if n < 3 {
        return false;
    }
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        for j in (i + 1)..n {
            let c = poly[j];
            let d = poly[(j + 1) % n];
            // adjacent edges share a vertex -- allowed
            if j == i + 1 || (i == 0 && j == n - 1) {
                continue;
            }
            if seg_intersect(a, b, c, d) {
                return false;
            }
        }
    }
    true
}

/// Point-in-triangle (allowing boundary) via exact `orient2d`.
fn point_in_tri(p: [f64; 2], a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> bool {
    let d1 = robust::orient2d(coord2(a), coord2(b), coord2(p));
    let d2 = robust::orient2d(coord2(b), coord2(c), coord2(p));
    let d3 = robust::orient2d(coord2(c), coord2(a), coord2(p));
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

/// Ear-clip a simple polygon (CCW forced), skipping ears whose new diagonal is
/// in `forbidden` (a chord that already has a face).  Returns `None` on failure.
fn ear_clip(poly: &[[f64; 2]], forbidden: &HashSet<(u32, u32)>) -> Option<Vec<[u32; 3]>> {
    let n = poly.len();
    if n < 3 {
        return None;
    }
    let mut idx: Vec<usize> = (0..n).collect();
    if poly_area2(poly) < 0.0 {
        idx.reverse();
    }
    let mut tris: Vec<[u32; 3]> = Vec::with_capacity(n.saturating_sub(2));
    let mut guard = 0usize;
    while idx.len() > 3 {
        guard += 1;
        if guard > n * n + 4 {
            return None;
        }
        let k = idx.len();
        let mut clipped = false;
        for i in 0..k {
            let ia = idx[(i + k - 1) % k];
            let ib = idx[i];
            let ic = idx[(i + 1) % k];
            if forbidden.contains(&ekey(ia as u32, ic as u32)) {
                continue;
            }
            let (a, b, c) = (poly[ia], poly[ib], poly[ic]);
            if robust::orient2d(coord2(a), coord2(b), coord2(c)) <= 0.0 {
                continue;
            }
            let mut empty = true;
            for &j in &idx {
                if j == ia || j == ib || j == ic {
                    continue;
                }
                if point_in_tri(poly[j], a, b, c) {
                    empty = false;
                    break;
                }
            }
            if empty {
                tris.push([ia as u32, ib as u32, ic as u32]);
                idx.remove(i);
                clipped = true;
                break;
            }
        }
        if !clipped {
            return None;
        }
    }
    tris.push([idx[0] as u32, idx[1] as u32, idx[2] as u32]);
    Some(tris)
}

/// A frame `(origin, u, v)` from the rim's Newell normal (fallback plane).
fn newell_frame(rim: &[V3]) -> Option<([f64; 3], [f64; 3], [f64; 3])> {
    let m = rim.len();
    if m < 3 {
        return None;
    }
    let mut n = [0.0f64; 3];
    for i in 0..m {
        let a = rim[i];
        let b = rim[(i + 1) % m];
        n[0] += (a[1] - b[1]) * (a[2] + b[2]);
        n[1] += (a[2] - b[2]) * (a[0] + b[0]);
        n[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    if norm(n) < 1e-20 {
        return None;
    }
    let n = [n[0] / norm(n), n[1] / norm(n), n[2] / norm(n)];
    let refv = if n[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
    let mut u = cross(refv, n);
    let ul = norm(u);
    if ul < 1e-20 {
        return None;
    }
    u = [u[0] / ul, u[1] / ul, u[2] / ul];
    let v = cross(n, u);
    let mut o = [0.0f64; 3];
    for p in rim {
        o[0] += p[0] / m as f64;
        o[1] += p[1] / m as f64;
        o[2] += p[2] / m as f64;
    }
    Some((o, u, v))
}

/// SI-free projected patch: project the rim into a reference frame (Tamponade's
/// fitted ReferenceSurface, else the rim's Newell plane), require the projected
/// polygon to be simple, ear-clip it in 2-D, and return the patch using the
/// ORIGINAL rim positions.  A piecewise-linear graph over the projection plane
/// cannot self-intersect, so the result is SI-free by construction.  `None`
/// when the projected polygon is not simple (the caller gives up).
fn projected_patch(
    v2: &[V3],
    t2: &[T3],
    loop_arr: &[u32],
    target: f64,
    adj: Option<&crate::tamponade::Adjacency>,
    forbidden: &HashSet<(u32, u32)>,
) -> Option<(Vec<V3>, Vec<[u32; 3]>)> {
    let m = loop_arr.len();
    if m < 3 {
        return None;
    }
    let rim: Vec<V3> = loop_arr.iter().map(|&i| v2[i as usize]).collect();
    let frame = match adj.and_then(|a| crate::tamponade::build_reference(v2, t2, loop_arr, target, a)) {
        Some(r) => {
            let (o, u, v, _n) = r.frame();
            (o, u, v)
        }
        None => newell_frame(&rim)?,
    };
    let (o, u_ax, v_ax) = frame;
    let poly: Vec<[f64; 2]> = rim
        .iter()
        .map(|p| {
            let d = [p[0] - o[0], p[1] - o[1], p[2] - o[2]];
            [dot(d, u_ax), dot(d, v_ax)]
        })
        .collect();
    if !polygon_simple(&poly) {
        return None;
    }
    let tris = ear_clip(&poly, forbidden)?;
    Some((rim, tris))
}

#[derive(Clone)]
pub struct FlapParams {
    pub refine: bool,
    pub fair: bool,
    pub max_loop: usize,
    pub max_faces: usize,
    pub collect_quality: bool,
    pub drop_lone_tris: bool,
    pub weld_cracks: bool,
    pub split_nm: bool,
    pub separate_stl: bool,
    pub bridge_open_chains: bool,
    pub weld_max_frac: f64,
    pub orient: String,
    pub avoid_coplanar: bool,
    pub coplanar_frac: f64,
    pub coplanar_cos: f64,
    pub sliver_cleanup: bool,
    /// Wall-clock budget for the whole call, in seconds. When set (and > 0),
    /// the per-loop candidate selector stops trying alternatives once the
    /// deadline passes and leaves the remaining loops to Stage-1, so an
    /// expensive selection cannot push the run past its preset budget.
    /// `None` = unlimited.
    pub time_budget: Option<f64>,
}

impl Default for FlapParams {
    fn default() -> Self {
        FlapParams {
            refine: true,
            fair: true,
            max_loop: 1200,
            max_faces: 400000,
            collect_quality: false,
            drop_lone_tris: true,
            weld_cracks: true,
            split_nm: true,
            separate_stl: true,
            bridge_open_chains: false,
            weld_max_frac: 0.02,
            orient: "reverse".to_string(),
            avoid_coplanar: true,
            coplanar_frac: 0.05,
            coplanar_cos: 0.99,
            sliver_cleanup: false,
            time_budget: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct LoopLog {
    pub m: usize,
    pub fan: bool,
    pub large: bool,
    pub action: Option<String>,
}

#[derive(Clone, Default)]
pub struct NmCollision {
    pub m: usize,
    pub fan: bool,
    pub edges: Vec<(u32, u32)>,
}

#[derive(Clone, Default)]
pub struct QualityAgg {
    pub n: usize,
    pub min_angle_p5: f64,
    pub min_angle_p50: f64,
    pub aspect_p50: f64,
    pub aspect_p95: f64,
}

#[derive(Clone, Default)]
pub struct Report {
    pub loops_found: usize,
    pub loops_filled: usize,
    pub loops_skipped: usize,
    pub loops_skipped_nm: usize,
    pub loops_skipped_sheet: usize,
    pub loops_fan_fallback: usize,
    pub loops_seeded: usize,
    pub loops_refined: usize,
    pub patch_faces: usize,
    pub new_vertices: usize,
    pub large_loops_fan: usize,
    pub refine: bool,
    pub fair: bool,
    pub split_splits: usize,
    pub split_nm_edges_in: usize,
    pub loops_skipped_coplanar: usize,
    pub loops_skipped_flat: usize,
    pub loops_skipped_sliver: usize,
    pub loops_skipped_safety: usize,
    pub safety_rejections: Vec<(usize, String)>,
    pub non_simple_reported: usize,
    pub orient_reversed: usize,
    pub welded_cracks: usize,
    pub dropped_lone_triangles: Option<usize>,
    pub chains_bridged: Option<usize>,
    pub separated_stl: Option<usize>,
    pub boundary_normalize: Option<topology::NormInfo>,
    pub nm_collisions: Vec<NmCollision>,
    pub loop_log: Vec<LoopLog>,
    pub errors: Vec<String>,
    pub quality_before: Option<QualityAgg>,
    pub quality_after: Option<QualityAgg>,
}

/// Run Flap on `verts`/`tris`, returning `(verts_out, tris_out, report)`.
pub fn flap_fill(verts: &[V3], tris: &[T3], prm: &FlapParams) -> (Vec<V3>, Vec<T3>, Report) {
    let mut report = Report {
        refine: prm.refine,
        fair: prm.fair,
        ..Default::default()
    };

    // Wall-clock budget for the whole call (see ``FlapParams::time_budget``).
    // ``None`` (the default) leaves selection unbounded; the Python caller
    // passes its preset budget so the per-loop candidate selector can bail out
    // of the alternative search instead of overshooting.  ``alt_reserve`` is
    // the time held back for the largest *single* alternative still to start:
    // it grows to the largest alternative duration seen so far (seeded at 25 %
    // of the budget) and gates the start of every alternative, so the whole
    // call stays within budget while never preempting an alternative mid-solve.
    let budget_dur = prm
        .time_budget
        .filter(|b| *b > 0.0)
        .map(Duration::from_secs_f64);
    let deadline = budget_dur.map(|b| Instant::now() + b);
    let mut alt_reserve = budget_dur.map(|b| b.mul_f64(0.25)).unwrap_or(Duration::ZERO);

    // ---- boundary normalisation / non-manifold split --------------------
    let (mut v2, mut t2): (Vec<V3>, Vec<T3>);
    if prm.weld_cracks {
        let (a, b, info) = topology::normalize_boundary(
            verts,
            tris,
            8,
            Some(prm.weld_max_frac),
            Some(0.1),
            Some(verts.len()),
        );
        v2 = a;
        t2 = b;
        report.welded_cracks = info.welds;
        report.split_splits = info.splits;
        report.split_nm_edges_in = 0;
        report.boundary_normalize = Some(info);
    } else if prm.split_nm {
        let (a, b, si) = topology::split_non_manifold(verts, tris);
        v2 = a;
        t2 = b;
        report.split_splits = si.splits;
        report.split_nm_edges_in = si.nm_edges_in;
    } else {
        v2 = verts.to_vec();
        t2 = tris.to_vec();
    }
    if prm.drop_lone_tris {
        let (a, b, d) = topology::drop_lone_triangles(&v2, &t2);
        v2 = a;
        t2 = b;
        report.dropped_lone_triangles = Some(d);
    }

    // median edge length of the normalised mesh
    let med_edge = {
        let mut lens: Vec<f64> = Vec::with_capacity(t2.len() * 3);
        for f in &t2 {
            lens.push(norm(sub(v2[f[0] as usize], v2[f[1] as usize])));
            lens.push(norm(sub(v2[f[1] as usize], v2[f[2] as usize])));
            lens.push(norm(sub(v2[f[2] as usize], v2[f[0] as usize])));
        }
        if lens.is_empty() {
            1.0
        } else {
            median(lens)
        }
    };

    let mut bridges: Vec<(u32, u32)> = Vec::new();
    if prm.bridge_open_chains {
        bridges = topology::close_open_chains(&v2, &t2, 10000, 8);
        report.chains_bridged = Some(bridges.len());
    }
    let (loops, skipped) = topology::boundary_loops(&v2, &t2, &bridges);
    report.loops_found = loops.len() + skipped;
    report.loops_skipped = skipped;
    report.non_simple_reported = skipped;

    let mut out_v: Vec<V3> = v2.clone();
    let mut out_t: Vec<T3> = t2.clone();

    // ---- edge maps (block-ordered, stable) ------------------------------
    let he = face_half_edges(&t2);
    let mut order: Vec<u32> = (0..he.len() as u32).collect();
    order.sort_by_key(|&i| encoded(he[i as usize].1, he[i as usize].2));
    let mut gcount: HashMap<(u32, u32), u32> = HashMap::new();
    let mut orig_first: HashMap<(u32, u32), u32> = HashMap::new();
    let mut run_start: HashMap<(u32, u32), (u32, u32)> = HashMap::new();
    let mut face_run: Vec<u32> = Vec::with_capacity(he.len());
    {
        let mut i = 0usize;
        while i < order.len() {
            let k = encoded(he[order[i] as usize].1, he[order[i] as usize].2);
            let mut j = i + 1;
            while j < order.len()
                && encoded(he[order[j] as usize].1, he[order[j] as usize].2) == k
            {
                j += 1;
            }
            let h0 = order[i] as usize;
            let key = tkey(he[h0].1, he[h0].2);
            let count = (j - i) as u32;
            let start = face_run.len() as u32;
            for &h in &order[i..j] {
                face_run.push(he[h as usize].0);
            }
            gcount.insert(key, count);
            orig_first.insert(key, face_run[start as usize]);
            run_start.insert(key, (start, count));
            i = j;
        }
    }
    let _ = &orig_first;
    let orig_edge_face = |k: (u32, u32)| -> Option<u32> {
        run_start.get(&k).map(|&(s, _)| face_run[s as usize])
    };
    let edge_faces_of = |k: (u32, u32)| -> Option<&[u32]> {
        run_start
            .get(&k)
            .map(|&(s, c)| &face_run[s as usize..(s + c) as usize])
    };
    let _ = &edge_faces_of;

    let stats = guard::component_stats(&v2, &t2, 2.0);
    let mut nloops_c = vec![0i64; stats.vol_c.len()];
    for lp in &loops {
        let k = tkey(lp[0], lp[1]);
        if let Some(fi) = orig_edge_face(k) {
            nloops_c[stats.lab[fi as usize] as usize] += 1;
        }
    }

    let adj = vertex_adjacency(&t2, v2.len());
    let vnorm = vertex_normals(&v2, &t2);

    let mut extra_ref: Vec<guard::PatchRef> = Vec::new();
    let mut q_before: Vec<(f64, f64)> = Vec::new();
    let mut q_after: Vec<(f64, f64)> = Vec::new();

    // Bounded (membrane) fairing + per-loop safety check, opt-in; default OFF
    // keeps the thin-plate path byte-identical.
    let membrane = std::env::var("SUTURA_FLAP_MEMBRANE")
        .map(|v| v != "0" && !v.is_empty())
        .unwrap_or(false);
    // Wider reference-surface data term (Tamponade's ReferenceSurface, fitted on
    // the intact ring beyond the damage), opt-in; default OFF.
    let ref_on = std::env::var("SUTURA_FLAP_REF")
        .map(|v| v != "0" && !v.is_empty())
        .unwrap_or(false);
    // Per-loop candidate selection: try thin-plate, then membrane, REF and the
    // unfaired refined patch, committing the first that passes the guards.  The
    // primary (thin-plate) candidate is committed exactly like the historical
    // default, so any loop the thin-plate path already filled is unchanged; the
    // alternatives only add baseline-relative safe fills that the default left
    // to Stage-1.  Default ON; `SUTURA_FLAP_SELECT=0` opts out.
    let select = std::env::var("SUTURA_FLAP_SELECT")
        .map(|v| v != "0" && !v.is_empty())
        .unwrap_or(true);
    let adj_ref = if ref_on {
        Some(crate::tamponade::build_adjacency(v2.len(), &t2))
    } else {
        None
    };

    for loop_v in &loops {
        let loop_arr = loop_v;
        let m = loop_arr.len();
        if m > prm.max_loop {
            report.large_loops_fan += 1;
        }
        let pts: Vec<V3> = loop_arr.iter().map(|&i| v2[i as usize]).collect();

        // forbidden local chords: an edge that already has >= 2 faces
        let mut forb: HashSet<(u32, u32)> = HashSet::new();
        for a in 0..m {
            let ga = loop_arr[a];
            for b in (a + 1)..m {
                let gb = loop_arr[b];
                let k = tkey(ga, gb);
                if gcount.get(&k).copied().unwrap_or(0) >= 2 {
                    forb.insert((a as u32, b as u32));
                }
            }
        }

        let tri_local = dp::min_area_triangulation(&pts, 1200, &forb);
        let mut is_fan = false;
        let mut p_pts: Vec<V3>;
        let mut faces: Vec<[u32; 3]>;
        if !tri_local.is_empty() {
            p_pts = pts.clone();
            faces = tri_local;
        } else {
            let cm = [
                pts.iter().map(|q| q[0]).sum::<f64>() / m as f64,
                pts.iter().map(|q| q[1]).sum::<f64>() / m as f64,
                pts.iter().map(|q| q[2]).sum::<f64>() / m as f64,
            ];
            p_pts = pts.clone();
            p_pts.push(cm);
            faces = (0..m).map(|i| [i as u32, ((i + 1) % m) as u32, m as u32]).collect();
            is_fan = true;
            report.loops_fan_fallback += 1;
        }

        // Liepa target = median rim edge length
        let mut rim_len: Vec<f64> = Vec::with_capacity(m);
        for i in 0..m {
            rim_len.push(norm(sub(pts[(i + 1) % m], pts[i])));
        }
        let target = if !rim_len.is_empty() {
            median(rim_len.clone())
        } else {
            med_edge
        };
        let large = m > prm.max_loop;
        let mut log = LoopLog {
            m,
            fan: is_fan,
            large,
            action: None,
        };

        // flat-sheet guard
        let k01 = tkey(loop_arr[0], loop_arr[1]);
        let fi_opt = orig_edge_face(k01);
        if let Some(fi) = fi_opt {
            if stats.flat_c[stats.lab[fi as usize] as usize] {
                report.loops_skipped_flat += 1;
                log.action = Some("flat".to_string());
                report.loop_log.push(log);
                continue;
            }
        }

        if prm.collect_quality && !faces.is_empty() {
            for f in &faces {
                let (a, b, c) = (p_pts[f[0] as usize], p_pts[f[1] as usize], p_pts[f[2] as usize]);
                let e0 = norm(sub(b, a));
                let e1 = norm(sub(c, b));
                let e2 = norm(sub(a, c));
                let s = 0.5 * (e0 + e1 + e2);
                let area = (s * (s - e0) * (s - e1) * (s - e2)).max(0.0).sqrt();
                let inr = if s > 0.0 { area / s.max(1e-20) } else { 1e-20 };
                let longest = e0.max(e1).max(e2);
                q_before.push((min_angle(a, b, c), longest / (2.0 * inr.max(1e-20))));
            }
        }

        // local surrounding density (2-ring, rim edges excluded)
        let rim_set: HashSet<u32> = loop_arr.iter().copied().collect();
        let mut ring1: HashSet<u32> = HashSet::new();
        for &gv in loop_arr {
            for &nb in &adj[gv as usize] {
                if !rim_set.contains(&nb) {
                    ring1.insert(nb);
                }
            }
        }
        let mut seen_e: HashSet<(u32, u32)> = HashSet::new();
        let mut surr_lens: Vec<f64> = Vec::new();
        let mut add_edges = |gv: u32, surr_lens: &mut Vec<f64>| {
            for &nb in &adj[gv as usize] {
                if rim_set.contains(&nb) || nb == gv {
                    continue;
                }
                let k = tkey(gv, nb);
                if seen_e.contains(&k) {
                    continue;
                }
                seen_e.insert(k);
                surr_lens.push(norm(sub(v2[gv as usize], v2[nb as usize])));
            }
        };
        for &gv in loop_arr {
            add_edges(gv, &mut surr_lens);
        }
        for &gv in &ring1 {
            add_edges(gv, &mut surr_lens);
        }
        let local_surr = if !surr_lens.is_empty() {
            median(surr_lens)
        } else {
            med_edge
        };

        let n_before_refine = p_pts.len();
        if prm.refine || prm.fair {
            refine::refine_patch(
                &mut p_pts,
                &mut faces,
                m,
                target,
                prm.max_faces,
                6,
                true,
                &forb,
                Some(med_edge),
                Some(local_surr),
            );
        }
        let was_seeded = p_pts.len() > n_before_refine;

        // orientation
        if prm.orient == "reverse" {
            for f in faces.iter_mut() {
                f.swap(1, 2);
            }
            report.orient_reversed += 1;
        } else if m >= 2 {
            if let Some(ofi) = orig_edge_face(k01) {
                let fo = t2[ofi as usize];
                let n_orig = cross(
                    sub(v2[fo[1] as usize], v2[fo[0] as usize]),
                    sub(v2[fo[2] as usize], v2[fo[0] as usize]),
                );
                for f in faces.iter_mut() {
                    let set: HashSet<u32> = f.iter().copied().collect();
                    if set.contains(&0) && set.contains(&1) {
                        let n_flap = cross(
                            sub(p_pts[f[1] as usize], p_pts[f[0] as usize]),
                            sub(p_pts[f[2] as usize], p_pts[f[0] as usize]),
                        );
                        if dot(n_orig, n_flap) < 0.0 {
                            for g in faces.iter_mut() {
                                g.swap(1, 2);
                            }
                        }
                        break;
                    }
                }
            }
        }

        // Snapshot the refined, pre-fair patch. The thin-plate fairing overshoots
        // on a curved rim and can fold the patch over itself; the coplanar guard
        // below then falls back to this unfaired patch (still refine-only Flap)
        // instead of discarding the whole loop.
        let mut p_prefair: Option<Vec<V3>> = None;
        let mut ghost: Vec<V3> = Vec::new();
        if prm.fair && p_pts.len() > m {
            p_prefair = Some(p_pts.clone());
            let centroid = [
                pts.iter().map(|q| q[0]).sum::<f64>() / m as f64,
                pts.iter().map(|q| q[1]).sum::<f64>() / m as f64,
                pts.iter().map(|q| q[2]).sum::<f64>() / m as f64,
            ];
            ghost = vec![[0.0f64; 3]; m];
            let ell = target.max(1e-12) * fair_fraction(m);
            for i in 0..m {
                let gv = loop_arr[i];
                let pv = v2[gv as usize];
                let nrm = vnorm[gv as usize];
                let mut d = sub(centroid, pv);
                let dn = norm(d);
                if dn < 1e-12 {
                    ghost[i] = pv;
                    continue;
                }
                d = [d[0] / dn, d[1] / dn, d[2] / dn];
                let tng = sub(d, [nrm[0] * dot(d, nrm), nrm[1] * dot(d, nrm), nrm[2] * dot(d, nrm)]);
                let tn = norm(tng);
                ghost[i] = if tn > 1e-9 {
                    [pv[0] - ell * tng[0] / tn, pv[1] - ell * tng[1] / tn, pv[2] - ell * tng[2] / tn]
                } else {
                    pv
                };
            }
            // Membrane (order 1) fairing is bounded by the rim, so a curved rim
            // no longer makes the dense interior overshoot and fold the patch
            // over itself (the thin-plate order-2 solve, the default, can).
            // Gated by SUTURA_FLAP_MEMBRANE so the default path is byte-identical.
            if ref_on {
                let built = adj_ref.as_ref().and_then(|adj| {
                    crate::tamponade::build_reference(&v2, &t2, loop_arr, target, adj)
                });
                match built {
                    Some(refs) => {
                        for _ in 0..3 {
                            let data: Vec<V3> =
                                p_pts[m..].iter().map(|&q| refs.place(q)).collect();
                            fair::fair_patch_energy(
                                &mut p_pts, &faces, m, &ghost, 1, Some(&data), 2.0,
                            );
                        }
                    }
                    None => {
                        fair::fair_patch_energy(&mut p_pts, &faces, m, &ghost, 1, None, 0.0);
                    }
                }
            } else if membrane {
                fair::fair_patch_energy(&mut p_pts, &faces, m, &ghost, 1, None, 0.0);
            } else {
                fair::fair_patch(&mut p_pts, &faces, m, &ghost);
            }
        }

        if prm.sliver_cleanup && !faces.is_empty() {
            let (np, nf) = refine::cleanup_slivers(&p_pts, &faces, m, 15.0, 500);
            p_pts = np;
            faces = nf;
        }
        if faces.is_empty() {
            report.loops_skipped_sliver += 1;
            log.action = Some("sliver".to_string());
            report.loop_log.push(log);
            continue;
        }

        // zero-volume-shell guard
        if let Some(fi) = fi_opt {
            let comp = stats.lab[fi as usize] as usize;
            if nloops_c[comp] == 1 {
                let mut vp = 0.0f64;
                let mut ap = 0.0f64;
                for f in &faces {
                    let (a, b, c) = (p_pts[f[0] as usize], p_pts[f[1] as usize], p_pts[f[2] as usize]);
                    vp += dot(a, cross(b, c)) / 6.0;
                    ap += 0.5 * norm(cross(sub(b, a), sub(c, a)));
                }
                let tot = (stats.vol_c[comp] + vp).abs();
                let area = stats.area_c[comp] + ap;
                if area > 0.0 && tot <= ZERO_VOL_FRAC * area.powf(1.5) {
                    report.loops_skipped_flat += 1;
                    log.action = Some("zero_vol".to_string());
                    report.loop_log.push(log);
                    continue;
                }
            }
        }

        let base = out_v.len();
        let gmap = |i: usize| -> u32 {
            if i < m {
                loop_arr[i]
            } else {
                (base + (i - m)) as u32
            }
        };
        let mut gfaces: Vec<T3> = faces
            .iter()
            .map(|f| [gmap(f[0] as usize), gmap(f[1] as usize), gmap(f[2] as usize)])
            .collect();
        let rim_edges: Vec<(u32, u32)> = (0..m)
            .map(|i| (loop_arr[i], loop_arr[(i + 1) % m]))
            .collect();

        // Per-loop candidate selection (SUTURA_FLAP_SELECT): the primary
        // (thin-plate) patch is evaluated first and committed with zero extra
        // work when it passes, exactly like the historical default (and
        // `SUTURA_FLAP_SELECT=0`), so any loop it fills is byte-identical.  Only
        // when the primary is rejected (coplanar fold) are the alternatives --
        // membrane, REF, the unfaired refined patch and a projected SI-free
        // patch -- built, one at a time, each checked before the next; the first
        // that passes the zero-volume / coplanar / safety guards is committed.
        // A loop no candidate can fill safely, or one the wall-clock budget
        // reaches, falls to Stage-1.
        if select && p_prefair.is_some() {
            let cand_ok = |p: &[V3], fc: &[[u32; 3]], gf: &[T3], primary: bool| -> bool {
                if let Some(fi) = fi_opt {
                    let comp = stats.lab[fi as usize] as usize;
                    if nloops_c[comp] == 1 {
                        let mut vp = 0.0f64;
                        let mut ap = 0.0f64;
                        for f in fc {
                            let (a, b, c) =
                                (p[f[0] as usize], p[f[1] as usize], p[f[2] as usize]);
                            vp += dot(a, cross(b, c)) / 6.0;
                            ap += 0.5 * norm(cross(sub(b, a), sub(c, a)));
                        }
                        let tot = (stats.vol_c[comp] + vp).abs();
                        let area = stats.area_c[comp] + ap;
                        if area > 0.0 && tot <= ZERO_VOL_FRAC * area.powf(1.5) {
                            return false;
                        }
                    }
                }
                if prm.avoid_coplanar
                    && guard::coplanar_collides(
                        p,
                        fc,
                        &[],
                        &extra_ref,
                        prm.coplanar_frac,
                        prm.coplanar_cos,
                    )
                {
                    return false;
                }
                if !primary {
                    let (safe, _why) = patch_safety(&out_v, &out_t, &p[m..], gf, &rim_edges);
                    if !safe {
                        return false;
                    }
                }
                true
            };
            // An alternative may start only when the reserve (the largest one
            // yet seen) still fits before the deadline.
            let fits =
                |reserve: Duration| deadline.is_none_or(|d| Instant::now() + reserve < d);
            // Primary: no alternative is built unless this is rejected.
            let mut chosen = cand_ok(&p_pts, &faces, &gfaces, true);
            if !chosen && fits(alt_reserve) {
                let base = p_prefair.as_ref().unwrap();
                // 1) Membrane (order-1) fairing of the refined patch.
                let a0 = Instant::now();
                {
                    let mut c = base.clone();
                    fair::fair_patch_energy(&mut c, &faces, m, &ghost, 1, None, 0.0);
                    if cand_ok(&c, &faces, &gfaces, false) {
                        p_pts = c;
                        chosen = true;
                    }
                }
                alt_reserve = alt_reserve.max(Instant::now() - a0);
                // 2) Reference fairing (opt-in via SUTURA_FLAP_REF).
                if !chosen && ref_on && fits(alt_reserve) {
                    let a0 = Instant::now();
                    if let Some(adj) = adj_ref.as_ref() {
                        if let Some(refs) =
                            crate::tamponade::build_reference(&v2, &t2, loop_arr, target, adj)
                        {
                            let mut c = base.clone();
                            for _ in 0..3 {
                                let data: Vec<V3> =
                                    c[m..].iter().map(|&q| refs.place(q)).collect();
                                fair::fair_patch_energy(
                                    &mut c, &faces, m, &ghost, 1, Some(&data), 2.0,
                                );
                            }
                            if cand_ok(&c, &faces, &gfaces, false) {
                                p_pts = c;
                                chosen = true;
                            }
                        }
                    }
                    alt_reserve = alt_reserve.max(Instant::now() - a0);
                }
                // 3) Unfaired refined patch.
                if !chosen && fits(alt_reserve) {
                    let a0 = Instant::now();
                    if cand_ok(base, &faces, &gfaces, false) {
                        p_pts = base.clone();
                        chosen = true;
                    }
                    alt_reserve = alt_reserve.max(Instant::now() - a0);
                }
                // 4) Projected SI-free patch (independent ear-clipped faces).
                if !chosen && fits(alt_reserve) {
                    let a0 = Instant::now();
                    if let Some((pp, pf)) =
                        projected_patch(&v2, &t2, loop_arr, target, adj_ref.as_ref(), &forb)
                    {
                        let base2 = out_v.len();
                        let gf2: Vec<T3> = pf
                            .iter()
                            .map(|f| {
                                let g = |i: u32| -> u32 {
                                    if (i as usize) < m {
                                        loop_arr[i as usize]
                                    } else {
                                        (base2 + (i as usize - m)) as u32
                                    }
                                };
                                [g(f[0]), g(f[1]), g(f[2])]
                            })
                            .collect();
                        if cand_ok(&pp, &pf, &gf2, false) {
                            p_pts = pp;
                            faces = pf;
                            gfaces = gf2;
                            chosen = true;
                        }
                    }
                    alt_reserve = alt_reserve.max(Instant::now() - a0);
                }
            }
            if !chosen {
                report.loops_skipped_coplanar += 1;
                log.action = Some("coplanar".to_string());
                report.loop_log.push(log);
                continue;
            }
        }

        // lone single-sided triangle guard
        if m == 3 {
            let mut adjs: HashSet<u32> = HashSet::new();
            for i in 0..3 {
                let k = tkey(loop_arr[i], loop_arr[(i + 1) % 3]);
                if let Some(fs) = edge_faces_of(k) {
                    for &x in fs {
                        adjs.insert(x);
                    }
                }
            }
            if adjs.len() <= 1 {
                report.loops_skipped_sheet += 1;
                log.action = Some("sheet".to_string());
                report.loop_log.push(log);
                continue;
            }
        }

        // non-manifold commit guard
        let mut ce: HashMap<(u32, u32), u32> = HashMap::new();
        for f in &gfaces {
            for (u, w) in [(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
                *ce.entry(tkey(u, w)).or_insert(0) += 1;
            }
        }
        let mut coll: Vec<(u32, u32)> = Vec::new();
        for (&k, &c) in &ce {
            if gcount.get(&k).copied().unwrap_or(0) + c > 2 {
                coll.push(k);
            }
        }
        if !coll.is_empty() {
            report.loops_skipped_nm += 1;
            report.nm_collisions.push(NmCollision {
                m,
                fan: is_fan,
                edges: coll.iter().take(4).copied().collect(),
            });
            log.action = Some("nm".to_string());
            report.loop_log.push(log);
            continue;
        }

        if prm.avoid_coplanar
            && guard::coplanar_collides(
                &p_pts,
                &faces,
                &[],
                &extra_ref,
                prm.coplanar_frac,
                prm.coplanar_cos,
            )
        {
            // The patch overlaps. When the fairing introduced the overlap (the
            // unfaired refined patch is clean), drop the fairing and keep the
            // refined patch instead of discarding the loop; skip only when the
            // unfaired patch also overlaps (the genuinely protected case: a
            // double surface / a patch that folds without fairing).
            let fair_fallback = std::env::var("SUTURA_FLAP_FAIR_FALLBACK")
                .map(|v| v != "0" && !v.is_empty())
                .unwrap_or(false);
            let pre_clean = fair_fallback
                && p_prefair.as_ref().is_some_and(|pp| {
                    !guard::coplanar_collides(
                        pp,
                        &faces,
                        &[],
                        &extra_ref,
                        prm.coplanar_frac,
                        prm.coplanar_cos,
                    )
                });
            if pre_clean {
                p_pts = p_prefair.take().unwrap();
            } else {
                report.loops_skipped_coplanar += 1;
                log.action = Some("coplanar".to_string());
                report.loop_log.push(log);
                continue;
            }
        }

        // Per-loop safety check (membrane path): never commit a patch that
        // self-intersects or leaves a reload-non-manifold seam; drop it and
        // leave the hole for a later tier instead.
        if (membrane || ref_on) && !select {
            let (safe, why) = patch_safety(&out_v, &out_t, &p_pts[m..], &gfaces, &rim_edges);
            if !safe {
                report.loops_skipped_safety += 1;
                report.safety_rejections.push((m, why.to_string()));
                log.action = Some("safety".to_string());
                report.loop_log.push(log);
                continue;
            }
        }

        // commit
        for j in m..p_pts.len() {
            out_v.push(p_pts[j]);
        }
        out_t.extend_from_slice(&gfaces);
        if prm.avoid_coplanar {
            let (c, n, _ml) = guard::face_cent_norm(&p_pts, &faces);
            let tris: Vec<[V3; 3]> = faces
                .iter()
                .map(|f| {
                    [
                        p_pts[f[0] as usize],
                        p_pts[f[1] as usize],
                        p_pts[f[2] as usize],
                    ]
                })
                .collect();
            extra_ref.push(guard::PatchRef {
                cent: c,
                norm: n,
                tri: tris,
            });
        }
        for (&k, &c) in &ce {
            *gcount.entry(k).or_insert(0) += c;
        }
        report.loops_filled += 1;
        report.patch_faces += faces.len();
        log.action = Some("filled".to_string());
        if was_seeded {
            report.loops_seeded += 1;
            report.loops_refined += 1;
        }
        if prm.collect_quality {
            for f in &faces {
                let (a, b, c) = (p_pts[f[0] as usize], p_pts[f[1] as usize], p_pts[f[2] as usize]);
                let e0 = norm(sub(b, a));
                let e1 = norm(sub(c, b));
                let e2 = norm(sub(a, c));
                let s = 0.5 * (e0 + e1 + e2);
                let area = (s * (s - e0) * (s - e1) * (s - e2)).max(0.0).sqrt();
                let inr = if s > 0.0 { area / s.max(1e-20) } else { 1e-20 };
                let longest = e0.max(e1).max(e2);
                q_after.push((min_angle(a, b, c), longest / (2.0 * inr.max(1e-20))));
            }
        }
        report.loop_log.push(log);
    }

    report.new_vertices = out_v.len() - v2.len();

    if prm.separate_stl && !out_t.is_empty() {
        let (nv, moved) = topology::separate_coincident_stl(&out_v, None, Some(v2.len()));
        out_v = nv;
        report.separated_stl = Some(moved);
    }

    if prm.collect_quality {
        let n_before = q_before.len();
        let mins: Vec<f64> = q_before.iter().map(|x| x.0).collect();
        let asps: Vec<f64> = q_before.iter().map(|x| x.1).collect();
        report.quality_before = Some(QualityAgg {
            n: n_before,
            min_angle_p5: percentile(mins.clone(), 5.0),
            min_angle_p50: percentile(mins, 50.0),
            aspect_p50: percentile(asps.clone(), 50.0),
            aspect_p95: percentile(asps, 95.0),
        });
        let mins: Vec<f64> = q_after.iter().map(|x| x.0).collect();
        let asps: Vec<f64> = q_after.iter().map(|x| x.1).collect();
        report.quality_after = Some(QualityAgg {
            n: q_after.len(),
            min_angle_p5: percentile(mins.clone(), 5.0),
            min_angle_p50: percentile(mins, 50.0),
            aspect_p50: percentile(asps.clone(), 50.0),
            aspect_p95: percentile(asps, 95.0),
        });
    }

    (out_v, out_t, report)
}

#[inline]
fn min_angle(a: V3, b: V3, c: V3) -> f64 {
    let ang = |p: V3, x: V3, y: V3| -> f64 {
        let v1 = sub(x, p);
        let v2 = sub(y, p);
        let d = dot(v1, v2);
        let n = norm(v1) * norm(v2);
        if n < 1e-20 {
            0.0
        } else {
            (d / n).clamp(-1.0, 1.0).acos().to_degrees()
        }
    };
    ang(a, b, c).min(ang(b, a, c)).min(ang(c, a, b))
}
