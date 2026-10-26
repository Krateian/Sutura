// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! CDT3D Phase B (P3): exact PLC segment recovery.
//!
//! Starting from the P2 initial Delaunay tetrahedralization, every PLC edge is
//! required to become a chain of tetrahedralization edges.  A missing
//! subsegment is split with an LNC point on the *original* PLC edge (the P1
//! `Lnc` type carries explicit endpoints), using Si's acute-vertex
//! classification:
//!
//! * two acute endpoints: split at the midpoint (the halves then remember one
//!   acute endpoint each);
//! * one acute endpoint `w`: use the sphere centred at `w` that touches the
//!   reference point, then apply the paper's close-to-reference/shift/midpoint
//!   rule;
//! * no acute endpoint: choose the reference vertex that maximises the
//!   circumradius of `(u, v, reference)`; if both touching-sphere radii exceed
//!   `L/2`, split at the midpoint, otherwise use the intersection with the
//!   smallest sphere.
//!
//! The reference candidate scan uses `f64` with exact rational fallback for
//! collinearity; the chosen split parameter is rounded to `f64`, while the
//! insertion cavity and every final validity check use the exact predicates
//! (`orient3d`, `insphere`, and symbolic perturbation for cospherical ties).
//!
//! The module is opt-in and independent of the default Splint path.

use crate::cdt3d::dt::{build_dt, build_neighbours, sorted_face, BoundingBox3, DtOptions};
use crate::cdt3d::plc::Plc;
use crate::point::Point3;
use crate::predicates3d::{insphere_sign, insphere_sign_perturbed, orient3d_sign};
use num_rational::BigRational;
use num_traits::{Signed, ToPrimitive, Zero};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

/// Safety cap on inserted Steiner points; exceeding it is a typed failure.
pub const DEFAULT_MAX_SPLITS: usize = 200_000;
pub(crate) const BOX_CORNERS: usize = 8;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Options for segment recovery.
#[derive(Clone, Copy, Debug)]
pub struct SegmentOptions {
    /// Options for the initial Delaunay stage.
    pub dt: DtOptions,
    /// Maximum number of subsegment splits before a typed failure.
    pub max_splits: usize,
}

impl Default for SegmentOptions {
    fn default() -> Self {
        Self {
            dt: DtOptions::default(),
            max_splits: DEFAULT_MAX_SPLITS,
        }
    }
}

/// Si's acute-vertex classification for one subsegment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentCategory {
    /// No acute endpoint.
    None,
    /// Exactly one acute endpoint (the remembered original PLC vertex).
    Acute(usize),
    /// Both endpoints acute; the first split is at the midpoint.
    Both,
}

/// One PLC edge and its current ordered split chain.
#[derive(Clone, Debug)]
pub struct SegmentChain {
    /// The original PLC edge, with `orig[0] < orig[1]`.
    pub orig: [usize; 2],
    /// `(parameter, vertex)` pairs sorted by parameter from `orig[0]` (0) to
    /// `orig[1]` (1).  Interior vertices are LNC points on this edge (or an
    /// existing vertex lying exactly on it).
    pub points: Vec<(f64, usize)>,
    /// Category of each interval; `meta.len() == points.len() - 1`.
    pub meta: Vec<SegmentCategory>,
}

/// Validity report for one segment recovery run.
#[derive(Clone, Debug, Default)]
pub struct SegmentReport {
    pub input_segments: usize,
    pub subsegments: usize,
    pub missing_before: usize,
    pub missing_after: usize,
    pub steiner_points: usize,
    pub reused_points: usize,
    pub splits: usize,
    pub splits_cat1: usize,
    pub splits_cat2: usize,
    pub splits_cat3: usize,
    pub acute_vertices: usize,
    pub cat1_segments: usize,
    pub cat2_segments: usize,
    pub cat3_segments: usize,
    pub passes: usize,
    pub tets: usize,
    pub orientation_violations: usize,
    pub duplicate_tets: usize,
    pub nonmanifold_faces: usize,
    pub inconsistent_neighbours: usize,
    pub empty_sphere_violations: usize,
    pub cospherical_faces: usize,
    pub hull_faces: usize,
    pub ghost_faces: usize,
    pub hull_faces_off_box: usize,
    pub unused_vertices: usize,
    pub valid: bool,
    pub hash: u64,
}

/// Recovered tetrahedralization with the PLC edge chains.
#[derive(Clone, Debug)]
pub struct SegmentMesh {
    /// PLC vertices, then box corners, then Steiner points.
    pub points: Vec<Point3>,
    pub plc_vertices: usize,
    pub box3: BoundingBox3,
    pub tets: Vec<[usize; 4]>,
    pub neighbours: Vec<[i32; 4]>,
    pub chains: Vec<SegmentChain>,
    pub report: SegmentReport,
}

