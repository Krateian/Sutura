// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Proxy-template producer, ported from `sutura_engine/methods/proxy.py`.
//!
//! Three watertight proxy strategies (signed resample + hole closing, unsigned
//! offset resample, morphological closing), a healthy-region mask and the
//! projection/rollback loop of `proxy_template_repair`.

use std::collections::HashMap;

use serde_json::{json, Value};
use sutura_geom::hole::close_holes_core;
use sutura_geom::resample::resample_uniform_core;
use sutura_geom::topology::{
    re_orient_faces_coherently_core, repair_non_manifold_edges_core,
    repair_non_manifold_vertices_core,
};
use sutura_geom::{morph_close_core, MorphCloseOptions};

use crate::closing::{check_validity, CloseOutput};
use crate::graft::helpers::{np_pairwise_sum, referenced_only};
use crate::tiers::common::closest_points;

pub const DEFAULT_VOXEL_DIVISOR: f64 = 80.0;
pub const MIN_GRID_CELLS: f64 = 32.0;
pub const MAX_GRID_CELLS: f64 = 256.0;
pub const DEFAULT_DILATION_RINGS: usize = 1;
pub const MIN_DEBRIS_COMPONENT_FACES: usize = 20;
pub const PROJECTION_DIST_FACTOR: f64 = 3.0;
pub const SI_FACE_CAP: usize = 200_000;
pub const MIN_FACE_BUDGET: usize = 20_000;
pub const HAUSDORFF_SAMPLES: usize = 100_000;
pub const HAUSDORFF_SEED: u64 = 12_345;

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

fn bbox_span(v: &[[f64; 3]]) -> [f64; 3] {
    let mut lo = v[0];
    let mut hi = v[0];
    for p in v {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]]
}

fn to_f32(v: &[[f64; 3]]) -> Vec<[f32; 3]> {
    v.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect()
}

fn to_f64(v: &[[f32; 3]]) -> Vec<[f64; 3]> {
    v.iter().map(|p| [p[0] as f64, p[1] as f64, p[2] as f64]).collect()
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent[rb] = ra;
        }
    }
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        0.5 * (values[n / 2 - 1] + values[n / 2])
    }
}

fn edge_key(a: i64, b: i64) -> (i64, i64) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Filter out small disconnected debris components (< `min_faces`), keeping
/// legitimate parts. Mirrors `_filter_debris_components` (vertex-connected
/// components, matching `topology.connected_components`).
pub fn filter_debris_components(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    min_faces: usize,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
    if tris.is_empty() || verts.is_empty() {
        return (verts.to_vec(), tris.to_vec());
    }
    let nv = verts.len();
    let mut uf = UnionFind::new(nv);
    for t in tris {
        let a = t[0] as usize;
        let b = t[1] as usize;
        let c = t[2] as usize;
        uf.union(a, b);
        uf.union(b, c);
        uf.union(c, a);
    }
    let mut roots: Vec<usize> = (0..nv).map(|v| uf.find(v)).collect();
    roots.sort_unstable();
    roots.dedup();
    if roots.len() <= 1 {
        return (verts.to_vec(), tris.to_vec());
    }

    let mut counts: HashMap<usize, usize> = HashMap::new();
    let face_labels: Vec<usize> = tris
        .iter()
        .map(|t| {
            let r = uf.find(t[0] as usize);
            *counts.entry(r).or_insert(0) += 1;
            r
        })
        .collect();

    let mut keep: Vec<usize> = counts
        .iter()
        .filter(|(_, c)| **c >= min_faces)
        .map(|(r, _)| *r)
        .collect();
    keep.sort_unstable();
    if keep.is_empty() {
        if let Some((&best, _)) = counts
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
        {
            keep.push(best);
        }
    }
    let keep_set: std::collections::HashSet<usize> = keep.into_iter().collect();
    let kept: Vec<[i64; 3]> = tris
        .iter()
        .zip(face_labels.iter())
        .filter(|(_, l)| keep_set.contains(l))
        .map(|(t, _)| *t)
        .collect();
    referenced_only(verts, &kept)
}

fn decimate_to_budget(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    max_budget: usize,
) -> Option<(Vec<[f64; 3]>, Vec<[i64; 3]>)> {
    let rep = sutura_geom::decimate::decimate_impl(
        verts, tris, max_budget, None, false, None,
    )
    .ok()?;
    let (dv, dt) = filter_debris_components(&rep.verts, &rep.tris, MIN_DEBRIS_COMPONENT_FACES);
    if dt.len() >= 4 && check_validity(&dv, &dt).0 {
        Some((dv, dt))
    } else {
        None
    }
}

