// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! PLC completion and validation for the CDT3D path.
//!
//! The module is deliberately independent of the default Splint pipeline.  It
//! reuses the exact arrangement classifier and the projected 2D CDT, adds the
//! conformance points that the reference arrangement does not supply, and
//! returns a canonical `f64` PLC with a report and a deterministic digest.

use crate::arrangement::{
    self, arrangement_core, clip_polygon_simplex, edge_key, ExtraConstraints,
};
use crate::cdt2d::{HostFrame, Point2D};
use crate::point::Point3;
use crate::triangle_intersection::{intersect_triangles, Triangle, TriangleIntersection};
use num_rational::BigRational;
use num_traits::{One, Signed, Zero};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Options for the PLC completion pass.  The defaults are the P0 gate values.
#[derive(Clone, Debug)]
pub struct PlcOptions {
    /// Maximum number of arrangement/conformance passes.
    pub max_passes: usize,
    /// Maximum number of positive-area coplanar overlap perturbations.
    pub perturb_limit: usize,
}

impl Default for PlcOptions {
    fn default() -> Self {
        Self {
            max_passes: 4,
            perturb_limit: 32,
        }
    }
}

/// Report for one PLC completion run.  Boundary and non-manifold edges are
/// reported but do not by themselves invalidate a PLC; proper intersections,
/// positive-area coplanar overlaps, T-junctions, touching points and
/// degeneracies do.
#[derive(Clone, Debug, Default)]
pub struct PlcReport {
    pub input_vertices: usize,
    pub input_faces: usize,
    pub canonical_vertices: usize,
    pub canonical_faces: usize,
    pub dropped_degenerate_faces: usize,
    pub dropped_duplicate_faces: usize,
    pub coplanar_positive_before: usize,
    pub coplanar_positive_after: usize,
    pub perturbed_vertices: usize,
    pub perturbed_faces: usize,
    pub passes: usize,
    pub converged: bool,
    pub output_vertices: usize,
    pub output_faces: usize,
    pub proper_si: usize,
    pub t_junctions: usize,
    pub touch_points: usize,
    pub duplicate_faces: usize,
    pub degenerate_faces: usize,
    pub rounded_duplicate_vertices: usize,
    pub nonmanifold_edges: usize,
    pub boundary_edges: usize,
    pub boundary_components: usize,
    pub components: usize,
    pub input_area: f64,
    pub output_area: f64,
    pub area_ratio: f64,
    pub plc_valid: bool,
    pub hash: u64,
}

/// A validated PLC in canonical `f64` form.
#[derive(Clone, Debug)]
pub struct Plc {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[usize; 3]>,
    pub report: PlcReport,
}