/// Recover every PLC edge as a chain of tetrahedralization edges.
pub fn recover_segments(plc: &Plc, opts: &SegmentOptions) -> Result<SegmentMesh, String> {
    if plc.tris.is_empty() {
        return Err("segment recovery needs a non-empty PLC".into());
    }
    let dt = build_dt(&plc.verts, &opts.dt)?;
    if !dt.report.dt_valid {
        return Err(format!("initial DT invalid: {:?}", dt.report));
    }
    let plc_vertices = dt.plc_vertices;
    let box3 = dt.box3;
    let initial_points = dt.verts.len();
    let mut store = PointStore::new(
        dt.verts
            .iter()
            .map(|p| Point3::Explicit(*p))
            .collect::<Vec<_>>(),
    )?;
    let mut tets = dt.tets;
    let mut neighbours = dt.neighbours;
    let (mut chains, acute) = build_classified_chains(plc)?;
    let mut approx: Vec<[f64; 3]> = store
        .points
        .iter()
        .map(|p| {
            p.to_f64()
                .expect("initial point must have approximate coordinates")
        })
        .collect();
    let mut edges = EdgeCounts::new(&tets);
    let missing_before = missing_count(&chains, &edges);
    let cat1_segments = chains
        .iter()
        .filter(|c| c.meta.iter().all(|m| *m == SegmentCategory::None))
        .count();
    let cat2_segments = chains
        .iter()
        .filter(|c| {
            c.meta
                .iter()
                .all(|m| matches!(m, SegmentCategory::Acute(_)))
        })
        .count();
    let cat3_segments = chains
        .iter()
        .filter(|c| c.meta.iter().any(|m| *m == SegmentCategory::Both))
        .count();

    let mut splits = 0usize;
    let mut reused = 0usize;
    let mut passes = 0usize;
    let mut splits_cat1 = 0usize;
    let mut splits_cat2 = 0usize;
    let mut splits_cat3 = 0usize;
    let mut seed_hint = 0usize;
    loop {
        passes += 1;
        let mut target = None;
        'scan: for (ci, chain) in chains.iter().enumerate() {
            for i in 0..chain.points.len().saturating_sub(1) {
                if !edges.contains(chain.points[i].1, chain.points[i + 1].1) {
                    target = Some((ci, i));
                    break 'scan;
                }
            }
        }
        let Some((ci, i)) = target else {
            break;
        };
        if splits >= opts.max_splits {
            return Err(format!(
                "segment recovery exceeded the {} split limit",
                opts.max_splits
            ));
        }

        let (s0, u) = chains[ci].points[i];
        let (s1, v) = chains[ci].points[i + 1];
        let [a, b] = chains[ci].orig;
        let meta = chains[ci].meta[i];
        let choice = choose_split(
            &store.points,
            &approx,
            plc_vertices,
            meta,
            a,
            b,
            u,
            v,
            s0,
            s1,
        )?;
        let (id, is_new) = if let Some(id) = choice.existing {
            (id, false)
        } else {
            let t = 1.0 - choice.s;
            if !(t > 0.0 && t < 1.0) {
                return Err(format!(
                    "split parameter {t} is not strictly inside PLC edge ({a},{b})"
                ));
            }
            let point = Point3::Lnc {
                a: plc.verts[a],
                b: plc.verts[b],
                t,
            };
            store.find_or_insert(point)?
        };
        if is_new {
            insert_point(
                &store.points,
                &mut tets,
                &mut neighbours,
                &mut edges,
                &mut seed_hint,
                id,
            )?;
            approx.push(
                store.points[id]
                    .to_f64()
                    .expect("new Steiner point must have approximate coordinates"),
            );
        } else {
            reused += 1;
        }
        let (left, right) = split_meta(meta, chains[ci].orig);
        chains[ci].points.insert(i + 1, (choice.s, id));
        chains[ci].meta[i] = left;
        chains[ci].meta.insert(i + 1, right);
        match meta {
            SegmentCategory::None => splits_cat1 += 1,
            SegmentCategory::Acute(_) => splits_cat2 += 1,
            SegmentCategory::Both => splits_cat3 += 1,
        }
        splits += 1;
    }

    let tets = canonicalize_mesh(&store.points, tets)?;
    let neighbours = build_neighbours_fast(&tets)?;
    let final_edges = EdgeCounts::new(&tets);
    let mut report = validate_mesh(
        &store.points,
        &tets,
        &neighbours,
        &chains,
        &final_edges,
        plc_vertices,
        initial_points,
        &box3,
    );
    report.input_segments = chains.len();
    report.missing_before = missing_before;
    report.splits = splits;
    report.splits_cat1 = splits_cat1;
    report.splits_cat2 = splits_cat2;
    report.splits_cat3 = splits_cat3;
    report.reused_points = reused;
    report.passes = passes;
    report.steiner_points = store.points.len() - initial_points;
    report.acute_vertices = acute.iter().filter(|x| **x).count();
    report.cat1_segments = cat1_segments;
    report.cat2_segments = cat2_segments;
    report.cat3_segments = cat3_segments;
    report.hash = segment_hash(&store.points, &tets, &chains);

    Ok(SegmentMesh {
        points: store.points,
        plc_vertices,
        box3,
        tets,
        neighbours,
        chains,
        report,
    })
}

fn build_chains(plc: &Plc) -> Result<Vec<SegmentChain>, String> {
    let n = plc.verts.len();
    let mut edges = BTreeSet::new();
    for t in &plc.tris {
        for k in 0..3 {
            let u = t[k];
            let v = t[(k + 1) % 3];
            if u >= n || v >= n {
                return Err("PLC triangle index out of range".into());
            }
            if u == v {
                return Err("PLC triangle repeats a vertex".into());
            }
            edges.insert(if u < v { [u, v] } else { [v, u] });
        }
    }
    Ok(edges
        .into_iter()
        .map(|orig| SegmentChain {
            orig,
            points: vec![(0.0, orig[0]), (1.0, orig[1])],
            meta: Vec::new(),
        })
        .collect())
}

fn classify_segment(orig: [usize; 2], acute: &[bool]) -> SegmentCategory {
    match (acute[orig[0]], acute[orig[1]]) {
        (true, true) => SegmentCategory::Both,
        (true, false) => SegmentCategory::Acute(orig[0]),
        (false, true) => SegmentCategory::Acute(orig[1]),
        (false, false) => SegmentCategory::None,
    }
}

fn build_classified_chains(plc: &Plc) -> Result<(Vec<SegmentChain>, Vec<bool>), String> {
    let mut chains = build_chains(plc)?;
    let acute = acute_vertices(plc, &chains);
    for chain in &mut chains {
        chain.meta = vec![classify_segment(chain.orig, &acute)];
    }
    Ok((chains, acute))
}

fn split_meta(meta: SegmentCategory, orig: [usize; 2]) -> (SegmentCategory, SegmentCategory) {
    match meta {
        SegmentCategory::None => (SegmentCategory::None, SegmentCategory::None),
        SegmentCategory::Acute(w) => (SegmentCategory::Acute(w), SegmentCategory::Acute(w)),
        SegmentCategory::Both => (
            SegmentCategory::Acute(orig[0]),
            SegmentCategory::Acute(orig[1]),
        ),
    }
}

/// Si's acute vertex: two incident PLC segments form an angle strictly smaller
/// than 90 degrees.
fn acute_vertices(plc: &Plc, chains: &[SegmentChain]) -> Vec<bool> {
    let n = plc.verts.len();
    let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); n];
    for c in chains {
        adjacency[c.orig[0]].push(c.orig[1]);
        adjacency[c.orig[1]].push(c.orig[0]);
    }
    for neighbors in &mut adjacency {
        neighbors.sort_unstable();
        neighbors.dedup();
    }

    let mut acute = vec![false; n];
    for v in 0..n {
        let neighbors = &adjacency[v];
        if neighbors.len() < 2 {
            continue;
        }
        let pv = plc.verts[v];
        let dirs: Vec<[f64; 3]> = neighbors
            .iter()
            .map(|&u| {
                let p = plc.verts[u];
                [p[0] - pv[0], p[1] - pv[1], p[2] - pv[2]]
            })
            .collect();
        'pairs: for i in 0..dirs.len() {
            for j in (i + 1)..dirs.len() {
                if dot_sign_explicit(dirs[i], dirs[j]) > 0.0 {
                    acute[v] = true;
                    break 'pairs;
                }
            }
        }
    }
    acute
}