fn manifold_resample(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    voxel: f64,
    offset: f64,
    absdist: bool,
    raystab_full: bool,
) -> Result<(Vec<[f64; 3]>, Vec<[i64; 3]>), String> {
    let oriented;
    let use_tris = if absdist {
        tris
    } else {
        oriented = re_orient_faces_coherently_core(tris)
            .map_err(|e| format!("{:?}", e))?;
        &oriented
    };
    let rep = resample_uniform_core(
        verts,
        use_tris,
        voxel,
        offset,
        absdist,
        raystab_full,
        None,
        Some("mass_point"),
    )?;
    let v32 = to_f32(&rep.verts);
    let (rv32, rt) = repair_non_manifold_vertices_core(&v32, &rep.tris);
    let rt = repair_non_manifold_edges_core(&rv32, &rt);
    reload_honest(&to_f64(&rv32), &rt)
}

fn reload_honest(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
) -> Result<(Vec<[f64; 3]>, Vec<[i64; 3]>), String> {
    let mut cur_v = verts.to_vec();
    let mut cur_t = tris.to_vec();
    for _ in 0..4 {
        let (wv32, wt) = crate::p_weld::weld_reload_equivalent(&cur_v, &cur_t)?;
        let (rv32, rt) = repair_non_manifold_vertices_core(&wv32, &wt);
        let rt = repair_non_manifold_edges_core(&rv32, &rt);
        let rv = to_f64(&rv32);
        let rep = close_holes_core(&rv, &rt, 10_000, true)?;
        let nv = rep.verts;
        let nt = rep.tris;
        if check_validity(&nv, &nt).0 {
            return Ok((nv, nt));
        }
        cur_v = nv;
        cur_t = nt;
    }
    Ok((cur_v, cur_t))
}

/// Construct a watertight, coarse proxy surface. Returns `(verts, tris, info)`.
pub fn build_proxy(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    voxel: Option<f64>,
) -> Result<(Vec<[f64; 3]>, Vec<[i64; 3]>, Value), String> {
    if verts.len() < 3 || tris.is_empty() {
        return Err("Cannot build proxy from empty or degenerate mesh".to_string());
    }
    let (iv, it) = referenced_only(verts, tris);
    let (iv, it) = filter_debris_components(&iv, &it, MIN_DEBRIS_COMPONENT_FACES);

    let span = bbox_span(&iv);
    let max_dim = span[0].max(span[1]).max(span[2]);
    let diag = (span[0] * span[0] + span[1] * span[1] + span[2] * span[2]).sqrt();
    if max_dim <= 1e-12 {
        return Err("Degenerate bounding box".to_string());
    }
    let mut voxel = voxel.unwrap_or(diag / DEFAULT_VOXEL_DIVISOR);
    if max_dim / voxel > MAX_GRID_CELLS {
        voxel = max_dim / MAX_GRID_CELLS;
    }
    if max_dim / voxel < MIN_GRID_CELLS {
        voxel = max_dim / MIN_GRID_CELLS;
    }

    // Strategy 1: signed resample + two hole-closing passes.
    if let Ok((rv, rt)) = manifold_resample(&iv, &it, voxel, 0.0, false, true) {
        let mut cv = rv;
        let mut ct = rt;
        let mut closed_ok = true;
        for _ in 0..2 {
            match close_holes_core(&cv, &ct, 10_000, true) {
                Ok(rep) => {
                    cv = rep.verts;
                    ct = rep.tris;
                }
                Err(_) => {
                    closed_ok = false;
                    break;
                }
            }
        }
        if closed_ok {
            let (pv, pt) =
                filter_debris_components(&cv, &ct, MIN_DEBRIS_COMPONENT_FACES);
            if pt.len() >= 4 && check_validity(&pv, &pt).0 {
                let faces = pt.len();
                return Ok((
                    pv,
                    pt,
                    json!({
                        "method": "uniform_resampled_signed",
                        "voxel": voxel,
                        "faces": faces,
                    }),
                ));
            }
        }
    }

    // Strategy 2: unsigned distance offset resample.
    if let Ok((rv, rt)) = manifold_resample(&iv, &it, voxel, voxel, true, false) {
        let (pv, pt) = filter_debris_components(&rv, &rt, MIN_DEBRIS_COMPONENT_FACES);
        if pt.len() >= 4 && check_validity(&pv, &pt).0 {
            let faces = pt.len();
            return Ok((
                pv,
                pt,
                json!({
                    "method": "uniform_resampled_unsigned",
                    "voxel": voxel,
                    "faces": faces,
                }),
            ));
        }
    }

    // Strategy 3: morphological closing fallback.
    let t32: Vec<[i32; 3]> = it
        .iter()
        .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
        .collect();
    let opts = MorphCloseOptions {
        voxel: Some(voxel),
        ..Default::default()
    };
    match morph_close_core(&iv, &t32, voxel * 2.0, &opts) {
        Ok((rv, rt, _rep)) => {
            let rt: Vec<[i64; 3]> =
                rt.iter().map(|r| [r[0] as i64, r[1] as i64, r[2] as i64]).collect();
            let (pv, pt) =
                filter_debris_components(&rv, &rt, MIN_DEBRIS_COMPONENT_FACES);
            if pt.len() >= 4 && check_validity(&pv, &pt).0 {
                let faces = pt.len();
                return Ok((
                    pv,
                    pt,
                    json!({
                        "method": "morph_close",
                        "voxel": voxel,
                        "faces": faces,
                    }),
                ));
            }
        }
        Err(e) => return Err(format!("Failed to build watertight proxy: {}", e)),
    }

    Err("All proxy generation strategies failed to produce a valid 2-manifold".to_string())
}

