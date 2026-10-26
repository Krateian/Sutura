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
mod cap;
mod components;
mod cut;
mod delaunay;
mod emit;
mod input_tets;
mod label;
mod normalize;
mod proxy;
mod relabel;
mod tetra;

use std::time::Instant;

use crate::shell::compute_shell_components;
use crate::winding::MeshBvh;

use adaptive::{AdaptiveStats, RETRY_COMPS};
use normalize::Normalizer;

pub(crate) const MIN_RES: usize = 16;
pub(crate) const MAX_RES: usize = 512;
/// Upper bound on greedy relabel flips (safety valve; the search is monotone).
const GREEDY_MAX_FLIPS: usize = 1_000_000;
/// H7: cap on the winding offset `|w - 0.5|` used as the soft unary.
const WIND_CAP: f64 = 0.5;
/// H7: ambiguous-band half-width for the relabel region guard (winding units).
const WIND_TAU: f64 = 0.1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SplintAdaptive {
    None,
    Hardened,
    Retry,
}

/// Opt-in, per-hypothesis switches for the v2 spike. Every boolean defaults to
/// `false`, so `SplintOpts::default()` reproduces the historical core output
/// bit-for-bit.
#[derive(Clone, Copy, Debug)]
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
    /// H7: rebuild the tet basis from the INPUT (fTetWild-shaped) and classify
    /// with a soft winding min-cut, bypassing the dual-contour proxy pillow.
    pub input_tets: bool,
    /// H7: Steiner sampling spacing as a fraction of the bbox diagonal.
    pub input_tets_steiner: f64,
    /// H7: min-cut face-area smoothness multiplier.
    pub input_tets_lambda: f64,
    /// H7: coarse interior-lattice cells per box axis.
    pub input_tets_lattice: usize,
    /// H7: keep an inside component only if it holds a tet whose winding offset
    /// `GWN - 0.5` exceeds this (`WIND_TAU` by default); shallower specks dropped.
    pub input_tets_deep: f64,
    /// H7b: evaluate the winding unary on a TEMPORARY cap of the input's open
    /// boundary loops (never emitted), so the opening reads as interior.
    pub input_tets_cap: bool,
    /// H4: after the cut/island stage under the input-tets basis, drop every
    /// inside component whose volume is below `input_tets_prune_frac` of the
    /// largest component's volume. Only applied when the input is a single
    /// connected body, so a genuinely separate input part is never removed.
    pub input_tets_prune: bool,
    /// H4: the volume-fraction threshold (relative to the largest component).
    pub input_tets_prune_frac: f64,
    /// H7c: after the H4 prune under the input-tets basis, join each residual
    /// inside basin to the largest with a min-cost tet bridge, keeping only the
    /// merges that reduce the emitted boundary-component count. Only applied when
    /// the input is a single connected body.
    pub input_tets_bridge: bool,
    /// H5: hard-inside seeds. For each input vertex, force the incident tet with
    /// the highest (least exterior) winding offset into the cut's source set, so
    /// the emitted boundary is constrained to pass through that vertex. Only
    /// applied when the input is a single connected body.
    pub input_tets_anchor: bool,
}

impl Default for SplintOpts {
    fn default() -> Self {
        Self {
            snap_f32: false,
            greedy_relabel: false,
            geodesic: false,
            geodesic_bridge: false,
            cavity_fill: false,
            input_tets: false,
            input_tets_steiner: 1.0 / 24.0,
            input_tets_lambda: 0.5,
            input_tets_lattice: 7,
            input_tets_deep: WIND_TAU,
            input_tets_cap: false,
            input_tets_prune: false,
            input_tets_prune_frac: 0.01,
            input_tets_bridge: false,
            input_tets_anchor: false,
        }
    }
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
    pub input_tets: bool,
    pub input_tets_points: usize,
    pub input_tets_corner_tets: usize,
    pub input_tets_cap: bool,
    pub cap_loops: usize,
    pub cap_planar: usize,
    pub cap_fanned: usize,
    pub cap_tris: usize,
    pub input_tets_prune: bool,
    pub prune_frac: f64,
    pub prune_kept: usize,
    pub prune_dropped: usize,
    pub prune_speck_vol: f64,
    pub prune_speck_faces: usize,
    pub prune_comp_vols: Vec<f64>,
    pub prune_comp_faces: Vec<usize>,
    pub input_tets_bridge_added: usize,
    pub input_tets_anchor: bool,
    pub anchor_tets: usize,
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

