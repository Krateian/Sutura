// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Dressing (#16) tier orchestration, ported from
//! `sutura_engine/pipeline/tiers/dressing.py` and `sutura_engine/dressing.py`.
//!
//! The level-set field build and marching-tetrahedra extraction already live
//! in `sutura_geom::dressing::dressing_coat`.  This module owns only the tier:
//! preset resolution, the closed-loop adaptive drain, post-coat component
//! cleanup, decimation rollback, P-WELD and the adoption gate/report.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Map, Value};
use sutura_geom::decimate::decimate_impl;
use sutura_geom::dressing::{
    dressing_coat as coat_core, CorrectionGrid, DressingInfo, DressingParams,
};
use sutura_geom::morph;
use sutura_geom::pointcloud;
use sutura_geom::topology::compute_normal_per_vertex_core;
use sutura_geom::winding::MeshBvh;

use crate::graft::helpers::{median_edge_length as unique_median_edge_length, np_pairwise_sum};
use crate::graft::shell::sample_surface_seeded;
use crate::report::defects::detect_defects;
use crate::tiers::common;

const DEFAULT_INTENSITY: &str = "balanced";
const FACE_BUDGET_MIN: usize = 50_000;
const FACE_BUDGET_MAX: usize = 500_000;
const CLEANUP_MIN_FACES: usize = 64;
const CLEANUP_ABS_VOXELS: f64 = 32.0;
const CLEANUP_REL_VOLUME: f64 = 1e-3;

const ADAPTIVE_DRAIN_ENV: &str = "SUTURA_DRESSING_ADAPTIVE_DRAIN";
const ADAPTIVE_DRAIN_ALPHA: f64 = 0.5;
const ADAPTIVE_DRAIN_K_CAP: f64 = 2.0;
const ADAPTIVE_DRAIN_SIGMA_VOX: f64 = 1.5;
const ADAPTIVE_DRAIN_MAX_ITERS: usize = 3;
const ADAPTIVE_DRAIN_G_ZERO: f64 = 0.5;
const ADAPTIVE_DRAIN_IMPROVE_EPS: f64 = 1e-3;
const ADAPTIVE_DRAIN_TANGENT_EPS: f64 = 0.05;
const ADAPTIVE_DRAIN_BAND_VOXELS: f64 = 2.0;
const ADAPTIVE_DRAIN_TARGET_VOXELS: f64 = 0.5;
const ADAPTIVE_DRAIN_MIN_SHIFT_FRAC: f64 = 0.25;
const ADAPTIVE_DRAIN_PARTS_SLACK: usize = 0;

const HAUSDORFF_SAMPLES: usize = 4000;
const HAUSDORFF_SEED: u64 = 12_345;

#[derive(Clone, Copy, Debug)]
struct IntensitySpec {
    voxel_div: (f64, f64),
    r_preset: f64,
    max_hausdorff: f64,
    time_budget: f64,
    drain_factor: f64,
}

fn spec(intensity: &str) -> IntensitySpec {
    match intensity {
        "quick" => IntensitySpec {
            voxel_div: (120.0, 200.0),
            r_preset: 4.0,
            max_hausdorff: 0.0150,
            time_budget: 30.0,
            drain_factor: 0.0,
        },
        "thorough" => IntensitySpec {
            voxel_div: (300.0, 500.0),
            r_preset: 1.8,
            max_hausdorff: 0.0040,
            time_budget: 180.0,
            drain_factor: 1.0,
        },
        "extreme" => IntensitySpec {
            voxel_div: (450.0, 800.0),
            r_preset: 1.2,
            max_hausdorff: 0.0025,
            time_budget: 300.0,
            drain_factor: 1.25,
        },
        _ => IntensitySpec {
            voxel_div: (200.0, 350.0),
            r_preset: 2.5,
            max_hausdorff: 0.0080,
            time_budget: 60.0,
            drain_factor: 1.0,
        },
    }
}

fn grid_budget(intensity: &str) -> f64 {
    match intensity {
        "quick" => 2_000_000.0,
        "thorough" => 20_000_000.0,
        "extreme" => 40_000_000.0,
        _ => 8_000_000.0,
    }
}

fn drain_modes() -> [(&'static str, f64); 5] {
    [
        ("none", 0.0),
        ("off", 0.0),
        ("half", 0.5),
        ("full", 1.0),
        ("deep", 1.25),
    ]
}

fn drain_mode_name(factor: f64) -> &'static str {
    for (name, value) in drain_modes() {
        if (value - factor).abs() < 1e-9 {
            return name;
        }
    }
    "custom"
}

fn python_round(x: f64, digits: usize) -> f64 {
    format!("{:.*}", digits, x).parse::<f64>().unwrap_or(x)
}

