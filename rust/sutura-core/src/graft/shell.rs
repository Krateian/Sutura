// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Whole-mesh `shell_wrap` main loop (Graft G1), matching
//! `sutura_engine/graft.py`.

use std::collections::HashMap;
use std::time::Instant;

use serde_json::{json, Map, Value};
use sutura_geom::{morph_close_core, MorphCloseOptions};

use crate::fmt::{fmt_g, py_repr_f64};
use sutura_geom::hole::close_holes_core;
use sutura_geom::manifold::{union_pair_core, watertight_check_core};
use sutura_geom::topology::{
    remove_duplicate_faces_core, remove_unreferenced_vertices_core, repair_non_manifold_edges_core,
    repair_non_manifold_vertices_core, weld_vertices_core,
};

use crate::p_weld::{
    p_weld_final, reload_strict_holes_nm, separate_weld_collisions, weld_reload_equivalent,
};
use crate::tiers::common;

use super::helpers::{
    auto_voxel, diag, gap_estimate, grid_cell_count, median_edge_length, np_pairwise_sum,
    project_envelope, r_ladder, referenced_only, split_long_edges, HEALTHY_DIST_FACTOR,
};

pub const GRID_BUDGET: i64 = 1_500_000;
pub const GRID_BUDGET_HARD_MAX: i64 = 25_000_000;
pub const GRID_BUDGET_BYTES_PER_CELL: i64 = 56;
pub const GRID_BUDGET_RAM_RESERVE: f64 = 0.35;
pub const MAX_GRID_DIV: f64 = 512.0;
pub const BUDGET_PAD: i64 = 8;
pub const MAX_TRIES: i64 = 3;
pub const LOCAL_MAX_BBOX_FRACTION: f64 = 0.30;
pub const LOCAL_WINDOW_MARGIN: f64 = 3.0;
pub const HEALTHY_HAUSDORFF_MAX: f64 = 0.0025;
pub const HAUSDORFF_SAMPLES: usize = 4000;
pub const GRAFT_MIN_INPUT_COVERAGE: f64 = 0.5;
pub const GRAFT_COVERAGE_TOL_MIN: f64 = 0.0125;
pub const GRAFT_COVERAGE_TOL_MAX: f64 = 0.05;
pub const DETAIL_TOL_MM: f64 = 0.1;
pub const DETAIL_TOL_FRACTION: f64 = 0.002;
pub const DETAIL_WARN_AREA_FRACTION: f64 = 0.02;
pub const REMESH_MAX_FACES: usize = 200_000;
pub const SHELL_WRAP_SI_MAX_FACES: usize = 20_000;
pub const HYBRID_MAX_HOLE: i64 = 200_000;
pub const REFINE_MAX_FACES: usize = 300_000;
pub const REFINE_FACE_FACTOR: usize = 4;
pub const GRAFT_WORK_MAX_ATTEMPTS: i64 = MAX_TRIES + 1;

#[derive(Clone, Debug)]
pub struct ShellWrapOptions {
    pub r: Option<f64>,
    pub voxel: Option<f64>,
    pub box_: Option<([f64; 3], [f64; 3])>,
    pub local: Option<bool>,
    pub remesh: bool,
    pub fill_cavities: bool,
    pub max_tries: i64,
    pub target_edge: Option<f64>,
    pub si_check: bool,
    pub guard: bool,
    pub grid_budget: Option<i64>,
    pub detail_tol: Option<f64>,
    pub intensity: Option<String>,
    pub time_budget: Option<f64>,
    pub sign_field: Option<bool>,
    pub hybrid: bool,
    pub raystab: Option<bool>,
    pub raystab_dirs: Option<usize>,
    pub raystab_seed: u64,
    pub raystab_parity: bool,
    pub antipodal: Option<bool>,
}

impl Default for ShellWrapOptions {
    fn default() -> Self {
        Self {
            r: None,
            voxel: None,
            box_: None,
            local: None,
            remesh: true,
            fill_cavities: false,
            max_tries: MAX_TRIES,
            target_edge: None,
            si_check: true,
            guard: true,
            grid_budget: None,
            detail_tol: None,
            intensity: None,
            time_budget: None,
            sign_field: None,
            hybrid: false,
            raystab: None,
            raystab_dirs: None,
            raystab_seed: 0,
            raystab_parity: false,
            antipodal: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ShellWrapOutput {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[i64; 3]>,
    pub report: Value,
}

/// Minimal RNG surface; G3 replaces the placeholder with NumPy's PCG64.
pub trait Rng {
    fn integers(&mut self, high: u64) -> u64;
    fn random_f64(&mut self) -> f64;
}

impl Rng for sutura_geom::rng::Pcg64 {
    fn integers(&mut self, high: u64) -> u64 {
        sutura_geom::rng::Pcg64::integers(self, high)
    }
    fn random_f64(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64) * (1.0 / 9_007_199_254_740_992.0)
    }
}

pub fn resolve_grid_budget(intensity: Option<&str>) -> i64 {
    let target = match intensity {
        Some("quick") => 1_500_000,
        Some("balanced") => 1_500_000,
        Some("thorough") => 10_000_000,
        Some("extreme") => 25_000_000,
        _ => 1_500_000,
    };
    target.clamp(GRID_BUDGET, GRID_BUDGET_HARD_MAX)
}

fn available_memory_bytes() -> Option<i64> {
    if cfg!(target_os = "macos") {
        let out = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let value: f64 = text.trim().parse().ok()?;
        return Some((value * 0.5) as i64);
    }
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("MemAvailable:") {
                let kb: f64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some((kb * 1024.0) as i64);
            }
        }
    }
    None
}

fn grid_budget_fits_ram(grid_budget: i64) -> (bool, i64, Option<i64>) {
    let Some(avail) = available_memory_bytes() else {
        return (true, 0, None);
    };
    let needed = grid_budget * GRID_BUDGET_BYTES_PER_CELL;
    (
        (needed as f64) <= avail as f64 * GRID_BUDGET_RAM_RESERVE,
        needed,
        Some(avail),
    )
}

fn grid_work(faces: i64, cells: i64) -> i64 {
    let faces = faces.max(0);
    let cells = cells.max(0);
    (faces * cells + 999) / 1000
}