/// Per-face health mask. False for faces touching a boundary/non-manifold or
/// inconsistently-wound edge, small debris components, self-intersections
/// (below the face cap) and, after `dilation_rings` ring dilations, faces
/// incident to an unhealthy vertex.
pub fn health_mask(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    dilation_rings: usize,
    si_face_cap: usize,
) -> Vec<bool> {
    let n_faces = tris.len();
    if n_faces == 0 {
        return Vec::new();
    }
    let mut healthy = vec![true; n_faces];
    let mut bad_face = vec![false; n_faces];

    let mut undir: HashMap<(i64, i64), usize> = HashMap::new();
    let mut dir: HashMap<(i64, i64), usize> = HashMap::new();
    for t in tris {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            *undir.entry(edge_key(a, b)).or_insert(0) += 1;
            *dir.entry((a, b)).or_insert(0) += 1;
        }
    }
    for (fi, t) in tris.iter().enumerate() {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let bad_u = undir.get(&edge_key(a, b)).copied().unwrap_or(0) != 2;
            let bad_d = dir.get(&(a, b)).copied().unwrap_or(0) > 1;
            if bad_u || bad_d {
                bad_face[fi] = true;
            }
        }
    }

    // Debris components: only faces linked through a two-face edge join.
    let mut edge_faces: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (fi, t) in tris.iter().enumerate() {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            edge_faces.entry(edge_key(a, b)).or_default().push(fi);
        }
    }
    let mut uf = UnionFind::new(n_faces);
    let mut has_shared = false;
    for faces in edge_faces.values() {
        if faces.len() == 2 {
            has_shared = true;
            uf.union(faces[0], faces[1]);
        }
    }
    if has_shared {
        let mut counts: HashMap<usize, usize> = HashMap::new();
        let labels: Vec<usize> = (0..n_faces)
            .map(|f| {
                let r = uf.find(f);
                *counts.entry(r).or_insert(0) += 1;
                r
            })
            .collect();
        let small: std::collections::HashSet<usize> = counts
            .iter()
            .filter(|(_, c)| **c < MIN_DEBRIS_COMPONENT_FACES)
            .map(|(r, _)| *r)
            .collect();
        for (fi, l) in labels.iter().enumerate() {
            if small.contains(l) {
                bad_face[fi] = true;
            }
        }
    }

    // Self intersections (native exact predicates, capped like Python).
    if n_faces <= si_face_cap {
        let ut: Vec<[usize; 3]> = tris
            .iter()
            .map(|r| [r[0] as usize, r[1] as usize, r[2] as usize])
            .collect();
        let (si_mask, _count, _skipped) = sutura_geom::self_intersections_core(verts, &ut);
        if si_mask.len() == n_faces {
            for (fi, hit) in si_mask.iter().enumerate() {
                if *hit {
                    bad_face[fi] = true;
                }
            }
        }
    }

    for (fi, bad) in bad_face.iter().enumerate() {
        if *bad {
            healthy[fi] = false;
        }
    }

    // Dilate the unhealthy set by vertex rings.
    let max_v = tris
        .iter()
        .flat_map(|t| t.iter().copied())
        .max()
        .unwrap_or(-1)
        .max(verts.len() as i64 - 1)
        + 1;
    let max_v = max_v.max(0) as usize;
    let mut v_to_faces: Vec<Vec<usize>> = vec![Vec::new(); max_v];
    for (fi, t) in tris.iter().enumerate() {
        for &v in t {
            let vi = v as usize;
            if vi < max_v {
                v_to_faces[vi].push(fi);
            }
        }
    }
    for _ in 0..dilation_rings {
        let mut unhealthy_v: Vec<usize> = Vec::new();
        for (fi, ok) in healthy.iter().enumerate() {
            if !*ok {
                for &v in &tris[fi] {
                    unhealthy_v.push(v as usize);
                }
            }
        }
        unhealthy_v.sort_unstable();
        unhealthy_v.dedup();
        for v in unhealthy_v {
            if v < v_to_faces.len() {
                for &fi in &v_to_faces[v] {
                    healthy[fi] = false;
                }
            }
        }
    }

    healthy
}

