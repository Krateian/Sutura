// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! CDT3D Phase B (P4): exact PLC face recovery.
//!
//! A PLC face is missing when at least one tetrahedralization edge properly
//! pierces its interior.  The tetrahedra incident to the piercing edges form
//! one or more face-connected cavities.  Each cavity is split by the face
//! plane, retetrahedralized locally, and verified with exact predicates.
//!
//! This module is opt-in and independent of the default Splint path.

use crate::cdt3d::plc::Plc;
use crate::cdt3d::segment::SegmentMesh;
use crate::point::Point3;
use crate::predicates3d::orient3d_sign;
use std::collections::{BTreeMap, BTreeSet};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Options for face recovery.
#[derive(Clone, Copy, Debug)]
pub struct FaceOptions {
    /// Maximum cavity-expansion rounds before a typed failure.
    pub max_expansions: usize,
    /// Test-only: bypass the local-Delaunay fast path so the gift-wrapping
    /// fallback is exercised on real cavities.  Never exposed through CLI/GUI.
    pub force_gift: bool,
}

impl Default for FaceOptions {
    fn default() -> Self {
        Self {
            max_expansions: 64,
            force_gift: false,
        }
    }
}

/// Validity report for one face-recovery run.
#[derive(Clone, Debug, Default)]
pub struct FaceReport {
    pub input_plc_triangles: usize,
    pub recovered_triangles: usize,
    pub missing_before: usize,
    pub missing_after: usize,
    pub cavities: usize,
    pub retetrahedralized: usize,
    pub expansions: usize,
    pub gift_wraps: usize,
    pub passes: usize,
    pub steiner_points: usize,
    pub tets: usize,
    pub orientation_violations: usize,
    pub duplicate_tets: usize,
    pub nonmanifold_faces: usize,
    pub inconsistent_neighbours: usize,
    pub empty_sphere_violations: usize,
    pub constraint_empty_sphere_faces: usize,
    pub cospherical_faces: usize,
    pub hull_faces_off_box: usize,
    pub unused_vertices: usize,
    pub coverage_checked: usize,
    pub coverage_failed: usize,
    pub coverage_certified: bool,
    pub valid: bool,
    pub hash: u64,
}

/// Recovered tetrahedralization after the face stage.
#[derive(Clone, Debug)]
pub struct FaceMesh {
    pub points: Vec<Point3>,
    pub plc_vertices: usize,
    pub box3: crate::cdt3d::dt::BoundingBox3,
    pub tets: Vec<[usize; 4]>,
    pub neighbours: Vec<[i32; 4]>,
    pub report: FaceReport,
}

/// Recover every PLC face as a union of tetrahedralization faces.
pub fn recover_faces(
    seg: &SegmentMesh,
    plc: &Plc,
    opts: &FaceOptions,
) -> Result<FaceMesh, String> {
    if plc.tris.is_empty() {
        return Err("face recovery needs a non-empty PLC".into());
    }
    let points = seg.points.clone();
    let mut tets = seg.tets.clone();
    let mut neighbours = seg.neighbours.clone();
    let missing = detect_missing_faces(&points, &tets, plc)?;
    let missing_before = missing.iter().filter(|m| **m).count();

    let mut cavities = 0usize;
    let mut retetrahedralized = 0usize;
    let mut expansions = 0usize;
    let mut gift_wraps = 0usize;
    let mut passes = 0usize;
    let mut hard_failed = BTreeSet::new();
    let max_rounds = plc.tris.len().saturating_mul(8).max(64);

    for _ in 0..max_rounds {
        let missing = detect_missing_faces(&points, &tets, plc)?;
        let targets: Vec<usize> = missing
            .iter()
            .enumerate()
            .filter(|(i, m)| **m && !hard_failed.contains(i))
            .map(|(i, _)| i)
            .collect();
        if targets.is_empty() {
            break;
        }
        let mut progress = false;
        for fi in targets {
            let face = plc.tris[fi];
            let comps = collect_cavities(&points, &tets, &neighbours, face)?;
            if comps.is_empty() {
                hard_failed.insert(fi);
                continue;
            }
            let mut fixed = false;
            for comp in &comps {
                cavities += 1;
                match retetra_cavity(&points, &tets, &neighbours, comp, face, opts) {
                    Ok((new_tets, used_gift)) => {
                        if used_gift {
                            gift_wraps += 1;
                        }
                        let mut merged = Vec::with_capacity(tets.len());
                        let in_comp: BTreeSet<usize> = comp.iter().copied().collect();
                        for (ti, t) in tets.iter().enumerate() {
                            if !in_comp.contains(&ti) {
                                merged.push(*t);
                            }
                        }
                        merged.extend(new_tets);
                        let canon = crate::cdt3d::segment::canonicalize_mesh(&points, merged)?;
                        let nb = crate::cdt3d::segment::build_neighbours_fast(&canon)?;
                        tets = canon;
                        neighbours = nb;
                        retetrahedralized += 1;
                        passes += 1;
                        progress = true;
                        fixed = true;
                        break;
                    }
                    Err(e) => {
                        if std::env::var_os("CDT3D_DEBUG_FACE").is_some() {
                            eprintln!("face {fi} cavity fail: {e}");
                        }
                        continue;
                    }
                }
            }
            if !fixed {
                hard_failed.insert(fi);
            }
            if progress {
                break;
            }
        }
        if !progress {
            break;
        }
    }

    let missing_after_vec = detect_missing_faces(&points, &tets, plc)?;
    let missing_after = missing_after_vec.iter().filter(|m| **m).count();
    let mut report =
        validate_face_mesh(&points, &tets, &neighbours, plc, seg.box3, missing_after);
    report.input_plc_triangles = plc.tris.len();
    report.recovered_triangles = plc.tris.len() - missing_after;
    report.missing_before = missing_before;
    report.cavities = cavities;
    report.retetrahedralized = retetrahedralized;
    report.expansions = expansions;
    report.gift_wraps = gift_wraps;
    report.passes = passes;
    report.steiner_points = points.len().saturating_sub(seg.plc_vertices + 8);
    let coverage = crate::cdt3d::certificate::certify_face_coverage(&points, &tets, plc)?;
    report.coverage_checked = coverage.plc_triangles;
    report.coverage_failed = coverage.failed_triangles;
    report.coverage_certified = coverage.certified;
    if !coverage.certified {
        report.valid = false;
    }
    report.hash = face_hash(&points, &tets);
    Ok(FaceMesh {
        points,
        plc_vertices: seg.plc_vertices,
        box3: seg.box3,
        tets,
        neighbours,
        report,
    })
}

struct BoundaryFace {
    oriented: [usize; 3],
    opp_tet: usize,
    cav_apex: usize,
}