fn dot_sign_explicit(a: [f64; 3], b: [f64; 3]) -> f64 {
    use crate::interval::Iv;
    let dot = Iv::exact(a[0])
        .mul(Iv::exact(b[0]))
        .add(Iv::exact(a[1]).mul(Iv::exact(b[1])))
        .add(Iv::exact(a[2]).mul(Iv::exact(b[2])));
    if let Some(sign) = dot.sign() {
        return sign as f64;
    }
    let d = crate::point::f64_to_rat(a[0]) * crate::point::f64_to_rat(b[0])
        + crate::point::f64_to_rat(a[1]) * crate::point::f64_to_rat(b[1])
        + crate::point::f64_to_rat(a[2]) * crate::point::f64_to_rat(b[2]);
    if d.is_zero() {
        0.0
    } else if d.is_negative() {
        -1.0
    } else {
        1.0
    }
}

fn edge_key(u: usize, v: usize) -> [usize; 2] {
    if u < v {
        [u, v]
    } else {
        [v, u]
    }
}

/// Incremental edge-incidence counts for the live tetrahedralization.
struct EdgeCounts {
    map: BTreeMap<[usize; 2], usize>,
}

impl EdgeCounts {
    fn new(tets: &[[usize; 4]]) -> Self {
        let mut out = Self {
            map: BTreeMap::new(),
        };
        for t in tets {
            out.add_tet(t);
        }
        out
    }

    fn add_tet(&mut self, t: &[usize; 4]) {
        for i in 0..4 {
            for j in (i + 1)..4 {
                *self.map.entry(edge_key(t[i], t[j])).or_insert(0) += 1;
            }
        }
    }

    fn remove_tet(&mut self, t: &[usize; 4]) {
        for i in 0..4 {
            for j in (i + 1)..4 {
                let key = edge_key(t[i], t[j]);
                let entry = self.map.get_mut(&key).expect("edge must exist");
                *entry -= 1;
                if *entry == 0 {
                    self.map.remove(&key);
                }
            }
        }
    }

    fn contains(&self, u: usize, v: usize) -> bool {
        self.map.contains_key(&edge_key(u, v))
    }
}

fn missing_count(chains: &[SegmentChain], edges: &EdgeCounts) -> usize {
    chains
        .iter()
        .map(|c| {
            c.points
                .windows(2)
                .filter(|w| !edges.contains(w[0].1, w[1].1))
                .count()
        })
        .sum()
}

struct PointStore {
    points: Vec<Point3>,
    canonical: BTreeMap<Vec<u64>, usize>,
    exact: BTreeMap<[BigRational; 3], usize>,
}

impl PointStore {
    fn new(points: Vec<Point3>) -> Result<Self, String> {
        let mut canonical = BTreeMap::new();
        let mut exact = BTreeMap::new();
        for (i, p) in points.iter().enumerate() {
            canonical.entry(p.canonical_key()).or_insert(i);
            let r = p
                .to_rational()
                .ok_or_else(|| format!("initial point {i} has no rational coordinates"))?;
            exact.entry(r).or_insert(i);
        }
        Ok(Self {
            points,
            canonical,
            exact,
        })
    }

    /// Reuse an existing vertex if it has the same construction key or the same
    /// exact rational coordinates, otherwise append a new point.
    fn find_or_insert(&mut self, p: Point3) -> Result<(usize, bool), String> {
        let ck = p.canonical_key();
        if let Some(&id) = self.canonical.get(&ck) {
            return Ok((id, false));
        }
        let r = p
            .to_rational()
            .ok_or_else(|| "new Steiner point has no rational coordinates".to_string())?;
        if let Some(&id) = self.exact.get(&r) {
            self.canonical.insert(ck, id);
            return Ok((id, false));
        }
        let id = self.points.len();
        self.points.push(p);
        self.canonical.insert(ck, id);
        self.exact.insert(r, id);
        Ok((id, true))
    }
}

struct SplitChoice {
    s: f64,
    existing: Option<usize>,
}

fn choose_split(
    points: &[Point3],
    approx: &[[f64; 3]],
    plc_vertices: usize,
    meta: SegmentCategory,
    orig_a: usize,
    orig_b: usize,
    u: usize,
    v: usize,
    s0: f64,
    s1: f64,
) -> Result<SplitChoice, String> {
    if meta == SegmentCategory::Both {
        return Ok(SplitChoice {
            s: ensure_inside(0.5 * (s0 + s1), u, v, s0, s1)?,
            existing: None,
        });
    }

    let pu = approx[u];
    let pv = approx[v];
    let uv = sub3(pv, pu);
    let l2 = dot3(uv, uv);
    let mut collinear = None;
    let mut best: Option<(f64, usize)> = None;
    for r in 0..points.len() {
        if r == u || r == v {
            continue;
        }
        // The padded box is synthetic; its corners must not drive PLC splits.
        if r >= plc_vertices && r < plc_vertices + BOX_CORNERS {
            continue;
        }
        let pr = approx[r];
        let ur = sub3(pr, pu);
        let vr = sub3(pr, pv);
        if dot3(ur, vr) >= 0.0 {
            continue;
        }
        let cross = cross3(ur, vr);
        let c2 = dot3(cross, cross);
        let r1 = dot3(ur, ur);
        let r2 = dot3(vr, vr);
        let near_collinear = c2 <= 1e-24 * (l2 * r1 * r2).max(1e-300);
        if (c2 <= 0.0 || near_collinear) && is_collinear(points, orig_a, orig_b, r)? {
            if collinear.is_none() {
                let q = parameter_on_segment(points, orig_a, orig_b, r)?;
                if q > s0 && q < s1 && q > 0.0 && q < 1.0 {
                    collinear = Some((q, r));
                }
            }
            continue;
        }
        let cr = if c2 > 0.0 {
            l2 * r1 * r2 / (4.0 * c2)
        } else {
            f64::INFINITY
        };
        let better = match best {
            None => true,
            Some((br, _)) => cr > br,
        };
        if better {
            best = Some((cr, r));
        }
    }
    if let Some((s, id)) = collinear {
        return Ok(SplitChoice {
            s,
            existing: Some(id),
        });
    }

    let s = match meta {
        SegmentCategory::None => {
            let ratio = match best {
                Some((_, r)) => ratio_category1(points, u, v, r)?,
                None => 0.5,
            };
            s0 + ratio * (s1 - s0)
        }
        SegmentCategory::Acute(w) => match best {
            Some((_, r)) => split_parameter_acute(points, orig_a, orig_b, w, s0, s1, r)?,
            None => 0.5 * (s0 + s1),
        },
        SegmentCategory::Both => unreachable!(),
    };
    let s = ensure_inside(s, u, v, s0, s1)?;
    Ok(SplitChoice { s, existing: None })
}

fn sub3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn ensure_inside(s: f64, u: usize, v: usize, s0: f64, s1: f64) -> Result<f64, String> {
    let s = if s > s0 && s < s1 { s } else { 0.5 * (s0 + s1) };
    if !(s > s0 && s < s1) {
        return Err(format!(
            "cannot split subsegment ({u},{v}) at f64 resolution"
        ));
    }
    Ok(s)
}