fn graft_grid_work_budget() -> Option<i64> {
    match std::env::var("SUTURA_GRAFT_GRID_BUDGET") {
        Ok(raw) => {
            let text = raw.trim().to_lowercase();
            if ["0", "false", "no", "off"].contains(&text.as_str()) {
                None
            } else {
                text.parse::<i64>()
                    .ok()
                    .filter(|v| *v > 0)
                    .or(Some(120_000_000))
            }
        }
        Err(_) => Some(120_000_000),
    }
}

fn si_count(v: &[[f64; 3]], t: &[[i64; 3]]) -> Option<i64> {
    if t.is_empty() {
        return None;
    }
    if t.len() > SHELL_WRAP_SI_MAX_FACES {
        return None;
    }
    let ut: Vec<[usize; 3]> = t
        .iter()
        .map(|r| [r[0] as usize, r[1] as usize, r[2] as usize])
        .collect();
    let (_mask, count, _skipped) = sutura_geom::self_intersections_core(v, &ut);
    if count < 0 {
        None
    } else {
        Some(count)
    }
}

fn sample_surface<R: Rng>(v: &[[f64; 3]], t: &[[i64; 3]], n: usize, rng: &mut R) -> Vec<[f64; 3]> {
    if t.is_empty() || n == 0 {
        return Vec::new();
    }
    let seed = rng.integers(2_147_483_647);
    sample_surface_seeded(v, t, n, seed)
}

pub fn sample_surface_seeded(v: &[[f64; 3]], t: &[[i64; 3]], n: usize, seed: u64) -> Vec<[f64; 3]> {
    let n = n.min(4_000_000).max(1);
    let mut areas = Vec::with_capacity(t.len());
    let mut total = 0.0;
    for r in t {
        let a = v[r[0] as usize];
        let b = v[r[1] as usize];
        let c = v[r[2] as usize];
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let cr = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        areas.push(0.5 * (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]).sqrt());
        total += areas[areas.len() - 1];
    }
    if !(total > 1e-12) {
        return Vec::new();
    }
    let cdf: Vec<f64> = {
        let mut acc = 0.0;
        let mut out = Vec::with_capacity(areas.len());
        for a in &areas {
            acc += *a / total;
            out.push(acc);
        }
        out
    };
    let mut rng = sutura_geom::rng::Pcg64::new(seed);
    let face_idx: Vec<usize> = (0..n)
        .map(|_| {
            let x = Rng::random_f64(&mut rng);
            let i = cdf.partition_point(|p| *p <= x);
            i.min(t.len() - 1)
        })
        .collect();
    let r1: Vec<f64> = (0..n).map(|_| Rng::random_f64(&mut rng).sqrt()).collect();
    let r2: Vec<f64> = (0..n).map(|_| Rng::random_f64(&mut rng)).collect();
    let mut pts = Vec::with_capacity(n);
    for k in 0..n {
        let fi = face_idx[k];
        let rr1 = r1[k];
        let rr2 = r2[k];
        let (aa, bb, cc) = (1.0 - rr1, rr1 * (1.0 - rr2), rr1 * rr2);
        let r = t[fi];
        let p0 = v[r[0] as usize];
        let p1 = v[r[1] as usize];
        let p2 = v[r[2] as usize];
        pts.push([
            aa * p0[0] + bb * p1[0] + cc * p2[0],
            aa * p0[1] + bb * p1[1] + cc * p2[1],
            aa * p0[2] + bb * p1[2] + cc * p2[2],
        ]);
    }
    pts
}

fn one_sided_hausdorff<R: Rng>(
    va: &[[f64; 3]],
    ta: &[[i64; 3]],
    vb: &[[f64; 3]],
    tb: &[[i64; 3]],
    d: f64,
    rng: &mut R,
) -> (Option<f64>, Option<f64>) {
    if ta.is_empty() || tb.is_empty() || d <= 0.0 {
        return (None, None);
    }
    let pts = sample_surface(va, ta, HAUSDORFF_SAMPLES, rng);
    if pts.is_empty() {
        return (None, None);
    }
    let cp = common::closest_points(vb, tb, &pts);
    let dist: Vec<f64> = cp.iter().map(|x| x.1).collect();
    let max = dist.iter().fold(f64::NEG_INFINITY, |m, x| m.max(*x));
    let mean = np_pairwise_sum(&dist) / dist.len() as f64;
    (Some(max / d), Some(mean / d))
}

fn coverage_tol(d: f64) -> f64 {
    if d <= 0.0 {
        return GRAFT_COVERAGE_TOL_MIN;
    }
    (d / 10000.0)
        .max(GRAFT_COVERAGE_TOL_MIN)
        .min(GRAFT_COVERAGE_TOL_MAX)
}

fn input_coverage<R: Rng>(
    v0: &[[f64; 3]],
    t0: &[[i64; 3]],
    wv: &[[f64; 3]],
    wt: &[[i64; 3]],
    d: f64,
    rng: &mut R,
) -> Option<f64> {
    if t0.is_empty() || wt.is_empty() || d <= 0.0 {
        return None;
    }
    let pts = sample_surface(v0, t0, HAUSDORFF_SAMPLES, rng);
    if pts.is_empty() {
        return None;
    }
    let cp = common::closest_points(wv, wt, &pts);
    let tol = coverage_tol(d);
    let hits = cp.iter().filter(|x| x.1 <= tol).count();
    Some(hits as f64 / cp.len() as f64)
}

fn behind_on_healthy<R: Rng>(
    v0: &[[f64; 3]],
    t0: &[[i64; 3]],
    region_mask: &[bool],
    wv: &[[f64; 3]],
    wt: &[[i64; 3]],
    d: f64,
    r: f64,
    rng: &mut R,
) -> Option<f64> {
    if t0.is_empty() || wt.is_empty() || d <= 0.0 {
        return None;
    }
    let pts = sample_surface(wv, wt, HAUSDORFF_SAMPLES, rng);
    if pts.is_empty() {
        return None;
    }
    let cp = common::closest_points(v0, t0, &pts);
    let mut best = f64::NEG_INFINITY;
    let mut any = false;
    for (_q, dist, fid) in cp {
        if fid < 0 {
            continue;
        }
        let in_healthy = region_mask.is_empty()
            || (fid as usize) >= region_mask.len()
            || !region_mask[fid as usize];
        if in_healthy && dist < HEALTHY_DIST_FACTOR * r {
            best = best.max(dist);
            any = true;
        }
    }
    if any {
        Some(best / d)
    } else {
        None
    }
}

#[derive(Clone, Debug, Default)]
struct Detail {
    max: f64,
    mean: f64,
    tolerance: f64,
    area_moved_fraction: f64,
    per_vertex_deviation: Vec<f64>,
}

