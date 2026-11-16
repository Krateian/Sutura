// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Closing tier geometry producers (`poisson_close`, `flat_back_close`),
//! ported from `sutura_engine/methods/closing.py`.

use std::collections::HashMap;

use serde_json::{json, Value};
use sutura_geom::resample::resample_uniform_core_with_cancel;
use sutura_geom::topology::{
    compute_normal_per_face_core, re_orient_faces_coherently_core,
};

use crate::graft::helpers::referenced_only;

pub const POISSON_DEFAULT_SCALE: f64 = 2.0;
pub const POISSON_FACE_BUDGET_MULT: usize = 4;
pub const POISSON_FACE_BUDGET_MIN: usize = 20_000;
pub const POISSON_FACE_BUDGET_MAX: usize = 200_000;
pub const POISSON_VERTEX_MODE: &str = "mass";
pub const DEFAULT_FLAT_BACK_THICKNESS_REL: f64 = 0.005;
pub const EAR_CLIP_MAX_VERTICES: usize = 1500;

#[derive(Clone, Debug)]
pub struct CloseOutput {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[i64; 3]>,
    pub report: Value,
}

fn bbox(v: &[[f64; 3]]) -> ([f64; 3], [f64; 3]) {
    let mut lo = v[0];
    let mut hi = v[0];
    for p in v {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (lo, hi)
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn mean(points: &[[f64; 3]]) -> [f64; 3] {
    let mut acc = [0.0; 3];
    for p in points {
        for k in 0..3 {
            acc[k] += p[k];
        }
    }
    let n = points.len() as f64;
    [acc[0] / n, acc[1] / n, acc[2] / n]
}

pub(crate) fn check_validity(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> (bool, String) {
    if verts.len() < 4 || tris.len() < 4 {
        return (false, "Too few vertices or faces".to_string());
    }
    let mut und: HashMap<(i64, i64), usize> = HashMap::new();
    let mut dir: HashMap<(i64, i64), usize> = HashMap::new();
    for t in tris {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let k = if a <= b { (a, b) } else { (b, a) };
            *und.entry(k).or_insert(0) += 1;
            *dir.entry((a, b)).or_insert(0) += 1;
        }
    }
    let mut n_open = 0usize;
    let mut n_nm = 0usize;
    for c in und.values() {
        if *c == 1 {
            n_open += 1;
        }
        if *c > 2 {
            n_nm += 1;
        }
    }
    if !und.values().all(|c| *c == 2) {
        return (
            false,
            format!(
                "Not 2-manifold: {} boundary edges, {} non-manifold edges",
                n_open, n_nm
            ),
        );
    }
    let dup = dir.values().filter(|c| **c > 1).count();
    if dup > 0 {
        return (
            false,
            format!(
                "Inconsistent face orientation: {} directed edge(s) traversed twice",
                dup
            ),
        );
    }
    let mut vol = 0.0;
    for t in tris {
        let a = verts[t[0] as usize];
        let b = verts[t[1] as usize];
        let c = verts[t[2] as usize];
        vol += dot(a, cross(b, c)) / 6.0;
    }
    if vol <= 0.0 {
        return (
            false,
            format!("Non-positive volume ({:.6}): inverted or flat shell", vol),
        );
    }
    (true, String::new())
}

fn keep_largest_component(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
    if tris.is_empty() {
        return (verts.to_vec(), tris.to_vec());
    }
    let nv = verts.len();
    let mut parent: Vec<usize> = (0..nv).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for t in tris {
        let a = t[0] as usize;
        let b = t[1] as usize;
        let c = t[2] as usize;
        for (u, v) in [(a, b), (b, c), (c, a)] {
            let ru = find(&mut parent, u);
            let rv = find(&mut parent, v);
            if ru != rv {
                parent[rv] = ru;
            }
        }
    }
    let mut counts: HashMap<usize, usize> = HashMap::new();
    for t in tris {
        let r = find(&mut parent, t[0] as usize);
        *counts.entry(r).or_insert(0) += 1;
    }
    if counts.len() <= 1 {
        return (verts.to_vec(), tris.to_vec());
    }
    let mut best_root = 0usize;
    let mut best = 0usize;
    for (&root, &count) in &counts {
        if count > best || (count == best && root < best_root) {
            best = count;
            best_root = root;
        }
    }
    let kept: Vec<[i64; 3]> = tris
        .iter()
        .copied()
        .filter(|t| find(&mut parent, t[0] as usize) == best_root)
        .collect();
    let sub_v: Vec<[f64; 3]> = verts.to_vec();
    referenced_only(&sub_v, &kept)
}

fn close_error(verts: &[[f64; 3]], tris: &[[i64; 3]], message: &str) -> CloseOutput {
    CloseOutput {
        verts: verts.to_vec(),
        tris: tris.to_vec(),
        report: json!({
            "watertight": false,
            "faces": tris.len(),
            "error": message,
            "notes": [],
        }),
    }
}

pub fn poisson_close(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    depth: Option<i32>,
) -> CloseOutput {
    poisson_close_with_cancel(verts, tris, depth, None)
}

/// `poisson_close` with a cooperative cancellation probe.
pub fn poisson_close_with_cancel(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    depth: Option<i32>,
    cancel: Option<&dyn sutura_geom::cancel::Cancel>,
) -> CloseOutput {
    if verts.len() < 3 || tris.is_empty() {
        return close_error(verts, tris, "Empty or degenerate input mesh");
    }
    let mut depth = match depth {
        Some(d) => d,
        None => {
            let n = verts.len();
            if n < 5_000 {
                8
            } else if n < 25_000 {
                9
            } else if n < 100_000 {
                10
            } else {
                11
            }
        }
    }
    .clamp(8, 11);

    let (in_v, in_t) = referenced_only(verts, tris);
    let mut in_t = match re_orient_faces_coherently_core(&in_t) {
        Ok(t) => t,
        Err(e) => return close_error(verts, tris, &format!("{:?}", e)),
    };
    let fnorm = compute_normal_per_face_core(&in_v, &in_t);
    let centroid = mean(&in_v);
    let mut orient_sum = 0.0;
    for (i, t) in in_t.iter().enumerate() {
        let c = [
            (in_v[t[0] as usize][0] + in_v[t[1] as usize][0] + in_v[t[2] as usize][0]) / 3.0,
            (in_v[t[0] as usize][1] + in_v[t[1] as usize][1] + in_v[t[2] as usize][1]) / 3.0,
            (in_v[t[0] as usize][2] + in_v[t[1] as usize][2] + in_v[t[2] as usize][2]) / 3.0,
        ];
        orient_sum += dot(
            sub(c, centroid),
            [fnorm[i][0] as f64, fnorm[i][1] as f64, fnorm[i][2] as f64],
        );
    }
    if orient_sum < 0.0 {
        for t in in_t.iter_mut() {
            t.swap(0, 2);
        }
    }

    let (lo, hi) = bbox(&in_v);
    let extent = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(hi[2] - lo[2]);
    let mut voxel = POISSON_DEFAULT_SCALE * extent / (2f64.powi(depth));
    let budget = (POISSON_FACE_BUDGET_MULT * in_t.len())
        .clamp(POISSON_FACE_BUDGET_MIN, POISSON_FACE_BUDGET_MAX);

    let resample = |vx: f64| {
        resample_uniform_core_with_cancel(
            &in_v,
            &in_t,
            vx,
            0.0,
            false,
            false,
            None,
            Some(POISSON_VERTEX_MODE),
            cancel,
        )
    };
    let mut out = match resample(voxel) {
        Ok(r) => r,
        Err(e) => return close_error(verts, tris, &e),
    };
    if out.tris.len() > budget {
        voxel *= (out.tris.len() as f64 / budget as f64).sqrt() * 1.03;
        out = match resample(voxel) {
            Ok(r) => r,
            Err(e) => return close_error(verts, tris, &e),
        };
    }
    let (out_v, out_t) = keep_largest_component(&out.verts, &out.tris);

    let ut: Vec<[usize; 3]> = out_t
        .iter()
        .map(|r| [r[0] as usize, r[1] as usize, r[2] as usize])
        .collect();
    let (_mask, si_before, _skipped) = sutura_geom::self_intersections_core(&out_v, &ut);
    let si_before = si_before.max(0);
    let (out_v, out_t) = if si_before > 0 {
        let params = json!({
            "maxholesize": 1000,
            "max_rounds": 3,
            "time_budget": 5.0,
            "face_fraction": 0.30,
        });
        let (ex_v, ex_t, _rep) = crate::stage1::si_excise::run(
            &out_v, &out_t, &out_v, &out_t, &params,
        );
        let eut: Vec<[usize; 3]> = ex_t
            .iter()
            .map(|r| [r[0] as usize, r[1] as usize, r[2] as usize])
            .collect();
        let (_m, si_after, _s) = sutura_geom::self_intersections_core(&ex_v, &eut);
        if si_after >= 0 && si_after < si_before {
            (ex_v, ex_t)
        } else {
            (out_v, out_t)
        }
    } else {
        (out_v, out_t)
    };
    let eut: Vec<[usize; 3]> = out_t
        .iter()
        .map(|r| [r[0] as usize, r[1] as usize, r[2] as usize])
        .collect();
    let (_m, si_after, _s) = sutura_geom::self_intersections_core(&out_v, &eut);
    let si_after = si_after.max(0);
    if si_after > 0 {
        return CloseOutput {
            verts: verts.to_vec(),
            tris: tris.to_vec(),
            report: json!({
                "faces": tris.len(),
                "watertight": false,
                "depth": depth,
                "voxel": voxel,
                "si_before": si_before,
                "si_after": si_after,
                "error": format!("Residual self-intersections ({}) after voxel resample", si_after),
                "notes": [],
            }),
        };
    }
    let (wt, err) = check_validity(&out_v, &out_t);
    if !wt {
        return CloseOutput {
            verts: verts.to_vec(),
            tris: tris.to_vec(),
            report: json!({
                "faces": tris.len(),
                "watertight": false,
                "depth": depth,
                "voxel": voxel,
                "error": err,
                "notes": [],
            }),
        };
    }
    let faces = out_t.len();
    CloseOutput {
        verts: out_v,
        tris: out_t,
        report: json!({
            "faces": faces,
            "watertight": true,
            "depth": depth,
            "voxel": voxel,
            "face_budget": budget,
            "vertex_mode": POISSON_VERTEX_MODE,
            "si_before": si_before,
            "si_after": si_after,
            "notes": ["back surface was estimated (voxel resample)"],
        }),
    }
}

/// Boundary loops as ordered vertex-index lists, from directed half-edges
/// (triangle winding fixes the orientation), sorted descending by length.
///
/// This is the port of Python's `closing.boundary_loops` and the source
/// `analysis::analyze_mesh` must use for the two Closing scores: the
/// `defects::detect_holes` walk is undirected and differs on 40 of 226 corpus
/// meshes in loop count or length.
pub fn boundary_loops(tris: &[[i64; 3]]) -> Vec<Vec<i64>> {
    if tris.is_empty() {
        return Vec::new();
    }
    let mut edges: Vec<(i64, i64)> = Vec::with_capacity(3 * tris.len());
    for t in tris {
        edges.push((t[0], t[1]));
    }
    for t in tris {
        edges.push((t[1], t[2]));
    }
    for t in tris {
        edges.push((t[2], t[0]));
    }
    let mut counts: HashMap<(i64, i64), usize> = HashMap::new();
    for &(a, b) in &edges {
        let k = if a <= b { (a, b) } else { (b, a) };
        *counts.entry(k).or_insert(0) += 1;
    }
    let bkeys: std::collections::HashSet<(i64, i64)> = counts
        .iter()
        .filter(|(_, c)| **c == 1)
        .map(|(k, _)| *k)
        .collect();
    if bkeys.is_empty() {
        return Vec::new();
    }
    let mut adj: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut order: Vec<i64> = Vec::new();
    for &(a, b) in &edges {
        let k = if a <= b { (a, b) } else { (b, a) };
        if bkeys.contains(&k) {
            if !adj.contains_key(&a) {
                order.push(a);
            }
            adj.entry(a).or_default().push(b);
        }
    }
    let mut visited: std::collections::HashSet<(i64, i64)> = std::collections::HashSet::new();
    let mut loops: Vec<Vec<i64>> = Vec::new();
    for &u in &order {
        let neighbors = match adj.get(&u) {
            Some(n) => n.clone(),
            None => continue,
        };
        for v in neighbors {
            if visited.contains(&(u, v)) {
                continue;
            }
            let mut lp = vec![u];
            visited.insert((u, v));
            let mut cur = v;
            while cur != u {
                lp.push(cur);
                let nexts: Vec<i64> = adj
                    .get(&cur)
                    .map(|ns| {
                        ns.iter()
                            .copied()
                            .filter(|n| !visited.contains(&(cur, *n)))
                            .collect()
                    })
                    .unwrap_or_default();
                if nexts.is_empty() {
                    break;
                }
                let nxt = nexts[0];
                visited.insert((cur, nxt));
                cur = nxt;
            }
            if lp.len() >= 3 {
                loops.push(lp);
            }
        }
    }
    loops.sort_by(|a, b| b.len().cmp(&a.len()));
    loops
}

pub(crate) fn loop_stats(verts: &[[f64; 3]], lp: &[i64]) -> (f64, f64, [f64; 3], [f64; 3]) {
    let pts: Vec<[f64; 3]> = lp.iter().map(|&i| verts[i as usize]).collect();
    let mut perim = 0.0;
    for i in 0..pts.len() {
        let j = (i + 1) % pts.len();
        perim += norm(sub(pts[j], pts[i]));
    }
    let mut va = [0.0; 3];
    for i in 0..pts.len() {
        let j = (i + 1) % pts.len();
        let c = cross(pts[i], pts[j]);
        va[0] += c[0];
        va[1] += c[1];
        va[2] += c[2];
    }
    let va = [0.5 * va[0], 0.5 * va[1], 0.5 * va[2]];
    let spanned = norm(va);
    (perim, spanned, va, mean(&pts))
}

pub(crate) fn jacobi_eigen(a: [[f64; 3]; 3]) -> ([f64; 3], [[f64; 3]; 3]) {
    let mut m = a;
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..32 {
        let mut off = 0.0;
        for i in 0..3 {
            for j in (i + 1)..3 {
                off += m[i][j] * m[i][j];
            }
        }
        if off.sqrt() < 1e-18 {
            break;
        }
        for p in 0..2 {
            for q in (p + 1)..3 {
                let apq = m[p][q];
                if apq.abs() < 1e-300 {
                    continue;
                }
                let theta = (m[q][q] - m[p][p]) / (2.0 * apq);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..3 {
                    let mkp = m[k][p];
                    let mkq = m[k][q];
                    m[k][p] = c * mkp - s * mkq;
                    m[k][q] = s * mkp + c * mkq;
                }
                for k in 0..3 {
                    let mpk = m[p][k];
                    let mqk = m[q][k];
                    m[p][k] = c * mpk - s * mqk;
                    m[q][k] = s * mpk + c * mqk;
                }
                for k in 0..3 {
                    let vkp = v[k][p];
                    let vkq = v[k][q];
                    v[k][p] = c * vkp - s * vkq;
                    v[k][q] = s * vkp + c * vkq;
                }
            }
        }
    }
    let mut pairs: Vec<(f64, [f64; 3])> = (0..3)
        .map(|i| (m[i][i], [v[0][i], v[1][i], v[2][i]]))
        .collect();
    pairs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    (
        [pairs[0].0, pairs[1].0, pairs[2].0],
        [pairs[0].1, pairs[1].1, pairs[2].1],
    )
}

fn ear_clip_2d(pts: &[[f64; 2]]) -> Vec<[i64; 3]> {
    let n = pts.len();
    if n < 3 {
        return Vec::new();
    }
    let mut indices: Vec<usize> = (0..n).collect();
    let mut area = 0.0;
    for i in 0..n {
        let j = (i + 1) % n;
        area += pts[i][0] * pts[j][1] - pts[i][1] * pts[j][0];
    }
    if 0.5 * area < 0.0 {
        indices.reverse();
    }
    let sign = |p1: [f64; 2], p2: [f64; 2], p3: [f64; 2]| {
        (p1[0] - p3[0]) * (p2[1] - p3[1]) - (p2[0] - p3[0]) * (p1[1] - p3[1])
    };
    let mut out = Vec::new();
    let mut count = 0usize;
    let max_iter = n * n * 2;
    while indices.len() > 3 && count < max_iter {
        count += 1;
        let mut ear = false;
        let l = indices.len();
        for i in 0..l {
            let prev = indices[(i + l - 1) % l];
            let curr = indices[i];
            let next = indices[(i + 1) % l];
            let p0 = pts[prev];
            let p1 = pts[curr];
            let p2 = pts[next];
            if (p1[0] - p0[0]) * (p2[1] - p0[1]) - (p1[1] - p0[1]) * (p2[0] - p0[0])
                <= 1e-12
            {
                continue;
            }
            let mut is_ear = true;
            for j in 0..l {
                if j == (i + l - 1) % l || j == i || j == (i + 1) % l {
                    continue;
                }
                let p = pts[indices[j]];
                let d1 = sign(p, p0, p1);
                let d2 = sign(p, p1, p2);
                let d3 = sign(p, p2, p0);
                let has_neg = d1 < -1e-12 || d2 < -1e-12 || d3 < -1e-12;
                let has_pos = d1 > 1e-12 || d2 > 1e-12 || d3 > 1e-12;
                if !(has_neg && has_pos) {
                    is_ear = false;
                    break;
                }
            }
            if is_ear {
                out.push([prev as i64, curr as i64, next as i64]);
                indices.remove(i);
                ear = true;
                break;
            }
        }
        if !ear {
            out.push([indices[0] as i64, indices[1] as i64, indices[2] as i64]);
            indices.remove(1);
        }
    }
    if indices.len() == 3 {
        out.push([indices[0] as i64, indices[1] as i64, indices[2] as i64]);
    }
    out
}

pub fn flat_back_close(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    thickness: Option<f64>,
) -> CloseOutput {
    flat_back_close_with_cancel(verts, tris, thickness, None)
}

/// `flat_back_close` with a cooperative cancellation probe.
pub fn flat_back_close_with_cancel(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    thickness: Option<f64>,
    cancel: Option<&dyn sutura_geom::cancel::Cancel>,
) -> CloseOutput {
    if verts.len() < 3 || tris.is_empty() {
        return close_error(verts, tris, "Empty or degenerate input mesh");
    }
    let (mut in_v, mut in_t) = referenced_only(verts, tris);
    let mut loops = boundary_loops(&in_t);
    if loops.is_empty() {
        let (wt, err) = check_validity(&in_v, &in_t);
        let mut report = json!({
            "watertight": wt,
            "faces": in_t.len(),
            "thickness": 0.0,
            "notes": ["mesh is already closed"],
        });
        if !wt {
            report["error"] = json!(err);
        }
        return CloseOutput {
            verts: in_v,
            tris: in_t,
            report,
        };
    }
    let mut perims: Vec<f64> = loops.iter().map(|l| loop_stats(&in_v, l).0).collect();
    let mut dom_idx = 0usize;
    for i in 1..perims.len() {
        if perims[i] > perims[dom_idx] {
            dom_idx = i;
        }
    }
    let mut dom_loop = loops[dom_idx].clone();
    let dom_len = dom_loop.len();
    let mut others: Vec<Vec<i64>> = loops
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != dom_idx)
        .map(|(_, l)| l.clone())
        .collect();
    if !others.is_empty() {
        let max_small = others.iter().map(|l| l.len()).max().unwrap_or(0);
        if max_small < dom_len {
            if let Ok(rep) = sutura_geom::hole::close_holes_core_with_cancel(
                &in_v, &in_t, max_small as i64 + 1, true, cancel) {
                in_v = rep.verts;
                in_t = rep.tris;
            }
            loops = boundary_loops(&in_t);
            if loops.is_empty() {
                let (wt, err) = check_validity(&in_v, &in_t);
                let mut report = json!({
                    "watertight": wt,
                    "faces": in_t.len(),
                    "thickness": 0.0,
                    "notes": ["flat back added"],
                });
                if !wt {
                    report["error"] = json!(err);
                }
                return CloseOutput {
                    verts: in_v,
                    tris: in_t,
                    report,
                };
            }
            perims = loops.iter().map(|l| loop_stats(&in_v, l).0).collect();
            let mut best = 0usize;
            for i in 1..perims.len() {
                if perims[i] > perims[best] {
                    best = i;
                }
            }
            dom_loop = loops[best].clone();
        }
    }
    others.clear();
    let pts: Vec<[f64; 3]> = dom_loop.iter().map(|&i| in_v[i as usize]).collect();
    let c = mean(&pts);
    let (lo, hi) = bbox(&in_v);
    let d = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let diag = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if diag <= 1e-12 {
        return close_error(verts, tris, "Degenerate bounding box");
    }
    let thickness_val = thickness.unwrap_or(DEFAULT_FLAT_BACK_THICKNESS_REL * diag);

    let mut cov = [[0.0f64; 3]; 3];
    for p in &pts {
        let q = sub(*p, c);
        for i in 0..3 {
            for j in 0..3 {
                cov[i][j] += q[i] * q[j];
            }
        }
    }
    let (_evals, evecs) = jacobi_eigen(cov);
    let mut n = evecs[2];
    let mut fn_sum = [0.0; 3];
    for t in &in_t {
        let a = in_v[t[0] as usize];
        let b = in_v[t[1] as usize];
        let cc = in_v[t[2] as usize];
        let f = cross(sub(b, a), sub(cc, a));
        fn_sum[0] += f[0];
        fn_sum[1] += f[1];
        fn_sum[2] += f[2];
    }
    if dot(fn_sum, n) < 0.0 {
        n = [-n[0], -n[1], -n[2]];
    }
    let mut all_proj = Vec::with_capacity(in_v.len());
    for p in &in_v {
        all_proj.push(dot(sub(*p, c), n));
    }
    let min_proj = all_proj.iter().fold(f64::INFINITY, |m, x| m.min(*x));
    let h_back = min_proj - thickness_val;
    let loop_proj: Vec<[f64; 3]> = pts
        .iter()
        .map(|p| {
            let t = dot(sub(*p, c), n) - h_back;
            [p[0] - t * n[0], p[1] - t * n[1], p[2] - t * n[2]]
        })
        .collect();
    let n_orig = in_v.len();
    let n_loop = dom_loop.len();
    let mut all_verts = in_v.clone();
    all_verts.extend(loop_proj.iter().copied());
    let proj_idx: Vec<i64> = (0..n_loop).map(|i| (n_orig + i) as i64).collect();
    let mut side_tris: Vec<[i64; 3]> = Vec::with_capacity(2 * n_loop);
    for i in 0..n_loop {
        let j = (i + 1) % n_loop;
        side_tris.push([dom_loop[i], proj_idx[i], dom_loop[j]]);
        side_tris.push([dom_loop[j], proj_idx[i], proj_idx[j]]);
    }
    let mut u_axis = evecs[0];
    let un = dot(u_axis, n);
    u_axis = [u_axis[0] - un * n[0], u_axis[1] - un * n[1], u_axis[2] - un * n[2]];
    let ul = norm(u_axis);
    u_axis = [u_axis[0] / ul, u_axis[1] / ul, u_axis[2] / ul];
    let v_axis = cross(n, u_axis);
    let pts2d: Vec<[f64; 2]> = loop_proj
        .iter()
        .map(|p| {
            let q = sub(*p, c);
            [dot(q, u_axis), dot(q, v_axis)]
        })
        .collect();

    let mut cap_tris: Vec<[i64; 3]>;
    if n_loop <= EAR_CLIP_MAX_VERTICES {
        let local = ear_clip_2d(&pts2d);
        cap_tris = local
            .iter()
            .map(|t| {
                [
                    proj_idx[t[0] as usize],
                    proj_idx[t[1] as usize],
                    proj_idx[t[2] as usize],
                ]
            })
            .collect();
        if let Some(t0) = local.first() {
            let a = loop_proj[t0[0] as usize];
            let b = loop_proj[t0[1] as usize];
            let cc = loop_proj[t0[2] as usize];
            let cn = cross(sub(b, a), sub(cc, a));
            if dot(cn, [-n[0], -n[1], -n[2]]) <= 0.0 {
                for t in cap_tris.iter_mut() {
                    t.swap(1, 2);
                }
            }
        }
        let mut all_t = in_t.clone();
        all_t.extend(side_tris.iter().copied());
        all_t.extend(cap_tris.iter().copied());
        in_t = all_t;
    } else {
        let c2 = mean(&pts2d.iter().map(|p| [p[0], p[1], 0.0]).collect::<Vec<_>>());
        let mut all_pos = true;
        let mut all_neg = true;
        for i in 0..pts2d.len() {
            let j = (i + 1) % pts2d.len();
            let a = [pts2d[i][0] - c2[0], pts2d[i][1] - c2[1]];
            let b = [pts2d[j][0] - c2[0], pts2d[j][1] - c2[1]];
            let cr = a[0] * b[1] - a[1] * b[0];
            if cr <= 1e-12 {
                all_pos = false;
            }
            if cr >= -1e-12 {
                all_neg = false;
            }
        }
        let is_star = all_pos || all_neg;
        if is_star {
            let c_back = mean(&loop_proj);
            let c_idx = all_verts.len() as i64;
            all_verts.push(c_back);
            cap_tris = (0..n_loop)
                .map(|i| {
                    [
                        proj_idx[(i + 1) % n_loop],
                        proj_idx[i],
                        c_idx,
                    ]
                })
                .collect();
            let t0 = cap_tris[0];
            let a = all_verts[t0[0] as usize];
            let b = all_verts[t0[1] as usize];
            let cc = all_verts[t0[2] as usize];
            let cn = cross(sub(b, a), sub(cc, a));
            if dot(cn, [-n[0], -n[1], -n[2]]) < 0.0 {
                for t in cap_tris.iter_mut() {
                    t.swap(1, 2);
                }
            }
            let mut all_t = in_t.clone();
            all_t.extend(side_tris.iter().copied());
            all_t.extend(cap_tris.iter().copied());
            in_t = all_t;
        } else {
            let mut open_t = in_t.clone();
            open_t.extend(side_tris.iter().copied());
            match sutura_geom::hole::close_holes_core_with_cancel(
                &all_verts, &open_t, n_loop as i64 + 100, true, cancel) {
                Ok(rep) => {
                    all_verts = rep.verts;
                    in_t = rep.tris;
                }
                Err(e) => return close_error(verts, tris, &e),
            }
        }
    }
    let (wt, err) = check_validity(&all_verts, &in_t);
    if !wt {
        return CloseOutput {
            verts: verts.to_vec(),
            tris: tris.to_vec(),
            report: json!({
                "faces": tris.len(),
                "watertight": false,
                "thickness": thickness_val,
                "error": err,
                "notes": [],
            }),
        };
    }
    let faces = in_t.len();
    CloseOutput {
        verts: all_verts,
        tris: in_t,
        report: json!({
            "faces": faces,
            "watertight": true,
            "thickness": thickness_val,
            "notes": ["flat back added"],
        }),
    }
}