/// Face-connected cavities among the tetrahedra incident to piercing edges.
fn collect_cavities(
    points: &[Point3],
    tets: &[[usize; 4]],
    neighbours: &[[i32; 4]],
    face: [usize; 3],
) -> Result<Vec<Vec<usize>>, String> {
    let f = [&points[face[0]], &points[face[1]], &points[face[2]]];
    let mut in_tf = vec![false; tets.len()];
    for (ti, t) in tets.iter().enumerate() {
        'edges: for i in 0..4 {
            for j in (i + 1)..4 {
                if edge_pierces_triangle(&points[t[i]], &points[t[j]], f[0], f[1], f[2]) {
                    in_tf[ti] = true;
                    break 'edges;
                }
            }
        }
    }

    let mut seen = vec![false; tets.len()];
    let mut comps = Vec::new();
    for start in 0..tets.len() {
        if !in_tf[start] || seen[start] {
            continue;
        }
        let mut stack = vec![start];
        seen[start] = true;
        let mut comp = Vec::new();
        while let Some(ti) = stack.pop() {
            comp.push(ti);
            for &n in &neighbours[ti] {
                if n >= 0 {
                    let n = n as usize;
                    if in_tf[n] && !seen[n] {
                        seen[n] = true;
                        stack.push(n);
                    }
                }
            }
        }
        comp.sort_unstable();
        comps.push(comp);
    }
    Ok(comps)
}

fn retetra_cavity(
    points: &[Point3],
    tets: &[[usize; 4]],
    neighbours: &[[i32; 4]],
    cavity: &[usize],
    face: [usize; 3],
    opts: &FaceOptions,
) -> Result<(Vec<[usize; 4]>, bool), String> {
    let in_cavity: BTreeSet<usize> = cavity.iter().copied().collect();
    let mut boundary: BTreeMap<[usize; 3], BoundaryFace> = BTreeMap::new();
    let mut verts = BTreeSet::new();
    for &ti in cavity {
        let t = tets[ti];
        for &v in &t {
            verts.insert(v);
        }
        for lv in 0..4 {
            let n = neighbours[ti][lv];
            if n < 0 {
                return Err("cavity touches the synthetic box".into());
            }
            if in_cavity.contains(&(n as usize)) {
                continue;
            }
            let key = crate::cdt3d::dt::sorted_face(&t, lv);
            let (a, b, c, d) = crate::cdt3d::segment::oriented_face_apex(points, &t, lv);
            if boundary
                .insert(
                    key,
                    BoundaryFace {
                        oriented: [a, b, c],
                        opp_tet: n as usize,
                        cav_apex: d,
                    },
                )
                .is_some()
            {
                return Err("duplicate cavity boundary face".into());
            }
        }
    }

    let p = [
        Point3::Explicit(points[face[0]].to_f64().unwrap()),
        Point3::Explicit(points[face[1]].to_f64().unwrap()),
        Point3::Explicit(points[face[2]].to_f64().unwrap()),
    ];
    let mut sign: BTreeMap<usize, f64> = BTreeMap::new();
    for &v in &verts {
        sign.insert(v, orient3d_sign(&p[0], &p[1], &p[2], &points[v]));
    }

    let mut crossing = 0usize;
    for bf in boundary.values() {
        let s = [
            sign[&bf.oriented[0]],
            sign[&bf.oriented[1]],
            sign[&bf.oriented[2]],
        ];
        let any_above = s.iter().any(|x| *x > 0.0);
        let any_below = s.iter().any(|x| *x < 0.0);
        if any_above && any_below {
            crossing += 1;
            if std::env::var_os("CDT3D_DEBUG_FACE").is_some() {
                eprintln!(
                    "crossing boundary face {:?} signs {:?} face {:?}",
                    bf.oriented, s, face
                );
            }
        }
    }
    if crossing > 0 && std::env::var_os("CDT3D_DEBUG_FACE").is_some() {
        eprintln!(
            "cavity boundary crosses the face plane ({crossing} faces); routing to gift-wrapping"
        );
    }

    let v1: Vec<usize> = verts.iter().copied().filter(|v| sign[v] >= 0.0).collect();
    let v2: Vec<usize> = verts.iter().copied().filter(|v| sign[v] <= 0.0).collect();

    if !opts.force_gift && crossing == 0 {
        if let (Ok(d1), Ok(d2)) = (
            half_delaunay(points, tets, &in_cavity, &v1),
            half_delaunay(points, tets, &in_cavity, &v2),
        ) {
            let mut fast = d1;
            fast.extend(d2);
            if !fast.is_empty()
                && verify_replacement(points, tets, cavity, &boundary, &fast, face).is_ok()
            {
                return Ok((fast, false));
            }
        }
    }

    let mut c1_boundary = Vec::new();
    let mut c2_boundary = Vec::new();
    let mut original_keys = BTreeSet::new();
    for (key, bf) in &boundary {
        original_keys.insert(*key);
        let s = [
            sign[&bf.oriented[0]],
            sign[&bf.oriented[1]],
            sign[&bf.oriented[2]],
        ];
        let any_above = s.iter().any(|x| *x > 0.0);
        let any_below = s.iter().any(|x| *x < 0.0);
        let cav_side = sign[&bf.cav_apex];
        let mixed = any_above && any_below;
        let assign_c1 = if mixed {
            cav_side >= 0.0
        } else if !any_below {
            if any_above {
                true
            } else {
                cav_side > 0.0
            }
        } else {
            false
        };
        let assign_c2 = if mixed {
            cav_side < 0.0
        } else if !any_above {
            if any_below {
                true
            } else {
                cav_side < 0.0
            }
        } else {
            false
        };
        if assign_c1 {
            c1_boundary.push(bf.oriented);
        }
        if assign_c2 {
            c2_boundary.push(bf.oriented);
        }
        if !assign_c1 && !assign_c2 {
            return Err("cavity boundary face has no clear side".into());
        }
    }

    if std::env::var_os("CDT3D_DEBUG_FACE").is_some() {
        eprintln!(
            "gift face {:?}: v1={} v2={} c1b={} c2b={}",
            face,
            v1.len(),
            v2.len(),
            c1_boundary.len(),
            c2_boundary.len()
        );
    }
    let (tets1, interface) = gift_fill(
        points,
        &v1,
        &c1_boundary,
        &c1_boundary,
        face,
        true,
        &original_keys,
    )?;
    if std::env::var_os("CDT3D_DEBUG_FACE").is_some() {
        eprintln!(
            "gift face {:?}: tets1={} interface={}",
            face,
            tets1.len(),
            interface.len()
        );
    }
    if tets1.is_empty() {
        return Err("gift-wrapping filled no C1 tetrahedra".into());
    }
    if let Some(bad) = interface
        .iter()
        .find(|f| !tri_on_plane(points, **f, face))
    {
        return Err(format!(
            "gift-wrapping left a non-plane C1 boundary face {bad:?}"
        ));
    }

    let mut c2_all = c2_boundary;
    for f in &interface {
        c2_all.push([f[0], f[2], f[1]]);
    }
    let (tets2, leftover) = gift_fill(
        points,
        &v2,
        &c2_all,
        &c2_all,
        face,
        false,
        &BTreeSet::new(),
    )?;
    if !leftover.is_empty() {
        return Err(format!(
            "gift-wrapping left {} C2 boundary faces",
            leftover.len()
        ));
    }

    let mut new_tets = tets1;
    new_tets.extend(tets2);
    if new_tets.is_empty() {
        return Err("gift-wrapping produced no cavity tetrahedra".into());
    }
    if std::env::var_os("CDT3D_DEBUG_FACE").is_some() {
        eprintln!(
            "gift face {:?}: tets={} verify={:?}",
            face,
            new_tets.len(),
            verify_replacement(points, tets, cavity, &boundary, &new_tets, face)
        );
    }
    verify_replacement(points, tets, cavity, &boundary, &new_tets, face)?;
    Ok((new_tets, true))
}

