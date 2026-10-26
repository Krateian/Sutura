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
mod relabel;
mod tetra;

use std::time::Instant;

use crate::shell::compute_shell_components;
use crate::winding::MeshBvh;

use adaptive::RETRY_COMPS;
use normalize::Normalizer;

pub(crate) const MIN_RES: usize = 16;
pub(crate) const MAX_RES: usize = 512;
/// Upper bound on greedy relabel flips (safety valve; the search is monotone).
const GREEDY_MAX_FLIPS: usize = 1_000_000;

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
    /// Greedy relabel of tet in/out flags to remove non-manifold boundary edges.
    pub greedy_relabel: bool,
    /// H6: replace the min-cut classification with a connected flood fill inside
    /// the closed proxy pillow, seeded by strong-interior tets.
    pub geodesic: bool,
    /// H6b: after the geodesic flood, join the two largest inside basins with a
    /// min-cost tet bridge. Only applied when the input is a single connected
    /// component, so a genuinely separate input body is never merged.
    pub geodesic_bridge: bool,
    /// H6c: fill enclosed outside cavities (no hull contact) whose ORIGINAL-INPUT
    /// winding number at a deterministic interior sample exceeds 0.5.
    pub cavity_fill: bool,
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
    pub relabel: bool,
    pub relabel_first_nm_edges: usize,
    pub relabel_final_nm_edges: usize,
    pub relabel_flips: usize,
    pub geodesic: bool,
    pub geodesic_seeds: usize,
    pub geodesic_filled: usize,
    pub geodesic_bridge: bool,
    pub bridge_added: usize,
    pub cavity_fill: bool,
    pub cavity_candidates: usize,
    pub cavity_filled: usize,
    pub cavity_windings: Vec<f64>,
    pub cavity_tet_counts: Vec<usize>,
    pub cavity_vols: Vec<f64>,
    pub cavity_max_windings: Vec<f64>,
    pub input_comps: usize,
    pub geodesic_comps: usize,
    pub inside_comps: usize,
    pub cut_inside_hash: u64,
    pub phi_hash: u64,
    pub init_hash: u64,
    pub proxy_hash: u64,
    pub centroid_hash: u64,
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