fn rat(p: &Point3) -> Result<[BigRational; 3], String> {
    p.to_rational()
        .ok_or_else(|| "implicit point has no rational coordinates".to_string())
}

fn sub_rat(a: &[BigRational; 3], b: &[BigRational; 3]) -> [BigRational; 3] {
    [&a[0] - &b[0], &a[1] - &b[1], &a[2] - &b[2]]
}

fn dot_rat(a: &[BigRational; 3], b: &[BigRational; 3]) -> BigRational {
    let mut s = &a[0] * &b[0];
    s += &a[1] * &b[1];
    s += &a[2] * &b[2];
    s
}

fn cross_rat(a: &[BigRational; 3], b: &[BigRational; 3]) -> [BigRational; 3] {
    [
        &a[1] * &b[2] - &a[2] * &b[1],
        &a[2] * &b[0] - &a[0] * &b[2],
        &a[0] * &b[1] - &a[1] * &b[0],
    ]
}

fn is_collinear(points: &[Point3], a: usize, b: usize, r: usize) -> Result<bool, String> {
    let pa = rat(&points[a])?;
    let pb = rat(&points[b])?;
    let pr = rat(&points[r])?;
    let ab = sub_rat(&pb, &pa);
    let ar = sub_rat(&pr, &pa);
    let cross = cross_rat(&ab, &ar);
    Ok(dot_rat(&cross, &cross).is_zero())
}

fn parameter_on_segment(points: &[Point3], a: usize, b: usize, r: usize) -> Result<f64, String> {
    let pa = rat(&points[a])?;
    let pb = rat(&points[b])?;
    let pr = rat(&points[r])?;
    let ab = sub_rat(&pb, &pa);
    let ar = sub_rat(&pr, &pa);
    let den = dot_rat(&ab, &ab);
    if den.is_zero() {
        return Err("degenerate PLC edge".into());
    }
    let q = (dot_rat(&ar, &ab) / den)
        .to_f64()
        .ok_or_else(|| "segment parameter is not finite".to_string())?;
    Ok(q)
}

fn ratio_category1(points: &[Point3], u: usize, v: usize, r: usize) -> Result<f64, String> {
    let pu = rat(&points[u])?;
    let pv = rat(&points[v])?;
    let pr = rat(&points[r])?;
    let uv = sub_rat(&pv, &pu);
    let ur = sub_rat(&pr, &pu);
    let vr = sub_rat(&pr, &pv);
    let z = dot_rat(&uv, &uv);
    let x = dot_rat(&ur, &ur);
    let y = dot_rat(&vr, &vr);
    if z.is_zero() {
        return Err("degenerate subsegment".into());
    }

    let four = BigRational::from_integer(4.into());
    if (&four * &x) > z.clone() && (&four * &y) > z.clone() {
        return Ok(0.5);
    }

    // The sphere with the smaller radius is the one used for the intersection.
    let (choose_u, q) = if x <= y {
        (true, &x / &z)
    } else {
        (false, &y / &z)
    };
    let qf = q
        .to_f64()
        .ok_or_else(|| "split ratio is not finite".to_string())?;
    if !(0.0..=1.0).contains(&qf) {
        return Ok(0.5);
    }
    let ratio = if choose_u { qf.sqrt() } else { 1.0 - qf.sqrt() };
    Ok(if ratio.is_finite() && ratio > 0.0 && ratio < 1.0 {
        ratio
    } else {
        0.5
    })
}

/// Si's split rule for a subsegment with one remembered acute vertex `w`.
fn split_parameter_acute(
    points: &[Point3],
    orig_a: usize,
    orig_b: usize,
    w: usize,
    s0: f64,
    s1: f64,
    r: usize,
) -> Result<f64, String> {
    let pa = match points[orig_a] {
        Point3::Explicit(p) => p,
        _ => return Err("PLC endpoint is not explicit".into()),
    };
    let pb = match points[orig_b] {
        Point3::Explicit(p) => p,
        _ => return Err("PLC endpoint is not explicit".into()),
    };
    let pr = points[r]
        .to_f64()
        .ok_or_else(|| "reference point has no approximate coordinates".to_string())?;
    let l0 = dist_f64(pa, pb);
    if !(l0 > 0.0) {
        return Err("degenerate PLC edge".into());
    }
    let (pw, s_w, dir) = if w == orig_a {
        (pa, 0.0, 1.0)
    } else if w == orig_b {
        (pb, 1.0, -1.0)
    } else {
        return Err("remembered acute vertex is not an original endpoint".into());
    };
    let rw = dist_f64(pw, pr);
    let s_p = s_w + dir * (rw / l0);

    let s_vw = if (s0 - s_w).abs() <= (s1 - s_w).abs() {
        s0
    } else {
        s1
    };
    let s_vo = if s_vw == s0 { s1 } else { s0 };
    let p = [
        pa[0] + s_p * (pb[0] - pa[0]),
        pa[1] + s_p * (pb[1] - pa[1]),
        pa[2] + s_p * (pb[2] - pa[2]),
    ];
    let d_pr = dist_f64(p, pr);
    let d_pvo = (s_p - s_vo).abs() * l0;
    let d_vwp = (s_p - s_vw).abs() * l0;

    if d_pr < d_pvo {
        return Ok(s_p);
    }
    if d_pr < 0.5 * d_vwp {
        let toward_vw = if s_vw > s_p { 1.0 } else { -1.0 };
        return Ok(s_p + toward_vw * (d_pr / l0));
    }
    Ok(0.5 * (s_vw + s_p))
}

fn dist_f64(a: [f64; 3], b: [f64; 3]) -> f64 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

pub(crate) fn oriented_face_apex(
    points: &[Point3],
    t: &[usize; 4],
    lv: usize,
) -> (usize, usize, usize, usize) {
    let mut f = [0usize; 3];
    let mut k = 0;
    for i in 0..4 {
        if i != lv {
            f[k] = t[i];
            k += 1;
        }
    }
    let d = t[lv];
    if orient3d_sign(&points[f[0]], &points[f[1]], &points[f[2]], &points[d]) < 0.0 {
        f.swap(1, 2);
    }
    (f[0], f[1], f[2], d)
}