fn half_delaunay(
    points: &[Point3],
    removed: &[[usize; 4]],
    in_cavity: &BTreeSet<usize>,
    vids: &[usize],
) -> Result<Vec<[usize; 4]>, String> {
    if vids.len() < 4 {
        return Ok(Vec::new());
    }
    let approx: Vec<[f64; 3]> = vids
        .iter()
        .map(|&v| {
            points[v]
                .to_f64()
                .ok_or_else(|| "point has no approximate coordinates".to_string())
        })
        .collect::<Result<_, _>>()?;
    let raw = match crate::splint::delaunay::delaunay3(&approx) {
        Ok(r) => r,
        Err(_) => return Ok(Vec::new()),
    };
    let removed_list: Vec<usize> = in_cavity.iter().copied().collect();
    let mut out = Vec::new();
    for t in raw {
        let g = [vids[t[0]], vids[t[1]], vids[t[2]], vids[t[3]]];
        let s = orient3d_sign(&points[g[0]], &points[g[1]], &points[g[2]], &points[g[3]]);
        if s == 0.0 {
            continue;
        }
        let mut g = g;
        if s < 0.0 {
            g.swap(2, 3);
        }
        let c = [
            (approx[t[0]][0] + approx[t[1]][0] + approx[t[2]][0] + approx[t[3]][0]) * 0.25,
            (approx[t[0]][1] + approx[t[1]][1] + approx[t[2]][1] + approx[t[3]][1]) * 0.25,
            (approx[t[0]][2] + approx[t[1]][2] + approx[t[2]][2] + approx[t[3]][2]) * 0.25,
        ];
        if removed_list
            .iter()
            .any(|&ti| point_in_tet(points, &removed[ti], c))
        {
            out.push(g);
        }
    }
    Ok(out)
}

fn point_in_tet(points: &[Point3], t: &[usize; 4], c: [f64; 3]) -> bool {
    let q = Point3::Explicit(c);
    for lv in 0..4 {
        let (a, b, d, _) = crate::cdt3d::segment::oriented_face_apex(points, t, lv);
        if orient3d_sign(&points[a], &points[b], &points[d], &q) < 0.0 {
            return false;
        }
    }
    true
}

fn cyclic_eq(x: [usize; 3], y: [usize; 3]) -> bool {
    (x[0] == y[0] && x[1] == y[1] && x[2] == y[2])
        || (x[0] == y[1] && x[1] == y[2] && x[2] == y[0])
        || (x[0] == y[2] && x[1] == y[0] && x[2] == y[1])
}

fn verify_replacement(
    points: &[Point3],
    tets: &[[usize; 4]],
    cavity: &[usize],
    boundary: &BTreeMap<[usize; 3], BoundaryFace>,
    new_tets: &[[usize; 4]],
    face: [usize; 3],
) -> Result<(), String> {
    let mut occ: BTreeMap<[usize; 3], Vec<(usize, usize)>> = BTreeMap::new();
    for (ti, t) in new_tets.iter().enumerate() {
        for lv in 0..4 {
            occ.entry(crate::cdt3d::dt::sorted_face(t, lv))
                .or_default()
                .push((ti, lv));
        }
    }
    for (key, list) in &occ {
        match list.len() {
            1 => {
                let bf = boundary
                    .get(key)
                    .ok_or_else(|| format!("new cavity has extra boundary face {key:?}"))?;
                let (ti, lv) = list[0];
                let (a, b, c, _) =
                    crate::cdt3d::segment::oriented_face_apex(points, &new_tets[ti], lv);
                if !cyclic_eq([a, b, c], bf.oriented) {
                    return Err("new boundary face has the wrong orientation".into());
                }
                let _ = bf.opp_tet;
            }
            2 => {}
            n => return Err(format!("new cavity face {key:?} has {n} incidences")),
        }
    }
    for key in boundary.keys() {
        if !occ.contains_key(key) {
            return Err(format!("cavity boundary face {key:?} was not recovered"));
        }
    }

    let f = [&points[face[0]], &points[face[1]], &points[face[2]]];
    for t in new_tets {
        for i in 0..4 {
            for j in (i + 1)..4 {
                if edge_pierces_triangle(&points[t[i]], &points[t[j]], f[0], f[1], f[2]) {
                    return Err("recovered cavity still pierces the face".into());
                }
            }
        }
    }

    let old_vol: f64 = cavity
        .iter()
        .map(|&ti| tet_volume6(points, &tets[ti]).abs())
        .sum();
    let new_vol: f64 = new_tets
        .iter()
        .map(|t| tet_volume6(points, t).abs())
        .sum();
    let scale = old_vol.abs().max(1.0);
    if (old_vol - new_vol).abs() > 1e-6 * scale {
        return Err(format!(
            "cavity volume changed ({old_vol} -> {new_vol})"
        ));
    }
    Ok(())
}

fn tet_volume6(points: &[Point3], t: &[usize; 4]) -> f64 {
    let a = points[t[0]].to_f64().unwrap();
    let b = points[t[1]].to_f64().unwrap();
    let c = points[t[2]].to_f64().unwrap();
    let d = points[t[3]].to_f64().unwrap();
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let ad = [d[0] - a[0], d[1] - a[1], d[2] - a[2]];
    let cross = [
        ac[1] * ad[2] - ac[2] * ad[1],
        ac[2] * ad[0] - ac[0] * ad[2],
        ac[0] * ad[1] - ac[1] * ad[0],
    ];
    ab[0] * cross[0] + ab[1] * cross[1] + ab[2] * cross[2]
}

