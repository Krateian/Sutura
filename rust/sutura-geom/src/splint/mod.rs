// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Splint core: deterministic UDF → Geogram Delaunay → winding cut.
//!
//! Steps 1–8 of the CelloCut spike: normalise, build the eps-offset proxy,
//! tetrahedralise it with the sequential Geogram PSM, label each tet from the
//! signed UDF/GWN field, run the min s-t cut, remove the fix-A single-tet
//! islands, and emit the inside/outside boundary. Step 9 (dual-contour
//! `thin_mesh`) is deliberately out of this core.
//!
//! `SplintAdaptive` adds the measured adaptive proxy sampling (hardened), and
//! an optional self-contained retry: it counts the raw-cut shells after fix A
//! (faces joined only across edges used by exactly two faces, the rule Manifold
//! Stage 2 sees) and reruns with broad sampling when that count exceeds
//! `RETRY_COMPS`.

mod adaptive;
mod components;
mod cut;
mod delaunay;
mod emit;
mod label;
mod normalize;
mod proxy;
mod tetra;

use std::time::Instant;

use crate::shell::compute_shell_components;
use crate::winding::MeshBvh;

use adaptive::RETRY_COMPS;
use normalize::Normalizer;

pub(crate) const MIN_RES: usize = 16;
pub(crate) const MAX_RES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SplintAdaptive {
    None,
    Hardened,
    Retry,
}

/// Opt-in, per-hypothesis switches for the v2 spike. Every field defaults to
/// `false`, so `SplintOpts::default()` reproduces the historical core output
/// bit-for-bit.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SplintOpts {
    /// H2: canonicalize emitted world-space vertices through `f32` and dedup on
    /// the `f32` grid (post `de_norm`), so the output equals its reload weld.
    pub snap_f32: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct SplintReport {
    pub ok: bool,
    pub res: usize,
    pub input_vertices: usize,
    pub input_faces: usize,
    pub proxy_vertices: usize,
    pub tets: usize,
    pub delaunay_hash: u64,
    pub internal_faces: usize,
    pub outer_faces: usize,
    pub out_tets: usize,
    pub interior_anchors: usize,
    pub output_vertices: usize,
    pub output_faces: usize,
    pub output_parts: usize,
    pub out_manifold: bool,
    pub fix_islands: bool,
    pub soft_band: bool,
    pub dropped_inside_comps: usize,
    pub dropped_inside_vol: f64,
    pub filled_outside_comps: usize,
    pub filled_outside_vol: f64,
    pub hard_inside_comps: usize,
    pub hard_inside_tets: usize,
    pub winding_hist: [usize; 10],
    pub adaptive: Option<&'static str>,
    pub cut_shells: usize,
    pub first_cut_shells: usize,
    pub adaptive_broad: bool,
    pub adaptive_retry: bool,
    pub retry_threshold: usize,
    pub adaptive_added: usize,
    pub adaptive_samples: usize,
    pub adaptive_thin: usize,
    pub adaptive_capped: bool,
    pub snap_f32: bool,
    pub secs_adaptive: f64,
    pub secs_udf: f64,
    pub secs_proxy: f64,
    pub secs_delaunay: f64,
    pub secs_cut: f64,
    pub seconds: f64,
}

pub(crate) struct SplintOutput {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[usize; 3]>,
    pub report: SplintReport,
}

