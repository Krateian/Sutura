// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Read-only per-object analysis (`--analyze`).
//!
//! Port of `sutura_engine.diagnosis.analyze_mesh` / `to_dict`, including the
//! three behavioural scores `ranking.rs` consumes (`repetition_score`,
//! `single_side_score`, `relief_score`). Each comes from a shared kernel so the
//! two languages cannot disagree: the Closing pair from `closing_scores` and
//! `repetition_score` from `repeat_patch::detect_repetition`.

use serde_json::{json, Map, Value};

use crate::classify::{classify_mesh, py_round};
use crate::reduce::sum_f64;
use crate::report::defects::detect_defects;
use crate::validate::connected_components;
use crate::Mesh;

/// `mesh_classifier.CLASSIFIER_VERSION`.
pub const CLASSIFIER_VERSION: &str = "1";

#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub faces: usize,
    pub vertices: usize,
    pub components: usize,
    pub boundary_loops: usize,
    pub largest_loop_len: usize,
    pub largest_loop_ratio: f64,
    pub open_area_ratio: f64,
    pub non_manifold_edges: usize,
    pub self_intersections: usize,
    pub mesh_type: String,
    pub type_confidence: f64,
    /// `repeat.detect_repetition`, from the shared `repeat_patch` kernel.
    pub repetition_score: f64,
    /// `closing.single_side_score`, from the shared `closing_scores` kernel.
    pub single_side_score: f64,
    /// `closing.relief_score`, from the shared `closing_scores` kernel.
    pub relief_score: f64,
    pub bbox_diagonal: f64,
    pub surface_area: f64,
    pub model: Option<String>,
    pub classifier_engine: String,
    pub possible_debris: Option<Value>,
    pub diagnosis_v2: Option<Value>,
}

impl Analysis {
    /// Fields this port does not produce yet. Empty: `--analyze` is complete.
    pub fn missing_fields(&self) -> &'static [&'static str] {
        &[]
    }

    /// The implemented part of `to_dict(analysis)`.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("faces".into(), json!(self.faces));
        out.insert("vertices".into(), json!(self.vertices));
        out.insert("components".into(), json!(self.components));
        out.insert("boundary_loops".into(), json!(self.boundary_loops));
        out.insert("largest_loop_len".into(), json!(self.largest_loop_len));
        out.insert(
            "largest_loop_ratio".into(),
            json!(py_round(self.largest_loop_ratio, 4)),
        );
        out.insert(
            "open_area_ratio".into(),
            json!(py_round(self.open_area_ratio, 4)),
        );
        out.insert("non_manifold_edges".into(), json!(self.non_manifold_edges));
        out.insert(
            "self_intersections".into(),
            json!(self.self_intersections),
        );
        out.insert("self_intersections_estimated".into(), json!(false));
        out.insert("mesh_type".into(), json!(self.mesh_type));
        out.insert(
            "type_confidence".into(),
            json!(py_round(self.type_confidence, 3)),
        );
        // Python rounds these in `diagnosis.to_dict`: repetition to 3 decimals,
        // the Closing pair to 4. The kernels already return those precisions,
        // so the second round is idempotent.
        out.insert(
            "repetition_score".into(),
            json!(py_round(self.repetition_score, 3)),
        );
        out.insert(
            "single_side_score".into(),
            json!(py_round(self.single_side_score, 4)),
        );
        out.insert(
            "relief_score".into(),
            json!(py_round(self.relief_score, 4)),
        );
        out.insert("bbox_diagonal".into(), json!(py_round(self.bbox_diagonal, 4)));
        out.insert("surface_area".into(), json!(py_round(self.surface_area, 3)));
        out.insert(
            "model".into(),
            match &self.model {
                Some(name) => json!(name),
                None => Value::Null,
            },
        );
        out.insert("classifier_engine".into(), json!(self.classifier_engine));
        out.insert("classifier_version".into(), json!(CLASSIFIER_VERSION));
        if let Some(diag) = &self.diagnosis_v2 {
            out.insert("diagnosis_v2".into(), diag.clone());
        }
        if let Some(debris) = &self.possible_debris {
            out.insert("possible_debris".into(), debris.clone());
        }
        Value::Object(out)
    }
}