/// Build and validate a PLC from a triangle soup.
///
/// The function is opt-in: the existing `arrangement_lite_core` and the
/// default Splint path are not changed.  A returned `Plc` may have
/// `report.plc_valid == false`; callers must check it and fall back rather
/// than feed the complex to the CDT.
pub fn build_plc(
    input_verts: &[[f64; 3]],
    input_tris: &[[usize; 3]],
    opts: &PlcOptions,
) -> Result<Plc, String> {
    if input_verts.is_empty() || input_tris.is_empty() {
        return Ok(Plc {
            verts: Vec::new(),
            tris: Vec::new(),
            report: PlcReport {
                input_vertices: input_verts.len(),
                input_faces: input_tris.len(),
                plc_valid: false,
                ..PlcReport::default()
            },
        });
    }

    let mut canon = canonicalize_input(input_verts, input_tris)?;
    let before_pairs = positive_coplanar_pairs(&canon.verts, &canon.tris);
    let coplanar_before = before_pairs.len();

    // Resolve positive-area coplanar overlaps deterministically.  A one-ulp
    // move off the common plane turns the pair into a proper/touching
    // intersection, which the exact arrangement already handles.  The move is
    // far below the f32 output resolution and is reported.
    let mut perturbed_vertices: BTreeSet<usize> = BTreeSet::new();
    let mut pairs = before_pairs;
    for _ in 0..opts.perturb_limit {
        if pairs.is_empty() {
            break;
        }
        let mut changed = false;
        for &(i, j, _) in &pairs {
            if let Some(v) = perturb_pair(&mut canon.verts, &canon.tris, i, j) {
                perturbed_vertices.insert(v);
                changed = true;
            }
        }
        if !changed {
            break;
        }
        pairs = positive_coplanar_pairs(&canon.verts, &canon.tris);
    }
    let perturbed_faces = if perturbed_vertices.is_empty() {
        0
    } else {
        canon
            .tris
            .iter()
            .filter(|t| t.iter().any(|v| perturbed_vertices.contains(v)))
            .count()
    };

    // Iterate the exact arrangement with all conformance points.  The first
    // pass resolves proper intersections and the input T-junctions/touches;
    // later passes catch vertices created by the first pass on a third
    // triangle.  The loop stops when the output is conforming and SI-free.
    let mut cur_v = canon.verts.clone();
    let mut cur_t = canon.tris.clone();
    let mut pool: Vec<[BigRational; 3]> = Vec::new();
    let mut out_tris: Vec<[usize; 3]> = Vec::new();
    let mut passes = 0usize;
    let mut converged = false;
    for _ in 0..opts.max_passes.max(1) {
        let extras = build_extras(&cur_v, &cur_t);
        let (p, t, _rep) = arrangement_core(&cur_v, &cur_t, &extras)?;
        let fv: Vec<[f64; 3]> = p.iter().map(arrangement::rat3_to_f64).collect();
        let after = build_extras(&fv, &t);
        let t_junctions: usize = after.edge_points.values().map(|v| v.len()).sum();
        let touch_points: usize = after.face_points.iter().map(|v| v.len()).sum();
        let proper_si = count_proper_si(&fv, &t);
        pool = p;
        out_tris = t;
        passes += 1;
        if t_junctions == 0 && touch_points == 0 && proper_si == 0 {
            converged = true;
            break;
        }
        cur_v = fv;
        cur_t = out_tris.clone();
    }

    let (out_v, out_t) = canonicalize_output(pool, out_tris);
    let mut report = validate(&out_v, &out_t);
    report.input_vertices = input_verts.len();
    report.input_faces = input_tris.len();
    report.canonical_vertices = canon.verts.len();
    report.canonical_faces = canon.tris.len();
    report.dropped_degenerate_faces = canon.dropped_degenerate;
    report.dropped_duplicate_faces = canon.dropped_duplicate;
    report.coplanar_positive_before = coplanar_before;
    report.perturbed_vertices = perturbed_vertices.len();
    report.perturbed_faces = perturbed_faces;
    report.passes = passes;
    report.converged = converged;
    report.input_area = triangle_area_sum(&canon.verts, &canon.tris);
    report.output_area = triangle_area_sum(&out_v, &out_t);
    report.area_ratio = if report.input_area > 0.0 {
        report.output_area / report.input_area
    } else {
        0.0
    };
    report.plc_valid = report.plc_valid
        && converged
        && report.coplanar_positive_after == 0
        && report.rounded_duplicate_vertices == 0;
    report.hash = plc_hash(&out_v, &out_t);

    Ok(Plc {
        verts: out_v,
        tris: out_t,
        report,
    })
}

struct CanonicalInput {
    verts: Vec<[f64; 3]>,
    tris: Vec<[usize; 3]>,
    dropped_degenerate: usize,
    dropped_duplicate: usize,
}

fn canonical_key(p: [f64; 3]) -> [u64; 3] {
    p.map(|x| if x == 0.0 { 0 } else { x.to_bits() })
}