#[inline]
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn bbox_diagonal(v: &[[f64; 3]]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut lo = v[0];
    let mut hi = v[0];
    for p in v {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let d = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    dot(d, d).sqrt()
}

fn bbox(v: &[[f64; 3]]) -> ([f64; 3], [f64; 3]) {
    if v.is_empty() {
        return ([0.0; 3], [0.0; 3]);
    }
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

fn face_normals(v: &[[f64; 3]], t: &[[i64; 3]]) -> Vec<[f64; 3]> {
    let mut out = Vec::with_capacity(t.len());
    for tri in t {
        let a = v[tri[0] as usize];
        let b = v[tri[1] as usize];
        let c = v[tri[2] as usize];
        let n = cross(
            [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
            [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
        );
        let len = dot(n, n).sqrt();
        out.push(if len > 0.0 {
            [n[0] / len, n[1] / len, n[2] / len]
        } else {
            [0.0, 0.0, 0.0]
        });
    }
    out
}

/// Area-weighted per-vertex normals, matching `dressing._vertex_normals`
/// (isolated vertices fall back to +Z).
fn vertex_normals(v: &[[f64; 3]], t: &[[i64; 3]]) -> Vec<[f64; 3]> {
    let mut vn = vec![[0.0f64; 3]; v.len()];
    for tri in t {
        let a = v[tri[0] as usize];
        let b = v[tri[1] as usize];
        let c = v[tri[2] as usize];
        let n = cross(
            [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
            [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
        );
        for &idx in tri {
            vn[idx as usize][0] += n[0];
            vn[idx as usize][1] += n[1];
            vn[idx as usize][2] += n[2];
        }
    }
    let mut out = Vec::with_capacity(vn.len());
    for n in vn {
        let len = dot(n, n).sqrt();
        if len > 0.0 {
            out.push([n[0] / len, n[1] / len, n[2] / len]);
        } else {
            out.push([0.0, 0.0, 1.0]);
        }
    }
    out
}

fn exact_si(v: &[[f64; 3]], t: &[[i64; 3]]) -> Option<i64> {
    if t.is_empty() {
        return None;
    }
    common::exact_si_count(v, t)
}

fn count_components(v: &[[f64; 3]], t: &[[i64; 3]]) -> usize {
    common::count_vertex_components(v.len(), t)
}

fn percentile_linear(values: &[f64], pct: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    if n == 1 {
        return sorted[0];
    }
    let rank = (pct / 100.0) * (n as f64 - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        let frac = rank - lo as f64;
        sorted[lo] + frac * (sorted[hi] - sorted[lo])
    }
}

fn normalize_scale(scale: Option<f64>) -> Option<f64> {
    scale.filter(|s| *s > 0.0)
}

fn resolve_dressing_voxel(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    diag: f64,
    intensity: &str,
    budget: Option<f64>,
) -> f64 {
    let s = spec(intensity);
    let (div_min, div_max) = s.voxel_div;
    if !(diag > 0.0) {
        return 1.0;
    }
    let med = unique_median_edge_length(verts, tris);
    let v_feature = if med > 0.0 { med / 1.5 } else { diag / 200.0 };
    let v_preset_min = diag / div_max;
    let v_preset_max = diag / div_min;
    let mut v_target = if tris.len() < 500 {
        v_feature.max(v_preset_min)
    } else {
        v_feature.clamp(v_preset_min, v_preset_max)
    };
    if let Some(budget) = budget {
        if !verts.is_empty() {
            let (lo, hi) = bbox(verts);
            let ext = [
                (hi[0] - lo[0]).max(1e-12),
                (hi[1] - lo[1]).max(1e-12),
                (hi[2] - lo[2]).max(1e-12),
            ];
            for _ in 0..120 {
                let dims = [
                    (ext[0] / v_target).ceil() as i128 + 6,
                    (ext[1] / v_target).ceil() as i128 + 6,
                    (ext[2] / v_target).ceil() as i128 + 6,
                ];
                if dims[0] * dims[1] * dims[2] <= budget as i128 {
                    break;
                }
                v_target *= 1.15;
            }
        }
    }
    v_target
}

fn resolve_viscosity(
    ell_median: f64,
    voxel: f64,
    intensity: &str,
    gap: Option<f64>,
    r_max_scale: Option<f64>,
    sigma_scale: Option<f64>,
) -> (f64, f64, f64) {
    let r_preset = spec(intensity).r_preset;
    let r_base = (0.3 * ell_median).clamp(0.25 * voxel, 0.8 * voxel);
    let mut r_max = if ell_median > 0.0 {
        r_preset.min(3.5 * ell_median)
    } else {
        r_preset
    };
    if let Some(gap) = gap {
        if gap > 0.0 {
            r_max = r_max.min(0.45 * gap);
        }
    }
    if let Some(scale) = r_max_scale {
        r_max *= scale;
    }
    r_max = r_max.max(r_base);
    let mut sigma = (2.0 * ell_median).max(2.0 * voxel);
    if let Some(scale) = sigma_scale {
        sigma *= scale;
    }
    (r_base, r_max, sigma)
}

fn resolve_drain(
    drain: Option<&Value>,
    intensity: &str,
    r_base: f64,
) -> (f64, Option<f64>, String) {
    match drain {
        None | Some(Value::Null) => {
            let factor = spec(intensity).drain_factor;
            (
                factor * r_base,
                Some(factor),
                drain_mode_name(factor).to_string(),
            )
        }
        Some(Value::String(s)) => {
            let key = s.trim().to_lowercase();
            let (mode, factor) = drain_modes()
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(name, value)| (*name, *value))
                .unwrap_or(("none", 0.0));
            (factor * r_base, Some(factor), mode.to_string())
        }
        Some(value) => {
            let delta = value.as_f64().unwrap_or(0.0);
            (delta, None, "custom".to_string())
        }
    }
}

fn defect_vertex_indices(
    v: &[[f64; 3]],
    t: &[[i64; 3]],
    defects: Option<&str>,
) -> (Vec<usize>, f64) {
    let mode = match defects {
        Some("holes_nm") => "holes_nm",
        _ => "all",
    };
    let mut idx: HashSet<i64> = HashSet::new();
    let mut gap = 0.0f64;
    let v32: Vec<[f32; 3]> = v
        .iter()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    let t32: Vec<[i32; 3]> = t
        .iter()
        .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
        .collect();
    let det = detect_defects(&v32, &t32, true);
    for hole in &det.holes {
        if let Some(ids) = &hole.verts_idx {
            idx.extend(ids.iter().copied());
        }
        gap = gap.max(hole.diameter);
    }
    for nm in &det.non_manifold {
        if let Some(ids) = &nm.verts_idx {
            idx.extend(ids.iter().copied());
        }
    }
    if mode == "all" && t.len() <= sutura_geom::SI_MAX_FACES {
        let ut = common::to_usize_tris(t);
        let (mask, _count, skipped) = sutura_geom::self_intersections_core(v, &ut);
        if !skipped {
            for (fi, hit) in mask.iter().enumerate() {
                if *hit {
                    for &vi in &t[fi] {
                        idx.insert(vi);
                    }
                }
            }
        }
    }
    let mut out: Vec<usize> = idx
        .into_iter()
        .filter(|i| *i >= 0)
        .map(|i| i as usize)
        .collect();
    out.sort_unstable();
    (out, gap)
}

fn variable_radius_g(
    dims: [usize; 3],
    origin: [f64; 3],
    voxel: f64,
    defect_idx: &[usize],
    verts: &[[f64; 3]],
    sigma: f64,
) -> Vec<f32> {
    let (dx, dy, dz) = (dims[0], dims[1], dims[2]);
    let n = dx * dy * dz;
    if defect_idx.is_empty() {
        return vec![0.0f32; n];
    }
    let mut mask = vec![0u8; n];
    let vox = voxel as f32;
    let sig = sigma as f32;
    for &vi in defect_idx {
        let p = verts[vi];
        let i = (((p[0] - origin[0]) / voxel).round_ties_even() as i64).clamp(0, dx as i64 - 1)
            as usize;
        let j = (((p[1] - origin[1]) / voxel).round_ties_even() as i64).clamp(0, dy as i64 - 1)
            as usize;
        let k = (((p[2] - origin[2]) / voxel).round_ties_even() as i64).clamp(0, dz as i64 - 1)
            as usize;
        mask[i + dx * (j + dy * k)] = 1;
    }
    let d = morph::edt_grid(&mask, dims);
    let mut g = Vec::with_capacity(n);
    for value in d {
        let df = value * vox;
        let z = df / sig;
        g.push((-0.5f32 * z * z).exp());
    }
    g
}

fn nearest_grid_values(
    grid: &[f32],
    pts: &[[f64; 3]],
    origin: [f64; 3],
    voxel: f64,
    dims: [usize; 3],
) -> Vec<f32> {
    let (dx, dy, dz) = (dims[0], dims[1], dims[2]);
    let mut out = Vec::with_capacity(pts.len());
    for p in pts {
        let i = (((p[0] - origin[0]) / voxel).round_ties_even() as i64).clamp(0, dx as i64 - 1)
            as usize;
        let j = (((p[1] - origin[1]) / voxel).round_ties_even() as i64).clamp(0, dy as i64 - 1)
            as usize;
        let k = (((p[2] - origin[2]) / voxel).round_ties_even() as i64).clamp(0, dz as i64 - 1)
            as usize;
        out.push(grid[i + dx * (j + dy * k)]);
    }
    out
}

fn signed_deviation(
    v0: &[[f64; 3]],
    t0: &[[i64; 3]],
    cv: &[[f64; 3]],
    bvh: &MeshBvh,
    fnormals: &[[f64; 3]],
    defect_set: &[bool],
) -> (Vec<f64>, Vec<bool>, Vec<f64>) {
    if cv.is_empty() || t0.is_empty() {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let cp = common::closest_points(v0, t0, cv);
    let mut dev = Vec::with_capacity(cv.len());
    let mut healthy = vec![true; cv.len()];
    let mut dists = Vec::with_capacity(cv.len());
    let mut sign = Vec::with_capacity(cv.len());
    let mut amb_idx: Vec<usize> = Vec::new();
    for (i, (q, dist, fid)) in cp.iter().enumerate() {
        let good = *fid >= 0 && (*fid as usize) < fnormals.len();
        let fi = if good { *fid as usize } else { 0 };
        let dvec = [cv[i][0] - q[0], cv[i][1] - q[1], cv[i][2] - q[2]];
        let dots = if good { dot(dvec, fnormals[fi]) } else { 0.0 };
        let s = if dots >= 0.0 { 1.0 } else { -1.0 };
        let cos = if *dist > 0.0 {
            dots / dist.max(1e-30)
        } else {
            0.0
        };
        if cos.abs() < ADAPTIVE_DRAIN_TANGENT_EPS {
            amb_idx.push(i);
        }
        sign.push(s);
        dists.push(*dist);
        if defect_set.is_empty() {
            healthy[i] = good;
        } else {
            healthy[i] = good
                && !t0[fi]
                    .iter()
                    .any(|vi| *vi >= 0 && defect_set.get(*vi as usize).copied().unwrap_or(false));
        }
    }
    if !amb_idx.is_empty() {
        let pts: Vec<[f64; 3]> = amb_idx.iter().map(|&i| cv[i]).collect();
        let w = bvh.winding_points_par(&pts);
        for (k, &i) in amb_idx.iter().enumerate() {
            sign[i] = if w.get(k).copied().unwrap_or(0.0) > 0.5 {
                -1.0
            } else {
                1.0
            };
        }
    }
    for i in 0..cv.len() {
        dev.push(sign[i] * dists[i]);
    }
    (dev, healthy, dists)
}

#[derive(Clone, Debug, Default)]
struct IterationStats {
    n: usize,
    median: Option<f64>,
    p95: Option<f64>,
    mean_abs: Option<f64>,
}

fn iteration_stats(dev: &[f64], healthy: &[bool]) -> IterationStats {
    let vals: Vec<f64> = dev
        .iter()
        .zip(healthy)
        .filter(|(_, h)| **h)
        .map(|(d, _)| d.abs())
        .collect();
    if vals.is_empty() {
        return IterationStats::default();
    }
    IterationStats {
        n: vals.len(),
        median: Some(percentile_linear(&vals, 50.0)),
        p95: Some(percentile_linear(&vals, 95.0)),
        mean_abs: Some(np_pairwise_sum(&vals) / vals.len() as f64),
    }
}

fn objective(
    stats: &IterationStats,
    holes: usize,
    nm: usize,
    parts: usize,
) -> (f64, f64, usize, usize) {
    let inf = f64::INFINITY;
    (
        stats.p95.unwrap_or(inf),
        stats.median.unwrap_or(inf),
        holes + nm,
        parts,
    )
}

fn objective_lt(a: &(f64, f64, usize, usize), b: &(f64, f64, usize, usize)) -> bool {
    a.0 < b.0
        || (a.0 == b.0 && (a.1 < b.1 || (a.1 == b.1 && (a.2 < b.2 || (a.2 == b.2 && a.3 < b.3)))))
}

fn gaussian_kernel(sigma: f64) -> Vec<f32> {
    let raw = (3.0 * sigma).round_ties_even() as i64;
    let r = raw.max(1) as i64;
    let mut k: Vec<f64> = (-r..=r)
        .map(|x| (-0.5 * (x as f64 / sigma).powi(2)).exp())
        .collect();
    let sum: f64 = k.iter().sum();
    for value in k.iter_mut() {
        *value /= sum;
    }
    k.into_iter().map(|x| x as f32).collect()
}

fn blur_axis(a: &[f32], dims: [usize; 3], k: &[f32], axis: usize) -> Vec<f32> {
    let (dx, dy, dz) = (dims[0], dims[1], dims[2]);
    let r = (k.len() / 2) as i64;
    let mut out = vec![0.0f32; a.len()];
    for z in 0..dz {
        for y in 0..dy {
            for x in 0..dx {
                let idx = x + dx * (y + dy * z);
                let mut acc = 0.0f32;
                for (tap, &w) in k.iter().enumerate() {
                    let off = tap as i64 - r;
                    let (mut ix, mut iy, mut iz) = (x as i64, y as i64, z as i64);
                    match axis {
                        0 => ix += off,
                        1 => iy += off,
                        _ => iz += off,
                    }
                    if ix >= 0
                        && iy >= 0
                        && iz >= 0
                        && (ix as usize) < dx
                        && (iy as usize) < dy
                        && (iz as usize) < dz
                    {
                        let src = ix as usize + dx * (iy as usize + dy * iz as usize);
                        acc += w * a[src];
                    }
                }
                out[idx] = acc;
            }
        }
    }
    out
}

fn gaussian_blur3(a: &[f32], dims: [usize; 3], sigma: f64) -> Vec<f32> {
    let k = gaussian_kernel(sigma);
    let mut out = a.to_vec();
    for axis in 0..3 {
        out = blur_axis(&out, dims, &k, axis);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn build_correction(
    dev: &[f64],
    healthy: &[bool],
    cv: &[[f64; 3]],
    origin: [f64; 3],
    dims: [usize; 3],
    voxel: f64,
    g: Option<&[f32]>,
) -> Vec<f32> {
    let (dx, dy, dz) = (dims[0], dims[1], dims[2]);
    let n = dx * dy * dz;
    let mut num = vec![0.0f32; n];
    let mut cnt = vec![0.0f32; n];
    let mut any = false;
    for i in 0..cv.len() {
        if !healthy.get(i).copied().unwrap_or(false) || dev[i].abs() <= 0.0 {
            continue;
        }
        let p = cv[i];
        let ix = ((p[0] - origin[0]) / voxel).round_ties_even() as i64;
        let iy = ((p[1] - origin[1]) / voxel).round_ties_even() as i64;
        let iz = ((p[2] - origin[2]) / voxel).round_ties_even() as i64;
        if ix < 0 || iy < 0 || iz < 0 || ix as usize >= dx || iy as usize >= dy || iz as usize >= dz
        {
            continue;
        }
        let flat = ix as usize + dx * (iy as usize + dy * iz as usize);
        num[flat] += dev[i] as f32;
        cnt[flat] += 1.0f32;
        any = true;
    }
    if !any {
        return vec![0.0f32; n];
    }
    let num = gaussian_blur3(&num, dims, ADAPTIVE_DRAIN_SIGMA_VOX);
    let cnt = gaussian_blur3(&cnt, dims, ADAPTIVE_DRAIN_SIGMA_VOX);
    let alpha = ADAPTIVE_DRAIN_ALPHA as f32;
    let cap = (ADAPTIVE_DRAIN_K_CAP * voxel) as f32;
    let mut delta = vec![0.0f32; n];
    for i in 0..n {
        if cnt[i] > 1e-6 {
            let mut d = (num[i] / cnt[i].max(1e-6)) * alpha;
            d = d.clamp(-cap, cap);
            if let Some(g) = g {
                if g[i] as f64 > ADAPTIVE_DRAIN_G_ZERO {
                    d = 0.0;
                }
            }
            delta[i] = d;
        }
    }
    delta
}

fn adaptive_enabled() -> bool {
    match std::env::var(ADAPTIVE_DRAIN_ENV) {
        Ok(value) => {
            let v = value.trim().to_lowercase();
            !matches!(v.as_str(), "0" | "false" | "no" | "off")
        }
        Err(_) => true,
    }
}

struct ExtractResult {
    verts: Vec<[f64; 3]>,
    tris: Vec<[i64; 3]>,
    info: DressingInfo,
}

#[allow(clippy::too_many_arguments)]
fn extract_iteration(
    v0: &[[f64; 3]],
    t0: &[[i64; 3]],
    voxel: f64,
    r_base: f64,
    r_max: f64,
    sigma: f64,
    defect_idx: &[usize],
    drain: f64,
    corr: Option<&CorrectionGrid>,
) -> ExtractResult {
    let defect_pts: Vec<[f64; 3]> = defect_idx.iter().map(|&i| v0[i]).collect();
    let params = DressingParams {
        voxel: Some(voxel),
        r_base: Some(r_base),
        r_max: Some(r_max),
        sigma: Some(sigma),
        band_voxels: 2.0,
        margin_voxels: 3.0,
        drain,
    };
    let ut = common::to_usize_tris(t0);
    let m = coat_core(v0, &ut, &defect_pts, &params, corr);
    extract_from(m)
}

fn extract_from(m: sutura_geom::dressing::DressingMesh) -> ExtractResult {
    let tris = m
        .tris
        .iter()
        .map(|r| [r[0] as i64, r[1] as i64, r[2] as i64])
        .collect();
    ExtractResult {
        verts: m.verts,
        tris,
        info: m.info,
    }
}

#[allow(clippy::too_many_arguments)]
fn adaptive_coat(
    v0: &[[f64; 3]],
    t0: &[[i64; 3]],
    voxel: f64,
    r_base: f64,
    r_max: f64,
    sigma: f64,
    defect_idx: &[usize],
    drain: f64,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, DressingInfo, Value) {
    let bvh = MeshBvh::from_arrays(v0, &common::to_usize_tris(t0));
    let fnormals = face_normals(v0, t0);
    let mut defect_set = vec![false; v0.len()];
    for &i in defect_idx {
        if i < defect_set.len() {
            defect_set[i] = true;
        }
    }

    let iter0 = extract_iteration(v0, t0, voxel, r_base, r_max, sigma, defect_idx, drain, None);
    let meta0 = iter0.info.clone();
    let dims = meta0.dims;
    let origin = meta0.origin;
    let g_voxel = meta0.voxel;
    let g0 = if dims[0] > 0 && dims[1] > 0 && dims[2] > 0 {
        Some(variable_radius_g(
            dims,
            origin,
            g_voxel,
            defect_idx,
            v0,
            meta0.sigma,
        ))
    } else {
        None
    };
    let threshold = meta0.r_base + ADAPTIVE_DRAIN_BAND_VOXELS * g_voxel;

    let measure = |cv: &[[f64; 3]], ct: &[[i64; 3]]| {
        let (dev, mut healthy, dist) = signed_deviation(v0, t0, cv, &bvh, &fnormals, &defect_set);
        let nv = ct
            .iter()
            .flat_map(|r| r.iter().copied())
            .max()
            .map(|m| m + 1)
            .unwrap_or(0)
            .max(0) as usize;
        let (ncomp, labels) = common::component_labels(nv, ct);
        if !labels.is_empty() && !cv.is_empty() {
            let near: Vec<bool> = dist.iter().map(|d| *d <= threshold).collect();
            let mut vlab = vec![-1i64; cv.len()];
            for tri in ct {
                let face_label = labels.get(tri[0] as usize).copied().unwrap_or(-1);
                for &vi in tri {
                    if vi >= 0 && (vi as usize) < vlab.len() {
                        vlab[vi as usize] = face_label;
                    }
                }
            }
            let mut cnt = vec![0usize; ncomp];
            for i in 0..vlab.len() {
                if vlab[i] >= 0 && near[i] {
                    cnt[vlab[i] as usize] += 1;
                }
            }
            let keep_lab: Vec<bool> = cnt.iter().map(|c| *c > 0).collect();
            for i in 0..healthy.len() {
                if vlab[i] >= 0 {
                    healthy[i] = healthy[i] && keep_lab[vlab[i] as usize];
                } else {
                    healthy[i] = false;
                }
            }
        }
        if let Some(g) = &g0 {
            if !cv.is_empty() {
                let gv = nearest_grid_values(g, cv, origin, g_voxel, dims);
                for i in 0..healthy.len() {
                    healthy[i] = healthy[i] && gv[i] as f64 <= ADAPTIVE_DRAIN_G_ZERO;
                }
            }
        }
        let stats = iteration_stats(&dev, &healthy);
        (dev, healthy, stats)
    };

    let (dev0, healthy0, st0) = measure(&iter0.verts, &iter0.tris);
    let (h0, nm0) = common::reload_strict_holes_nm(&iter0.verts, &iter0.tris);
    let p0 = count_components(&iter0.verts, &iter0.tris);
    let mut best = BestIteration {
        iter: 0,
        verts: iter0.verts,
        tris: iter0.tris,
        info: iter0.info,
        dev: dev0,
        healthy: healthy0,
        corrected: false,
        objective: objective(&st0, h0, nm0, p0),
    };

    let mut detail: Vec<Value> = vec![json!({
        "iteration": 0,
        "corrected": false,
        "engine": "rust",
        "median": st0.median,
        "p95": st0.p95,
        "mean_abs": st0.mean_abs,
        "n": st0.n,
        "holes": h0,
        "non_manifold": nm0,
        "parts": p0,
        "valid": true,
    })];

    let make_loop = |detail: &[Value], chosen: usize, corrected: bool| -> Value {
        json!({
            "adaptive": true,
            "iterations": detail.len(),
            "chosen_iteration": chosen,
            "correction_applied": corrected,
            "alpha": ADAPTIVE_DRAIN_ALPHA,
            "k_cap": ADAPTIVE_DRAIN_K_CAP,
            "sigma_voxels": ADAPTIVE_DRAIN_SIGMA_VOX,
            "engine": "rust",
            "iterations_detail": detail,
        })
    };

    if st0.n == 0
        || st0
            .p95
            .map(|p| p <= ADAPTIVE_DRAIN_TARGET_VOXELS * g_voxel)
            .unwrap_or(false)
    {
        let loop_report = make_loop(&detail, 0, false);
        return (best.verts, best.tris, best.info, loop_report);
    }
    let delta0 = build_correction(
        &best.dev,
        &best.healthy,
        &best.verts,
        origin,
        dims,
        g_voxel,
        g0.as_deref(),
    );
    let max0 = delta0.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    if (max0 as f64) < ADAPTIVE_DRAIN_MIN_SHIFT_FRAC * g_voxel {
        let loop_report = make_loop(&detail, 0, false);
        return (best.verts, best.tris, best.info, loop_report);
    }

    let mut corr_total = vec![0.0f32; dims[0] * dims[1] * dims[2]];
    for k in 1..=ADAPTIVE_DRAIN_MAX_ITERS {
        let delta = build_correction(
            &best.dev,
            &best.healthy,
            &best.verts,
            origin,
            dims,
            g_voxel,
            g0.as_deref(),
        );
        if !delta.iter().any(|x| *x != 0.0) {
            break;
        }
        for (a, b) in corr_total.iter_mut().zip(delta.iter()) {
            *a += *b;
        }
        let corr = CorrectionGrid {
            origin,
            dims,
            voxel: g_voxel,
            values: corr_total.clone(),
        };
        let iter = extract_iteration(
            v0,
            t0,
            voxel,
            r_base,
            r_max,
            sigma,
            defect_idx,
            drain,
            Some(&corr),
        );
        if !iter.info.correction_applied {
            break;
        }
        let (devk, healthyk, stk) = measure(&iter.verts, &iter.tris);
        let (hk, nmk) = common::reload_strict_holes_nm(&iter.verts, &iter.tris);
        let pk = count_components(&iter.verts, &iter.tris);
        let valid = hk <= h0 && nmk <= nm0 && pk <= p0 + ADAPTIVE_DRAIN_PARTS_SLACK;
        let obj = objective(&stk, hk, nmk, pk);
        detail.push(json!({
            "iteration": k,
            "corrected": true,
            "engine": "rust",
            "median": stk.median,
            "p95": stk.p95,
            "mean_abs": stk.mean_abs,
            "n": stk.n,
            "holes": hk,
            "non_manifold": nmk,
            "parts": pk,
            "valid": valid,
        }));
        if !valid || !objective_lt(&obj, &best.objective) {
            break;
        }
        let prev_p95 = best.objective.0;
        let prev_med = best.objective.1;
        best = BestIteration {
            iter: k,
            verts: iter.verts,
            tris: iter.tris,
            info: iter.info,
            dev: devk,
            healthy: healthyk,
            corrected: true,
            objective: obj,
        };
        if prev_p95 - obj.0 < ADAPTIVE_DRAIN_IMPROVE_EPS
            && prev_med - obj.1 < ADAPTIVE_DRAIN_IMPROVE_EPS
        {
            break;
        }
    }

    let loop_report = make_loop(&detail, best.iter, best.corrected);
    (best.verts, best.tris, best.info, loop_report)
}

struct BestIteration {
    iter: usize,
    verts: Vec<[f64; 3]>,
    tris: Vec<[i64; 3]>,
    info: DressingInfo,
    dev: Vec<f64>,
    healthy: Vec<bool>,
    corrected: bool,
    objective: (f64, f64, usize, usize),
}

/// Coherent face orientation + positive signed volume, matching
/// `dressing._reorient_faces`.
fn reorient_faces(v: &[[f64; 3]], t: &[[i64; 3]]) -> Vec<[i64; 3]> {
    let mut tris = t.to_vec();
    if tris.is_empty() {
        return tris;
    }
    let mut edge_to_faces: HashMap<(i64, i64), Vec<(usize, i8)>> = HashMap::new();
    for (f, tri) in tris.iter().enumerate() {
        let (a, b, c) = (tri[0], tri[1], tri[2]);
        for (u, w) in [(a, b), (b, c), (c, a)] {
            let key = if u < w { (u, w) } else { (w, u) };
            edge_to_faces
                .entry(key)
                .or_default()
                .push((f, if u < w { 1 } else { -1 }));
        }
    }
    let mut flip = vec![false; tris.len()];
    let mut seen = vec![false; tris.len()];
    for start in 0..tris.len() {
        if seen[start] {
            continue;
        }
        seen[start] = true;
        let mut stack = vec![start];
        while let Some(f) = stack.pop() {
            let (a, b, c) = (tris[f][0], tris[f][1], tris[f][2]);
            for (u, w) in [(a, b), (b, c), (c, a)] {
                let key = if u < w { (u, w) } else { (w, u) };
                let mut dirf = if u < w { 1i8 } else { -1i8 };
                if flip[f] {
                    dirf = -dirf;
                }
                if let Some(neigh) = edge_to_faces.get(&key) {
                    for &(g, dirg) in neigh {
                        if g == f || seen[g] {
                            continue;
                        }
                        flip[g] = dirg != -dirf;
                        seen[g] = true;
                        stack.push(g);
                    }
                }
            }
        }
    }
    for (fi, &flipped) in flip.iter().enumerate() {
        if flipped {
            tris[fi].swap(1, 2);
        }
    }
    let mut vol = 0.0f64;
    for tri in &tris {
        let a = v[tri[0] as usize];
        let b = v[tri[1] as usize];
        let c = v[tri[2] as usize];
        vol += dot(a, cross(b, c)) / 6.0;
    }
    if vol < 0.0 {
        for tri in tris.iter_mut() {
            tri.swap(1, 2);
        }
    }
    tris
}

fn clean_coat_components(
    v0: &[[f64; 3]],
    t0: &[[i64; 3]],
    cv: &[[f64; 3]],
    ct: &[[i64; 3]],
    voxel: f64,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let mut info = json!({
        "ran": true,
        "components_before": 0,
        "components_after": 0,
        "kept_positive": 0,
        "kept_cavities": 0,
        "removed_debris": 0,
        "removed_negative": 0,
        "removed_other": 0,
        "removed_faces": 0,
        "removed_volume": 0.0,
        "min_faces": CLEANUP_MIN_FACES,
        "abs_voxels": CLEANUP_ABS_VOXELS,
        "rel_volume": CLEANUP_REL_VOLUME,
        "input_winding": false,
        "reason": Value::Null,
    });
    if ct.is_empty() {
        info["reason"] = Value::from("empty");
        return (cv.to_vec(), ct.to_vec(), info);
    }
    let nv = ct
        .iter()
        .flat_map(|r| r.iter().copied())
        .max()
        .map(|m| (m + 1).max(0) as usize)
        .unwrap_or(0);
    let (_ncomp_all, node_labels) = common::component_labels(nv, ct);
    let face_labels: Vec<i64> = ct
        .iter()
        .map(|tri| node_labels.get(tri[0] as usize).copied().unwrap_or(0))
        .collect();
    let mut uniq: Vec<i64> = face_labels.clone();
    uniq.sort_unstable();
    uniq.dedup();
    let remap: HashMap<i64, usize> = uniq.iter().enumerate().map(|(i, l)| (*l, i)).collect();
    let inv: Vec<usize> = face_labels.iter().map(|l| remap[l]).collect();
    let ncomp = uniq.len();
    info["components_before"] = Value::from(ncomp);

    let mut cvol = vec![0.0f64; ncomp];
    let mut cfaces = vec![0usize; ncomp];
    let mut cmin = vec![[f64::INFINITY; 3]; ncomp];
    let mut cmax = vec![[f64::NEG_INFINITY; 3]; ncomp];
    let mut ccent = vec![[0.0f64; 3]; ncomp];
    for (fi, tri) in ct.iter().enumerate() {
        let a = cv[tri[0] as usize];
        let b = cv[tri[1] as usize];
        let c = cv[tri[2] as usize];
        let ci = inv[fi];
        cvol[ci] += dot(a, cross(b, c)) / 6.0;
        cfaces[ci] += 1;
        for k in 0..3 {
            ccent[ci][k] += (a[k] + b[k] + c[k]) / 3.0;
        }
        for p in [a, b, c] {
            for k in 0..3 {
                cmin[ci][k] = cmin[ci][k].min(p[k]);
                cmax[ci][k] = cmax[ci][k].max(p[k]);
            }
        }
    }
    for ci in 0..ncomp {
        let f = cfaces[ci] as f64;
        if f > 0.0 {
            for k in 0..3 {
                ccent[ci][k] /= f;
            }
        }
    }

    let largest = cvol.iter().fold(0.0f64, |m, x| m.max(x.abs()));
    let floor = (CLEANUP_ABS_VOXELS * voxel.powi(3)).max(CLEANUP_REL_VOLUME * largest);
    let debris: Vec<bool> = (0..ncomp)
        .map(|i| cfaces[i] < CLEANUP_MIN_FACES || cvol[i].abs() < floor)
        .collect();
    let (in_min, in_max) = bbox(cv);
    let tol = voxel;
    let positive: Vec<bool> = cvol.iter().map(|x| *x > 0.0).collect();
    let mut keep = vec![false; ncomp];
    for i in 0..ncomp {
        if debris[i] {
            continue;
        }
        let inside_bbox = (0..3).all(|k| cmax[i][k] >= in_min[k] - tol)
            && (0..3).all(|k| cmin[i][k] <= in_max[k] + tol);
        if !inside_bbox {
            continue;
        }
        keep[i] = positive[i];
    }

    let neg_ids: Vec<usize> = (0..ncomp).filter(|&i| !positive[i] && !debris[i]).collect();
    let kept_pos: Vec<usize> = (0..ncomp).filter(|&i| keep[i]).collect();
    if !neg_ids.is_empty() && !kept_pos.is_empty() {
        let pts: Vec<[f64; 3]> = neg_ids.iter().map(|&i| ccent[i]).collect();
        let mut winding: Option<Vec<f64>> = None;
        if !v0.is_empty() && !t0.is_empty() {
            let w = common::winding_points(v0, t0, &pts);
            if !w.is_empty() {
                info["input_winding"] = Value::Bool(true);
                winding = Some(w);
            }
        }
        for (k, &i) in neg_ids.iter().enumerate() {
            let nested = kept_pos.iter().any(|&p| {
                (0..3).all(|d| cmin[i][d] >= cmin[p][d] - tol)
                    && (0..3).all(|d| cmax[i][d] <= cmax[p][d] + tol)
            });
            if !nested {
                continue;
            }
            if winding
                .as_ref()
                .map(|w| w.get(k).copied().unwrap_or(0.0) < 0.5)
                .unwrap_or(true)
            {
                keep[i] = true;
            }
        }
    }

    info["kept_positive"] = Value::from((0..ncomp).filter(|&i| keep[i] && positive[i]).count());
    info["kept_cavities"] = Value::from((0..ncomp).filter(|&i| keep[i] && !positive[i]).count());
    info["components_after"] = Value::from(keep.iter().filter(|x| **x).count());

    let face_keep: Vec<bool> = inv.iter().map(|&ci| keep[ci]).collect();
    if !face_keep.iter().any(|x| *x) {
        info["ran"] = Value::Bool(false);
        info["reason"] = Value::from("cleanup would remove all geometry");
        return (cv.to_vec(), ct.to_vec(), info);
    }
    if face_keep.iter().all(|x| *x) {
        return (cv.to_vec(), ct.to_vec(), info);
    }
    info["removed_debris"] = Value::from((0..ncomp).filter(|&i| !keep[i] && debris[i]).count());
    info["removed_negative"] = Value::from(
        (0..ncomp)
            .filter(|&i| !keep[i] && !positive[i] && !debris[i])
            .count(),
    );
    info["removed_other"] = Value::from(
        (0..ncomp)
            .filter(|&i| !keep[i] && positive[i] && !debris[i])
            .count(),
    );
    info["removed_faces"] = Value::from(
        (0..ncomp)
            .filter(|&i| !keep[i])
            .map(|i| cfaces[i])
            .sum::<usize>(),
    );
    info["removed_volume"] = Value::from(
        (0..ncomp)
            .filter(|&i| !keep[i])
            .map(|i| cvol[i].abs())
            .sum::<f64>(),
    );

    let kept_tris: Vec<[i64; 3]> = ct
        .iter()
        .zip(&face_keep)
        .filter(|(_, keep)| **keep)
        .map(|(tri, _)| *tri)
        .collect();
    let mut used: Vec<i64> = kept_tris.iter().flat_map(|r| r.iter().copied()).collect();
    used.sort_unstable();
    used.dedup();
    let mut remap_v: HashMap<i64, i64> = HashMap::new();
    for (i, &old) in used.iter().enumerate() {
        remap_v.insert(old, i as i64);
    }
    let out_v: Vec<[f64; 3]> = used.iter().map(|&i| cv[i as usize]).collect();
    let out_t: Vec<[i64; 3]> = kept_tris
        .iter()
        .map(|r| [remap_v[&r[0]], remap_v[&r[1]], remap_v[&r[2]]])
        .collect();
    (out_v, out_t, info)
}

fn target_faces(n_input_faces: usize) -> usize {
    n_input_faces.clamp(FACE_BUDGET_MIN, FACE_BUDGET_MAX)
}

fn decimate_coat(
    v: &[[f64; 3]],
    t: &[[i64; 3]],
    target: usize,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    if t.len() <= ((target as f64) * 1.05) as usize {
        return (
            v.to_vec(),
            t.to_vec(),
            json!({"decimated": false, "reason": "already_within_budget", "target": target}),
        );
    }
    let normals = compute_normal_per_vertex_core(v, t);
    let flat: Vec<f64> = normals
        .iter()
        .flat_map(|n| [f64::from(n[0]), f64::from(n[1]), f64::from(n[2])])
        .collect();
    let rep = match decimate_impl(v, t, target, None, false, Some((&flat, 3))) {
        Ok(r) => r,
        Err(e) => {
            return (
                v.to_vec(),
                t.to_vec(),
                json!({"decimated": false, "reason": format!("error: {e}"), "target": target}),
            )
        }
    };
    if rep.tris.is_empty() {
        return (
            v.to_vec(),
            t.to_vec(),
            json!({"decimated": false, "reason": "empty_result", "target": target}),
        );
    }
    let (h, nm) = common::reload_strict_holes_nm(&rep.verts, &rep.tris);
    if h > 0 || nm > 0 {
        return (
            v.to_vec(),
            t.to_vec(),
            json!({
                "decimated": false,
                "reason": "topology_violation",
                "holes": h,
                "non_manifold": nm,
                "target": target,
            }),
        );
    }
    let si = exact_si(&rep.verts, &rep.tris);
    if si.map(|s| s > 0).unwrap_or(false) {
        return (
            v.to_vec(),
            t.to_vec(),
            json!({"decimated": false, "reason": "si_introduced", "si": si, "target": target}),
        );
    }
    (
        rep.verts,
        rep.tris,
        json!({
            "decimated": true,
            "reason": Value::Null,
            "target": target,
            "faces": rep.faces_out,
            "si": si,
        }),
    )
}

fn p_weld(v: &[[f64; 3]], t: &[[i64; 3]]) -> (Vec<[f32; 3]>, Vec<[i64; 3]>, Option<Value>) {
    let v32: Vec<[f32; 3]> = v
        .iter()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    let (h, nm) = common::reload_strict_holes_nm(v, t);
    if h == 0 && nm == 0 {
        return (v32, t.to_vec(), None);
    }
    let res = crate::p_weld::p_weld_final(v, t);
    let tris = res
        .tris
        .iter()
        .map(|r| [r[0] as i64, r[1] as i64, r[2] as i64])
        .collect();
    (res.verts, tris, res.report)
}

fn one_sided_hausdorff(
    va: &[[f64; 3]],
    ta: &[[i64; 3]],
    vb: &[[f64; 3]],
    tb: &[[i64; 3]],
    diag: f64,
) -> (Option<f64>, Option<f64>) {
    if ta.is_empty() || tb.is_empty() || diag <= 0.0 {
        return (None, None);
    }
    let mut rng = sutura_geom::rng::Pcg64::new(HAUSDORFF_SEED);
    let seed = rng.integers(2_147_483_647);
    let pts = sample_surface_seeded(va, ta, HAUSDORFF_SAMPLES, seed);
    if pts.is_empty() {
        return (None, None);
    }
    let cp = common::closest_points(vb, tb, &pts);
    if cp.is_empty() {
        return (None, None);
    }
    let dist: Vec<f64> = cp.iter().map(|x| x.1).collect();
    let max = dist.iter().fold(f64::NEG_INFINITY, |m, x| m.max(*x));
    let mean = np_pairwise_sum(&dist) / dist.len() as f64;
    (Some(max / diag), Some(mean / diag))
}

fn normal_angle_deviation(
    wv: &[[f64; 3]],
    wt: &[[i64; 3]],
    v0: &[[f64; 3]],
    t0: &[[i64; 3]],
    defect_idx: &[usize],
) -> Option<Value> {
    if t0.is_empty() || wt.is_empty() || wv.is_empty() {
        return None;
    }
    let ivn = vertex_normals(v0, t0);
    let cvn = vertex_normals(wv, wt);
    let (_dist, idx, take) = pointcloud::nearest_points(v0, wv, 1);
    if take == 0 || idx.len() < wv.len() {
        return None;
    }
    let mut dset = vec![false; v0.len()];
    for &i in defect_idx {
        if i < dset.len() {
            dset[i] = true;
        }
    }
    let mut vals: Vec<f64> = Vec::new();
    for (i, &j) in idx.iter().enumerate() {
        if j < 0 || (j as usize) >= ivn.len() {
            continue;
        }
        let j = j as usize;
        if !defect_idx.is_empty() && dset[j] {
            continue;
        }
        let cos = dot(cvn[i], ivn[j]).abs().clamp(0.0, 1.0);
        vals.push(cos.acos().to_degrees());
    }
    if vals.is_empty() {
        return None;
    }
    let max = vals.iter().fold(f64::NEG_INFINITY, |m, x| m.max(*x));
    Some(json!({
        "p50": python_round(percentile_linear(&vals, 50.0), 4),
        "p95": python_round(percentile_linear(&vals, 95.0), 4),
        "p99": python_round(percentile_linear(&vals, 99.0), 4),
        "max": python_round(max, 4),
        "n": vals.len(),
    }))
}

#[derive(Clone, Debug)]
pub struct CoatOptions {
    pub voxel: Option<f64>,
    pub intensity: String,
    pub grid_budget: Option<f64>,
    pub r_base: Option<f64>,
    pub r_max: Option<f64>,
    pub sigma: Option<f64>,
    pub guard: bool,
    pub time_budget: Option<f64>,
    pub drain: Option<Value>,
    pub defects: Option<String>,
    pub r_max_scale: Option<f64>,
    pub sigma_scale: Option<f64>,
}

impl Default for CoatOptions {
    fn default() -> Self {
        CoatOptions {
            voxel: None,
            intensity: DEFAULT_INTENSITY.to_string(),
            grid_budget: None,
            r_base: None,
            r_max: None,
            sigma: None,
            guard: true,
            time_budget: None,
            drain: None,
            defects: None,
            r_max_scale: None,
            sigma_scale: None,
        }
    }
}

impl CoatOptions {
    pub fn from_value(params: &Value) -> Self {
        let mut o = CoatOptions::default();
        if let Some(v) = params.get("intensity").and_then(Value::as_str) {
            if matches!(v, "quick" | "balanced" | "thorough" | "extreme") {
                o.intensity = v.to_string();
            }
        }
        if let Some(v) = params.get("voxel").and_then(Value::as_f64) {
            o.voxel = Some(v);
        }
        if let Some(v) = params.get("grid_budget").and_then(Value::as_f64) {
            o.grid_budget = Some(v);
        }
        for (key, slot) in [
            ("r_base", &mut o.r_base),
            ("r_max", &mut o.r_max),
            ("sigma", &mut o.sigma),
        ] {
            if let Some(v) = params.get(key).and_then(Value::as_f64) {
                *slot = Some(v);
            }
        }
        if let Some(v) = params.get("time_budget").and_then(Value::as_f64) {
            o.time_budget = Some(v);
        }
        if let Some(v) = params.get("drain") {
            if !v.is_null() {
                o.drain = Some(v.clone());
            }
        }
        if let Some(v) = params.get("defects").and_then(Value::as_str) {
            o.defects = Some(v.to_string());
        }
        o.r_max_scale = normalize_scale(params.get("r_max_scale").and_then(Value::as_f64));
        o.sigma_scale = normalize_scale(params.get("sigma_scale").and_then(Value::as_f64));
        if let Some(v) = params.get("guard").and_then(Value::as_bool) {
            o.guard = v;
        }
        o
    }
}

/// Producer entry point matching `sutura_engine.dressing.dressing_coat`.
///
/// Returns `(verts, tris, report)` with the same post-coat cleanup,
/// decimation, P-WELD and fidelity report as the Python module.
pub fn coat(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    opts: &CoatOptions,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let intensity = if matches!(
        opts.intensity.as_str(),
        "quick" | "balanced" | "thorough" | "extreme"
    ) {
        opts.intensity.as_str()
    } else {
        DEFAULT_INTENSITY
    };
    let ispec = spec(intensity);
    let time_budget = opts.time_budget.unwrap_or(ispec.time_budget);
    let mut report_map = Map::new();
    for (key, value) in [
        ("method", json!(16)),
        ("display_name", json!("Dressing")),
        ("display_name_full", json!("Dressing (viscosity coat)")),
        ("invents_geometry", json!(true)),
        ("ran", json!(false)),
        ("ok", json!(false)),
        ("category", json!("error")),
        ("intensity", json!(intensity)),
        ("engine", json!("rust")),
        ("voxel", Value::Null),
        ("dims", json!([])),
        ("cells_evaluated", Value::Null),
        ("r_base", Value::Null),
        ("r_max", Value::Null),
        ("sigma", Value::Null),
        ("faces_before", json!(tris.len())),
        ("faces_coat", json!(tris.len())),
        ("faces_after", json!(tris.len())),
        ("decimated", json!(false)),
        ("decimation", Value::Null),
        ("si_before", Value::Null),
        ("si_after", Value::Null),
        ("si_exact_unknown", json!(false)),
        ("holes_after", Value::Null),
        ("nm_after", Value::Null),
        ("hausdorff_rel_max", Value::Null),
        ("hausdorff_rel_mean", Value::Null),
        ("hausdorff_input_to_coat", Value::Null),
        ("fidelity_ok", Value::Null),
        ("normal_angle_p50", Value::Null),
        ("normal_angle_p95", Value::Null),
        ("normal_angle_p99", Value::Null),
        ("normal_angle_max", Value::Null),
        ("normal_angle_n", json!(0)),
        ("components_after", Value::Null),
        ("volume_after", Value::Null),
        ("drain", Value::Null),
        (
            "defects",
            json!(if matches!(opts.defects.as_deref(), Some("holes_nm")) {
                "holes_nm"
            } else {
                "all"
            }),
        ),
        (
            "r_max_scale",
            opts.r_max_scale.map(Value::from).unwrap_or(Value::Null),
        ),
        (
            "sigma_scale",
            opts.sigma_scale.map(Value::from).unwrap_or(Value::Null),
        ),
        ("within_time_budget", Value::Null),
        ("time_budget_s", json!(time_budget)),
        ("warnings", json!([])),
        ("seconds", json!(0.0)),
    ] {
        report_map.insert(key.to_string(), value);
    }
    let mut report = Value::Object(report_map);
    if verts.is_empty() || tris.is_empty() {
        report["error"] = Value::from("empty mesh");
        return (verts.to_vec(), tris.to_vec(), report);
    }

    let diag = bbox_diagonal(verts);
    let ell = unique_median_edge_length(verts, tris);
    let budget = opts.grid_budget.or(Some(grid_budget(intensity)));
    let voxel = opts
        .voxel
        .unwrap_or_else(|| resolve_dressing_voxel(verts, tris, diag, intensity, budget));
    report["si_before"] = exact_si(verts, tris)
        .map(Value::from)
        .unwrap_or(Value::Null);
    let rms = opts.r_max_scale;
    let sgs = opts.sigma_scale;
    let (defect_idx, gap) = defect_vertex_indices(verts, tris, opts.defects.as_deref());
    let (mut rb, mut rmx, mut sg) = resolve_viscosity(ell, voxel, intensity, Some(gap), rms, sgs);
    if let Some(v) = opts.r_base {
        rb = v;
    }
    if let Some(v) = opts.r_max {
        rmx = v;
    }
    if let Some(v) = opts.sigma {
        sg = v;
    }
    let (drain_delta, drain_factor, drain_mode) = resolve_drain(opts.drain.as_ref(), intensity, rb);
    let drain_report = json!({
        "mode": drain_mode,
        "factor": drain_factor,
        "delta_r": python_round(drain_delta, 6),
        "amount": python_round(drain_delta, 6),
        "drained": drain_delta > 0.0,
    });
    report["drain"] = drain_report;

    let start = std::time::Instant::now();
    let (mut cv, mut ct, info, loop_report) = if drain_delta > 0.0 && adaptive_enabled() {
        let (cv, ct, info, lp) =
            adaptive_coat(verts, tris, voxel, rb, rmx, sg, &defect_idx, drain_delta);
        (cv, ct, info, Some(lp))
    } else {
        let r = extract_iteration(
            verts,
            tris,
            voxel,
            rb,
            rmx,
            sg,
            &defect_idx,
            drain_delta,
            None,
        );
        let lp = json!({
            "adaptive": false,
            "iterations": 1,
            "chosen_iteration": 0,
            "correction_applied": false,
            "alpha": ADAPTIVE_DRAIN_ALPHA,
            "k_cap": ADAPTIVE_DRAIN_K_CAP,
            "sigma_voxels": ADAPTIVE_DRAIN_SIGMA_VOX,
            "engine": "rust",
            "iterations_detail": [{
                "iteration": 0, "corrected": false, "engine": "rust",
                "median": Value::Null, "p95": Value::Null,
                "mean_abs": Value::Null, "n": 0, "holes": Value::Null,
                "non_manifold": Value::Null, "parts": Value::Null,
                "valid": true,
            }],
        });
        (r.verts, r.tris, r.info, Some(lp))
    };
    if let Some(lp) = loop_report {
        if let Some(obj) = report["drain"].as_object_mut() {
            if let Some(loop_obj) = lp.as_object() {
                for (k, v) in loop_obj {
                    obj.insert(k.clone(), v.clone());
                }
            }
        }
    }
    if std::env::var_os("SUTURA_PIPELINE_DEBUG").is_some() {
        eprintln!(
            "[dressing-rust] intensity={} voxel={} r_base={} r_max={} sigma={} faces_coat={}",
            intensity,
            info.voxel,
            info.r_base,
            info.r_max,
            info.sigma,
            ct.len()
        );
    }
    if ct.is_empty() {
        if std::env::var_os("SUTURA_PIPELINE_DEBUG").is_some() {
            eprintln!(
                "[dressing-rust] empty coat voxel={} r_base={} r_max={} sigma={} dims={:?} origin={:?}",
                info.voxel, info.r_base, info.r_max, info.sigma, info.dims, info.origin
            );
        }
        report["error"] = Value::from("level-set extraction produced no geometry");
        report["seconds"] = Value::from(0.0);
        return (verts.to_vec(), tris.to_vec(), report);
    }
    ct = reorient_faces(&cv, &ct);
    report["ran"] = Value::Bool(true);

    let voxel = info.voxel;
    report["voxel"] = Value::from(voxel);
    report["dims"] = json!(info.dims.to_vec());
    report["cells_evaluated"] = Value::from(info.voxels);
    report["grid_coarsened"] = Value::Bool(info.coarsened);
    report["r_base"] = Value::from(python_round(info.r_base, 6));
    report["r_max"] = Value::from(python_round(info.r_max, 6));
    report["sigma"] = Value::from(python_round(info.sigma, 6));
    report["band"] = Value::from(info.band);
    report["band_cells"] = Value::from(info.band_cells);
    report["field_seconds"] = Value::from(info.field_seconds);
    report["extract_seconds"] = Value::from(info.extract_seconds);
    report["rust_manifold"] = Value::Bool(info.manifold);
    report["faces_coat"] = Value::from(ct.len());

    let (cv2, ct2, cleanup) = clean_coat_components(verts, tris, &cv, &ct, voxel);
    cv = cv2;
    ct = ct2;
    report["cleanup"] = cleanup;
    report["faces_coat_clean"] = Value::from(ct.len());

    let target = target_faces(tris.len());
    let (dv, dt, drec) = decimate_coat(&cv, &ct, target);
    report["decimation"] = drec.clone();
    report["decimated"] = Value::Bool(
        drec.get("decimated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    );
    cv = dv;
    ct = dt;

    let (wv32, wt, pw) = p_weld(&cv, &ct);
    let wv: Vec<[f64; 3]> = wv32
        .iter()
        .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
        .collect();
    let (h, nm) = common::reload_strict_holes_nm(&wv, &wt);
    report["p_weld"] = pw.unwrap_or(Value::Null);
    report["holes_after"] = Value::from(h);
    report["nm_after"] = Value::from(nm);
    let si_after = exact_si(&wv, &wt);
    report["si_after"] = si_after.map(Value::from).unwrap_or(Value::Null);
    report["si_exact_unknown"] = Value::Bool(si_after.is_none());
    report["faces_after"] = Value::from(wt.len());

    if let Some(na) = normal_angle_deviation(&wv, &wt, verts, tris, &defect_idx) {
        report["normal_angle_p50"] = na["p50"].clone();
        report["normal_angle_p95"] = na["p95"].clone();
        report["normal_angle_p99"] = na["p99"].clone();
        report["normal_angle_max"] = na["max"].clone();
        report["normal_angle_n"] = na["n"].clone();
    }
    report["components_after"] = Value::from(count_components(&wv, &wt));
    report["volume_after"] = Value::from(python_round(
        sutura_geom::measures::signed_volume(&wv, &wt),
        6,
    ));

    if opts.guard && diag > 0.0 {
        let (hmax, hmean) = one_sided_hausdorff(&wv, &wt, verts, tris, diag);
        report["hausdorff_rel_max"] = hmax.map(Value::from).unwrap_or(Value::Null);
        report["hausdorff_rel_mean"] = hmean.map(Value::from).unwrap_or(Value::Null);
        let (i2c, _) = one_sided_hausdorff(verts, tris, &wv, &wt, diag);
        report["hausdorff_input_to_coat"] = i2c.map(Value::from).unwrap_or(Value::Null);
    }
    let measured = report["hausdorff_rel_max"].as_f64();
    let fidelity_ok = !opts.guard
        || measured.is_none()
        || measured.map(|m| m <= ispec.max_hausdorff).unwrap_or(true);
    report["fidelity_ok"] = Value::Bool(fidelity_ok);
    let seconds = start.elapsed().as_secs_f64();
    report["seconds"] = Value::from(seconds);
    let within = seconds <= time_budget;
    report["within_time_budget"] = Value::Bool(within);

    let watertight = h == 0 && nm == 0;
    let si_ok = si_after.map(|s| s == 0).unwrap_or(true);
    report["ok"] = Value::Bool(watertight && si_ok && fidelity_ok);
    report["category"] = Value::from(if watertight && si_ok && fidelity_ok {
        "watertight"
    } else {
        "partial"
    });

    let mut warnings: Vec<Value> = Vec::new();
    if let Some(measured) = measured {
        if !fidelity_ok {
            warnings.push(json!({
                "code": "dressing_detail_loss",
                "message_en": format!(
                    "Dressing used a {:.3} mm viscosity radius: the coat deviates {:.2}% of the diagonal from the input surface (fine detail may be smoothed).",
                    rb,
                    100.0 * measured
                ),
                "message_tr": format!(
                    "Dressing {:.3} mm viskozite yarıçapı kullandı: kaplama girdi yüzeyinden çaprazın %{:.2} kadar sapıyor (ince detay yumuşamış olabilir).",
                    rb,
                    100.0 * measured
                ),
            }));
        }
    }
    if !within {
        warnings.push(json!({
            "code": "dressing_time",
            "message_en": format!(
                "Dressing took {:.1} s, over the {} preset budget of {:.0} s.",
                seconds, intensity, time_budget
            ),
            "message_tr": format!(
                "Dressing {:.1} sn sürdü; {} ön ayarının {:.0} sn bütçesini aştı.",
                seconds, intensity, time_budget
            ),
        }));
    }
    report["warnings"] = Value::Array(warnings);

    (wv, wt, report)
}

fn base_record() -> Map<String, Value> {
    let mut rec = Map::new();
    for (key, value) in [
        ("tier", json!("dressing")),
        ("ran", json!(false)),
        ("adopted", json!(false)),
        ("watertight", json!(false)),
        ("fidelity_ok", Value::Null),
        ("engine", Value::Null),
        ("voxel", Value::Null),
        ("r_base", Value::Null),
        ("r_max", Value::Null),
        ("sigma", Value::Null),
        ("faces", Value::Null),
        ("faces_coat", Value::Null),
        ("decimated", json!(false)),
        ("drain", Value::Null),
        ("defects", Value::Null),
        ("r_max_scale", Value::Null),
        ("sigma_scale", Value::Null),
        ("cleanup", Value::Null),
        ("hausdorff_rel_max", Value::Null),
        ("hausdorff_input_to_coat", Value::Null),
        ("normal_angle_p50", Value::Null),
        ("normal_angle_p95", Value::Null),
        ("normal_angle_n", json!(0)),
        ("volume_before", Value::Null),
        ("volume_after", Value::Null),
        ("volume_delta_rel", Value::Null),
        ("components_before", Value::Null),
        ("components_after", Value::Null),
        ("volume_ok", Value::Null),
        ("parts_ok", Value::Null),
        ("normal_ok", Value::Null),
        ("si_before", Value::Null),
        ("si_after", Value::Null),
        ("si_exact_unknown", json!(false)),
        ("holes", Value::Null),
        ("non_manifold", Value::Null),
        ("within_time_budget", Value::Null),
        ("seconds", Value::Null),
        ("forced", json!(false)),
        ("guard_rejected", Value::Null),
        ("guard_reason", Value::Null),
        ("baseline_self_intersections", Value::Null),
        ("candidate_self_intersections", Value::Null),
        ("si_policy", Value::Null),
        ("si_checked", json!(false)),
        ("si_flagged", json!(false)),
        ("warnings", json!([])),
        ("reason", Value::Null),
    ] {
        rec.insert(key.to_string(), value);
    }
    rec
}

fn gate_f64(params: &Value, key: &str, default: f64) -> f64 {
    params
        .get("gates")
        .and_then(|g| g.get(key))
        .and_then(Value::as_f64)
        .unwrap_or(default)
}

fn gate_i64(params: &Value, key: &str, default: i64) -> i64 {
    params
        .get("gates")
        .and_then(|g| g.get(key))
        .and_then(Value::as_i64)
        .unwrap_or(default)
}

/// Dressing tier: coat the original input, then apply the Python adoption gate
/// against the mesh the ladder currently holds.
pub fn run(
    v: &[[f64; 3]],
    t: &[[i64; 3]],
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    intensity: &str,
    params: &Value,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let mut rec = base_record();
    let mut opts = CoatOptions::from_value(params);
    if opts.intensity == DEFAULT_INTENSITY {
        if !matches!(intensity, "quick" | "balanced" | "thorough" | "extreme") {
            opts.intensity = DEFAULT_INTENSITY.to_string();
        } else {
            opts.intensity = intensity.to_string();
        }
    }
    let force_adopt = params
        .get("force_adopt")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let (cand_v, cand_t, drec) = coat(input_v, input_t, &opts);
    rec.insert("ran".into(), json!(true));
    for (from, to) in [
        ("drain", "drain"),
        ("defects", "defects"),
        ("r_max_scale", "r_max_scale"),
        ("sigma_scale", "sigma_scale"),
        ("engine", "engine"),
        ("voxel", "voxel"),
        ("r_base", "r_base"),
        ("r_max", "r_max"),
        ("sigma", "sigma"),
        ("faces_coat", "faces_coat"),
        ("cleanup", "cleanup"),
        ("fidelity_ok", "fidelity_ok"),
        ("hausdorff_rel_max", "hausdorff_rel_max"),
        ("hausdorff_input_to_coat", "hausdorff_input_to_coat"),
        ("si_before", "si_before"),
        ("si_after", "si_after"),
        ("within_time_budget", "within_time_budget"),
        ("normal_angle_p50", "normal_angle_p50"),
        ("normal_angle_p95", "normal_angle_p95"),
    ] {
        if let Some(value) = drec.get(from) {
            rec.insert(to.into(), value.clone());
        }
    }
    if let Some(value) = drec.get("decimated") {
        rec.insert("decimated".into(), json!(value.as_bool().unwrap_or(false)));
    }
    if let Some(value) = drec.get("si_exact_unknown") {
        rec.insert(
            "si_exact_unknown".into(),
            json!(value.as_bool().unwrap_or(false)),
        );
    }
    if let Some(value) = drec.get("normal_angle_n") {
        rec.insert("normal_angle_n".into(), json!(value.as_i64().unwrap_or(0)));
    }
    if let Some(value) = drec.get("seconds") {
        rec.insert("seconds".into(), value.clone());
    }
    if let Some(value) = drec.get("warnings") {
        rec.insert("warnings".into(), value.clone());
    }
    if let Some(err) = drec.get("error") {
        rec.insert(
            "reason".into(),
            Value::from(format!(
                "dressing: {}",
                err.as_str().unwrap_or("failed to produce geometry")
            )),
        );
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }

    if cand_t.is_empty() {
        rec.insert(
            "reason".into(),
            Value::from("dressing returned no geometry"),
        );
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }

    let (f32_v, welded_t) = match common::weld_reload_equivalent(&cand_v, &cand_t) {
        Ok(x) => x,
        Err(_) => {
            rec.insert(
                "reason".into(),
                Value::from("dressing candidate weld failed"),
            );
            return (v.to_vec(), t.to_vec(), Value::Object(rec));
        }
    };
    let (f32_v, repaired_t) = common::repair_welded_topology(&f32_v, &welded_t);
    let cand_v: Vec<[f64; 3]> = f32_v
        .iter()
        .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
        .collect();
    let cand_t = repaired_t;

    let v32: Vec<[f32; 3]> = cand_v
        .iter()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    let t32: Vec<[i32; 3]> = cand_t
        .iter()
        .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
        .collect();
    let det = detect_defects(&v32, &t32, false);
    let cand_holes = det.holes.len();
    let cand_nm = det.non_manifold.len();
    rec.insert("holes".into(), json!(cand_holes));
    rec.insert("non_manifold".into(), json!(cand_nm));
    rec.insert("faces".into(), json!(cand_t.len()));

    let si_after = rec.get("si_after").and_then(Value::as_i64);
    let si_ok = si_after.map(|s| s == 0).unwrap_or(true);

    let vol_before = sutura_geom::measures::signed_volume(v, t);
    let parts_before = count_components(v, t);
    let vol_after_f = drec.get("volume_after").and_then(Value::as_f64);
    let parts_after_u = drec
        .get("components_after")
        .and_then(Value::as_i64)
        .map(|x| x.max(0) as usize);
    rec.insert(
        "volume_before".into(),
        Value::from(python_round(vol_before, 6)),
    );
    rec.insert(
        "volume_after".into(),
        drec.get("volume_after").cloned().unwrap_or(Value::Null),
    );
    rec.insert("components_before".into(), Value::from(parts_before as i64));
    rec.insert(
        "components_after".into(),
        drec.get("components_after").cloned().unwrap_or(Value::Null),
    );
    let volume_delta_rel = match vol_after_f {
        Some(after) => {
            let denom = vol_before.abs().max(1e-12);
            Some((after - vol_before).abs() / denom)
        }
        None => None,
    };
    rec.insert(
        "volume_delta_rel".into(),
        volume_delta_rel.map(Value::from).unwrap_or(Value::Null),
    );

    let max_volume_delta = gate_f64(params, "max_volume_delta", 0.10);
    let max_parts_slack = gate_i64(params, "max_parts_slack", 1);
    let max_normal_p95 = gate_f64(params, "max_normal_angle_p95", 30.0);
    let vol_ok = volume_delta_rel
        .map(|d| d <= max_volume_delta)
        .unwrap_or(true);
    let parts_ok = match parts_after_u {
        Some(after) => after as i64 <= parts_before as i64 + max_parts_slack,
        None => true,
    };
    let na95 = rec.get("normal_angle_p95").and_then(Value::as_f64);
    let normal_ok = na95.map(|v| v <= max_normal_p95).unwrap_or(true);
    rec.insert("volume_ok".into(), Value::Bool(vol_ok));
    rec.insert("parts_ok".into(), Value::Bool(parts_ok));
    rec.insert("normal_ok".into(), Value::Bool(normal_ok));
    let fidelity_failed = drec.get("fidelity_ok").and_then(Value::as_bool) == Some(false);
    let shape_ok = !fidelity_failed && vol_ok && parts_ok && normal_ok;
    rec.insert("shape_ok".into(), Value::Bool(shape_ok));

    let base_si = exact_si(v, t);
    // `check_candidate` remeasures the candidate's exact SI when the producer
    // could not (si_after == None); mirror that so a huge/unmeasurable coat is
    // not silently adopted.
    let guard_candidate_si = si_after.or_else(|| exact_si(&cand_v, &cand_t));
    let mut policy = common::policy_from_json(&Value::Null);
    policy.si_policy = "strict_zero".to_string();
    let guard = common::guard_fields(
        input_t.len(),
        input_v.len(),
        cand_t.len(),
        cand_v.len(),
        0,
        base_si,
        guard_candidate_si,
        &policy,
        false,
    );
    for key in [
        "guard_rejected",
        "guard_reason",
        "si_policy",
        "si_checked",
        "si_flagged",
        "baseline_self_intersections",
        "candidate_self_intersections",
    ] {
        if rec.contains_key(key) {
            if let Some(value) = guard.get(key) {
                rec.insert(key.into(), value.clone());
            }
        }
    }
    let guard_ok = guard
        .get("guard_ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if guard_ok && cand_holes == 0 && cand_nm == 0 && si_ok && (shape_ok || force_adopt) {
        rec.insert("adopted".into(), json!(true));
        rec.insert("forced".into(), json!(force_adopt && !shape_ok));
        rec.insert("watertight".into(), json!(false));
        rec.insert("candidate_watertight".into(), json!(true));
        rec.insert(
            "reason".into(),
            Value::from(format!(
                "strict-watertight candidate (stage 2 pending{})",
                if force_adopt && !shape_ok {
                    ", shape gates forced"
                } else {
                    ""
                }
            )),
        );
        return (cand_v, cand_t, Value::Object(rec));
    }

    let mut reasons: Vec<String> = Vec::new();
    if cand_holes != 0 || cand_nm != 0 {
        reasons.push(format!("holes={} non-manifold={}", cand_holes, cand_nm));
    }
    if !si_ok {
        reasons.push(format!(
            "exact self-intersections={}",
            si_after
                .map(|s| s.to_string())
                .unwrap_or_else(|| "None".to_string())
        ));
    }
    if !force_adopt {
        if fidelity_failed {
            let v = rec
                .get("hausdorff_rel_max")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            reasons.push(format!("fidelity {:.3}% > gate", 100.0 * v));
        }
        if !vol_ok {
            let v = volume_delta_rel.unwrap_or(0.0);
            reasons.push(format!("volume delta {:.2}% > gate", 100.0 * v));
        }
        if !parts_ok {
            reasons.push(format!(
                "components {} > {} + {}",
                parts_after_u
                    .map(|x| x.to_string())
                    .unwrap_or_else(|| "0".to_string()),
                parts_before,
                max_parts_slack
            ));
        }
        if !normal_ok {
            reasons.push(format!("normal p95 {:.1} deg > gate", na95.unwrap_or(0.0)));
        }
    }
    rec.insert(
        "reason".into(),
        Value::from(if reasons.is_empty() {
            "candidate not adopted".to_string()
        } else {
            format!("candidate not adopted ({})", reasons.join(", "))
        }),
    );
    (v.to_vec(), t.to_vec(), Value::Object(rec))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_cube() -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
        let v: Vec<[f64; 3]> = (0..8)
            .map(|i| [(i & 1) as f64, ((i >> 1) & 1) as f64, ((i >> 2) & 1) as f64])
            .collect();
        let t = vec![
            [0, 1, 3],
            [0, 3, 2],
            [4, 6, 7],
            [4, 7, 5],
            [0, 4, 5],
            [0, 5, 1],
            [2, 3, 7],
            [2, 7, 6],
            [0, 2, 6],
            [0, 6, 4],
            [1, 5, 7],
            [1, 7, 3],
        ];
        (v, t)
    }

    #[test]
    fn preset_tables_match_python() {
        let quick = spec("quick");
        assert_eq!(quick.voxel_div, (120.0, 200.0));
        assert_eq!(quick.r_preset, 4.0);
        assert_eq!(quick.max_hausdorff, 0.0150);
        assert_eq!(quick.drain_factor, 0.0);
        let balanced = spec("balanced");
        assert_eq!(balanced.voxel_div, (200.0, 350.0));
        assert_eq!(balanced.r_preset, 2.5);
        assert_eq!(balanced.max_hausdorff, 0.0080);
        assert_eq!(balanced.drain_factor, 1.0);
        let thorough = spec("thorough");
        assert_eq!(thorough.voxel_div, (300.0, 500.0));
        assert_eq!(thorough.r_preset, 1.8);
        let extreme = spec("extreme");
        assert_eq!(extreme.voxel_div, (450.0, 800.0));
        assert_eq!(extreme.r_preset, 1.2);
        assert_eq!(extreme.drain_factor, 1.25);
        assert_eq!(grid_budget("balanced"), 8_000_000.0);
        assert_eq!(grid_budget("extreme"), 40_000_000.0);
    }

    #[test]
    fn resolves_feature_anchored_voxel_like_python() {
        let (v, t) = open_cube();
        let diag = bbox_diagonal(&v);
        let voxel = resolve_dressing_voxel(&v, &t, diag, "balanced", None);
        let med = unique_median_edge_length(&v, &t);
        assert!(
            (voxel - med / 1.5).abs() < 1e-12,
            "{voxel} vs {}",
            med / 1.5
        );
    }

    #[test]
    fn coat_is_deterministic_and_adopts_open_cube() {
        let (v, t) = open_cube();
        let mut params = json!({"intensity": "quick", "drain": "none"});
        let (a_v, a_t, a_rec) = run(&v, &t, &v, &t, "quick", &params);
        let (b_v, b_t, _b_rec) = run(&v, &t, &v, &t, "quick", &params);
        assert_eq!(a_v, b_v, "dressing must be deterministic");
        assert_eq!(a_t, b_t, "dressing must be deterministic");
        assert_eq!(a_rec.get("ran").and_then(Value::as_bool), Some(true));
        assert_eq!(a_rec.get("holes").and_then(Value::as_i64), Some(0));
        assert_eq!(a_rec.get("non_manifold").and_then(Value::as_i64), Some(0));
        assert_eq!(a_rec.get("si_after").and_then(Value::as_i64), Some(0));
        // The gate is reported and rejects the shape-changing coat...
        assert_eq!(
            a_rec.get("adopted").and_then(Value::as_bool),
            Some(false),
            "{a_rec}"
        );
        assert!(a_rec
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("candidate not adopted"));
        assert!(a_rec.get("shape_ok").is_some());
        assert!(a_rec.get("volume_ok").is_some());
        assert!(a_rec.get("parts_ok").is_some());
        assert!(a_rec.get("normal_ok").is_some());
        // ...while --dressing-force-adopt keeps a watertight SI-free coat.
        params["force_adopt"] = json!(true);
        let (_v, _t, forced_rec) = run(&v, &t, &v, &t, "quick", &params);
        assert_eq!(
            forced_rec.get("adopted").and_then(Value::as_bool),
            Some(true),
            "{forced_rec}"
        );
        assert_eq!(
            forced_rec.get("forced").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn coat_reports_python_keys() {
        let (v, t) = open_cube();
        let opts = CoatOptions {
            intensity: "quick".to_string(),
            drain: Some(json!("none")),
            ..CoatOptions::default()
        };
        let (_cv, _ct, drec) = coat(&v, &t, &opts);
        for key in [
            "ran",
            "engine",
            "voxel",
            "r_base",
            "r_max",
            "sigma",
            "faces_coat",
            "holes_after",
            "nm_after",
            "fidelity_ok",
            "hausdorff_rel_max",
            "si_after",
            "drain",
            "cleanup",
            "decimation",
            "normal_angle_p95",
        ] {
            assert!(drec.get(key).is_some(), "missing {key}: {drec}");
        }
        assert_eq!(drec.get("engine").and_then(Value::as_str), Some("rust"));
    }
}