fn detail_loss(
    v0: &[[f64; 3]],
    t0: &[[i64; 3]],
    res_v: &[[f64; 3]],
    res_t: &[[i64; 3]],
    region_mask: &[bool],
    d: f64,
    detail_tol: Option<f64>,
) -> Detail {
    let mut healthy_faces: Vec<bool> = if region_mask.is_empty() {
        vec![true; t0.len()]
    } else {
        region_mask.iter().map(|x| !*x).collect()
    };
    if !healthy_faces.iter().any(|x| *x) {
        healthy_faces = vec![true; t0.len()];
    }
    let sub: Vec<[i64; 3]> = t0
        .iter()
        .zip(healthy_faces.iter())
        .filter(|(_, keep)| **keep)
        .map(|(r, _)| *r)
        .collect();
    let (hv, ht) = referenced_only(v0, &sub);
    let cp = common::closest_points(res_v, res_t, &hv);
    let dist: Vec<f64> = cp.iter().map(|x| x.1).collect();
    let mut area = Vec::with_capacity(ht.len());
    for r in &ht {
        let a = hv[r[0] as usize];
        let b = hv[r[1] as usize];
        let c = hv[r[2] as usize];
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let cr = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        area.push(0.5 * (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]).sqrt());
    }
    let tol = detail_tol.unwrap_or_else(|| DETAIL_TOL_MM.max(DETAIL_TOL_FRACTION * d));
    let mut moved_area: Vec<f64> = Vec::with_capacity(ht.len());
    for (fi, r) in ht.iter().enumerate() {
        let face_dev = dist[r[0] as usize]
            .max(dist[r[1] as usize])
            .max(dist[r[2] as usize]);
        if face_dev > tol {
            moved_area.push(area[fi]);
        }
    }
    let total = {
        let s = np_pairwise_sum(&area);
        if s == 0.0 {
            1.0
        } else {
            s
        }
    };
    let moved = np_pairwise_sum(&moved_area) / total;
    let d2: Vec<f64> = common::closest_points(v0, t0, res_v)
        .iter()
        .map(|x| x.1)
        .collect();
    Detail {
        max: dist
            .iter()
            .fold(f64::NEG_INFINITY, |m, x| m.max(*x))
            .max(0.0),
        mean: if dist.is_empty() {
            0.0
        } else {
            np_pairwise_sum(&dist) / dist.len() as f64
        },
        tolerance: tol,
        area_moved_fraction: moved,
        per_vertex_deviation: d2,
    }
}

fn p_weld(v: &[[f64; 3]], t: &[[i64; 3]]) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let v32: Vec<[f32; 3]> = v
        .iter()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    let v64: Vec<[f64; 3]> = v32
        .iter()
        .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
        .collect();
    let (h, nm) = reload_strict_holes_nm(&v64, t);
    if h == 0 && nm == 0 {
        return (v64, t.to_vec(), Value::Null);
    }
    let res = p_weld_final(&v64, t);
    let out_v: Vec<[f64; 3]> = res
        .verts
        .iter()
        .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
        .collect();
    let out_t: Vec<[i64; 3]> = res
        .tris
        .iter()
        .map(|r| [r[0] as i64, r[1] as i64, r[2] as i64])
        .collect();
    let report = res.report.unwrap_or_else(|| json!({"applied": false}));
    (out_v, out_t, report)
}

#[derive(Clone, Debug)]
struct Candidate {
    v: Vec<[f64; 3]>,
    t: Vec<[i64; 3]>,
    holes: usize,
    non_manifold: usize,
    r: f64,
    r_eff: f64,
    mode: String,
    watertight: bool,
    fidelity_ok: bool,
    h1: Option<f64>,
    hh: Option<f64>,
    hh_behind: Option<f64>,
    projected_fraction: f64,
    dims: Vec<usize>,
    voxel: f64,
    remeshed: bool,
    si_before: Option<i64>,
    si_after: Option<i64>,
    si_rolled_back: bool,
    pweld: Value,
    detail: Detail,
}

fn resolve_true_env(value: Option<bool>, key: &str) -> bool {
    match value {
        Some(v) => v,
        None => match std::env::var(key) {
            Ok(raw) => ["1", "true", "yes", "on"].contains(&raw.trim().to_lowercase().as_str()),
            Err(_) => false,
        },
    }
}

fn resolve_false_env(value: Option<bool>, key: &str) -> bool {
    match value {
        Some(v) => v,
        None => match std::env::var(key) {
            Ok(raw) => !["0", "false", "no", "off"].contains(&raw.trim().to_lowercase().as_str()),
            Err(_) => true,
        },
    }
}

fn fp_arrays(v: &[[f64; 3]], t: &[[i64; 3]]) -> u64 {
    let mut h: u64 = 0;
    for p in v {
        for x in p {
            for b in x.to_le_bytes() {
                h = h.wrapping_mul(131).wrapping_add(b as u64);
            }
        }
    }
    for r in t {
        for x in r {
            for b in x.to_le_bytes() {
                h = h.wrapping_mul(131).wrapping_add(b as u64);
            }
        }
    }
    h
}