fn has_positive_area(verts: &[[f64; 3]], tris: &[[i64; 3]], diag: f64) -> bool {
    if tris.is_empty() || diag <= 0.0 {
        return false;
    }
    tris.iter().any(|t| {
        let a = verts[t[0] as usize];
        let b = verts[t[1] as usize];
        let c = verts[t[2] as usize];
        norm(cross(sub(b, a), sub(c, a))) > 1e-12 * diag * diag
    })
}

pub(crate) fn surface_samples(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    samples: usize,
    seed: u64,
) -> Vec<[f64; 3]> {
    if verts.is_empty() || tris.is_empty() || samples == 0 {
        return Vec::new();
    }
    if verts.len() >= samples {
        if samples == 1 {
            return vec![verts[0]];
        }
        let last = (verts.len() - 1) as f64;
        let denom = (samples - 1) as f64;
        return (0..samples)
            .map(|k| {
                let value = last * k as f64 / denom;
                verts[value as usize]
            })
            .collect();
    }
    let extra = samples - verts.len();
    let mut out = verts.to_vec();
    let pts = crate::graft::shell::sample_surface_seeded(verts, tris, extra, seed);
    if !pts.is_empty() {
        out.extend(pts);
    }
    out
}

fn one_sided_hausdorff_rel(
    in_v: &[[f64; 3]],
    in_t: &[[i64; 3]],
    out_v: &[[f64; 3]],
    out_t: &[[i64; 3]],
) -> (Option<f64>, Option<f64>) {
    if in_t.is_empty() || out_t.is_empty() {
        return (None, None);
    }
    let (in_v, in_t) = referenced_only(in_v, in_t);
    let (out_v, out_t) = referenced_only(out_v, out_t);
    if in_v.is_empty() || out_v.is_empty() || in_t.is_empty() || out_t.is_empty() {
        return (None, None);
    }
    let span = bbox_span(&in_v);
    let diag = (span[0] * span[0] + span[1] * span[1] + span[2] * span[2]).sqrt();
    if diag <= 1e-12 {
        return (None, None);
    }
    let out_span = bbox_span(&out_v);
    let out_diag =
        (out_span[0] * out_span[0] + out_span[1] * out_span[1] + out_span[2] * out_span[2])
            .sqrt();
    if !has_positive_area(&in_v, &in_t, diag)
        || !has_positive_area(&out_v, &out_t, out_diag)
    {
        return (None, None);
    }
    let pts = surface_samples(&in_v, &in_t, HAUSDORFF_SAMPLES, HAUSDORFF_SEED);
    if pts.is_empty() {
        return (None, None);
    }
    let cp = closest_points(&out_v, &out_t, &pts);
    if cp.is_empty() {
        return (None, None);
    }
    let dist: Vec<f64> = cp.iter().map(|x| x.1).collect();
    let max = dist.iter().fold(f64::NEG_INFINITY, |m, x| m.max(*x));
    let mean = np_pairwise_sum(&dist) / dist.len() as f64;
    let round6 = |x: f64| (x * 1_000_000.0).round() / 1_000_000.0;
    (Some(round6(max / diag)), Some(round6(mean / diag)))
}

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