fn canonicalize_input(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> Result<CanonicalInput, String> {
    let mut map: HashMap<[u64; 3], usize> = HashMap::new();
    let mut out_v: Vec<[f64; 3]> = Vec::new();
    let mut old_to_new: Vec<usize> = Vec::with_capacity(verts.len());
    for &p in verts {
        let key = canonical_key(p);
        let idx = *map.entry(key).or_insert_with(|| {
            out_v.push(p);
            out_v.len() - 1
        });
        old_to_new.push(idx);
    }

    let mut out_t: Vec<[usize; 3]> = Vec::new();
    let mut seen: HashSet<[usize; 3]> = HashSet::new();
    let mut dropped_degenerate = 0usize;
    let mut dropped_duplicate = 0usize;
    for t in tris {
        if t.iter().any(|&v| v >= old_to_new.len()) {
            return Err("triangle index out of range".to_string());
        }
        let tri = [old_to_new[t[0]], old_to_new[t[1]], old_to_new[t[2]]];
        if tri[0] == tri[1] || tri[1] == tri[2] || tri[2] == tri[0] {
            dropped_degenerate += 1;
            continue;
        }
        if is_degenerate(&out_v, &tri) {
            dropped_degenerate += 1;
            continue;
        }
        let mut key = tri;
        key.sort_unstable();
        if !seen.insert(key) {
            dropped_duplicate += 1;
            continue;
        }
        out_t.push(tri);
    }
    Ok(CanonicalInput {
        verts: out_v,
        tris: out_t,
        dropped_degenerate,
        dropped_duplicate,
    })
}

fn is_degenerate(verts: &[[f64; 3]], tri: &[usize; 3]) -> bool {
    let a = Point3::Explicit(verts[tri[0]]);
    let b = Point3::Explicit(verts[tri[1]]);
    let c = Point3::Explicit(verts[tri[2]]);
    match HostFrame::from_points(&a, &b, &c) {
        Some(frame) => frame.project(&a).is_none(),
        None => true,
    }
}

fn sub3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn aabb(verts: &[[f64; 3]], tri: &[usize; 3]) -> ([f64; 3], [f64; 3]) {
    let a = verts[tri[0]];
    let b = verts[tri[1]];
    let c = verts[tri[2]];
    (
        [
            a[0].min(b[0]).min(c[0]),
            a[1].min(b[1]).min(c[1]),
            a[2].min(b[2]).min(c[2]),
        ],
        [
            a[0].max(b[0]).max(c[0]),
            a[1].max(b[1]).max(c[1]),
            a[2].max(b[2]).max(c[2]),
        ],
    )
}

fn in_aabb(p: [f64; 3], lo: [f64; 3], hi: [f64; 3]) -> bool {
    p[0] >= lo[0]
        && p[0] <= hi[0]
        && p[1] >= lo[1]
        && p[1] <= hi[1]
        && p[2] >= lo[2]
        && p[2] <= hi[2]
}

/// Exact coplanarity of an explicit point with an explicit triangle.  The
/// projected 2D CDT must only receive points that are exactly in the host
/// plane; otherwise a near-coincident derived point is projected onto the
/// plane and creates a false touch point.
fn on_plane(a: [f64; 3], b: [f64; 3], c: [f64; 3], p: [f64; 3]) -> bool {
    crate::predicates3d::orient3d_sign_explicit(a, b, c, p) == 0.0
}

fn polygon_area(poly: &[Point2D]) -> BigRational {
    if poly.len() < 3 {
        return BigRational::zero();
    }
    let mut s = BigRational::zero();
    for i in 0..poly.len() {
        let a = &poly[i];
        let b = &poly[(i + 1) % poly.len()];
        s = s + &a.s * &b.t - &b.s * &a.t;
    }
    s.abs() / BigRational::new(2.into(), 1.into())
}

fn coplanar_overlap_area(
    verts: &[[f64; 3]],
    a: &[usize; 3],
    b: &[usize; 3],
) -> Option<BigRational> {
    let pa = [
        Point3::Explicit(verts[a[0]]),
        Point3::Explicit(verts[a[1]]),
        Point3::Explicit(verts[a[2]]),
    ];
    let frame = HostFrame::from_points(&pa[0], &pa[1], &pa[2])?;
    let mut poly = Vec::with_capacity(3);
    for &vi in b {
        poly.push(frame.project(&Point3::Explicit(verts[vi]))?);
    }
    let clipped = clip_polygon_simplex(&poly);
    let area = polygon_area(&clipped);
    if area.is_positive() {
        Some(area)
    } else {
        None
    }
}

/// Positive-area coplanar overlap pairs, in deterministic `(i, j)` order.
fn positive_coplanar_pairs(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
) -> Vec<(usize, usize, BigRational)> {
    let mut cands = arrangement::broad_phase_candidates(verts, tris);
    cands.sort_unstable();
    cands.dedup();
    let mut out = Vec::new();
    for (i, j) in cands {
        let a = tris[i];
        let b = tris[j];
        let ta = Triangle::explicit(verts[a[0]], verts[a[1]], verts[a[2]]);
        let tb = Triangle::explicit(verts[b[0]], verts[b[1]], verts[b[2]]);
        if matches!(
            intersect_triangles(&ta, &tb),
            TriangleIntersection::CoplanarOverlap
        ) {
            if let Some(area) = coplanar_overlap_area(verts, &a, &b) {
                out.push((i, j, area));
            }
        }
    }
    out
}

/// Move one non-shared vertex of `j` (or `i`) off the common plane by one ulp.
/// The choice is deterministic: the first non-shared vertex, then the
/// coordinate with the largest absolute normal component.
fn perturb_pair(verts: &mut [[f64; 3]], tris: &[[usize; 3]], i: usize, j: usize) -> Option<usize> {
    let a = tris[i];
    let b = tris[j];
    let v = (0..3)
        .map(|k| b[k])
        .find(|v| !a.contains(v))
        .or_else(|| (0..3).map(|k| a[k]).find(|v| !b.contains(v)))?;
    let owner = if b.contains(&v) { b } else { a };
    let p0 = verts[owner[0]];
    let p1 = verts[owner[1]];
    let p2 = verts[owner[2]];
    let n = cross3(sub3(p1, p0), sub3(p2, p0));
    let axis = (0..3)
        .max_by(|&x, &y| n[x].abs().partial_cmp(&n[y].abs()).unwrap())
        .unwrap_or(0);
    let c = verts[v][axis];
    verts[v][axis] = if n[axis] >= 0.0 {
        c.next_up()
    } else {
        c.next_down()
    };
    Some(v)
}

/// Build the extra conformance points for one arrangement pass:
/// every vertex strictly inside an input edge, and every vertex strictly
/// inside an input triangle.
fn build_extras(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> ExtraConstraints {
    let zero = BigRational::zero();
    let one = BigRational::one();

    let mut edge_host: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for (ti, t) in tris.iter().enumerate() {
        for k in 0..3 {
            let u = t[k];
            let v = t[(k + 1) % 3];
            let key = (u.min(v), u.max(v));
            edge_host.entry(key).or_insert(ti);
        }
    }

    let mut edge_sets: BTreeMap<[u64; 6], BTreeSet<usize>> = BTreeMap::new();
    for (&(u, v), &ti) in &edge_host {
        let t = tris[ti];
        let frame = match HostFrame::from_points(
            &Point3::Explicit(verts[t[0]]),
            &Point3::Explicit(verts[t[1]]),
            &Point3::Explicit(verts[t[2]]),
        ) {
            Some(f) => f,
            None => continue,
        };
        let edge_pos = if (t[0] == u && t[1] == v) || (t[0] == v && t[1] == u) {
            0
        } else if (t[1] == u && t[2] == v) || (t[1] == v && t[2] == u) {
            1
        } else {
            2
        };
        let pu = verts[u];
        let pv = verts[v];
        let lo = [pu[0].min(pv[0]), pu[1].min(pv[1]), pu[2].min(pv[2])];
        let hi = [pu[0].max(pv[0]), pu[1].max(pv[1]), pu[2].max(pv[2])];
        for x in 0..verts.len() {
            if x == u || x == v {
                continue;
            }
            if !in_aabb(verts[x], lo, hi) {
                continue;
            }
            if !on_plane(verts[t[0]], verts[t[1]], verts[t[2]], verts[x]) {
                continue;
            }
            let st = match frame.project(&Point3::Explicit(verts[x])) {
                Some(st) => st,
                None => continue,
            };
            let on = match edge_pos {
                0 => st.t == zero && st.s > zero && st.s < one,
                1 => &st.s + &st.t == one && st.s > zero && st.s < one,
                _ => st.s == zero && st.t > zero && st.t < one,
            };
            if on {
                edge_sets
                    .entry(edge_key(verts, u, v))
                    .or_default()
                    .insert(x);
            }
        }
    }

    let mut face_points: Vec<Vec<Point3>> = vec![Vec::new(); tris.len()];
    for (ti, t) in tris.iter().enumerate() {
        let frame = match HostFrame::from_points(
            &Point3::Explicit(verts[t[0]]),
            &Point3::Explicit(verts[t[1]]),
            &Point3::Explicit(verts[t[2]]),
        ) {
            Some(f) => f,
            None => continue,
        };
        let (lo, hi) = aabb(verts, t);
        let mut pts = BTreeSet::new();
        for x in 0..verts.len() {
            if t.contains(&x) || !in_aabb(verts[x], lo, hi) {
                continue;
            }
            if !on_plane(verts[t[0]], verts[t[1]], verts[t[2]], verts[x]) {
                continue;
            }
            let st = match frame.project(&Point3::Explicit(verts[x])) {
                Some(st) => st,
                None => continue,
            };
            let rem = &one - &st.s - &st.t;
            if st.s.is_positive() && st.t.is_positive() && rem.is_positive() {
                pts.insert(x);
            }
        }
        face_points[ti] = pts
            .into_iter()
            .map(|x| Point3::Explicit(verts[x]))
            .collect();
    }

    let edge_points: HashMap<[u64; 6], Vec<Point3>> = edge_sets
        .into_iter()
        .map(|(k, set)| {
            (
                k,
                set.into_iter()
                    .map(|x| Point3::Explicit(verts[x]))
                    .collect(),
            )
        })
        .collect();

    ExtraConstraints {
        edge_points,
        face_points,
        force_edge_points: true,
    }
}

fn count_proper_si(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> usize {
    let mut cands = arrangement::broad_phase_candidates(verts, tris);
    cands.sort_unstable();
    cands.dedup();
    let mut n = 0usize;
    for (i, j) in cands {
        let a = tris[i];
        let b = tris[j];
        let ta = Triangle::explicit(verts[a[0]], verts[a[1]], verts[a[2]]);
        let tb = Triangle::explicit(verts[b[0]], verts[b[1]], verts[b[2]]);
        if matches!(
            intersect_triangles(&ta, &tb),
            TriangleIntersection::ProperSegment { .. }
        ) {
            n += 1;
        }
    }
    n
}

fn triangle_area(p: [f64; 3], q: [f64; 3], r: [f64; 3]) -> f64 {
    let n = cross3(sub3(q, p), sub3(r, p));
    0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()
}

fn triangle_area_sum(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> f64 {
    tris.iter()
        .map(|t| triangle_area(verts[t[0]], verts[t[1]], verts[t[2]]))
        .sum()
}

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let ra = find(parent, a);
    let rb = find(parent, b);
    if ra != rb {
        parent[rb] = ra;
    }
}

fn validate(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> PlcReport {
    let mut report = PlcReport::default();
    report.output_vertices = verts.len();
    report.output_faces = tris.len();

    let mut pos_keys: HashSet<[u64; 3]> = HashSet::new();
    for &p in verts {
        if !pos_keys.insert(canonical_key(p)) {
            report.rounded_duplicate_vertices += 1;
        }
    }

    let mut face_keys: HashSet<[usize; 3]> = HashSet::new();
    for t in tris {
        let mut k = *t;
        k.sort_unstable();
        if !face_keys.insert(k) {
            report.duplicate_faces += 1;
        }
        if is_degenerate(verts, t) {
            report.degenerate_faces += 1;
        }
    }

    let extras = build_extras(verts, tris);
    report.t_junctions = extras.edge_points.values().map(|v| v.len()).sum();
    report.touch_points = extras.face_points.iter().map(|v| v.len()).sum();
    report.proper_si = count_proper_si(verts, tris);
    report.coplanar_positive_after = positive_coplanar_pairs(verts, tris).len();

    let mut edge_count: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for t in tris {
        for k in 0..3 {
            let u = t[k];
            let v = t[(k + 1) % 3];
            *edge_count.entry((u.min(v), u.max(v))).or_insert(0) += 1;
        }
    }
    report.nonmanifold_edges = edge_count.values().filter(|&&c| c > 2).count();
    report.boundary_edges = edge_count.values().filter(|&&c| c % 2 == 1).count();

    let mut parent: Vec<usize> = (0..verts.len()).collect();
    for &(u, v) in edge_count.keys() {
        union(&mut parent, u, v);
    }
    let mut roots = BTreeSet::new();
    for t in tris {
        roots.insert(find(&mut parent, t[0]));
    }
    report.components = roots.len();

    // Connected components of the boundary-edge graph (loops or branching
    // networks; reported as components, not asserted to be simple loops).
    let boundary: Vec<(usize, usize)> = edge_count
        .iter()
        .filter(|(_, &c)| c % 2 == 1)
        .map(|(&e, _)| e)
        .collect();
    let mut bparent: Vec<usize> = (0..verts.len()).collect();
    for &(u, v) in &boundary {
        union(&mut bparent, u, v);
    }
    let mut broots = BTreeSet::new();
    for &(u, v) in &boundary {
        broots.insert(find(&mut bparent, u));
        broots.insert(find(&mut bparent, v));
    }
    report.boundary_components = broots.len();

    report.plc_valid = report.proper_si == 0
        && report.t_junctions == 0
        && report.touch_points == 0
        && report.coplanar_positive_after == 0
        && report.duplicate_faces == 0
        && report.degenerate_faces == 0
        && report.rounded_duplicate_vertices == 0;
    report
}

fn cmp_point(a: &[BigRational; 3], b: &[BigRational; 3]) -> std::cmp::Ordering {
    a[0].cmp(&b[0])
        .then_with(|| a[1].cmp(&b[1]))
        .then_with(|| a[2].cmp(&b[2]))
}

fn rotate_min(t: [usize; 3]) -> [usize; 3] {
    let m = if t[0] <= t[1] && t[0] <= t[2] {
        0
    } else if t[1] <= t[2] {
        1
    } else {
        2
    };
    [t[m], t[(m + 1) % 3], t[(m + 2) % 3]]
}

fn canonicalize_output(
    pool: Vec<[BigRational; 3]>,
    tris: Vec<[usize; 3]>,
) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
    let mut order: Vec<usize> = (0..pool.len()).collect();
    order.sort_by(|&a, &b| cmp_point(&pool[a], &pool[b]));
    let mut remap = vec![0usize; pool.len()];
    for (new, &old) in order.iter().enumerate() {
        remap[old] = new;
    }
    let out_v: Vec<[f64; 3]> = order
        .iter()
        .map(|&i| arrangement::rat3_to_f64(&pool[i]))
        .collect();
    let mut out_t: Vec<[usize; 3]> = tris
        .iter()
        .map(|t| [remap[t[0]], remap[t[1]], remap[t[2]]])
        .filter(|t| t[0] != t[1] && t[1] != t[2] && t[2] != t[0])
        .map(rotate_min)
        .collect();
    out_t.sort_unstable();
    (out_v, out_t)
}

fn plc_hash(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let step = |h: &mut u64, v: u64| {
        *h ^= v;
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for p in verts {
        let k = canonical_key(*p);
        for x in k {
            step(&mut h, x);
        }
    }
    for t in tris {
        for &v in t {
            step(&mut h, v as u64);
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn real_sample(name: &str) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let path = format!(
            "{}/../../tests/real-world-samples/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        );
        read_stl_welded(&path)
    }

    fn fixture(name: &str) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
        read_stl_welded(&path)
    }

    /// Binary STL reader with exact f32 vertex welding (test helper).
    fn read_stl_welded(path: &str) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let data = std::fs::read(path).expect("read STL");
        let n = u32::from_le_bytes(data[80..84].try_into().unwrap()) as usize;
        let mut index: HashMap<[u32; 3], usize> = HashMap::new();
        let mut verts = Vec::new();
        let mut tris = Vec::new();
        for f in 0..n {
            let base = 84 + 50 * f + 12;
            let mut tri = [0usize; 3];
            for (k, slot) in tri.iter_mut().enumerate() {
                let o = base + 12 * k;
                let c: [u32; 3] = std::array::from_fn(|j| {
                    u32::from_le_bytes(data[o + 4 * j..o + 4 * j + 4].try_into().unwrap())
                });
                *slot = *index.entry(c).or_insert_with(|| {
                    verts.push(c.map(|b| f32::from_bits(b) as f64));
                    verts.len() - 1
                });
            }
            tris.push(tri);
        }
        (verts, tris)
    }

    fn opts() -> PlcOptions {
        PlcOptions::default()
    }

    #[test]
    fn crossing_triangles_are_split_and_valid() {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0],
            [1.0, 1.0, 1.0],
        ];
        let tris = vec![[0, 1, 2], [3, 4, 5]];
        let plc = build_plc(&verts, &tris, &opts()).unwrap();
        assert!(plc.report.plc_valid, "report: {:?}", plc.report);
        assert_eq!(plc.report.proper_si, 0);
        assert!(plc.report.output_faces > 2);
    }

    #[test]
    fn t_junction_is_split() {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 2.0],
        ];
        let tris = vec![[0, 1, 2], [3, 1, 4]];
        let plc = build_plc(&verts, &tris, &opts()).unwrap();
        assert!(plc.report.plc_valid, "report: {:?}", plc.report);
        assert_eq!(plc.report.proper_si, 0);
        assert_eq!(plc.report.t_junctions, 0);
        assert!(plc.report.output_faces >= 3);
    }

    #[test]
    fn positive_coplanar_overlap_is_perturbed_and_resolved() {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            [0.5, 0.5, 0.0],
            [1.5, 0.5, 0.0],
            [0.5, 1.5, 0.0],
        ];
        let tris = vec![[0, 1, 2], [3, 4, 5]];
        let plc = build_plc(&verts, &tris, &opts()).unwrap();
        assert_eq!(plc.report.coplanar_positive_before, 1);
        assert_eq!(plc.report.coplanar_positive_after, 0);
        assert!(plc.report.perturbed_vertices >= 1);
        assert!(plc.report.plc_valid, "report: {:?}", plc.report);
    }

    #[test]
    fn duplicate_and_degenerate_faces_are_dropped() {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [2.0, 0.0, 0.0],
        ];
        let tris = vec![[0, 1, 2], [0, 1, 2], [0, 1, 3]];
        let plc = build_plc(&verts, &tris, &opts()).unwrap();
        assert_eq!(plc.report.dropped_duplicate_faces, 1);
        assert_eq!(plc.report.dropped_degenerate_faces, 1);
        assert!(plc.report.plc_valid, "report: {:?}", plc.report);
    }

    #[test]
    fn thingi10k_100827_is_valid_and_deterministic() {
        let (v, t) = real_sample("thingi10k_100827.stl");
        let mut hashes = Vec::new();
        let mut last = None;
        for _ in 0..5 {
            let plc = build_plc(&v, &t, &opts()).unwrap();
            assert!(plc.report.plc_valid, "report: {:?}", plc.report);
            assert_eq!(plc.report.proper_si, 0);
            assert_eq!(plc.report.t_junctions, 0);
            assert_eq!(plc.report.coplanar_positive_after, 0);
            hashes.push(plc.report.hash);
            last = Some(plc.report.clone());
        }
        assert!(
            hashes.windows(2).all(|w| w[0] == w[1]),
            "hashes: {hashes:?}"
        );
        let r = last.unwrap();
        println!(
            "100827: valid={} outV={} outF={} passes={} boundary_edges={} components={} hash={:016x}",
            r.plc_valid, r.output_vertices, r.output_faces, r.passes, r.boundary_edges, r.components, r.hash
        );
    }

    #[test]
    #[ignore = "P0 public-corpus gate; run with --release -- --ignored"]
    fn plc_public_corpus_gate() {
        let samples = [
            ("real", "thingi10k_100827.stl"),
            ("fixture", "case1_open_cup.stl"),
            ("fixture", "case2_pipe.stl"),
            ("fixture", "case3_broken_cup.stl"),
            ("fixture", "case4_cracked_sphere.stl"),
            ("fixture", "case5_box_missing_face.stl"),
            ("fixture", "case6_thin_cup.stl"),
            ("fixture", "plate_a_dent.stl"),
        ];
        for (kind, name) in samples {
            let (v, t) = if kind == "real" {
                real_sample(name)
            } else {
                fixture(name)
            };
            let start = std::time::Instant::now();
            let mut hashes = Vec::new();
            let mut last = None;
            for _ in 0..5 {
                let plc = build_plc(&v, &t, &opts()).unwrap();
                assert!(plc.report.plc_valid, "{name}: report {:?}", plc.report);
                hashes.push(plc.report.hash);
                last = Some(plc);
            }
            let secs = start.elapsed().as_secs_f64() / 5.0;
            assert!(
                hashes.windows(2).all(|w| w[0] == w[1]),
                "{name}: hashes {hashes:?}"
            );
            let plc = last.unwrap();
            println!(
                "{name}: valid={} inV={} inF={} outV={} outF={} passes={} si={} tj={} touch={} cop={} bnd={} nm={} comps={} area_ratio={:.6} secs={:.3} hash={:016x}",
                plc.report.plc_valid,
                plc.report.input_vertices,
                plc.report.input_faces,
                plc.report.output_vertices,
                plc.report.output_faces,
                plc.report.passes,
                plc.report.proper_si,
                plc.report.t_junctions,
                plc.report.touch_points,
                plc.report.coplanar_positive_after,
                plc.report.boundary_edges,
                plc.report.nonmanifold_edges,
                plc.report.components,
                plc.report.area_ratio,
                secs,
                plc.report.hash,
            );
        }
    }
}