pub fn shell_wrap(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    opts: &ShellWrapOptions,
) -> Result<ShellWrapOutput, String> {
    let t_start = Instant::now();
    let deadline = opts
        .time_budget
        .map(|tb| t_start + std::time::Duration::from_secs_f64(tb));
    let v0 = verts.to_vec();
    let t0 = tris.to_vec();
    if std::env::var_os("SUTURA_GRAFT_DEBUG").is_some() {
        eprintln!("[graft-rust] input fp={} faces={} verts={}",
                  fp_arrays(&v0, &t0), t0.len(), v0.len());
    }
    let grid_budget_auto = opts.grid_budget.is_none();
    let mut grid_budget = opts
        .grid_budget
        .unwrap_or_else(|| resolve_grid_budget(opts.intensity.as_deref()));
    let mut report = Map::new();
    report.insert("method".into(), json!(13));
    report.insert("display_name".into(), json!("Graft"));
    report.insert("display_name_full".into(), json!("Graft (shell wrap)"));
    report.insert("invents_geometry".into(), json!(true));
    report.insert("ok".into(), json!(false));
    report.insert("category".into(), json!("error"));
    report.insert("tries".into(), json!(0));
    report.insert("r_used".into(), Value::Null);
    report.insert("mode".into(), json!("whole"));
    report.insert("r_ladder".into(), json!([]));
    report.insert("voxel".into(), Value::Null);
    report.insert("grid_budget".into(), json!(grid_budget));
    report.insert("intensity".into(), json!(opts.intensity.clone()));
    report.insert("time_budget".into(), json!(opts.time_budget));
    report.insert("budget_exceeded".into(), json!(false));
    report.insert("budget_limited_by".into(), Value::Null);
    report.insert("work_budget".into(), Value::Null);
    report.insert("aborted".into(), json!(false));
    report.insert("low_memory".into(), json!(false));
    report.insert("grid_fallback".into(), Value::Null);
    report.insert("reject_reason".into(), Value::Null);
    report.insert("sign_field".into(), Value::Null);
    report.insert("projected_fraction".into(), json!(0.0));
    report.insert("remeshed".into(), json!(false));
    report.insert("si_before".into(), Value::Null);
    report.insert("si_after".into(), Value::Null);
    report.insert("si_rolled_back".into(), json!(false));
    report.insert("detail_max_mm".into(), Value::Null);
    report.insert("detail_mean_mm".into(), Value::Null);
    report.insert("detail_tolerance_mm".into(), Value::Null);
    report.insert("detail_area_moved".into(), Value::Null);
    report.insert("warnings".into(), json!([]));
    report.insert("per_vertex_deviation".into(), Value::Null);
    report.insert("fidelity_ok".into(), Value::Null);
    report.insert("hausdorff_one_sided".into(), Value::Null);
    report.insert("hausdorff_healthy".into(), Value::Null);
    report.insert("hausdorff_healthy_behind".into(), Value::Null);
    report.insert("input_coverage".into(), Value::Null);
    report.insert("coverage_ok".into(), Value::Null);
    report.insert("holes".into(), Value::Null);
    report.insert("non_manifold".into(), Value::Null);
    report.insert("faces_before".into(), json!(t0.len()));
    report.insert("faces_after".into(), json!(t0.len()));

    if v0.is_empty() || t0.is_empty() {
        report.insert("error".into(), json!("empty mesh"));
        return Ok(ShellWrapOutput {
            verts: v0,
            tris: t0,
            report: Value::Object(report),
        });
    }

    if grid_budget_auto {
        let preset = grid_budget;
        let (fits, _needed, avail) = grid_budget_fits_ram(preset);
        if !fits {
            let balanced = resolve_grid_budget(Some("balanced"));
            let (fits_bal, _nb, _ab) = match avail {
                Some(a) => {
                    let needed = balanced * GRID_BUDGET_BYTES_PER_CELL;
                    (
                        (needed as f64) <= a as f64 * GRID_BUDGET_RAM_RESERVE,
                        needed,
                        Some(a),
                    )
                }
                None => (true, 0, None),
            };
            if fits_bal && balanced < preset {
                grid_budget = balanced;
                report.insert("grid_budget".into(), json!(grid_budget));
                report.insert(
                    "grid_fallback".into(),
                    json!({"from": preset, "to": balanced, "intensity": opts.intensity.clone()}),
                );
            } else {
                let reason = format!(
                    "deterministic grid budget ({} cells) would not fit the available RAM; Graft skipped",
                    preset
                );
                report.insert("low_memory".into(), json!(true));
                report.insert("reject_reason".into(), json!("low_memory"));
                report.insert("reason".into(), json!(reason));
                report.insert(
                    "warnings".into(),
                    json!([{"code": "low_memory", "message": reason.clone()}]),
                );
                return Ok(ShellWrapOutput {
                    verts: v0,
                    tris: t0,
                    report: Value::Object(report),
                });
            }
        }
    }

    let use_raystab = resolve_true_env(opts.raystab, "SUTURA_RAYSTAB");
    report.insert("raystab".into(), Value::Null);
    let use_antipodal = resolve_false_env(opts.antipodal, "SUTURA_ANTIPODAL");
    report.insert("antipodal".into(), Value::Null);
    let use_sign_field = resolve_true_env(opts.sign_field, "SUTURA_GRAFT_SIGN_FIELD");
    report.insert("sign_field".into(), json!(use_sign_field));

    let d = diag(&v0);
    let voxel = opts
        .voxel
        .unwrap_or_else(|| auto_voxel(&v0, &t0, Some(grid_budget)));
    report.insert("voxel".into(), json!(voxel));

    let (gap, mut analysis, region_mask) = gap_estimate(&v0, &t0);
    let damaged = damaged_bbox(&v0, &t0, &region_mask);
    if let Some((dlo, dhi)) = damaged {
        let dd = [dhi[0] - dlo[0], dhi[1] - dlo[1], dhi[2] - dlo[2]];
        let frac = (dd[0] * dd[0] + dd[1] * dd[1] + dd[2] * dd[2]).sqrt() / d;
        analysis["damaged_bbox_fraction"] = json!(if d > 0.0 { frac } else { 0.0 });
    }

    let ladder = if let Some(r) = opts.r {
        vec![r]
    } else {
        let mut ladder = r_ladder(gap, voxel, d, opts.max_tries);
        if use_sign_field {
            ladder.insert(0, 0.0);
        }
        ladder
    };
    report.insert(
        "r_ladder".into(),
        json!(ladder.iter().map(|x| round6(*x)).collect::<Vec<f64>>()),
    );
    report.insert("analysis".into(), analysis.clone());

    let use_local = opts.local.unwrap_or_else(|| {
        damaged.is_some()
            && gap > 0.0
            && analysis["damaged_bbox_fraction"].as_f64().unwrap_or(0.0) < LOCAL_MAX_BBOX_FRACTION
    });
    let mut local_box = None;
    if use_local {
        if let Some((dlo, dhi)) = damaged {
            let last = *ladder.last().unwrap_or(&0.0);
            let margin = (LOCAL_WINDOW_MARGIN * last).max(0.02 * d);
            local_box = Some((
                [dlo[0] - margin, dlo[1] - margin, dlo[2] - margin],
                [dhi[0] + margin, dhi[1] + margin, dhi[2] + margin],
            ));
        }
    }
    let box_arg = opts.box_;
    let mut use_local = use_local;
    if box_arg.is_some() {
        use_local = false;
        local_box = None;
    }

    let mut rng = sutura_geom::rng::Pcg64::new(20260928);
    let per_attempt_cost = grid_cell_count(&v0, voxel);
    let work_budget = match opts.intensity.as_deref() {
        Some("quick") => GRAFT_WORK_MAX_ATTEMPTS * 1_500_000,
        Some("thorough") => GRAFT_WORK_MAX_ATTEMPTS * 10_000_000,
        Some("extreme") => GRAFT_WORK_MAX_ATTEMPTS * 25_000_000,
        _ => GRAFT_WORK_MAX_ATTEMPTS * 1_500_000,
    };
    report.insert("work_budget".into(), json!(work_budget));
    let raw_grid_cost = grid_work(t0.len() as i64, per_attempt_cost);
    let holes = analysis["holes"].as_i64().unwrap_or(0);
    let hole_factor = 1 + holes / 100;
    let grid_cost = raw_grid_cost * hole_factor;
    let fb_budget = graft_grid_work_budget();
    report.insert("fallback_work_units".into(), json!(grid_cost));
    report.insert("grid_work_raw".into(), json!(raw_grid_cost));
    report.insert("hole_work_factor".into(), json!(hole_factor));
    report.insert("fallback_work_budget".into(), json!(fb_budget));
    report.insert("grid_cells".into(), json!(per_attempt_cost));
    if let Some(budget) = fb_budget {
        if grid_cost > budget {
            let reason = format!(
                "Graft skipped: estimated grid work {} is above the deterministic fallback budget {}; the stage-1 result was kept",
                grid_cost, budget
            );
            report.insert("budget_exceeded".into(), json!(true));
            report.insert("budget_limited_by".into(), json!("work"));
            report.insert("precheck_skipped".into(), json!(true));
            report.insert("reject_reason".into(), json!("budget"));
            report.insert("reason".into(), json!(reason));
            report.insert(
                "warnings".into(),
                json!([{"code": "skipped_budget", "message": reason}]),
            );
            return Ok(ShellWrapOutput {
                verts: v0,
                tris: t0,
                report: Value::Object(report),
            });
        }
    }

    let mut spent_work: i64 = 0;
    let mut best: Option<Candidate> = None;
    for (attempt, rval) in ladder.iter().enumerate() {
        let rval = *rval;
        if let Some(deadline) = deadline {
            if Instant::now() >= deadline {
                let reason = format!(
                    "Graft was stopped for time ({} budget, checked between attempts); the stage-1 result was kept",
                    opts.time_budget
                        .map(|x| format!("{} s", fmt_g(x, 6)))
                        .unwrap_or_else(|| "time".into())
                );
                report.insert("aborted".into(), json!(true));
                report.insert("reject_reason".into(), json!("aborted_time"));
                report.insert("reason".into(), json!(reason));
                report.insert(
                    "warnings".into(),
                    json!([{
                        "code": "aborted_time",
                        "message": reason,
                        "message_en": reason,
                        "message_tr": format!(
                            "Graft süre nedeniyle durduruldu ({} bütçe, denemeler arasında denetlenir); Aşama 1 sonucu korundu",
                            opts.time_budget
                                .map(|x| format!("{} sn", fmt_g(x, 6)))
                                .unwrap_or_else(|| "süre".into())),
                    }]),
                );
                return Ok(ShellWrapOutput {
                    verts: v0,
                    tris: t0,
                    report: Value::Object(report),
                });
            }
        }
        if best.is_some() && (spent_work + per_attempt_cost) > work_budget {
            report.insert("budget_exceeded".into(), json!(true));
            report.insert("budget_limited_by".into(), json!("work"));
            break;
        }
        spent_work += per_attempt_cost;
        report.insert("tries".into(), json!(attempt + 1));
        let is_sign = rval <= 0.0;
        let attempt_local = use_local && !is_sign;
        let r_eff = if rval > 0.0 { rval } else { voxel };
        let mut mode = if is_sign {
            "sign_field".to_string()
        } else if attempt_local {
            "local".to_string()
        } else {
            "whole".to_string()
        };
        let mopts = MorphCloseOptions {
            voxel: Some(voxel),
            bbox: if attempt_local { local_box } else { box_arg },
            fill_cavities: opts.fill_cavities,
            surface: None,
            raystab: use_raystab,
            raystab_dirs: opts.raystab_dirs,
            raystab_seed: opts.raystab_seed,
            raystab_parity: opts.raystab_parity,
            antipodal: Some(use_antipodal),
            hermite: None,
        };
        let t32: Vec<[i32; 3]> = t0
            .iter()
            .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
            .collect();
        let closed = match morph_close_core(&v0, &t32, rval, &mopts) {
            Ok(x) => x,
            Err(_) => continue,
        };
        let mut ev = closed.0;
        let mut et: Vec<[i64; 3]> = closed
            .1
            .iter()
            .map(|r| [r[0] as i64, r[1] as i64, r[2] as i64])
            .collect();
        let info = closed.2;
        if et.is_empty() {
            continue;
        }
        if attempt_local {
            let stitched = stitch_local(&ev, &et, &v0, &t0);
            if stitched.2 {
                ev = stitched.0;
                et = stitched.1;
            } else {
                mode = "whole".to_string();
                let t32: Vec<[i32; 3]> = t0
                    .iter()
                    .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
                    .collect();
                let mopts = MorphCloseOptions {
                    bbox: box_arg,
                    ..mopts
                };
                match morph_close_core(&v0, &t32, rval, &mopts) {
                    Ok((nv, nt, _)) => {
                        ev = nv;
                        et = nt
                            .iter()
                            .map(|r| [r[0] as i64, r[1] as i64, r[2] as i64])
                            .collect();
                    }
                    Err(_) => continue,
                }
                if et.is_empty() {
                    continue;
                }
            }
        }

        let raw_ev = ev.clone();
        let raw_et = et.clone();
        let raw_si = if opts.si_check {
            si_count(&raw_ev, &raw_et)
        } else {
            None
        };
        let mut remeshed = false;
        if opts.remesh && et.len() <= REMESH_MAX_FACES {
            let healthy: Vec<bool> = if region_mask.is_empty() {
                Vec::new()
            } else {
                region_mask.iter().map(|x| !*x).collect()
            };
            let (hv0, ht0) = if !healthy.is_empty() && healthy.iter().any(|x| *x) {
                let sub: Vec<[i64; 3]> = t0
                    .iter()
                    .zip(healthy.iter())
                    .filter(|(_, keep)| **keep)
                    .map(|(r, _)| *r)
                    .collect();
                referenced_only(&v0, &sub)
            } else {
                (v0.clone(), t0.clone())
            };
            let med_h = if ht0.is_empty() {
                0.0
            } else {
                median_edge_length(&hv0, &ht0)
            };
            let tgt = opts.target_edge.unwrap_or(med_h);
            if tgt > 0.0 {
                let (ev2, et2) = split_long_edges(&ev, &et, tgt);
                let refine_cap = REFINE_MAX_FACES
                    .min(REFINE_FACE_FACTOR * t0.len())
                    .max(2000);
                if !et2.is_empty() && et2.len() <= refine_cap {
                    ev = ev2;
                    et = et2;
                    remeshed = true;
                }
            }
        }
        let mut ref_si = if opts.si_check {
            si_count(&ev, &et)
        } else {
            None
        };
        let mut si_rolled = false;
        if let (Some(a), Some(b)) = (raw_si, ref_si) {
            if b > a {
                ev = raw_ev;
                et = raw_et;
                ref_si = raw_si;
                si_rolled = true;
                remeshed = false;
            }
        }

        let (pv, healthy) = project_envelope(&ev, &et, &v0, &t0, r_eff);
        let (wv, wt, pw) = p_weld(&pv, &et);
        let (h, nm) = reload_strict_holes_nm(&wv, &wt);

        let mut h1 = None;
        let mut hh = None;
        let mut hh_behind = None;
        if opts.guard {
            let r = one_sided_hausdorff(&v0, &t0, &wv, &wt, d, &mut rng);
            h1 = r.0;
            hh_behind = behind_on_healthy(&v0, &t0, &region_mask, &wv, &wt, d, r_eff, &mut rng);
        }
        let detail = detail_loss(&v0, &t0, &wv, &wt, &region_mask, d, opts.detail_tol);
        if opts.guard && d > 0.0 {
            hh = Some(detail.max / d);
        }
        let mut fidelity_ok = !opts.guard || hh.is_none() || hh.unwrap() <= HEALTHY_HAUSDORFF_MAX;

        let mut wv = wv;
        let mut wt = wt;
        let mut h = h;
        let mut nm = nm;
        let mut pw = pw;
        let mut detail = detail;
        if opts.hybrid && !region_mask.is_empty() && !(h == 0 && nm == 0 && fidelity_ok) {
            if let Some((yv, yt)) = hybrid_close(&v0, &t0, &region_mask) {
                let (yh, ynm) = reload_strict_holes_nm(&yv, &yt);
                if yh == 0 && ynm == 0 {
                    wv = yv;
                    wt = yt;
                    h = yh;
                    nm = ynm;
                    mode = "hybrid".to_string();
                    if opts.guard {
                        let r = one_sided_hausdorff(&v0, &t0, &wv, &wt, d, &mut rng);
                        h1 = r.0;
                        hh_behind =
                            behind_on_healthy(&v0, &t0, &region_mask, &wv, &wt, d, r_eff, &mut rng);
                    }
                    detail = detail_loss(&v0, &t0, &wv, &wt, &region_mask, d, opts.detail_tol);
                    if opts.guard && d > 0.0 {
                        hh = Some(detail.max / d);
                    }
                    fidelity_ok =
                        !opts.guard || hh.is_none() || hh.unwrap() <= HEALTHY_HAUSDORFF_MAX;
                    pw = json!({"applied": false, "method": "hybrid-verbatim"});
                }
            }
        }

        let watertight = h == 0 && nm == 0;
        let projected_fraction = if healthy.is_empty() {
            0.0
        } else {
            healthy.iter().filter(|x| **x).count() as f64 / healthy.len() as f64
        };
        let dims = info.dims.to_vec();
        let cand = Candidate {
            v: wv,
            t: wt,
            holes: h,
            non_manifold: nm,
            r: rval,
            r_eff,
            mode,
            watertight,
            fidelity_ok,
            h1,
            hh,
            hh_behind,
            projected_fraction,
            dims,
            voxel: info.voxel,
            remeshed,
            si_before: ref_si,
            si_after: ref_si,
            si_rolled_back: si_rolled,
            pweld: pw,
            detail,
        };
        if !is_sign {
            match &best {
                None => best = Some(cand.clone()),
                Some(b) if (cand.holes + cand.non_manifold) < (b.holes + b.non_manifold) => {
                    best = Some(cand.clone())
                }
                _ => {}
            }
        }
        if watertight && fidelity_ok {
            best = Some(cand);
            break;
        }
    }

    let Some(best) = best else {
        report.insert("error".into(), json!("morph_close produced no candidate"));
        return Ok(ShellWrapOutput {
            verts: v0,
            tris: t0,
            report: Value::Object(report),
        });
    };

    let mut input_coverage_value = None;
    if opts.guard && d > 0.0 && !best.t.is_empty() {
        let mut coverage_rng = sutura_geom::rng::Pcg64::new(12345);
        input_coverage_value = input_coverage(&v0, &t0, &best.v, &best.t, d, &mut coverage_rng);
    }
    let coverage_ok = match input_coverage_value {
        None => true,
        Some(c) => c >= GRAFT_MIN_INPUT_COVERAGE,
    };
    report.insert("ok".into(), json!(best.watertight));
    report.insert(
        "category".into(),
        json!(if best.watertight {
            "watertight"
        } else {
            "partial"
        }),
    );
    report.insert("r_used".into(), json!(round6(best.r)));
    report.insert("mode".into(), json!(best.mode));
    report.insert("dims".into(), json!(best.dims));
    report.insert("voxel".into(), json!(best.voxel));
    report.insert(
        "projected_fraction".into(),
        json!(round4(best.projected_fraction)),
    );
    report.insert("remeshed".into(), json!(best.remeshed));
    report.insert("si_before".into(), json!(best.si_before));
    report.insert("si_after".into(), json!(best.si_after));
    report.insert("si_rolled_back".into(), json!(best.si_rolled_back));
    report.insert("hausdorff_one_sided".into(), json!(best.h1.map(round5)));
    report.insert("hausdorff_healthy".into(), json!(best.hh.map(round5)));
    report.insert(
        "hausdorff_healthy_behind".into(),
        json!(best.hh_behind.map(round5)),
    );
    report.insert("fidelity_ok".into(), json!(best.fidelity_ok));
    report.insert(
        "input_coverage".into(),
        json!(input_coverage_value.map(round4)),
    );
    report.insert("coverage_ok".into(), json!(coverage_ok));
    report.insert("holes".into(), json!(best.holes));
    report.insert("non_manifold".into(), json!(best.non_manifold));
    report.insert("p_weld".into(), best.pweld);
    report.insert("faces_after".into(), json!(best.t.len()));
    report.insert("detail_max_mm".into(), json!(round5(best.detail.max)));
    report.insert("detail_mean_mm".into(), json!(round5(best.detail.mean)));
    report.insert(
        "detail_tolerance_mm".into(),
        json!(round5(best.detail.tolerance)),
    );
    report.insert(
        "detail_area_moved".into(),
        json!(round5(best.detail.area_moved_fraction)),
    );
    report.insert(
        "per_vertex_deviation".into(),
        json!(best.detail.per_vertex_deviation),
    );
    report.insert("detail_loss".into(), json!(best.hh.map(round5)));
    let mut warnings = warnings_for(best.r_eff, &best.detail);
    if let Some(gf) = report.get("grid_fallback") {
        if !gf.is_null() {
            let inten = gf
                .get("intensity")
                .and_then(Value::as_str)
                .unwrap_or("preset");
            let msg = format!(
                "{} grid does not fit in RAM; used Balanced grid",
                capitalize(inten)
            );
            warnings.push(json!({
                "code": "grid_fallback",
                "message": msg,
                "message_en": msg,
                "message_tr": format!(
                    "{} ızgarası RAM'e sığmıyor; Balanced ızgarası kullanıldı",
                    capitalize(inten)),
            }));
        }
    }
    if !coverage_ok {
        let cov = report
            .get("input_coverage")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        warnings.push(json!({
            "code": "coverage",
            "message_en": format!(
                "Graft closed the mesh but its input coverage ({}) is below {}; the result does not reproduce enough of the input surface (likely a collapsed fragment).",
                py_repr_f64(round4(cov)), GRAFT_MIN_INPUT_COVERAGE),
            "message_tr": format!(
                "Graft ağı kapattı ancak girdi kapsaması ({}) {} eşiğinin altında; sonuç girdi yüzeyinin yeterli kısmını yeniden üretmiyor (muhtemelen çökmüş bir parça).",
                py_repr_f64(round4(cov)), GRAFT_MIN_INPUT_COVERAGE),
        }));
    }
    if best.watertight && !best.fidelity_ok {
        let hh = report
            .get("hausdorff_healthy")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        warnings.push(json!({
            "code": "fidelity",
            "message_en": format!(
                "Graft closed the mesh but the healthy-region deviation ({}) exceeds the tolerance; the result is watertight but its healthy shape may have changed.",
                py_repr_f64(round5(hh))),
            "message_tr": format!(
                "Graft ağı kapattı ancak sağlıklı bölge sapması ({}) toleransı aşıyor; sonuç su geçirmez fakat sağlıklı şekli değişmiş olabilir.",
                py_repr_f64(round5(hh))),
        }));
    }
    report.insert("warnings".into(), Value::Array(warnings));

    Ok(ShellWrapOutput {
        verts: best.v,
        tris: best.t,
        report: Value::Object(report),
    })
}