fn point_in_triangle_projected(p: [f64; 3], t: [[f64; 3]; 3]) -> bool {
    let ab = [t[1][0] - t[0][0], t[1][1] - t[0][1], t[1][2] - t[0][2]];
    let ac = [t[2][0] - t[0][0], t[2][1] - t[0][1], t[2][2] - t[0][2]];
    let n = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    let axis = (0..3)
        .max_by(|&i, &j| n[i].abs().partial_cmp(&n[j].abs()).unwrap())
        .unwrap();
    let (i, j) = match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    let cross = |a: [f64; 3], b: [f64; 3], c: [f64; 3]| {
        (b[i] - a[i]) * (c[j] - a[j]) - (b[j] - a[j]) * (c[i] - a[i])
    };
    let d0 = cross(t[0], t[1], p);
    let d1 = cross(t[1], t[2], p);
    let d2 = cross(t[2], t[0], p);
    let eps = 1e-7;
    (d0 >= -eps && d1 >= -eps && d2 >= -eps) || (d0 <= eps && d1 <= eps && d2 <= eps)
}

fn face_is_plc_piece(points: &[Point3], plc: &Plc, face: [usize; 3]) -> bool {
    let mut p = [[0.0f64; 3]; 3];
    for (i, &v) in face.iter().enumerate() {
        match points[v].to_f64() {
            Some(x) => p[i] = x,
            None => return false,
        }
    }
    'tri: for tri in &plc.tris {
        let a = &points[tri[0]];
        let b = &points[tri[1]];
        let c = &points[tri[2]];
        for &v in &face {
            if orient3d_sign(a, b, c, &points[v]) != 0.0 {
                continue 'tri;
            }
        }
        let t = [plc.verts[tri[0]], plc.verts[tri[1]], plc.verts[tri[2]]];
        if p.iter().all(|&q| point_in_triangle_projected(q, t)) {
            return true;
        }
    }
    false
}

fn validate_face_mesh(
    points: &[Point3],
    tets: &[[usize; 4]],
    neighbours: &[[i32; 4]],
    plc: &Plc,
    box3: crate::cdt3d::dt::BoundingBox3,
    missing_after: usize,
) -> FaceReport {
    let plc_vertices = plc.verts.len();
    let mut r = FaceReport {
        tets: tets.len(),
        missing_after,
        ..FaceReport::default()
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
                .entry(crate::cdt3d::dt::sorted_face(t, lv))
                .or_default()
                .push((ti, lv));
        }
    }
    for (face, list) in &occurrences {
        match list.len() {
            1 => {
                if !crate::cdt3d::segment::face_on_box(points, plc_vertices, &box3, *face) {
                    r.hull_faces_off_box += 1;
                }
            }
            2 => {}
            _ => r.nonmanifold_faces += 1,
        }
    }
    for (ti, t) in tets.iter().enumerate() {
        for lv in 0..4 {
            let face = crate::cdt3d::dt::sorted_face(t, lv);
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
            let Some(lv2) = (0..4).find(|&j| crate::cdt3d::dt::sorted_face(&tets[n], j) == face)
            else {
                r.inconsistent_neighbours += 1;
                continue;
            };
            if neighbours[n][lv2] != ti as i32 || tets[n][lv2] == t[lv] {
                r.inconsistent_neighbours += 1;
            }
            let (a, b, c, d) = crate::cdt3d::segment::oriented_face_apex(points, t, lv);
            let e = tets[n][lv2];
            let s = crate::predicates3d::insphere_sign(
                &points[a],
                &points[b],
                &points[c],
                &points[d],
                &points[e],
            );
            if s > 0.0 {
                if face_is_plc_piece(points, plc, face) {
                    r.constraint_empty_sphere_faces += 1;
                } else {
                    r.empty_sphere_violations += 1;
                }
            } else if s == 0.0 {
                r.cospherical_faces += 1;
            }
        }
    }
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

#[derive(Clone, Copy)]
struct Grid {
    origin: [f64; 3],
    inv: [f64; 3],
    res: [i64; 3],
}

impl Grid {
    fn new(lo: [f64; 3], hi: [f64; 3], faces: usize) -> Self {
        let target = (faces.max(1) as f64).cbrt().ceil().clamp(1.0, 128.0) as i64;
        let mut res = [target; 3];
        let mut inv = [0.0f64; 3];
        for a in 0..3 {
            let extent = hi[a] - lo[a];
            if extent > 0.0 && extent.is_finite() {
                inv[a] = target as f64 / extent;
            } else {
                res[a] = 1;
                inv[a] = 0.0;
            }
        }
        Self { origin: lo, inv, res }
    }

    fn cell(&self, p: [f64; 3], a: usize) -> i64 {
        if self.inv[a] == 0.0 {
            0
        } else {
            let c = ((p[a] - self.origin[a]) * self.inv[a]).floor() as i64;
            c.clamp(0, self.res[a] - 1)
        }
    }

    fn range(&self, lo: [f64; 3], hi: [f64; 3], a: usize) -> (i64, i64) {
        (self.cell(lo, a), self.cell(hi, a))
    }

    fn cells(&self, lo: [f64; 3], hi: [f64; 3]) -> Vec<[i64; 3]> {
        let (x0, x1) = self.range(lo, hi, 0);
        let (y0, y1) = self.range(lo, hi, 1);
        let (z0, z1) = self.range(lo, hi, 2);
        let mut out = Vec::new();
        for z in z0..=z1 {
            for y in y0..=y1 {
                for x in x0..=x1 {
                    out.push([x, y, z]);
                }
            }
        }
        out
    }
}

fn aabb(points: &[[f64; 3]], ids: &[usize]) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for &i in ids {
        for a in 0..3 {
            lo[a] = lo[a].min(points[i][a]);
            hi[a] = hi[a].max(points[i][a]);
        }
    }
    (lo, hi)
}