    // H7b: a temporary cap of the input's open boundary loops, used ONLY to
    // evaluate the winding unary (never emitted, never part of the basis).
    let cap = if opts.input_tets && opts.input_tets_cap {
        Some(cap::temporary_cap(&nverts, tris))
    } else {
        None
    };
    let capped_bvh = cap.as_ref().map(|c| {
        let mut cv = nverts.clone();
        cv.extend_from_slice(&c.extra_verts);
        let mut ct: Vec<[usize; 3]> = tris.to_vec();
        ct.extend_from_slice(&c.tris);
        MeshBvh::from_arrays(&cv, &ct)
    });
    let wind_bvh: &MeshBvh = capped_bvh.as_ref().unwrap_or(&bvh);

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
    let (proxy, corner_pts) = if opts.input_tets {
        let t_proxy = Instant::now();
        let basis = input_tets::build(
            &nverts,
            tris,
            opts.input_tets_steiner,
            opts.input_tets_lattice,
        );
        let secs_proxy = t_proxy.elapsed().as_secs_f64();
        (
            proxy::Proxy {
                verts: basis.points,
                shell_len: 0,
                shell_tris: Vec::new(),
                voxel: basis.voxel,
                secs_udf: 0.0,
                secs_proxy,
                adaptive: AdaptiveStats::default(),
            },
            Some(basis.corner),
        )
    } else {
        (
            proxy::build(&bvh, res, normalizer.eps(), shell_map.as_deref(), broad)?,
            None,
        )
    };
    let t_delaunay = Instant::now();
    // The Geogram engine emits the same cells in a nondeterministic order;
    // canonicalise once here so every index-ordered downstream pass (min-cut
    // tie-breaks, island BFS, greedy relabel) is a pure function of the input.
    let mut tets = delaunay::delaunay3(&proxy.verts)?;
    delaunay::canonicalize_tets(&mut tets);
    let secs_delaunay = t_delaunay.elapsed().as_secs_f64();
    let topo = tetra::build(&tets, &proxy.verts);
    let labels = if opts.input_tets {
        label::label_tets_winding(wind_bvh, &topo.centroids, &topo.out_tet, parallel, WIND_CAP)
    } else {
        label::label_tets(&bvh, &topo.centroids, &topo.out_tet, parallel)
    };
    let corner_tet: Option<Vec<bool>> = corner_pts.as_ref().map(|cp| {
        tets.iter()
            .map(|t| t.iter().any(|&v| cp[v]))
            .collect()
    });
    let input_comps = input_components(verts, tris);
    // H5: per-input-vertex hard-inside seed. The soft winding unary labels an
    // open scan's folded flap exterior, so the cut would leave those input
    // vertices uncovered (large input->output distance). For every input vertex
    // pick the incident, non-corner tet with the largest (least exterior) phi
    // and force it into the source set, so the emitted boundary must pass
    // through that vertex. Deterministic: index order, strict `>` keeps the
    // first (lowest) tet on ties.
    let anchor: Option<Vec<bool>> = if opts.input_tets && opts.input_tets_anchor && input_comps == 1
    {
        let maps = input_tets::map_input_vertices(&proxy.verts, &nverts);
        let ct = corner_tet.as_ref().expect("H7 corner flags");
        let step = 0.5 * proxy.voxel;
        let nearest = |q: [f64; 3]| -> usize {
            let mut best = usize::MAX;
            let mut bd = f64::INFINITY;
            for (ti, c) in topo.centroids.iter().enumerate() {
                if topo.out_tet[ti] || ct[ti] {
                    continue;
                }
                let d = (c[0] - q[0]).powi(2) + (c[1] - q[1]).powi(2) + (c[2] - q[2]).powi(2);
                if d < bd {
                    bd = d;
                    best = ti;
                }
            }
            best
        };
        let mut a = vec![false; tets.len()];
        for f in tris {
            let (Some(_i0), Some(_i1), Some(_i2)) = (maps[f[0]], maps[f[1]], maps[f[2]]) else {
                continue;
            };
            let p0 = nverts[f[0]];
            let p1 = nverts[f[1]];
            let p2 = nverts[f[2]];
            let c = [
                (p0[0] + p1[0] + p2[0]) / 3.0,
                (p0[1] + p1[1] + p2[1]) / 3.0,
                (p0[2] + p1[2] + p2[2]) / 3.0,
            ];
            let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
            let e2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
            let mut n = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            let nl = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if nl <= 1e-30 {
                continue;
            }
            for k in 0..3 {
                n[k] /= nl;
            }
            let plus = [c[0] + n[0] * step, c[1] + n[1] * step, c[2] + n[2] * step];
            let minus = [c[0] - n[0] * step, c[1] - n[1] * step, c[2] - n[2] * step];
            // The interior side is the probe with the higher capped winding; the
            // sign of the face normal is irrelevant because both sides are
            // sampled and compared.
            let t_in = if wind_bvh.winding_at(plus) >= wind_bvh.winding_at(minus) {
                nearest(plus)
            } else {
                nearest(minus)
            };
            if t_in != usize::MAX {
                a[t_in] = true;
            }
        }
        Some(a)
    } else {
        None
    };
    let cut = {
        let t = Instant::now();
        let cut = if opts.input_tets {
            cut::cut_winding_smooth(
                &tets,
                &proxy.verts,
                &topo.internal,
                &topo.out_tet,
                corner_tet.as_ref().expect("H7 corner flags"),
                &labels.phi,
                opts.input_tets_lambda,
                opts.input_tets_deep,
                normalizer.scale(),
                anchor.as_deref(),
            )
        } else {
            cut::cut_and_cleanup(
                &tets,
                &proxy.verts,
                &topo.internal,
                &topo.out_tet,
                &labels.phi,
                &labels.init_label,
                normalizer.scale(),
                proxy.voxel,
            )
        };
        (cut, t.elapsed().as_secs_f64())
    };
    let (mut cut, secs_cut) = cut;
    let geodesic_outcome = if opts.geodesic && !opts.input_tets {
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
    let cavity_outcome = if opts.cavity_fill && !opts.input_tets {
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
    // H4: volume-fraction prune of inside components. Always computed under the
    // input-tets basis (so the component volumes/facet counts are reported for
    // measurement), but it only DROPS anything when the flag is on and the input
    // is a single body. A zero fraction is a pure measurement pass (no drop).
    let prune_outcome = if opts.input_tets {
        let frac = if opts.input_tets_prune && input_comps == 1 {
            opts.input_tets_prune_frac
        } else {
            0.0
        };
        Some(cut::prune_inside_by_volume(
            &proxy.verts,
            &tets,
            &topo.internal,
            &mut cut.is_inside,
            frac,
            normalizer.scale(),
            anchor.as_deref(),
        ))
    } else {
        None
    };
    // H7c: after the prune, join each residual inside basin to the largest with a
    // min-cost tet bridge (acceptance-gated on the emitted boundary-component
    // count), so a material-sized second basin is merged instead of dropped.
    let bridge_h7c_added = if opts.input_tets && opts.input_tets_bridge && input_comps == 1 {
        cut::bridge_basins_to_largest(
            &proxy.verts,
            &topo.internal,
            &topo.out_tet,
            &labels.phi,
            &mut cut.is_inside,
            proxy.verts.len(),
        )
    } else {
        0
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
    let relabel_tau = if opts.input_tets {
        WIND_TAU
    } else {
        0.25 * proxy.voxel
    };
    let relabel_outcome = (opts.greedy_relabel || opts.input_tets).then(|| {
        relabel::greedy_relabel(
            &tets,
            &topo.internal,
            &topo.out_tet,
            &mut cut.is_inside,
            &labels.phi,
            relabel_tau,
            GREEDY_MAX_FLIPS,
        )
    });
    let emitted = emit::emit_boundary(
        &proxy.verts,
        &topo.centroids,
        &topo.internal,
        &cut.is_inside,
        &normalizer,
        opts.snap_f32 || opts.input_tets,
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
        input_tets: opts.input_tets,
        input_tets_points: if opts.input_tets {
            proxy.verts.len()
        } else {
            0
        },
        input_tets_corner_tets: corner_tet
            .as_ref()
            .map_or(0, |c| c.iter().filter(|&&b| b).count()),
        input_tets_cap: opts.input_tets && opts.input_tets_cap,
        cap_loops: cap.as_ref().map_or(0, |c| c.loops),
        cap_planar: cap.as_ref().map_or(0, |c| c.planar),
        cap_fanned: cap.as_ref().map_or(0, |c| c.fanned),
        cap_tris: cap.as_ref().map_or(0, |c| c.tris.len()),
        input_tets_prune: opts.input_tets_prune,
        prune_frac: if opts.input_tets_prune {
            opts.input_tets_prune_frac
        } else {
            0.0
        },
        prune_kept: prune_outcome.as_ref().map_or(0, |p| p.kept),
        prune_dropped: prune_outcome.as_ref().map_or(0, |p| p.dropped),
        prune_speck_vol: prune_outcome.as_ref().map_or(0.0, |p| p.speck_vol),
        prune_speck_faces: prune_outcome.as_ref().map_or(0, |p| p.speck_faces),
        prune_comp_vols: prune_outcome
            .as_ref()
            .map_or_else(Vec::new, |p| p.comp_vols.clone()),
        prune_comp_faces: prune_outcome
            .as_ref()
            .map_or_else(Vec::new, |p| p.comp_faces.clone()),
        input_tets_bridge_added: bridge_h7c_added,
        input_tets_anchor: opts.input_tets && opts.input_tets_anchor && input_comps == 1,
        anchor_tets: anchor.as_ref().map_or(0, |a| a.iter().filter(|&&b| b).count()),
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