/// f64 triangle area, NumPy pairwise reduction (`diagnosis._surface_area`).
pub fn surface_area_f64(verts: &[[f32; 3]], tris: &[[i32; 3]]) -> f64 {
    if tris.is_empty() || verts.is_empty() {
        return 0.0;
    }
    let mut areas: Vec<f64> = Vec::with_capacity(tris.len());
    for f in tris {
        let (i, j, k) = (f[0] as usize, f[1] as usize, f[2] as usize);
        if i >= verts.len() || j >= verts.len() || k >= verts.len() {
            areas.push(0.0);
            continue;
        }
        let a = verts[i].map(f64::from);
        let b = verts[j].map(f64::from);
        let c = verts[k].map(f64::from);
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        areas.push(0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt());
    }
    sum_f64(&areas)
}

/// `diagnosis._non_manifold_edge_count`: edges shared by >2 faces.
pub fn non_manifold_edge_count(tris: &[[i32; 3]]) -> usize {
    let mut counts: std::collections::HashMap<(i32, i32), usize> =
        std::collections::HashMap::new();
    for t in tris {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a <= b { (a, b) } else { (b, a) };
            *counts.entry(key).or_insert(0) += 1;
        }
    }
    counts.values().filter(|&&n| n > 2).count()
}

/// `diagnosis._loop_spanned_area` (Newell's method) over one boundary loop.
pub fn loop_spanned_area(verts: &[[f32; 3]], indices: &[i64]) -> f64 {
    if indices.len() < 3 {
        return 0.0;
    }
    let pts: Vec<[f64; 3]> = indices
        .iter()
        .map(|&i| verts[i as usize].map(f64::from))
        .collect();
    let n = pts.len();
    let mut nx = Vec::with_capacity(n);
    let mut ny = Vec::with_capacity(n);
    let mut nz = Vec::with_capacity(n);
    for i in 0..n {
        let p = pts[i];
        let q = pts[(i + 1) % n];
        nx.push((p[1] - q[1]) * (p[2] + q[2]));
        ny.push((p[2] - q[2]) * (p[0] + q[0]));
        nz.push((p[0] - q[0]) * (p[1] + q[1]));
    }
    let (x, y, z) = (sum_f64(&nx), sum_f64(&ny), sum_f64(&nz));
    0.5 * (x * x + y * y + z * z).sqrt()
}