fn damaged_bbox(
    v: &[[f64; 3]],
    t: &[[i64; 3]],
    region_mask: &[bool],
) -> Option<([f64; 3], [f64; 3])> {
    if !region_mask.iter().any(|x| *x) {
        return None;
    }
    let mut used: Vec<i64> = t
        .iter()
        .zip(region_mask.iter())
        .filter(|(_, r)| **r)
        .flat_map(|(tri, _)| tri.iter().copied())
        .collect();
    used.sort_unstable();
    used.dedup();
    if used.is_empty() {
        return None;
    }
    let pts: Vec<[f64; 3]> = used.iter().map(|&i| v[i as usize]).collect();
    Some(super::helpers::bbox(&pts))
}

fn warnings_for(r_used: f64, detail: &Detail) -> Vec<Value> {
    if detail.area_moved_fraction > DETAIL_WARN_AREA_FRACTION || detail.max > 4.0 * detail.tolerance
    {
        let pct = 100.0 * detail.area_moved_fraction;
        return vec![json!({
            "code": "detail_loss",
            "message_en": format!(
                "Graft used a {:.3} mm envelope: {:.1}% of the surface moved more than {:.3} mm (fine surface texture smoothed).",
                r_used, pct, detail.tolerance),
            "message_tr": format!(
                "Graft {:.3} mm zarf kullandı: yüzeyin %{:.1}'i {:.3} mm'den fazla hareket etti (ince yüzey dokusu yumuşatıldı).",
                r_used, pct, detail.tolerance),
        })];
    }
    Vec::new()
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn py_round(x: f64, digits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{:.*}", digits, x).parse().unwrap_or(x)
}

fn round6(x: f64) -> f64 {
    py_round(x, 6)
}

fn round5(x: f64) -> f64 {
    py_round(x, 5)
}

fn round4(x: f64) -> f64 {
    py_round(x, 4)
}

fn to_f32(v: &[[f64; 3]]) -> Vec<[f32; 3]> {
    v.iter()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect()
}

fn from_f32(v: &[[f32; 3]]) -> Vec<[f64; 3]> {
    v.iter()
        .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
        .collect()
}

fn stitch_local(
    patch_v: &[[f64; 3]],
    patch_t: &[[i64; 3]],
    orig_v: &[[f64; 3]],
    orig_t: &[[i64; 3]],
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, bool) {
    let patch_v = patch_v.to_vec();
    let patch_t = patch_t.to_vec();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let v32 = to_f32(&patch_v);
        let (welded, inverse) = match weld_vertices_core(&v32) {
            Ok(x) => x,
            Err(_) => return (patch_v.clone(), patch_t.clone(), false),
        };
        let pv = from_f32(&welded);
        let pt: Vec<[i64; 3]> = patch_t
            .iter()
            .map(|r| {
                [
                    inverse[r[0] as usize],
                    inverse[r[1] as usize],
                    inverse[r[2] as usize],
                ]
            })
            .collect();
        let pt32: Vec<[i32; 3]> = pt
            .iter()
            .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
            .collect();
        if !watertight_check_core(&pv, &pt32) {
            return (pv, pt, false);
        }
        let at: Vec<[usize; 3]> = pt
            .iter()
            .map(|r| [r[0] as usize, r[1] as usize, r[2] as usize])
            .collect();
        let bt: Vec<[usize; 3]> = orig_t
            .iter()
            .map(|r| [r[0] as usize, r[1] as usize, r[2] as usize])
            .collect();
        match union_pair_core(&pv, &at, orig_v, &bt) {
            Ok((ov, ot)) if !ot.is_empty() => (
                ov,
                ot.iter()
                    .map(|r| [r[0] as i64, r[1] as i64, r[2] as i64])
                    .collect(),
                true,
            ),
            _ => (pv, pt, false),
        }
    }));
    match result {
        Ok(x) => x,
        Err(_) => (patch_v, patch_t, false),
    }
}