fn error_output(verts: &[[f64; 3]], tris: &[[i64; 3]], error: String) -> CloseOutput {
    CloseOutput {
        verts: verts.to_vec(),
        tris: tris.to_vec(),
        report: json!({
            "watertight": false,
            "output_faces": tris.len(),
            "proxy_faces": 0,
            "projected_fraction": 0.0,
            "projected_fraction_healthy": 0.0,
            "reverted_count": 0,
            "error": error,
            "notes": [],
        }),
    }
}

/// Repair a broken mesh by proxy-template projection and collision rollback.
pub fn proxy_template_repair(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    voxel: Option<f64>,
    max_iters: i32,
) -> CloseOutput {
    if verts.len() < 3 || tris.is_empty() {
        return CloseOutput {
            verts: verts.to_vec(),
            tris: tris.to_vec(),
            report: json!({
                "watertight": false,
                "output_faces": tris.len(),
                "proxy_faces": 0,
                "projected_fraction": 0.0,
                "projected_fraction_healthy": 0.0,
                "reverted_count": 0,
                "error": "Empty or degenerate input mesh",
                "notes": [],
            }),
        };
    }
    match proxy_template_repair_inner(verts, tris, voxel, max_iters) {
        Ok(out) => out,
        Err(e) => error_output(verts, tris, e),
    }
}