/// Mark every PLC triangle that is pierced by at least one tet edge.
pub fn detect_missing_faces(
    points: &[Point3],
    tets: &[[usize; 4]],
    plc: &Plc,
) -> Result<Vec<bool>, String> {
    let approx: Vec<[f64; 3]> = points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            p.to_f64()
                .ok_or_else(|| format!("point {i} has no approximate coordinates"))
        })
        .collect::<Result<_, _>>()?;
    let nf = plc.tris.len();
    let mut missing = vec![false; nf];
    if nf == 0 || tets.is_empty() {
        return Ok(missing);
    }
    for f in plc.tris.iter() {
        if f.iter().any(|&v| v >= approx.len()) {
            return Err("PLC triangle index out of range".into());
        }
    }

    let (lo, hi) = aabb(&approx, &(0..plc.verts.len()).collect::<Vec<_>>());
    let grid = Grid::new(lo, hi, nf);
    let mut buckets: BTreeMap<[i64; 3], Vec<usize>> = BTreeMap::new();
    for (fi, f) in plc.tris.iter().enumerate() {
        let (flo, fhi) = aabb(&approx, f);
        for c in grid.cells(flo, fhi) {
            buckets.entry(c).or_default().push(fi);
        }
    }

    let mut edges: BTreeSet<[usize; 2]> = BTreeSet::new();
    for t in tets {
        for i in 0..4 {
            for j in (i + 1)..4 {
                edges.insert(if t[i] < t[j] {
                    [t[i], t[j]]
                } else {
                    [t[j], t[i]]
                });
            }
        }
    }

    let mut stamp = vec![0u32; nf];
    let mut epoch = 0u32;
    let mut candidates = Vec::new();
    for e in edges {
        let (elo, ehi) = aabb(&approx, &e);
        epoch = epoch.wrapping_add(1);
        candidates.clear();
        for c in grid.cells(elo, ehi) {
            if let Some(list) = buckets.get(&c) {
                for &fi in list {
                    if stamp[fi] != epoch {
                        stamp[fi] = epoch;
                        candidates.push(fi);
                    }
                }
            }
        }
        candidates.sort_unstable();
        for &fi in &candidates {
            if missing[fi] {
                continue;
            }
            let f = plc.tris[fi];
            let p0 = Point3::Explicit(plc.verts[f[0]]);
            let p1 = Point3::Explicit(plc.verts[f[1]]);
            let p2 = Point3::Explicit(plc.verts[f[2]]);
            if edge_pierces_triangle(&points[e[0]], &points[e[1]], &p0, &p1, &p2) {
                missing[fi] = true;
            }
        }
    }
    Ok(missing)
}

/// True when the open segment `a--b` crosses the interior of triangle
/// `(p0,p1,p2)` and the two are not coplanar.  Exact `orient3d` only.
pub(crate) fn edge_pierces_triangle(
    a: &Point3,
    b: &Point3,
    p0: &Point3,
    p1: &Point3,
    p2: &Point3,
) -> bool {
    let sa = orient3d_sign(p0, p1, p2, a);
    let sb = orient3d_sign(p0, p1, p2, b);
    if sa == 0.0 || sb == 0.0 || (sa > 0.0) == (sb > 0.0) {
        return false;
    }
    let d0 = orient3d_sign(p0, p1, a, b);
    let d1 = orient3d_sign(p1, p2, a, b);
    let d2 = orient3d_sign(p2, p0, a, b);
    (d0 > 0.0 && d1 > 0.0 && d2 > 0.0) || (d0 < 0.0 && d1 < 0.0 && d2 < 0.0)
}

fn sorted_tri(f: [usize; 3]) -> [usize; 3] {
    let mut k = f;
    k.sort_unstable();
    k
}

fn tri_on_plane(points: &[Point3], tri: [usize; 3], plane: [usize; 3]) -> bool {
    tri.iter().all(|&v| {
        orient3d_sign(
            &points[plane[0]],
            &points[plane[1]],
            &points[plane[2]],
            &points[v],
        ) == 0.0
    })
}

fn point_in_tet_inclusive(points: &[Point3], t: &[usize; 4], v: usize) -> bool {
    for lv in 0..4 {
        let (a, b, c, _) = crate::cdt3d::segment::oriented_face_apex(points, t, lv);
        if orient3d_sign(&points[a], &points[b], &points[c], &points[v]) < 0.0 {
            return false;
        }
    }
    true
}

fn point_in_tet_strict(points: &[Point3], t: &[usize; 4], v: usize) -> bool {
    for lv in 0..4 {
        let (a, b, c, _) = crate::cdt3d::segment::oriented_face_apex(points, t, lv);
        if orient3d_sign(&points[a], &points[b], &points[c], &points[v]) <= 0.0 {
            return false;
        }
    }
    true
}

fn tet_triangle_interiors_intersect(points: &[Point3], t: &[usize; 4], tri: [usize; 3]) -> bool {
    if tri.iter().any(|&v| point_in_tet_strict(points, t, v)) {
        return true;
    }
    for i in 0..4 {
        for j in (i + 1)..4 {
            if edge_pierces_triangle(
                &points[t[i]],
                &points[t[j]],
                &points[tri[0]],
                &points[tri[1]],
                &points[tri[2]],
            ) {
                return true;
            }
        }
    }
    for k in 0..3 {
        let s0 = tri[k];
        let s1 = tri[(k + 1) % 3];
        for lv in 0..4 {
            let (a, b, c, _) = crate::cdt3d::segment::oriented_face_apex(points, t, lv);
            if edge_pierces_triangle(
                &points[s0],
                &points[s1],
                &points[a],
                &points[b],
                &points[c],
            ) {
                return true;
            }
        }
    }
    false
}

fn triangles_intersect_as_subsimplex(points: &[Point3], t: &[usize; 4], tau: [usize; 3]) -> bool {
    let shared: Vec<usize> = tau.iter().copied().filter(|&v| t.contains(&v)).collect();
    match shared.len() {
        3 => true,
        2 => {
            let un_tau = *tau.iter().find(|v| !shared.contains(v)).unwrap();
            for (pos, &orig) in t.iter().enumerate() {
                if shared.contains(&orig) {
                    continue;
                }
                let mut repl = *t;
                repl[pos] = un_tau;
                if orient3d_sign(
                    &points[repl[0]],
                    &points[repl[1]],
                    &points[repl[2]],
                    &points[repl[3]],
                ) < 0.0
                {
                    return true;
                }
            }
            false
        }
        1 => {
            let common = shared[0];
            let other: Vec<usize> = tau.iter().copied().filter(|&v| v != common).collect();
            if point_in_tet_inclusive(points, t, other[0])
                || point_in_tet_inclusive(points, t, other[1])
            {
                return false;
            }
            let tau_edges = [
                (common, other[0]),
                (common, other[1]),
                (other[0], other[1]),
            ];
            for (s0, s1) in tau_edges {
                for lv in 0..4 {
                    let (a, b, c, _) =
                        crate::cdt3d::segment::oriented_face_apex(points, t, lv);
                    if edge_pierces_triangle(
                        &points[s0],
                        &points[s1],
                        &points[a],
                        &points[b],
                        &points[c],
                    ) {
                        return false;
                    }
                }
            }
            for i in 0..4 {
                for j in (i + 1)..4 {
                    if edge_pierces_triangle(
                        &points[t[i]],
                        &points[t[j]],
                        &points[tau[0]],
                        &points[tau[1]],
                        &points[tau[2]],
                    ) {
                        return false;
                    }
                }
            }
            true
        }
        0 => {
            if tau.iter().any(|&v| point_in_tet_inclusive(points, t, v)) {
                return false;
            }
            for k in 0..3 {
                let s0 = tau[k];
                let s1 = tau[(k + 1) % 3];
                for lv in 0..4 {
                    let (a, b, c, _) =
                        crate::cdt3d::segment::oriented_face_apex(points, t, lv);
                    if edge_pierces_triangle(
                        &points[s0],
                        &points[s1],
                        &points[a],
                        &points[b],
                        &points[c],
                    ) {
                        return false;
                    }
                }
            }
            for i in 0..4 {
                for j in (i + 1)..4 {
                    if edge_pierces_triangle(
                        &points[t[i]],
                        &points[t[j]],
                        &points[tau[0]],
                        &points[tau[1]],
                        &points[tau[2]],
                    ) {
                        return false;
                    }
                }
            }
            true
        }
        _ => unreachable!(),
    }
}