fn run_pass(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    res: usize,
    adaptive_on: bool,
    broad: bool,
    parallel: bool,
    opts: SplintOpts,
) -> Result<(SplintOutput, usize), String> {
    let t0 = Instant::now();
    let normalizer = Normalizer::new(verts, res);
    let nverts: Vec<[f64; 3]> = verts.iter().map(|p| normalizer.norm(*p)).collect();
    let bvh = MeshBvh::from_arrays(&nverts, tris);

    let shell_map = if adaptive_on {
        let tris_u32: Vec<[u32; 3]> = tris
            .iter()
            .map(|t| [t[0] as u32, t[1] as u32, t[2] as u32])
            .collect();
        let (map, _) = compute_shell_components(&tris_u32);
        Some(map)
    } else {
        None
    };
    let proxy = proxy::build(&bvh, res, normalizer.eps(), shell_map.as_deref(), broad)?;
    let t_delaunay = Instant::now();
    let tets = delaunay::delaunay3(&proxy.verts)?;
    let secs_delaunay = t_delaunay.elapsed().as_secs_f64();
    let topo = tetra::build(&tets, &proxy.verts);
    let labels = label::label_tets(&bvh, &topo.centroids, &topo.out_tet, parallel);
    let cut = {
        let t = Instant::now();
        let cut = cut::cut_and_cleanup(
            &tets,
            &proxy.verts,
            &topo.internal,
            &topo.out_tet,
            &labels.phi,
            &labels.init_label,
            normalizer.scale(),
            proxy.voxel,
        );
        (cut, t.elapsed().as_secs_f64())
    };
    let (cut, secs_cut) = cut;
    let emitted = emit::emit_boundary(
        &proxy.verts,
        &topo.centroids,
        &topo.internal,
        &cut.is_inside,
        &normalizer,
        opts.snap_f32,
    );
    let cut_shells = components::cut_shells(&emitted.tris);

    let report = SplintReport {
        ok: true,
        res,
        input_vertices: verts.len(),
        input_faces: tris.len(),
        proxy_vertices: proxy.verts.len(),
        tets: tets.len(),
        delaunay_hash: delaunay::tets_hash(&tets),
        internal_faces: topo.internal.len(),
        outer_faces: topo.outer_faces,
        out_tets: topo.out_tet.iter().filter(|&&b| b).count(),
        interior_anchors: labels.interior_anchors,
        output_vertices: emitted.verts.len(),
        output_faces: emitted.tris.len(),
        output_parts: emitted.output_parts,
        out_manifold: emitted.out_manifold,
        fix_islands: true,
        soft_band: false,
        dropped_inside_comps: cut.dropped_inside_comps,
        dropped_inside_vol: cut.dropped_inside_vol,
        filled_outside_comps: cut.filled_outside_comps,
        filled_outside_vol: cut.filled_outside_vol,
        hard_inside_comps: cut.hard_inside_comps,
        hard_inside_tets: cut.hard_inside_tets,
        winding_hist: labels.winding_hist,
        adaptive: None,
        cut_shells,
        first_cut_shells: cut_shells,
        adaptive_broad: broad,
        adaptive_retry: false,
        retry_threshold: RETRY_COMPS,
        adaptive_added: proxy.adaptive.added,
        adaptive_samples: proxy.adaptive.samples,
        adaptive_thin: proxy.adaptive.thin,
        adaptive_capped: proxy.adaptive.capped,
        snap_f32: opts.snap_f32,
        secs_adaptive: proxy.adaptive.secs,
        secs_udf: proxy.secs_udf,
        secs_proxy: proxy.secs_proxy,
        secs_delaunay,
        secs_cut,
        seconds: t0.elapsed().as_secs_f64(),
    };
    Ok((
        SplintOutput {
            verts: emitted.verts,
            tris: emitted.tris,
            report,
        },
        cut_shells,
    ))
}

pub(crate) fn splint_cut_core(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    res: usize,
    adaptive: SplintAdaptive,
    parallel: bool,
) -> Result<SplintOutput, String> {
    splint_cut_core_opts(verts, tris, res, adaptive, parallel, SplintOpts::default())
}

pub(crate) fn splint_cut_core_opts(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    res: usize,
    adaptive: SplintAdaptive,
    parallel: bool,
    opts: SplintOpts,
) -> Result<SplintOutput, String> {
    if verts.is_empty() || tris.is_empty() {
        return Err("empty input".into());
    }
    if !(MIN_RES..=MAX_RES).contains(&res) {
        return Err(format!(
            "res must be between {MIN_RES} and {MAX_RES} (got {res})"
        ));
    }
    let (mut first, first_shells) = match adaptive {
        SplintAdaptive::None => run_pass(verts, tris, res, false, false, parallel, opts)?,
        SplintAdaptive::Hardened => run_pass(verts, tris, res, true, false, parallel, opts)?,
        SplintAdaptive::Retry => run_pass(verts, tris, res, true, false, parallel, opts)?,
    };
    first.report.adaptive = match adaptive {
        SplintAdaptive::None => None,
        SplintAdaptive::Hardened => Some("hardened"),
        SplintAdaptive::Retry => Some("retry"),
    };
    first.report.first_cut_shells = first_shells;
    first.report.cut_shells = first_shells;

    if adaptive == SplintAdaptive::Retry && first_shells > RETRY_COMPS {
        let (mut broad, broad_shells) = run_pass(verts, tris, res, true, true, parallel, opts)?;
        broad.report.adaptive = Some("retry");
        broad.report.first_cut_shells = first_shells;
        broad.report.cut_shells = broad_shells;
        broad.report.adaptive_retry = true;
        broad.report.adaptive_broad = true;
        broad.report.seconds += first.report.seconds;
        return Ok(broad);
    }
    first.report.adaptive_retry = false;
    first.report.adaptive_broad = false;
    Ok(first)
}

#[cfg(test)]
mod tests;