fn contains_point(points: &[Point3], t: &[usize; 4], p: usize) -> Result<bool, String> {
    for lv in 0..4 {
        let (a, b, c, _) = oriented_face_apex(points, t, lv);
        if orient3d_sign(&points[a], &points[b], &points[c], &points[p]) < 0.0 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn locate_tet(
    points: &[Point3],
    tets: &[[usize; 4]],
    neighbours: &[[i32; 4]],
    p: usize,
    seed_hint: usize,
) -> Result<usize, String> {
    if tets.is_empty() {
        return Err("cannot insert a point into an empty tetrahedralization".into());
    }
    let mut t = if seed_hint < tets.len() { seed_hint } else { 0 };
    let max_steps = tets.len().saturating_mul(4).saturating_add(16);
    for _ in 0..max_steps {
        let mut moved = false;
        for lv in 0..4 {
            let (a, b, c, _) = oriented_face_apex(points, &tets[t], lv);
            let s = orient3d_sign(&points[a], &points[b], &points[c], &points[p]);
            if s < 0.0 {
                let n = neighbours[t][lv];
                if n < 0 {
                    return Err("Steiner point lies outside the synthetic box".into());
                }
                t = n as usize;
                moved = true;
                break;
            }
        }
        if !moved {
            return Ok(t);
        }
    }
    for (i, tet) in tets.iter().enumerate() {
        if contains_point(points, tet, p)? {
            return Ok(i);
        }
    }
    Err("Steiner point location failed".into())
}

fn collect_containing(
    points: &[Point3],
    tets: &[[usize; 4]],
    neighbours: &[[i32; 4]],
    p: usize,
    seed: usize,
) -> Result<Vec<usize>, String> {
    let mut seen = vec![false; tets.len()];
    let mut stack = vec![seed];
    seen[seed] = true;
    let mut out = vec![seed];
    while let Some(t) = stack.pop() {
        for lv in 0..4 {
            let (a, b, c, _) = oriented_face_apex(points, &tets[t], lv);
            if orient3d_sign(&points[a], &points[b], &points[c], &points[p]) != 0.0 {
                continue;
            }
            let n = neighbours[t][lv];
            if n < 0 {
                return Err("Steiner point lies on the box boundary".into());
            }
            let n = n as usize;
            if seen[n] {
                continue;
            }
            if contains_point(points, &tets[n], p)? {
                seen[n] = true;
                stack.push(n);
                out.push(n);
            }
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// Extend a Bowyer--Watson cavity across boundary faces that are exactly
/// coplanar with the new point.  The resulting cavity is the closure of the
/// point's containing star in the coplanar directions, so every remaining
/// boundary face is strictly visible from `p`.  This is the deterministic
/// (symbolic-perturbation) tie rule for a point inserted on an existing mesh
/// face or edge.
fn expand_coplanar_cavity(
    points: &[Point3],
    tets: &[[usize; 4]],
    neighbours: &[[i32; 4]],
    conflict: &mut [bool],
    p: usize,
) -> Result<(), String> {
    let mut queue: VecDeque<usize> = conflict
        .iter()
        .enumerate()
        .filter(|(_, c)| **c)
        .map(|(i, _)| i)
        .collect();
    let mut steps = 0usize;
    let limit = tets.len().saturating_mul(8).saturating_add(16);
    while let Some(t) = queue.pop_front() {
        steps += 1;
        if steps > limit {
            return Err(format!("coplanar insertion cavity for point {p} did not converge"));
        }
        for lv in 0..4 {
            let n = neighbours[t][lv];
            if n >= 0 && conflict[n as usize] {
                continue;
            }
            let (a, b, c, _) = oriented_face_apex(points, &tets[t], lv);
            let s = orient3d_sign(&points[a], &points[b], &points[c], &points[p]);
            if s < 0.0 {
                return Err(format!(
                    "insertion cavity for point {p} is not star-shaped"
                ));
            }
            if s == 0.0 {
                if n < 0 {
                    return Err(format!(
                        "Steiner point {p} lies on the synthetic box boundary"
                    ));
                }
                conflict[n as usize] = true;
                queue.push_back(n as usize);
            }
        }
    }
    Ok(())
}

fn insert_point(
    points: &[Point3],
    tets: &mut Vec<[usize; 4]>,
    neighbours: &mut Vec<[i32; 4]>,
    edges: &mut EdgeCounts,
    seed_hint: &mut usize,
    p: usize,
) -> Result<(), String> {
    let seed = locate_tet(points, tets, neighbours, p, *seed_hint)?;
    let containing = collect_containing(points, tets, neighbours, p, seed)?;

    let mut conflict = vec![false; tets.len()];
    let mut queue = VecDeque::new();
    for &t in &containing {
        if !conflict[t] {
            conflict[t] = true;
            queue.push_back(t);
        }
    }
    while let Some(t) = queue.pop_front() {
        for lv in 0..4 {
            let n = neighbours[t][lv];
            if n < 0 {
                continue;
            }
            let n = n as usize;
            if conflict[n] {
                continue;
            }
            let tet = tets[n];
            let s = insphere_sign_perturbed(
                &points[tet[0]],
                &points[tet[1]],
                &points[tet[2]],
                &points[tet[3]],
                &points[p],
                [tet[0], tet[1], tet[2], tet[3], p],
            );
            if s > 0.0 {
                conflict[n] = true;
                queue.push_back(n);
            }
        }
    }

    expand_coplanar_cavity(points, tets, neighbours, &mut conflict, p)?;

    let mut faces: Vec<([usize; 3], [usize; 3], usize, usize)> = Vec::new();
    for (ti, t) in tets.iter().enumerate() {
        if !conflict[ti] {
            continue;
        }
        for lv in 0..4 {
            let n = neighbours[ti][lv];
            if n >= 0 && conflict[n as usize] {
                continue;
            }
            let (a, b, c, _) = oriented_face_apex(points, t, lv);
            let s = orient3d_sign(&points[a], &points[b], &points[c], &points[p]);
            if s <= 0.0 {
                return Err(format!(
                    "degenerate insertion cavity boundary for point {p}"
                ));
            }
            faces.push((sorted_face(t, lv), [a, b, c], ti, lv));
        }
    }
    if faces.is_empty() {
        return Err("empty insertion cavity".into());
    }
    faces.sort_by_key(|(key, _, _, _)| *key);
    for w in faces.windows(2) {
        if w[0].0 == w[1].0 {
            return Err("duplicate insertion cavity face".into());
        }
    }

    let removed = conflict.iter().filter(|c| **c).count();
    let mut new_tets = Vec::with_capacity(tets.len() - removed + faces.len());
    let mut remap: Vec<i32> = vec![-1; tets.len()];
    for (ti, t) in tets.iter().enumerate() {
        if !conflict[ti] {
            remap[ti] = new_tets.len() as i32;
            new_tets.push(*t);
        }
    }
    let kept = new_tets.len();
    for (_, f, _, _) in &faces {
        new_tets.push([f[0], f[1], f[2], p]);
    }
    for (ti, t) in tets.iter().enumerate() {
        if conflict[ti] {
            edges.remove_tet(t);
        }
    }
    for t in &new_tets[kept..] {
        edges.add_tet(t);
    }

    let mut new_neighbours = vec![[-1i32; 4]; new_tets.len()];
    for (ti, t) in tets.iter().enumerate() {
        if conflict[ti] {
            continue;
        }
        let ni = remap[ti] as usize;
        for lv in 0..4 {
            let n = neighbours[ti][lv];
            if n < 0 {
                new_neighbours[ni][lv] = -1;
            } else if !conflict[n as usize] {
                new_neighbours[ni][lv] = remap[n as usize];
            }
        }
    }

    for (face_index, (key, _, ti, lv)) in faces.iter().enumerate() {
        let nt = kept + face_index;
        let n = neighbours[*ti][*lv];
        if n < 0 {
            new_neighbours[nt][3] = -1;
            continue;
        }
        let ni = remap[n as usize];
        if ni < 0 {
            return Err("boundary face neighbour is not a kept tet".into());
        }
        new_neighbours[nt][3] = ni;
        let lv2 = (0..4)
            .find(|&j| sorted_face(&tets[n as usize], j) == *key)
            .ok_or_else(|| "boundary face not found on the kept neighbour".to_string())?;
        new_neighbours[ni as usize][lv2] = nt as i32;
    }

    let mut side: HashMap<[usize; 3], (usize, usize)> =
        HashMap::with_capacity(faces.len().saturating_mul(3));
    for face_index in 0..faces.len() {
        let nt = kept + face_index;
        for lv in 0..3 {
            let key = sorted_face(&new_tets[nt], lv);
            if let Some((other_nt, other_lv)) = side.remove(&key) {
                if new_neighbours[nt][lv] != -1 || new_neighbours[other_nt][other_lv] != -1 {
                    return Err("new tet side face is not shared exactly twice".into());
                }
                new_neighbours[nt][lv] = other_nt as i32;
                new_neighbours[other_nt][other_lv] = nt as i32;
            } else {
                side.insert(key, (nt, lv));
            }
        }
    }
    if !side.is_empty() {
        return Err("unmatched new tet side face".into());
    }

    *tets = new_tets;
    *neighbours = new_neighbours;
    *seed_hint = kept;
    Ok(())
}

#[derive(Clone, Copy)]
struct FaceOcc {
    first: (usize, usize),
    second: Option<(usize, usize)>,
    count: usize,
}

/// Order-independent neighbour rebuild using a hash map for the face lookup;
/// the output array is filled by iterating the tets, so determinism does not
/// depend on hash iteration order.
pub(crate) fn build_neighbours_fast(tets: &[[usize; 4]]) -> Result<Vec<[i32; 4]>, String> {
    let mut faces: HashMap<[usize; 3], FaceOcc> =
        HashMap::with_capacity(tets.len().saturating_mul(4));
    for (ti, t) in tets.iter().enumerate() {
        for lv in 0..4 {
            let key = sorted_face(t, lv);
            match faces.entry(key) {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(FaceOcc {
                        first: (ti, lv),
                        second: None,
                        count: 1,
                    });
                }
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    let occ = e.get_mut();
                    occ.count += 1;
                    if occ.count == 2 {
                        occ.second = Some((ti, lv));
                    }
                }
            }
        }
    }

    let mut neighbours = vec![[-1i32; 4]; tets.len()];
    for (ti, t) in tets.iter().enumerate() {
        for lv in 0..4 {
            let key = sorted_face(t, lv);
            let occ = faces
                .get(&key)
                .ok_or_else(|| "internal face map is missing an entry".to_string())?;
            match occ.count {
                1 => {}
                2 => {
                    let other = if occ.first == (ti, lv) {
                        occ.second.expect("second occurrence must exist")
                    } else {
                        occ.first
                    };
                    neighbours[ti][lv] = other.0 as i32;
                }
                n => {
                    return Err(format!(
                        "non-manifold face {key:?} is incident to {n} tetrahedra"
                    ))
                }
            }
        }
    }
    Ok(neighbours)
}

pub(crate) fn canonicalize_mesh(points: &[Point3], tets: Vec<[usize; 4]>) -> Result<Vec<[usize; 4]>, String> {
    let n = points.len();
    let mut out = Vec::with_capacity(tets.len());
    for mut t in tets {
        if t.iter().any(|&v| v >= n) {
            return Err(format!(
                "tet {t:?} references a vertex outside the point set"
            ));
        }
        t.sort_unstable();
        if t[0] == t[1] || t[1] == t[2] || t[2] == t[3] {
            return Err(format!("tet {t:?} repeats a vertex"));
        }
        let s = orient3d_sign(&points[t[0]], &points[t[1]], &points[t[2]], &points[t[3]]);
        if s == 0.0 {
            return Err(format!("degenerate zero-volume tet {t:?}"));
        }
        if s < 0.0 {
            t.swap(2, 3);
        }
        out.push(t);
    }
    out.sort_unstable();
    if let Some(w) = out.windows(2).find(|w| w[0] == w[1]) {
        return Err(format!("duplicate tet {:?}", w[0]));
    }
    Ok(out)
}

pub(crate) fn face_on_box(points: &[Point3], plc_vertices: usize, b: &BoundingBox3, face: [usize; 3]) -> bool {
    if face
        .iter()
        .any(|&v| v < plc_vertices || v >= plc_vertices + BOX_CORNERS)
    {
        return false;
    }
    let mut p = [[0.0; 3]; 3];
    for (i, &v) in face.iter().enumerate() {
        match points[v] {
            Point3::Explicit(x) => p[i] = x,
            _ => return false,
        }
    }
    (0..3).any(|a| {
        let l = b.lo[a];
        let h = b.hi[a];
        (p[0][a] == l && p[1][a] == l && p[2][a] == l)
            || (p[0][a] == h && p[1][a] == h && p[2][a] == h)
    })
}

fn validate_mesh(
    points: &[Point3],
    tets: &[[usize; 4]],
    neighbours: &[[i32; 4]],
    chains: &[SegmentChain],
    edges: &EdgeCounts,
    plc_vertices: usize,
    initial_points: usize,
    box3: &BoundingBox3,
) -> SegmentReport {
    let mut r = SegmentReport {
        subsegments: chains
            .iter()
            .map(|c| c.points.len().saturating_sub(1))
            .sum(),
        tets: tets.len(),
        steiner_points: points.len().saturating_sub(initial_points),
        ..SegmentReport::default()
    };

    for w in tets.windows(2) {
        if w[0] == w[1] {
            r.duplicate_tets += 1;
        }
    }

    let mut used = vec![false; points.len()];
    for t in tets {
        for &v in t {
            used[v] = true;
        }
        let s = orient3d_sign(&points[t[0]], &points[t[1]], &points[t[2]], &points[t[3]]);
        if s <= 0.0 {
            r.orientation_violations += 1;
        }
    }
    r.unused_vertices = used.iter().filter(|u| !**u).count();

    let mut occurrences: BTreeMap<[usize; 3], Vec<(usize, usize)>> = BTreeMap::new();
    for (ti, t) in tets.iter().enumerate() {
        for lv in 0..4 {
            occurrences
                .entry(sorted_face(t, lv))
                .or_default()
                .push((ti, lv));
        }
    }
    for (face, list) in &occurrences {
        match list.len() {
            1 => {
                r.hull_faces += 1;
                if face_on_box(points, plc_vertices, box3, *face) {
                    r.ghost_faces += 1;
                } else {
                    r.hull_faces_off_box += 1;
                }
            }
            2 => {}
            _ => r.nonmanifold_faces += 1,
        }
    }

    for (ti, t) in tets.iter().enumerate() {
        for lv in 0..4 {
            let face = sorted_face(t, lv);
            let n = neighbours[ti][lv];
            if n < 0 {
                if occurrences.get(&face).map(|v| v.len()) != Some(1) {
                    r.inconsistent_neighbours += 1;
                }
                continue;
            }
            let n = n as usize;
            if n >= tets.len() || n == ti {
                r.inconsistent_neighbours += 1;
                continue;
            }
            let Some(lv2) = (0..4).find(|&j| sorted_face(&tets[n], j) == face) else {
                r.inconsistent_neighbours += 1;
                continue;
            };
            if neighbours[n][lv2] != ti as i32 || tets[n][lv2] == t[lv] {
                r.inconsistent_neighbours += 1;
            }

            let (a, b, c, d) = oriented_face_apex(points, t, lv);
            let e = tets[n][lv2];
            let s = insphere_sign(&points[a], &points[b], &points[c], &points[d], &points[e]);
            if s > 0.0 {
                r.empty_sphere_violations += 1;
            } else if s == 0.0 {
                r.cospherical_faces += 1;
            }
        }
    }

    r.missing_after = missing_count(chains, edges);
    r.valid = r.tets > 0
        && r.missing_after == 0
        && r.duplicate_tets == 0
        && r.orientation_violations == 0
        && r.nonmanifold_faces == 0
        && r.inconsistent_neighbours == 0
        && r.empty_sphere_violations == 0
        && r.hull_faces_off_box == 0
        && r.unused_vertices == 0;
    r
}

fn segment_hash(points: &[Point3], tets: &[[usize; 4]], chains: &[SegmentChain]) -> u64 {
    let mut h = FNV_OFFSET;
    let mut step = |x: u64| {
        h ^= x;
        h = h.wrapping_mul(FNV_PRIME);
    };
    step(points.len() as u64);
    for p in points {
        for x in p.canonical_key() {
            step(x);
        }
    }
    step(tets.len() as u64);
    for t in tets {
        for &v in t {
            step(v as u64);
        }
    }
    step(chains.len() as u64);
    for c in chains {
        step(c.orig[0] as u64);
        step(c.orig[1] as u64);
        step(c.points.len() as u64);
        for &(s, id) in &c.points {
            step(s.to_bits());
            step(id as u64);
        }
        for m in &c.meta {
            match *m {
                SegmentCategory::None => step(0),
                SegmentCategory::Acute(w) => {
                    step(1);
                    step(w as u64);
                }
                SegmentCategory::Both => step(2),
            }
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdt3d::testutil::plc_for;
    use std::time::Instant;

    fn opts() -> SegmentOptions {
        SegmentOptions::default()
    }

    fn explicit(p: [f64; 3]) -> Point3 {
        Point3::Explicit(p)
    }

    #[test]
    fn face_apex_is_positively_oriented() {
        let points = vec![
            explicit([0.0, 0.0, 0.0]),
            explicit([1.0, 0.0, 0.0]),
            explicit([0.0, 1.0, 0.0]),
            explicit([0.0, 0.0, -1.0]),
        ];
        let t = [0usize, 1, 2, 3];
        assert!(
            orient3d_sign(&points[0], &points[1], &points[2], &points[3]) > 0.0,
            "fixture tet must be positive"
        );
        for lv in 0..4 {
            let (a, b, c, d) = oriented_face_apex(&points, &t, lv);
            assert!(
                orient3d_sign(&points[a], &points[b], &points[c], &points[d]) > 0.0,
                "face {lv} is not positively oriented with its apex"
            );
        }
    }

    #[test]
    fn category1_both_large_radii_splits_at_the_midpoint() {
        let points = vec![
            explicit([0.0, 0.0, 0.0]),
            explicit([1.0, 0.0, 0.0]),
            explicit([0.5, 0.49, 0.0]),
        ];
        let ratio = ratio_category1(&points, 0, 1, 2).unwrap();
        assert!((ratio - 0.5).abs() < 1e-15, "ratio {ratio}");
    }

    #[test]
    fn category1_uses_the_smallest_sphere() {
        let points = vec![
            explicit([0.0, 0.0, 0.0]),
            explicit([1.0, 0.0, 0.0]),
            explicit([0.1, 0.1, 0.0]),
        ];
        let ratio = ratio_category1(&points, 0, 1, 2).unwrap();
        let r1 = (0.1f64 * 0.1 + 0.1 * 0.1).sqrt();
        assert!((ratio - r1).abs() < 1e-12, "ratio {ratio}, expected {r1}");
    }

    #[test]
    fn category3_splits_at_the_midpoint_without_a_reference() {
        let points = vec![explicit([0.0, 0.0, 0.0])];
        let approx = vec![[0.0, 0.0, 0.0]];
        let choice = choose_split(
            &points,
            &approx,
            0,
            SegmentCategory::Both,
            0,
            1,
            0,
            1,
            0.0,
            1.0,
        )
        .unwrap();
        assert_eq!(choice.s, 0.5);
        assert!(choice.existing.is_none());
    }

    #[test]
    fn acute_vertex_classification_uses_incident_segments() {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.5, 0.8660254037844386, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let plc = crate::cdt3d::plc::Plc {
            verts: verts[..3].to_vec(),
            tris: vec![[0, 1, 2]],
            report: crate::cdt3d::plc::PlcReport::default(),
        };
        let chains = build_chains(&plc).unwrap();
        let acute = acute_vertices(&plc, &chains);
        assert!(acute[0], "60 degree corner must be acute");

        let right = crate::cdt3d::plc::Plc {
            verts: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            tris: vec![[0, 1, 2]],
            report: crate::cdt3d::plc::PlcReport::default(),
        };
        let right_chains = build_chains(&right).unwrap();
        let right_acute = acute_vertices(&right, &right_chains);
        assert!(!right_acute[0], "right angle is not acute");
    }

    #[test]
    fn coplanar_insertion_cavity_expands_across_the_shared_face() {
        let points = vec![
            explicit([0.0, 0.0, 0.0]),
            explicit([1.0, 0.0, 0.0]),
            explicit([0.0, 1.0, 0.0]),
            explicit([0.0, 0.0, -1.0]),
            explicit([0.0, 0.0, 1.0]),
            explicit([0.25, 0.25, 0.0]),
        ];
        let mut tets = canonicalize_mesh(&points[..5], vec![[0, 1, 2, 3], [0, 1, 2, 4]])
            .expect("fixture tets");
        let mut neighbours = build_neighbours_fast(&tets).expect("neighbours");
        let mut conflict = vec![false; tets.len()];
        conflict[0] = true;
        expand_coplanar_cavity(&points, &tets, &neighbours, &mut conflict, 5)
            .expect("coplanar cavity must expand");
        assert!(conflict[1], "the tet across the coplanar face must join the cavity");

        // Drive the full insertion and validate the resulting local mesh.
        let mut edges = EdgeCounts::new(&tets);
        let mut seed_hint = 0usize;
        insert_point(
            &points,
            &mut tets,
            &mut neighbours,
            &mut edges,
            &mut seed_hint,
            5,
        )
        .expect("coplanar insertion must succeed");
        assert!(
            tets.iter().any(|t| t.contains(&5)),
            "the inserted point must be present"
        );
        let rebuilt = build_neighbours_fast(&tets).expect("valid manifold result");
        for (ti, t) in tets.iter().enumerate() {
            assert!(
                orient3d_sign(&points[t[0]], &points[t[1]], &points[t[2]], &points[t[3]]) > 0.0,
                "tet {ti} must be positive"
            );
            for lv in 0..4 {
                let n = rebuilt[ti][lv];
                if n < 0 {
                    continue;
                }
                let n = n as usize;
                let face = crate::cdt3d::dt::sorted_face(t, lv);
                let lv2 = (0..4)
                    .find(|&j| crate::cdt3d::dt::sorted_face(&tets[n], j) == face)
                    .expect("reciprocal face");
                assert_eq!(rebuilt[n][lv2], ti as i32, "reciprocal neighbour");
                let (a, b, c, d) = oriented_face_apex(&points, t, lv);
                let e = tets[n][lv2];
                assert!(
                    insphere_sign(&points[a], &points[b], &points[c], &points[d], &points[e]) <= 0.0,
                    "local Delaunay face must remain empty-sphere"
                );
            }
        }
    }

    #[test]
    fn collinear_vertex_on_a_missing_segment_is_reused() {
        // V lies exactly on the missing segment A--B but is only referenced by
        // the second triangle.  The split scan must find it collinearly and
        // reuse it instead of splitting the segment forever.
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.5, 0.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
        let plc = crate::cdt3d::plc::Plc {
            verts,
            tris: vec![[0, 1, 2], [3, 2, 4]],
            report: crate::cdt3d::plc::PlcReport::default(),
        };
        let mesh = recover_segments(&plc, &SegmentOptions::default())
            .expect("collinear split must terminate");
        assert_eq!(mesh.report.missing_after, 0, "{:?}", mesh.report);
        assert!(mesh.report.reused_points >= 1, "{:?}", mesh.report);
        assert!(mesh.report.valid, "{:?}", mesh.report);
    }

    #[test]
    fn segment_recovery_recovers_all_edges_and_stays_valid() {
        let plc = plc_for("real", "thingi10k_100827.stl");
        assert!(plc.report.plc_valid, "PLC report: {:?}", plc.report);
        let mesh = recover_segments(&plc, &opts()).unwrap();
        assert!(mesh.report.valid, "report: {:?}", mesh.report);
        assert_eq!(mesh.report.missing_after, 0);
        assert_eq!(mesh.report.inconsistent_neighbours, 0);
        assert_eq!(mesh.report.empty_sphere_violations, 0);
        assert_eq!(mesh.report.hull_faces_off_box, 0);
    }

    #[test]
    fn thingi10k_100827_segment_recovery_is_deterministic() {
        let plc = plc_for("real", "thingi10k_100827.stl");
        assert!(plc.report.plc_valid, "PLC report: {:?}", plc.report);
        let mut hashes = Vec::new();
        let mut last = None;
        for _ in 0..5 {
            let mesh = recover_segments(&plc, &opts()).unwrap();
            assert!(mesh.report.valid, "report: {:?}", mesh.report);
            hashes.push(mesh.report.hash);
            last = Some(mesh);
        }
        assert!(
            hashes.windows(2).all(|w| w[0] == w[1]),
            "hashes: {hashes:?}"
        );
        let mesh = last.unwrap();
        println!(
            "100827: segments={} missing_before={} steiner={} splits={} subseg={} tets={} hash={:016x}",
            mesh.report.input_segments,
            mesh.report.missing_before,
            mesh.report.steiner_points,
            mesh.report.splits,
            mesh.report.subsegments,
            mesh.report.tets,
            mesh.report.hash
        );
    }

    #[test]
    #[ignore = "P3 public-corpus gate; run with --release -- --ignored"]
    fn segment_public_corpus_gate() {
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
            let plc = plc_for(kind, name);
            assert!(plc.report.plc_valid, "{name}: PLC report {:?}", plc.report);
            let start = Instant::now();
            let mut hashes = Vec::new();
            let mut last = None;
            for _ in 0..5 {
                let mesh = recover_segments(&plc, &opts()).unwrap();
                assert!(mesh.report.valid, "{name}: report {:?}", mesh.report);
                hashes.push(mesh.report.hash);
                last = Some(mesh);
            }
            let secs = start.elapsed().as_secs_f64() / 5.0;
            assert!(
                hashes.windows(2).all(|w| w[0] == w[1]),
                "{name}: hashes {hashes:?}"
            );
            let mesh = last.unwrap();
            println!(
                "{name}: seg={} miss_before={} steiner={} reused={} splits={} cat1/2/3={}/{}/{} acute={} subseg={} tets={} empty={} off_box={} ngb={} unused={} secs={:.3} hash={:016x}",
                mesh.report.input_segments,
                mesh.report.missing_before,
                mesh.report.steiner_points,
                mesh.report.reused_points,
                mesh.report.splits,
                mesh.report.cat1_segments,
                mesh.report.cat2_segments,
                mesh.report.cat3_segments,
                mesh.report.acute_vertices,
                mesh.report.subsegments,
                mesh.report.tets,
                mesh.report.empty_sphere_violations,
                mesh.report.hull_faces_off_box,
                mesh.report.inconsistent_neighbours,
                mesh.report.unused_vertices,
                secs,
                mesh.report.hash,
            );
        }
    }
}
