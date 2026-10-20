// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Splint core: deterministic UDF → Geogram Delaunay → winding cut.
//!
//! Steps 1–8 of the CelloCut spike: normalise, build the eps-offset proxy,
//! tetrahedralise it with the sequential Geogram PSM, label each tet from the
//! signed UDF/GWN field, run the min s-t cut, remove the fix-A single-tet
//! islands, and emit the inside/outside boundary. Step 9 (dual-contour
//! `thin_mesh`) and adaptive sampling are deliberately out of this core.

mod cut;
mod delaunay;
mod emit;
mod label;
mod normalize;
mod proxy;
mod tetra;

use std::time::Instant;

use crate::winding::MeshBvh;

use normalize::Normalizer;

pub(crate) const MIN_RES: usize = 16;
pub(crate) const MAX_RES: usize = 512;

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

pub(crate) fn splint_cut_core(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    res: usize,
    parallel: bool,
) -> Result<SplintOutput, String> {
    if verts.is_empty() || tris.is_empty() {
        return Err("empty input".into());
    }
    if !(MIN_RES..=MAX_RES).contains(&res) {
        return Err(format!(
            "res must be between {MIN_RES} and {MAX_RES} (got {res})"
        ));
    }
    let t0 = Instant::now();
    let normalizer = Normalizer::new(verts, res);
    let nverts: Vec<[f64; 3]> = verts.iter().map(|p| normalizer.norm(*p)).collect();
    let bvh = MeshBvh::from_arrays(&nverts, tris);

    let proxy = proxy::build(&bvh, res, normalizer.eps())?;
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
    );

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
        secs_udf: proxy.secs_udf,
        secs_proxy: proxy.secs_proxy,
        secs_delaunay,
        secs_cut,
        seconds: t0.elapsed().as_secs_f64(),
    };
    Ok(SplintOutput {
        verts: emitted.verts,
        tris: emitted.tris,
        report,
    })
}

#[cfg(test)]
mod tests;