fn hybrid_close(
    v0: &[[f64; 3]],
    t0: &[[i64; 3]],
    region_mask: &[bool],
) -> Option<(Vec<[f64; 3]>, Vec<[i64; 3]>)> {
    if region_mask.is_empty() || !region_mask.iter().any(|x| !*x) {
        return None;
    }
    let sub: Vec<[i64; 3]> = t0
        .iter()
        .zip(region_mask.iter())
        .filter(|(_, bad)| !**bad)
        .map(|(r, _)| *r)
        .collect();
    let (bv, bt) = referenced_only(v0, &sub);
    if bt.is_empty() {
        return None;
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut ov = bv.clone();
        let mut ot = bt.clone();
        for _ in 0..2 {
            let v32 = to_f32(&ov);
            ot = repair_non_manifold_edges_core(&v32, &ot);
            let v32 = to_f32(&ov);
            let (nv, nt) = repair_non_manifold_vertices_core(&v32, &ot);
            ov = from_f32(&nv);
            ot = nt;
            ot = remove_duplicate_faces_core(&ot);
            if let Ok(report) = close_holes_core(&ov, &ot, HYBRID_MAX_HOLE, true) {
                ov = report.verts;
                ot = report.tris;
            }
            let v32 = to_f32(&ov);
            let (nv, nt) = remove_unreferenced_vertices_core(&v32, &ot);
            ov = from_f32(&nv);
            ot = nt;
        }
        let capped = cap_boundary_loops(&ov, &ot);
        ov = capped.0;
        ot = capped.1;
        let (ov, ot, _) = p_weld(&ov, &ot);
        let (mut h, mut nm) = reload_strict_holes_nm(&ov, &ot);
        let mut ov = ov;
        let mut ot = ot;
        if h != 0 {
            let (sv, st) = separate_weld_collisions(&ov, &ot);
            if let Ok((wv, wt)) = weld_reload_equivalent(&from_f32(&sv), &st) {
                let (wv, wt) = cap_boundary_loops(&from_f32(&wv), &wt);
                let (wh, wnm) = reload_strict_holes_nm(&wv, &wt);
                if wh == 0 && wnm == 0 {
                    ov = wv;
                    ot = wt;
                    h = 0;
                    nm = 0;
                }
            }
        }
        if h == 0 && nm == 0 {
            Some((ov, ot))
        } else {
            None
        }
    }));
    result.ok().flatten()
}

