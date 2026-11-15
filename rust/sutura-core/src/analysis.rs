// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Read-only per-object analysis (`--analyze`).
//!
//! Port of `sutura_engine.diagnosis.analyze_mesh` / `to_dict`. The three
//! behavioural scores that `ranking.rs` consumes (`repetition_score`,
//! `single_side_score`, `relief_score`) and the `diagnosis_v2` block are not
//! ported yet; [`Analysis::missing_fields`] names them so a caller cannot
//! mistake a partial report for a complete one.

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
    pub bbox_diagonal: f64,
    pub surface_area: f64,
    pub model: Option<String>,
    pub classifier_engine: String,
    pub possible_debris: Option<Value>,
}

impl Analysis {
    /// Fields this port does not produce yet.
    pub fn missing_fields(&self) -> &'static [&'static str] {
        &[
            "repetition_score",
            "single_side_score",
            "relief_score",
            "diagnosis_v2",
        ]
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
    out.bbox_diagonal = diag;
    out.surface_area = surface_area_f64(v, t);
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
            out.largest_loop_ratio = max_diam / diag;
        }
        let spanned: f64 = defects
            .holes
            .iter()
            .map(|h| loop_spanned_area(v, h.verts_idx.as_deref().unwrap_or(&[])))
            .sum();
        if out.surface_area > 0.0 {
            out.open_area_ratio = spanned / out.surface_area;
        }
    }

    if count_self_intersections {
        let (fv, ft) = (
            v.iter().map(|p| p.map(f64::from)).collect::<Vec<_>>(),
            t.iter()
                .map(|f| [f[0] as usize, f[1] as usize, f[2] as usize])
                .collect::<Vec<_>>(),
        );
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