/// `diagnosis.analyze_mesh` for one mesh, minus the unported fields.
pub fn analyze_mesh(
    mesh: &Mesh,
    engine: &str,
    count_self_intersections: bool,
) -> Result<Analysis, String> {
    let v = &mesh.verts;
    let t = &mesh.tris;
    if t.is_empty() || v.is_empty() {
        return Ok(Analysis::default());
    }
    let cls = classify_mesh(
        &v.iter().map(|p| p.map(f64::from)).collect::<Vec<_>>(),
        &t.iter()
            .map(|f| [f[0] as usize, f[1] as usize, f[2] as usize])
            .collect::<Vec<_>>(),
        engine,
    );
    let mut out = Analysis {
        faces: t.len(),
        vertices: v.len(),
        mesh_type: cls
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        type_confidence: cls
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        classifier_engine: if engine == "classic" {
            "classic".to_string()
        } else {
            "experimental".to_string()
        },
        ..Analysis::default()
    };

    // `np.linalg.norm` over a float32 array: the squares and their sum stay
    // in f32, so the bbox diagonal must be computed in f32 and widened.
    let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for p in v {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let d = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let diag = f64::from((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt());
    // `diagnosis.analyze_mesh` stores `round(diag, 6)` and `to_dict` then
    // reports `round(bbox_diagonal, 4)`. Rounding the raw value straight to 4
    // decimals is not the same thing: double rounding lands on the other
    // neighbour whenever the 5th-7th decimals straddle a .5 boundary
    // (93.1711502 -> Python 93.1711, direct 93.1712). Keep both steps.
    out.bbox_diagonal = py_round(py_round(diag, 6), 4);
    // `analyze_mesh` stores `round(area, 3)` and `open_area_ratio` then divides
    // by that stored value, so the rounding has to happen here too rather than
    // only at JSON time.
    out.surface_area = py_round(surface_area_f64(v, t), 3);
    out.non_manifold_edges = non_manifold_edge_count(t);
    out.components = connected_components(t);

    let defects = detect_defects(v, t, true);
    out.boundary_loops = defects.holes.len();
    if !defects.holes.is_empty() {
        out.largest_loop_len = defects
            .holes
            .iter()
            .filter_map(|h| h.verts_idx.as_ref().map(Vec::len))
            .max()
            .unwrap_or(0);
        let max_diam = defects
            .holes
            .iter()
            .map(|h| h.diameter)
            .fold(0.0f64, f64::max);
        if diag > 0.0 {
            // `diagnosis.analyze_mesh` rounds at STORE time, and the method
            // ranking reads the stored field -- not the JSON. A raw value here
            // left `open_area_ratio` at 0.00031... where Python keeps 0.0003,
            // and the graft score multiplies it by 9 (`0.45 / 0.05`), so the
            // rounded recommendation read 0.103 vs 0.102.
            out.largest_loop_ratio = py_round(max_diam / diag, 4);
        }
        let spanned: f64 = defects
            .holes
            .iter()
            .map(|h| loop_spanned_area(v, h.verts_idx.as_deref().unwrap_or(&[])))
            .sum();
        if out.surface_area > 0.0 {
            out.open_area_ratio = py_round(spanned / out.surface_area, 4);
        }
    }

    // `diagnosis.analyze_mesh` attaches the JSON report (detail fast, masks).
    let fv: Vec<[f64; 3]> = v.iter().map(|p| p.map(f64::from)).collect();
    let ft: Vec<[usize; 3]> = t
        .iter()
        .map(|f| [f[0] as usize, f[1] as usize, f[2] as usize])
        .collect();

    // The two Closing detectors, through the shared kernel. The loop list comes
    // from `closing::boundary_loops` -- the DIRECTED half-edge walk that
    // `closing.boundary_loops` performs -- and NOT from `defects::detect_holes`,
    // whose undirected walk disagrees with it on 40 of 226 corpus meshes in loop
    // count or length. Both scores are dominated by the largest loop, so the
    // wrong walk would silently produce different numbers.
    //
    // `diagnosis.analyze_mesh` only computes these when the mesh is under
    // `CLOSING_SIGNAL_MAX_FACES` (200000); above that they stay 0.0.
    const CLOSING_SIGNAL_MAX_FACES: usize = 200_000;
    if t.len() <= CLOSING_SIGNAL_MAX_FACES {
        let tri_i64: Vec<[i64; 3]> = t
            .iter()
            .map(|f| [f[0] as i64, f[1] as i64, f[2] as i64])
            .collect();
        let loops_i64 = crate::closing::boundary_loops(&tri_i64);
        let loops: Vec<Vec<usize>> = loops_i64
            .iter()
            .map(|lp| lp.iter().map(|&i| i as usize).collect())
            .collect();
        // `analyze_mesh` stores these rounded to 4 decimals and the ranking
        // reads the stored field (the damage class tests `repetition_score >=
        // 0.5`), so round here rather than only at JSON time.
        out.single_side_score = py_round(
            sutura_geom::closing_scores::single_side_score(&fv, &ft, &loops), 4);
        out.relief_score = py_round(
            sutura_geom::closing_scores::relief_score(&fv, &ft, &loops), 4);
    }

    // `diagnosis.analyze_mesh` computes repetition only under
    // REPETITION_MAX_FACES (500000); above that it stays 0.0.
    const REPETITION_MAX_FACES: usize = 500_000;
    if t.len() <= REPETITION_MAX_FACES {
        let (rep_score, _route, _free, _patch) =
            sutura_geom::repeat_patch::detect_repetition(&fv, &ft);
        out.repetition_score = py_round(rep_score, 4);
    }

    out.diagnosis_v2 = Some(sutura_geom::diagnosis::report_json::report_to_value(
        &sutura_geom::diagnosis::diagnose_core(&fv, &ft, "fast"),
        "fast",
        true,
        0.0,
    ));

    if count_self_intersections {
        let (_mask, count, skipped) = sutura_geom::self_intersections_core(&fv, &ft);
        out.self_intersections = if skipped || count < 0 { 0 } else { count as usize };
    }

    // `diagnosis.analyze_mesh` fills `possible_debris` through the same core
    // (min_faces=8, tau=0.005 defaults), so the report block is identical.
    let fv: Vec<[f64; 3]> = v.iter().map(|p| p.map(f64::from)).collect();
    if let Some(d) = crate::report::possible_debris(&fv, t, 8, 0.005) {
        out.possible_debris = Some(json!({
            "count": d.count,
            "capped": d.capped,
            "parts": d.parts.iter().map(|p| json!({
                "faces": p.faces,
                "volume_pct": p.volume_pct,
                "size_pct": p.size_pct,
                "distance_mm": p.distance_mm,
            })).collect::<Vec<_>>(),
        }));
    }
    Ok(out)
}

/// `diagnosis.analyze_file`: analyse every object in a file.
///
/// Multi-object files go through `mesh_io::load_meshes`, the same reader the
/// pyo3 bridge exposes, so `--analyze` and the pipeline see the same objects.
pub fn analyze_file(
    path: impl AsRef<std::path::Path>,
    engine: &str,
    count_self_intersections: bool,
) -> Result<Vec<Analysis>, String> {
    let meshes = crate::mesh_io::load_meshes(path).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(meshes.len());
    for (name, mesh) in meshes {
        let mut a = analyze_mesh(&mesh, engine, count_self_intersections)?;
        a.model = name;
        out.push(a);
    }
    Ok(out)
}

/// `diagnosis.combine`: worst-case merge of several object analyses.
///
/// Element-wise max, except `boundary_loops`/`surface_area` which SUM (a hole in
/// any object is a hole in the file) and `type_confidence` which takes the MIN
/// (the file is only as trustworthy as its least confident object). The
/// per-object `model`, `diagnosis_v2` and `possible_debris` are dropped because
/// they no longer describe one thing.
pub fn combine(objs: &[Analysis]) -> Analysis {
    if objs.is_empty() {
        return Analysis::default();
    }
    if objs.len() == 1 {
        return objs[0].clone();
    }
    let mut c = objs[0].clone();
    c.model = None;
    c.diagnosis_v2 = None;
    c.possible_debris = None;
    c.faces = objs.iter().map(|o| o.faces).max().unwrap_or(0);
    c.vertices = objs.iter().map(|o| o.vertices).max().unwrap_or(0);
    c.components = objs.iter().map(|o| o.components).max().unwrap_or(0);
    c.boundary_loops = objs.iter().map(|o| o.boundary_loops).sum();
    c.largest_loop_len = objs.iter().map(|o| o.largest_loop_len).max().unwrap_or(0);
    c.largest_loop_ratio = objs
        .iter()
        .map(|o| o.largest_loop_ratio)
        .fold(0.0f64, f64::max);
    c.open_area_ratio = objs.iter().map(|o| o.open_area_ratio).fold(0.0f64, f64::max);
    c.non_manifold_edges = objs.iter().map(|o| o.non_manifold_edges).max().unwrap_or(0);
    c.self_intersections = objs.iter().map(|o| o.self_intersections).max().unwrap_or(0);
    // `self_intersections_estimated` is always false in this port (`to_json`
    // pins it), so `any(...)` would be false too -- nothing to merge.
    c.type_confidence = objs
        .iter()
        .map(|o| o.type_confidence)
        .fold(f64::INFINITY, f64::min);
    if !c.type_confidence.is_finite() {
        c.type_confidence = 0.0;
    }
    c.repetition_score = objs
        .iter()
        .map(|o| o.repetition_score)
        .fold(0.0f64, f64::max);
    c.single_side_score = objs
        .iter()
        .map(|o| o.single_side_score)
        .fold(0.0f64, f64::max);
    c.relief_score = objs.iter().map(|o| o.relief_score).fold(0.0f64, f64::max);
    c.bbox_diagonal = objs.iter().map(|o| o.bbox_diagonal).fold(0.0f64, f64::max);
    c.surface_area = objs.iter().map(|o| o.surface_area).sum();
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `analyze_mesh` rounds these at STORE time and the ranking consumes the
    /// stored field, so a raw value silently changes a recommendation.
    ///
    /// The graft score multiplies `open_area_ratio` by 9 (`0.45 / 0.05`), so a
    /// stray 5th decimal moves the reported 3-decimal score: for a raw
    /// 0.000051 the score reads 0.100, while the stored 0.0001 reads 0.101.
    #[test]
    fn store_time_rounding_matches_python() {
        assert_eq!(py_round(0.000051_f64, 4), 0.0001);
        assert_eq!(py_round(26898.114_f64, 3), 26898.114);

        let graft = |ratio: f64| 0.45 * (ratio / 0.05) + 0.30 * (1.0_f64 / 3.0);
        assert_eq!(py_round(graft(0.000051_f64), 3), 0.100);
        assert_eq!(py_round(graft(py_round(0.000051_f64, 4)), 3), 0.101);
    }
}