fn proxy_template_repair_inner(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    voxel: Option<f64>,
    max_iters: i32,
) -> Result<CloseOutput, String> {
    let span = bbox_span(verts);
    let diag = (span[0] * span[0] + span[1] * span[1] + span[2] * span[2]).sqrt();

    let h_mask = health_mask(verts, tris, DEFAULT_DILATION_RINGS, SI_FACE_CAP);
    let has_healthy = h_mask.iter().any(|x| *x);

    if !has_healthy {
        let (pv, pt, _info) = build_proxy(verts, tris, voxel)?;
        let (is_wt, err_msg) = check_validity(&pv, &pt);
        if !is_wt {
            return Ok(CloseOutput {
                verts: verts.to_vec(),
                tris: tris.to_vec(),
                report: json!({
                    "watertight": false,
                    "output_faces": tris.len(),
                    "proxy_faces": pt.len(),
                    "projected_fraction": 0.0,
                    "projected_fraction_healthy": 0.0,
                    "reverted_count": 0,
                    "error": err_msg,
                    "notes": [],
                }),
            });
        }
        return Ok(CloseOutput {
            report: json!({
                "watertight": true,
                "proxy_faces": pt.len(),
                "output_faces": pt.len(),
                "projected_fraction": 0.0,
                "projected_fraction_healthy": 0.0,
                "reverted_count": 0,
                "notes": ["regions without healthy source were reconstructed from a coarse proxy"],
            }),
            verts: pv,
            tris: pt,
        });
    }

    let healthy_faces: Vec<[i64; 3]> = tris
        .iter()
        .zip(h_mask.iter())
        .filter(|(_, h)| **h)
        .map(|(t, _)| *t)
        .collect();
    let (healthy_v, healthy_t) = referenced_only(verts, &healthy_faces);

    let mut lengths = Vec::with_capacity(healthy_t.len() * 3);
    for t in &healthy_t {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            lengths.push(norm(sub(
                healthy_v[b as usize],
                healthy_v[a as usize],
            )));
        }
    }
    let target_edge_len = median(&mut lengths);

    let voxel_arg = voxel.unwrap_or(diag / DEFAULT_VOXEL_DIVISOR);
    let (mut pv, mut pt, proxy_info) = build_proxy(verts, tris, Some(voxel_arg))?;
    let proxy_faces_count = pt.len();
    let used_voxel = proxy_info
        .get("voxel")
        .and_then(Value::as_f64)
        .unwrap_or(voxel_arg);

    let max_budget = (tris.len() * 2).max(MIN_FACE_BUDGET);
    if pt.len() > max_budget {
        if let Some((dv, dt)) = decimate_to_budget(&pv, &pt, max_budget) {
            pv = dv;
            pt = dt;
        }
    }

    let orig_normals: Vec<[f64; 3]> = pt
        .iter()
        .map(|t| {
            let a = pv[t[0] as usize];
            let b = pv[t[1] as usize];
            let c = pv[t[2] as usize];
            cross(sub(b, a), sub(c, a))
        })
        .collect();

    let cp = closest_points(verts, tris, &pv);
    let mut closest_pts = vec![[0.0; 3]; pv.len()];
    let mut distances = vec![f64::INFINITY; pv.len()];
    let mut nearest_healthy = vec![false; pv.len()];
    for (i, (q, d, f)) in cp.iter().enumerate() {
        if i >= pv.len() {
            break;
        }
        closest_pts[i] = *q;
        distances[i] = *d;
        if *f >= 0 && (*f as usize) < h_mask.len() {
            nearest_healthy[i] = h_mask[*f as usize];
        }
    }
    let subset_count = nearest_healthy.iter().filter(|x| **x).count();
    let max_dist =
        (PROJECTION_DIST_FACTOR * used_voxel).max(2.0 * target_edge_len);
    let mut proj_mask = vec![false; pv.len()];
    for i in 0..pv.len() {
        proj_mask[i] = nearest_healthy[i] && distances[i] < max_dist;
    }
    let mut projected_pv = pv.clone();
    for i in 0..pv.len() {
        if proj_mask[i] {
            projected_pv[i] = closest_pts[i];
        }
    }

    let mut total_reverted = 0usize;
    for _ in 0..max_iters.max(0) {
        let mut bad_faces = Vec::new();
        for (fi, t) in pt.iter().enumerate() {
            let v0 = projected_pv[t[0] as usize];
            let v1 = projected_pv[t[1] as usize];
            let v2 = projected_pv[t[2] as usize];
            let face_normal = cross(sub(v1, v0), sub(v2, v0));
            let area = 0.5 * norm(face_normal);
            let dot_align = dot(face_normal, orig_normals[fi]);
            if area < 1e-8 || dot_align <= 0.0 {
                bad_faces.push(fi);
            }
        }
        if bad_faces.is_empty() {
            break;
        }
        let mut bad_v: Vec<usize> = Vec::new();
        for &fi in &bad_faces {
            for &v in &pt[fi] {
                bad_v.push(v as usize);
            }
        }
        bad_v.sort_unstable();
        bad_v.dedup();
        let reverted_now = bad_v.iter().filter(|v| proj_mask[**v]).count();
        if reverted_now == 0 {
            break;
        }
        for &v in &bad_v {
            projected_pv[v] = pv[v];
            proj_mask[v] = false;
        }
        total_reverted += reverted_now;
    }

    let projected_count = proj_mask.iter().filter(|x| **x).count();
    let projected_fraction_healthy = if subset_count > 0 {
        projected_count as f64 / subset_count as f64
    } else {
        0.0
    };
    let projected_fraction = if proj_mask.is_empty() {
        0.0
    } else {
        projected_count as f64 / proj_mask.len() as f64
    };

    if pt.len() > max_budget {
        if let Some((dv, dt)) = decimate_to_budget(&projected_pv, &pt, max_budget) {
            projected_pv = dv;
            pt = dt;
        }
    }

    let (mut is_wt, mut err_msg) = check_validity(&projected_pv, &pt);
    if !is_wt {
        projected_pv = pv.clone();
        let (wt, err) = check_validity(&projected_pv, &pt);
        is_wt = wt;
        err_msg = err;
    }
    if !is_wt {
        return Ok(CloseOutput {
            verts: verts.to_vec(),
            tris: tris.to_vec(),
            report: json!({
                "watertight": false,
                "output_faces": tris.len(),
                "proxy_faces": proxy_faces_count,
                "projected_fraction": 0.0,
                "projected_fraction_healthy": 0.0,
                "reverted_count": total_reverted,
                "error": err_msg,
                "notes": [],
            }),
        });
    }

    let (h_max, h_mean) =
        one_sided_hausdorff_rel(&healthy_v, &healthy_t, &projected_pv, &pt);

    Ok(CloseOutput {
        report: json!({
            "watertight": true,
            "proxy_faces": proxy_faces_count,
            "output_faces": pt.len(),
            "projected_fraction": round4(projected_fraction),
            "projected_fraction_healthy": round4(projected_fraction_healthy),
            "reverted_count": total_reverted,
            "hausdorff_max": h_max,
            "hausdorff_mean": h_mean,
            "notes": ["regions without healthy source were reconstructed from a coarse proxy"],
        }),
        verts: projected_pv,
        tris: pt,
    })
}