fn convex_hull_tets(points: &[Point3], t: &[usize; 4], v: usize) -> Vec<[usize; 4]> {
    let mut out = vec![*t];
    if point_in_tet_inclusive(points, t, v) {
        return out;
    }
    for lv in 0..4 {
        let (a, b, c, _) = crate::cdt3d::segment::oriented_face_apex(points, t, lv);
        if orient3d_sign(&points[a], &points[b], &points[c], &points[v]) < 0.0 {
            let mut h = [a, b, c, v];
            if orient3d_sign(&points[h[0]], &points[h[1]], &points[h[2]], &points[h[3]]) < 0.0 {
                h.swap(1, 2);
            }
            out.push(h);
        }
    }
    out
}

fn hull_boundary_faces(points: &[Point3], t: &[usize; 4], v: usize) -> BTreeSet<[usize; 3]> {
    if point_in_tet_inclusive(points, t, v) {
        return (0..4)
            .map(|lv| crate::cdt3d::dt::sorted_face(t, lv))
            .collect();
    }
    let mut pts = t.to_vec();
    if !pts.contains(&v) {
        pts.push(v);
    }
    let mut faces = BTreeSet::new();
    for a in 0..pts.len() {
        for b in (a + 1)..pts.len() {
            for c in (b + 1)..pts.len() {
                let tri = [pts[a], pts[b], pts[c]];
                let mut pos = 0usize;
                let mut neg = 0usize;
                for &q in &pts {
                    if tri.contains(&q) {
                        continue;
                    }
                    let s = orient3d_sign(
                        &points[tri[0]],
                        &points[tri[1]],
                        &points[tri[2]],
                        &points[q],
                    );
                    if s > 0.0 {
                        pos += 1;
                    } else if s < 0.0 {
                        neg += 1;
                    }
                }
                if pos > 0 && neg > 0 {
                    continue;
                }
                if pos == 0 && neg == 0 {
                    continue;
                }
                faces.insert(sorted_tri(tri));
            }
        }
    }
    faces
}

fn vertex_visible_from_tet(
    points: &[Point3],
    t: &[usize; 4],
    v: usize,
    boundary: &[[usize; 3]],
) -> bool {
    let hull = convex_hull_tets(points, t, v);
    let hull_faces = hull_boundary_faces(points, t, v);
    for tau in boundary {
        let tau_key = sorted_tri(*tau);
        for h in &hull {
            let is_face = (0..4).any(|lv| crate::cdt3d::dt::sorted_face(h, lv) == tau_key);
            if is_face && !hull_faces.contains(&tau_key) {
                return false;
            }
            if tet_triangle_interiors_intersect(points, h, *tau) {
                return false;
            }
        }
    }
    true
}

fn gift_fill(
    points: &[Point3],
    vids: &[usize],
    boundary: &[[usize; 3]],
    initial_boundary: &[[usize; 3]],
    plane: [usize; 3],
    skip_new_plane: bool,
    original_keys: &BTreeSet<[usize; 3]>,
) -> Result<(Vec<[usize; 4]>, Vec<[usize; 3]>), String> {
    let mut front: BTreeMap<[usize; 3], [usize; 3]> = BTreeMap::new();
    let mut pending: BTreeSet<[usize; 3]> = BTreeSet::new();
    for &f in boundary {
        let key = sorted_tri(f);
        if front.insert(key, f).is_some() {
            return Err(format!("duplicate half-cavity boundary face {key:?}"));
        }
        pending.insert(key);
    }

    let mut new_tets = Vec::new();
    let mut steps = 0usize;
    let max_steps = 1_000_000usize;
    while let Some(&key) = pending.iter().next() {
        pending.remove(&key);
        let Some(&oriented) = front.get(&key) else {
            continue;
        };
        if skip_new_plane
            && tri_on_plane(points, oriented, plane)
            && !original_keys.contains(&key)
        {
            continue;
        }
        steps += 1;
        if steps > max_steps {
            return Err("gift-wrapping exceeded its step limit".into());
        }

        let mut chosen = None;
        for &w in vids {
            if oriented.contains(&w) {
                continue;
            }
            if orient3d_sign(
                &points[oriented[0]],
                &points[oriented[1]],
                &points[oriented[2]],
                &points[w],
            ) <= 0.0
            {
                continue;
            }
            let t = [oriented[0], oriented[1], oriented[2], w];
            let mut ok = true;
            for tau in front.values() {
                if !triangles_intersect_as_subsimplex(points, &t, *tau) {
                    ok = false;
                    break;
                }
            }
            if !ok {
                continue;
            }
            for &v in vids {
                if t.contains(&v) {
                    continue;
                }
                let s = crate::predicates3d::insphere_sign_perturbed(
                    &points[t[0]],
                    &points[t[1]],
                    &points[t[2]],
                    &points[t[3]],
                    &points[v],
                    [t[0], t[1], t[2], t[3], v],
                );
                if s < 0.0 {
                    continue;
                }
                if vertex_visible_from_tet(points, &t, v, initial_boundary) {
                    ok = false;
                    break;
                }
            }
            if ok {
                chosen = Some(w);
                break;
            }
        }
        let Some(w) = chosen else {
            return Err(format!(
                "gift-wrapping found no valid apex for front face {oriented:?}"
            ));
        };

        front.remove(&key);
        let t = [oriented[0], oriented[1], oriented[2], w];
        for lv in 0..4 {
            let fkey = crate::cdt3d::dt::sorted_face(&t, lv);
            if fkey == key {
                continue;
            }
            let (a, b, c, _) = crate::cdt3d::segment::oriented_face_apex(points, &t, lv);
            let outward = [a, c, b];
            if front.remove(&fkey).is_some() {
                pending.remove(&fkey);
            } else {
                front.insert(fkey, outward);
                pending.insert(fkey);
            }
        }
        new_tets.push(t);
    }

    Ok((new_tets, front.values().copied().collect()))
}