/// Number of edge-connected components of the input triangle soup (vertices are
/// unioned through the triangles that reference them). Used to decide whether a
/// multi-part output is a spurious split (input is one body) or a faithful copy
/// of a genuinely separate input body. Deterministic: index order only.
fn input_components(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> usize {
    let n = verts.len();
    if n == 0 {
        return 0;
    }
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    let mut parent: Vec<usize> = (0..n).collect();
    for t in tris {
        let ra = find(&mut parent, t[0]);
        for &v in &t[1..] {
            let rv = find(&mut parent, v);
            if ra != rv {
                parent[rv] = ra;
            }
        }
    }
    let mut roots = std::collections::BTreeSet::new();
    for t in tris {
        roots.insert(find(&mut parent, t[0]));
    }
    roots.len()
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
    // The Geogram engine emits the same cells in a nondeterministic order;
    // canonicalise once here so every index-ordered downstream pass (min-cut
    // tie-breaks, island BFS, greedy relabel) is a pure function of the input.
    let mut tets = delaunay::delaunay3(&proxy.verts)?;
    delaunay::canonicalize_tets(&mut tets);
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
    let (mut cut, secs_cut) = cut;
    let geodesic_outcome = if opts.geodesic {
        // The pillow is the dual-contour offset shell (leading proxy vertices);
        // a tet is inside it when the pillow's own winding number exceeds 0.5.
        let pillow =
            MeshBvh::from_arrays(&proxy.verts[..proxy.shell_len], &proxy.shell_tris);
        let allowed: Vec<bool> = if parallel {
            use rayon::prelude::*;
            topo.centroids
                .par_iter()
                .map(|c| pillow.winding_at(*c) > 0.5)
                .collect()
        } else {
            topo.centroids
                .iter()
                .map(|c| pillow.winding_at(*c) > 0.5)
                .collect()
        };
        let g = cut::geodesic_flood(
            &tets,
            &proxy.verts,
            &topo.internal,
            &topo.out_tet,
            &allowed,
            &labels.phi,
            0.25 * proxy.voxel,
            normalizer.scale(),
        );
        cut.is_inside = g.is_inside;
        Some((g.seeds, g.filled, g.comps))
    } else {
        None
    };
    let input_comps = input_components(verts, tris);
    let bridge_added = if geodesic_outcome.is_some() && opts.geodesic_bridge && input_comps == 1 {
        // Accept the bridge only if it actually drops the number of emitted
        // boundary components; otherwise keep the flood result.
        let before = cut::boundary_part_count(&topo.internal, &cut.is_inside, proxy.verts.len());
        let snapshot = cut.is_inside.clone();
        let added = cut::bridge_inside_basins(
            &proxy.verts,
            &topo.internal,
            &topo.out_tet,
            &labels.phi,
            &mut cut.is_inside,
        );
        let after = cut::boundary_part_count(&topo.internal, &cut.is_inside, proxy.verts.len());
        if added > 0 && after < before {
            added
        } else {
            cut.is_inside = snapshot;
            0
        }
    } else {
        0
    };
    let cavity_outcome = if opts.cavity_fill {
        Some(cut::fill_enclosed_cavities(
            &proxy.verts,
            &tets,
            &topo.centroids,
            &topo.internal,
            &topo.out_tet,
            &mut cut.is_inside,
            &bvh,
            normalizer.scale(),
        ))
    } else {
        None
    };
    let inside_comps = cut::inside_components(&topo.internal, &topo.out_tet, &cut.is_inside);
    let mut proxy_hash: u64 = 0xcbf29ce484222325;
    for p in &proxy.verts {
        for &x in p {
            proxy_hash ^= x.to_bits();
            proxy_hash = proxy_hash.wrapping_mul(0x100000001b3);
        }
    }
    let mut phi_hash: u64 = 0xcbf29ce484222325;
    for &p in &labels.phi {
        phi_hash ^= p.to_bits();
        phi_hash = phi_hash.wrapping_mul(0x100000001b3);
    }
    let mut centroid_hash: u64 = 0xcbf29ce484222325;
    for p in &topo.centroids {
        for &x in p {
            centroid_hash ^= x.to_bits();
            centroid_hash = centroid_hash.wrapping_mul(0x100000001b3);
        }
    }
    let mut init_hash: u64 = 0xcbf29ce484222325;
    for &b in &labels.init_label {
        init_hash ^= u64::from(b);
        init_hash = init_hash.wrapping_mul(0x100000001b3);
    }
    let cut_inside_hash = {
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in &cut.is_inside {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    };
    let relabel_outcome = opts.greedy_relabel.then(|| {
        relabel::greedy_relabel(
            &tets,
            &topo.internal,
            &topo.out_tet,
            &mut cut.is_inside,
            &labels.phi,
            0.25 * proxy.voxel,
            GREEDY_MAX_FLIPS,
        )
    });
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
        relabel: relabel_outcome.is_some(),
        relabel_first_nm_edges: relabel_outcome.as_ref().map_or(0, |r| r.first_nm_edges),
        relabel_final_nm_edges: relabel_outcome.as_ref().map_or(0, |r| r.final_nm_edges),
        relabel_flips: relabel_outcome.as_ref().map_or(0, |r| r.flips),
        geodesic: geodesic_outcome.is_some(),
        geodesic_seeds: geodesic_outcome.map_or(0, |(s, _, _)| s),
        geodesic_filled: geodesic_outcome.map_or(0, |(_, f, _)| f),
        geodesic_bridge: geodesic_outcome.is_some() && opts.geodesic_bridge && input_comps == 1,
        bridge_added,
        cavity_fill: opts.cavity_fill,
        cavity_candidates: cavity_outcome.as_ref().map_or(0, |c| c.candidates),
        cavity_filled: cavity_outcome.as_ref().map_or(0, |c| c.filled),
        cavity_windings: cavity_outcome.as_ref().map_or_else(Vec::new, |c| c.windings.clone()),
        cavity_tet_counts: cavity_outcome.as_ref().map_or_else(Vec::new, |c| c.tet_counts.clone()),
        cavity_vols: cavity_outcome.as_ref().map_or_else(Vec::new, |c| c.vols.clone()),
        cavity_max_windings: cavity_outcome
            .as_ref()
            .map_or_else(Vec::new, |c| c.max_windings.clone()),
        input_comps,
        geodesic_comps: geodesic_outcome.map_or(0, |(_, _, c)| c),
        inside_comps,
        cut_inside_hash,
        phi_hash,
        init_hash,
        proxy_hash,
        centroid_hash,
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