fn cap_boundary_loops(v: &[[f64; 3]], t: &[[i64; 3]]) -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
    if t.is_empty() {
        return (v.to_vec(), t.to_vec());
    }
    let mut half_edges: Vec<(i64, i64)> = Vec::with_capacity(3 * t.len());
    for r in t {
        half_edges.push((r[0], r[1]));
    }
    for r in t {
        half_edges.push((r[1], r[2]));
    }
    for r in t {
        half_edges.push((r[2], r[0]));
    }
    let eset: std::collections::HashSet<(i64, i64)> = half_edges.iter().copied().collect();
    let bnd: Vec<(i64, i64)> = half_edges
        .iter()
        .copied()
        .filter(|&(a, b)| !eset.contains(&(b, a)))
        .collect();
    if bnd.is_empty() {
        return (v.to_vec(), t.to_vec());
    }
    let mut parent: HashMap<i64, i64> = HashMap::new();
    fn find(parent: &mut HashMap<i64, i64>, x: i64) -> i64 {
        let entry = *parent.entry(x).or_insert(x);
        if entry != x {
            let root = find(parent, entry);
            parent.insert(x, root);
            root
        } else {
            x
        }
    }
    for &(a, b) in &bnd {
        let ra = find(&mut parent, a);
        let rb = find(&mut parent, b);
        if ra != rb {
            parent.insert(ra, rb);
        }
    }
    let mut order: Vec<i64> = Vec::new();
    let mut comps: HashMap<i64, Vec<(i64, i64)>> = HashMap::new();
    for &(a, b) in &bnd {
        let root = find(&mut parent, a);
        if !comps.contains_key(&root) {
            order.push(root);
        }
        comps.entry(root).or_default().push((a, b));
    }
    let mut addv: Vec<[f64; 3]> = Vec::new();
    let mut addt: Vec<[i64; 3]> = Vec::new();
    for root in order {
        let edges = &comps[&root];
        let mut cvts: Vec<i64> = edges.iter().flat_map(|&(a, b)| [a, b]).collect();
        cvts.sort_unstable();
        cvts.dedup();
        let mut xs = Vec::with_capacity(cvts.len());
        let mut ys = Vec::with_capacity(cvts.len());
        let mut zs = Vec::with_capacity(cvts.len());
        for &i in &cvts {
            let p = v[i as usize];
            xs.push(p[0]);
            ys.push(p[1]);
            zs.push(p[2]);
        }
        let n = cvts.len() as f64;
        addv.push([
            np_pairwise_sum(&xs) / n,
            np_pairwise_sum(&ys) / n,
            np_pairwise_sum(&zs) / n,
        ]);
        let cidx = (v.len() + addv.len() - 1) as i64;
        for &(a, b) in edges {
            addt.push([b, a, cidx]);
        }
    }
    let mut nv = v.to_vec();
    nv.extend(addv);
    let mut nt = t.to_vec();
    nt.extend(addt);
    nt.retain(|r| r[0] != r[1] && r[1] != r[2] && r[0] != r[2]);
    let mut seen: std::collections::HashSet<[i64; 3]> = std::collections::HashSet::new();
    nt.retain(|r| {
        let mut k = *r;
        k.sort_unstable();
        seen.insert(k)
    });
    let mut used: Vec<i64> = nt.iter().flat_map(|r| r.iter().copied()).collect();
    used.sort_unstable();
    used.dedup();
    let remap: HashMap<i64, i64> = used
        .iter()
        .enumerate()
        .map(|(i, &u)| (u, i as i64))
        .collect();
    let ov: Vec<[f64; 3]> = used.iter().map(|&u| nv[u as usize]).collect();
    let ot: Vec<[i64; 3]> = nt
        .iter()
        .map(|r| [remap[&r[0]], remap[&r[1]], remap[&r[2]]])
        .collect();
    (ov, ot)
}