fn canonical_key(p: &Point3) -> Vec<u64> {
    p.canonical_key()
}

fn face_hash(points: &[Point3], tets: &[[usize; 4]]) -> u64 {
    let mut h = FNV_OFFSET;
    let mut step = |x: u64| {
        h ^= x;
        h = h.wrapping_mul(FNV_PRIME);
    };
    step(points.len() as u64);
    for p in points {
        for x in canonical_key(p) {
            step(x);
        }
    }
    step(tets.len() as u64);
    for t in tets {
        for &v in t {
            step(v as u64);
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdt3d::testutil::plc_for;
    use crate::cdt3d::segment::{recover_segments, SegmentOptions};
    use std::time::Instant;

    fn explicit(p: [f64; 3]) -> Point3 {
        Point3::Explicit(p)
    }

    #[test]
    fn a_crossing_edge_marks_the_face_missing() {
        let points = vec![
            explicit([-1.0, -1.0, 0.0]),
            explicit([1.0, -1.0, 0.0]),
            explicit([0.0, 1.0, 0.0]),
            explicit([0.0, 0.0, -1.0]),
            explicit([0.0, 0.0, 1.0]),
        ];
        let plc = Plc {
            verts: points[..3]
                .iter()
                .map(|p| p.to_f64().unwrap())
                .collect(),
            tris: vec![[0, 1, 2]],
            report: crate::cdt3d::plc::PlcReport::default(),
        };
        let tets = vec![[0, 1, 3, 4], [1, 2, 3, 4]];
        let m = detect_missing_faces(&points, &tets, &plc).unwrap();
        assert_eq!(m, vec![true]);
    }

    #[test]
    fn a_coplanar_or_above_edge_does_not_mark_the_face() {
        let points = vec![
            explicit([-1.0, -1.0, 0.0]),
            explicit([1.0, -1.0, 0.0]),
            explicit([0.0, 1.0, 0.0]),
            explicit([0.0, 0.0, 1.0]),
            explicit([0.2, 0.2, 1.0]),
        ];
        let plc = Plc {
            verts: points[..3]
                .iter()
                .map(|p| p.to_f64().unwrap())
                .collect(),
            tris: vec![[0, 1, 2]],
            report: crate::cdt3d::plc::PlcReport::default(),
        };
        let tets = vec![[0, 1, 2, 3], [0, 1, 2, 4]];
        let m = detect_missing_faces(&points, &tets, &plc).unwrap();
        assert_eq!(m, vec![false]);
    }

    #[test]
    fn gift_wrapping_fills_a_single_tetrahedron() {
        let points = vec![
            explicit([0.0, 0.0, 0.0]),
            explicit([1.0, 0.0, 0.0]),
            explicit([0.0, 1.0, 0.0]),
            explicit([0.0, 0.0, 1.0]),
        ];
        let mut t = [0usize, 1, 2, 3];
        if orient3d_sign(&points[t[0]], &points[t[1]], &points[t[2]], &points[t[3]]) < 0.0 {
            t.swap(2, 3);
        }
        let boundary: Vec<[usize; 3]> = (0..4)
            .map(|lv| {
                let (a, b, c, _) =
                    crate::cdt3d::segment::oriented_face_apex(&points, &t, lv);
                [a, b, c]
            })
            .collect();
        let (tets, leftover) = gift_fill(
            &points,
            &[0, 1, 2, 3],
            &boundary,
            &boundary,
            [0, 1, 2],
            false,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(tets.len(), 1);
        assert_eq!(sorted_tri([tets[0][0], tets[0][1], tets[0][2]]).len(), 3);
        assert_eq!(
            {
                let mut k = tets[0];
                k.sort_unstable();
                k
            },
            {
                let mut k = t;
                k.sort_unstable();
                k
            }
        );
        assert!(leftover.is_empty());
    }

    #[test]
    fn gift_wrapping_fills_a_two_tet_bipyramid() {
        let points = vec![
            explicit([0.0, 0.0, 0.0]),
            explicit([1.0, 0.0, 0.0]),
            explicit([0.0, 1.0, 0.0]),
            explicit([0.2, 0.2, 1.0]),
            explicit([0.2, 0.2, -1.0]),
        ];
        let make_positive = |mut t: [usize; 4]| {
            if orient3d_sign(&points[t[0]], &points[t[1]], &points[t[2]], &points[t[3]]) < 0.0 {
                t.swap(2, 3);
            }
            t
        };
        let t1 = make_positive([0, 1, 2, 3]);
        let t2 = make_positive([0, 1, 2, 4]);
        let mut faces: BTreeMap<[usize; 3], [usize; 3]> = BTreeMap::new();
        for t in [t1, t2] {
            for lv in 0..4 {
                let (a, b, c, _) =
                    crate::cdt3d::segment::oriented_face_apex(&points, &t, lv);
                let key = sorted_tri([a, b, c]);
                if faces.remove(&key).is_none() {
                    faces.insert(key, [a, b, c]);
                }
            }
        }
        let boundary: Vec<[usize; 3]> = faces.values().copied().collect();
        let (tets, leftover) = gift_fill(
            &points,
            &[0, 1, 2, 3, 4],
            &boundary,
            &boundary,
            [0, 1, 2],
            false,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(tets.len(), 2);
        assert!(leftover.is_empty());
        for t in &tets {
            assert!(
                orient3d_sign(&points[t[0]], &points[t[1]], &points[t[2]], &points[t[3]]) > 0.0
            );
        }
    }

    #[test]
    fn visibility_detects_a_shared_internal_face() {
        let points = vec![
            explicit([0.0, 0.0, 0.0]),
            explicit([1.0, 0.0, 0.0]),
            explicit([0.0, 1.0, 0.0]),
            explicit([0.0, 0.0, 1.0]),
            explicit([0.1, 0.1, -1.0]),
        ];
        let mut t = [0usize, 1, 2, 3];
        if orient3d_sign(&points[t[0]], &points[t[1]], &points[t[2]], &points[t[3]]) < 0.0 {
            t.swap(2, 3);
        }
        let base = [0usize, 1, 2];
        assert!(!vertex_visible_from_tet(&points, &t, 4, &[base]));

        let side = [0usize, 1, 3];
        assert!(vertex_visible_from_tet(&points, &t, 4, &[side]));
    }

    #[test]
    fn an_lnc_on_a_face_edge_is_not_a_piercing() {
        let points = vec![
            explicit([-1.0, -1.0, 0.0]),
            explicit([1.0, -1.0, 0.0]),
            explicit([0.0, 1.0, 0.0]),
            Point3::Lnc {
                a: [-1.0, -1.0, 0.0],
                b: [1.0, -1.0, 0.0],
                t: 0.5,
            },
            explicit([0.0, 0.0, 1.0]),
            explicit([1.0, 0.0, 1.0]),
            explicit([0.0, 1.0, 1.0]),
        ];
        let plc = Plc {
            verts: points[..3]
                .iter()
                .map(|p| p.to_f64().unwrap())
                .collect(),
            tris: vec![[0, 1, 2]],
            report: crate::cdt3d::plc::PlcReport::default(),
        };
        let tets = vec![[3, 4, 5, 6]];
        let m = detect_missing_faces(&points, &tets, &plc).unwrap();
        assert_eq!(m, vec![false]);
    }

    #[test]
    fn thingi10k_100827_missing_detection_is_deterministic() {
        let plc = plc_for("real", "thingi10k_100827.stl");
        let seg = recover_segments(&plc, &SegmentOptions::default()).unwrap();
        let first = detect_missing_faces(&seg.points, &seg.tets, &plc).unwrap();
        let count = first.iter().filter(|m| **m).count();
        for _ in 0..4 {
            let again = detect_missing_faces(&seg.points, &seg.tets, &plc).unwrap();
            assert_eq!(again, first);
        }
        println!("100827: faces={} missing={count}", plc.tris.len());
    }

    #[test]
    fn thingi10k_100827_face_recovery_is_structurally_valid() {
        let plc = plc_for("real", "thingi10k_100827.stl");
        let seg = recover_segments(&plc, &SegmentOptions::default()).unwrap();
        let face = recover_faces(&seg, &plc, &FaceOptions::default()).unwrap();
        assert_eq!(face.report.orientation_violations, 0, "{:?}", face.report);
        assert_eq!(face.report.duplicate_tets, 0, "{:?}", face.report);
        assert_eq!(face.report.inconsistent_neighbours, 0, "{:?}", face.report);
        assert_eq!(face.report.hull_faces_off_box, 0, "{:?}", face.report);
        assert_eq!(face.report.unused_vertices, 0, "{:?}", face.report);
        println!("100827: {:?}", face.report);
    }

    #[test]
    fn thingi10k_100827_force_gift_matches_the_default_tets() {
        let plc = plc_for("real", "thingi10k_100827.stl");
        let seg = recover_segments(&plc, &SegmentOptions::default()).unwrap();
        let normal = recover_faces(&seg, &plc, &FaceOptions::default()).unwrap();
        let forced = recover_faces(
            &seg,
            &plc,
            &FaceOptions {
                force_gift: true,
                ..FaceOptions::default()
            },
        )
        .unwrap();
        assert!(forced.report.cavities > 0);
        assert_eq!(forced.report.gift_wraps, forced.report.cavities);
        assert_eq!(forced.report.missing_after, 0);
        assert!(forced.report.coverage_certified);
        assert!(forced.report.valid);
        assert_eq!(forced.tets, normal.tets);
        assert_eq!(forced.report.hash, normal.report.hash);
    }

    #[test]
    fn thingi10k_100827_face_recovery_is_deterministic() {
        let plc = plc_for("real", "thingi10k_100827.stl");
        let seg = recover_segments(&plc, &SegmentOptions::default()).unwrap();
        let first = recover_faces(&seg, &plc, &FaceOptions::default()).unwrap();
        for _ in 0..4 {
            let seg = recover_segments(&plc, &SegmentOptions::default()).unwrap();
            let again = recover_faces(&seg, &plc, &FaceOptions::default()).unwrap();
            assert_eq!(again.report.hash, first.report.hash);
            assert_eq!(again.report.missing_after, first.report.missing_after);
            assert_eq!(again.report.tets, first.report.tets);
            assert_eq!(again.tets, first.tets);
        }
    }

    #[test]
    #[ignore = "P4 public-corpus gate; run with --release -- --ignored"]
    fn face_recovery_public_corpus_gate() {
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
            let seg = recover_segments(&plc, &SegmentOptions::default()).unwrap();
            let start = Instant::now();
            let face = recover_faces(&seg, &plc, &FaceOptions::default()).unwrap();
            let secs = start.elapsed().as_secs_f64();
            println!(
                "{name}: faces={} miss_before={} miss_after={} cav={} retet={} exp={} gift={} tets={} ori={} dup={} nm={} ngb={} unused={} empty={} cempty={} cov={} cfail={} valid={} hash={:016x} secs={:.3}",
                face.report.input_plc_triangles,
                face.report.missing_before,
                face.report.missing_after,
                face.report.cavities,
                face.report.retetrahedralized,
                face.report.expansions,
                face.report.gift_wraps,
                face.report.tets,
                face.report.orientation_violations,
                face.report.duplicate_tets,
                face.report.nonmanifold_faces,
                face.report.inconsistent_neighbours,
                face.report.unused_vertices,
                face.report.empty_sphere_violations,
                face.report.constraint_empty_sphere_faces,
                face.report.coverage_certified,
                face.report.coverage_failed,
                face.report.valid,
                face.report.hash,
                secs
            );
            assert!(
                face.report.valid,
                "{name}: face recovery report is not valid: {:?}",
                face.report
            );
        }
    }

    #[test]
    #[ignore = "P4 gift-path public-corpus regression; run with --release -- --ignored"]
    fn plate_a_dent_force_gift_covers() {
        let plc = plc_for("fixture", "plate_a_dent.stl");
        let seg = recover_segments(&plc, &SegmentOptions::default()).unwrap();
        let forced = recover_faces(
            &seg,
            &plc,
            &FaceOptions {
                force_gift: true,
                ..FaceOptions::default()
            },
        )
        .unwrap();
        assert!(forced.report.cavities > 0);
        assert_eq!(forced.report.gift_wraps, forced.report.cavities);
        assert_eq!(forced.report.missing_after, 0);
        assert!(forced.report.coverage_certified);
        assert!(forced.report.valid);
    }

    #[test]
    #[ignore = "P4 public-corpus gate; run with --release -- --ignored"]
    fn face_detection_public_corpus_gate() {
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
            let seg = recover_segments(&plc, &SegmentOptions::default()).unwrap();
            let start = Instant::now();
            let missing = detect_missing_faces(&seg.points, &seg.tets, &plc).unwrap();
            let secs = start.elapsed().as_secs_f64();
            let count = missing.iter().filter(|m| **m).count();
            println!(
                "{name}: faces={} tets={} missing={} secs={secs:.3}",
                plc.tris.len(),
                seg.tets.len(),
                count
            );
        }
    }
}
