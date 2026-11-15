// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! JSON form of a diagnosis report.
//!
//! This is the single source for the report shape: the pyo3 bridge builds its
//! dict from this value (via `json.loads`), so the Python pipeline and the
//! Rust CLI cannot drift. Field order and the exact f64 values are preserved
//! (`serde_json` with `preserve_order`, `float_roundtrip`).

use serde_json::{json, Map, Value};

use super::{finite, finite3, finite6, rle, DiagReport};

fn channel(
    count: Option<i64>,
    estimated: bool,
    available: bool,
    severity: f64,
    confidence: f64,
    method: &str,
) -> Value {
    json!({
        "count": count,
        "estimated": estimated,
        "available": available,
        "severity": finite(severity),
        "confidence": finite(confidence),
        "method": method,
    })
}

fn mask_value(mask: &[u8]) -> Value {
    let runs_vec = rle(mask);
    let estimated_bytes = runs_vec.len() * 24 + 16;
    let mut out = Map::new();
    out.insert("encoding".into(), json!("rle"));
    out.insert("length".into(), json!(mask.len()));
    if estimated_bytes > 1_000_000 {
        out.insert("omitted".into(), json!(true));
        out.insert("reason".into(), json!("size"));
        return Value::Object(out);
    }
    let runs: Vec<Value> = runs_vec
        .iter()
        .map(|(s, n, v)| json!([s, n, v]))
        .collect();
    out.insert("runs".into(), Value::Array(runs));
    out.insert("omitted".into(), json!(false));
    Value::Object(out)
}

/// `report_to_pydict` as a JSON value.
pub fn report_to_value(r: &DiagReport, detail: &str, masks: bool, seconds: f64) -> Value {
    let mut root = Map::new();
    root.insert("schema_version".into(), json!(1));
    root.insert("detail".into(), json!(detail));

    let mut ch = Map::new();
    ch.insert(
        "orientation".into(),
        channel(
            Some((r.orientation.inconsistent_edges + r.orientation.flipped_faces) as i64),
            false,
            true,
            r.orientation.flipped_faces as f64,
            1.0,
            "edge_table+bfs",
        ),
    );
    ch.insert(
        "nm_vertex".into(),
        channel(
            Some(r.nm_vertices.vertices as i64),
            false,
            true,
            r.nm_vertices.vertices as f64,
            1.0,
            "vertex_fan",
        ),
    );
    ch.insert(
        "duplicate".into(),
        channel(
            Some((r.duplicates.faces + r.duplicates.near_vertices) as i64),
            true,
            true,
            (r.duplicates.faces + r.duplicates.near_vertices) as f64,
            0.9,
            "hash+grid",
        ),
    );
    ch.insert(
        "degenerate".into(),
        channel(
            Some((r.degenerate.zero_area + r.degenerate.needle) as i64),
            false,
            true,
            (r.degenerate.zero_area + r.degenerate.needle) as f64,
            0.95,
            "area+quality",
        ),
    );
    ch.insert(
        "self_intersection".into(),
        channel(
            r.self_intersections.total.map(|v| v as i64),
            r.self_intersections.estimated,
            r.self_intersections.available,
            r.self_intersections.total.unwrap_or(0) as f64,
            1.0,
            r.self_intersections.method,
        ),
    );
    ch.insert(
        "loops".into(),
        channel(
            Some(r.loops.count as i64),
            r.loops.estimated,
            true,
            r.loops.count as f64,
            1.0,
            "boundary_walk",
        ),
    );
    ch.insert(
        "parts".into(),
        channel(
            Some((r.parts.debris + r.parts.overlap_edges.len()) as i64),
            true,
            true,
            r.parts.debris as f64,
            0.8,
            "components+aabb",
        ),
    );
    ch.insert(
        "interior".into(),
        channel(
            Some(r.interior.faces as i64),
            r.interior.estimated,
            r.interior.available,
            r.interior.faces as f64,
            r.interior.confidence,
            r.interior.method,
        ),
    );
    ch.insert(
        "thin_wall".into(),
        channel(
            if r.thin_wall.available {
                Some(r.thin_wall.thin_faces as i64)
            } else {
                None
            },
            r.thin_wall.estimated,
            r.thin_wall.available,
            r.thin_wall.thin_ratio,
            r.thin_wall.confidence,
            r.thin_wall.method,
        ),
    );
    ch.insert(
        "noise".into(),
        channel(
            if r.noise.available {
                Some(r.noise.flagged_vertices as i64)
            } else {
                None
            },
            r.noise.estimated,
            r.noise.available,
            r.noise.noise_score,
            r.noise.confidence,
            r.noise.method,
        ),
    );
    root.insert("channels".into(), Value::Object(ch));

    root.insert(
        "orientation".into(),
        json!({
            "inconsistent_edges": r.orientation.inconsistent_edges,
            "flipped_faces": r.orientation.flipped_faces,
            "flipped_patches": r.orientation.flipped_patches,
            "volume_signs": r.orientation.volume_signs.iter().map(|x| finite(*x)).collect::<Vec<f64>>(),
        }),
    );
    root.insert(
        "nm_vertices".into(),
        json!({
            "vertices": r.nm_vertices.vertices,
            "bowtie": r.nm_vertices.bowtie,
            "pinched": r.nm_vertices.pinched,
        }),
    );
    root.insert(
        "duplicates".into(),
        json!({
            "faces": r.duplicates.faces,
            "reversed_faces": r.duplicates.reversed_faces,
            "vertices": r.duplicates.vertices,
            "near_vertices": r.duplicates.near_vertices,
        }),
    );
    root.insert(
        "degenerate".into(),
        json!({
            "zero_area": r.degenerate.zero_area,
            "needle": r.degenerate.needle,
            "load_bearing": r.degenerate.load_bearing,
            "quality_p99": finite(r.degenerate.quality_p99),
        }),
    );
    root.insert(
        "parts".into(),
        json!({
            "closed": r.parts.closed,
            "open": r.parts.open,
            "debris": r.parts.debris,
            "overlap_edges": r.parts.overlap_edges,
            "nested_shells": r.parts.nested_shells,
            "overlap_estimated": r.parts.overlap_estimated,
            "nested_estimated": r.parts.nested_estimated,
        }),
    );

    let mut si = Map::new();
    si.insert("total".into(), json!(r.self_intersections.total));
    si.insert("intra_body".into(), json!(r.self_intersections.intra_body));
    si.insert("inter_body".into(), json!(r.self_intersections.inter_body));
    si.insert("coplanar".into(), json!(r.self_intersections.coplanar));
    si.insert("touch".into(), json!(r.self_intersections.touch));
    si.insert("clusters".into(), json!(r.self_intersections.clusters));
    si.insert("estimated".into(), json!(r.self_intersections.estimated));
    si.insert("available".into(), json!(r.self_intersections.available));
    si.insert("reason".into(), json!(r.self_intersections.reason));
    si.insert(
        "candidate_pairs".into(),
        json!(r.self_intersections.candidate_pairs),
    );
    // Only the >1M fast path prefilters; elsewhere these keys are absent so the
    // dict matches main c137f54.
    if let Some(filtered_pairs) = r.self_intersections.filtered_pairs {
        si.insert("filtered_pairs".into(), json!(filtered_pairs));
    }
    if let Some(prefilter) = r.self_intersections.prefilter {
        si.insert("prefilter".into(), json!(prefilter));
    }
    si.insert(
        "scanned_pairs".into(),
        json!(r.self_intersections.scanned_pairs),
    );
    si.insert(
        "scanned_total".into(),
        json!(r.self_intersections.scanned_total),
    );
    root.insert("self_intersections".into(), Value::Object(si));

    let mut lp = Map::new();
    lp.insert("count".into(), json!(r.loops.count));
    lp.insert("total_length".into(), json!(r.loops.total_length));
    lp.insert("largest".into(), json!(r.loops.largest));
    lp.insert("planar".into(), json!(r.loops.planar));
    lp.insert("pinched".into(), json!(r.loops.pinched));
    lp.insert("open_edges".into(), json!(r.loops.open_edges));
    lp.insert("estimated".into(), json!(r.loops.estimated));
    let items: Vec<Value> = r
        .loops
        .items
        .iter()
        .map(|info| {
            json!({
                "kind": info.kind,
                "length": info.length,
                "perimeter": finite(info.perimeter),
                "diameter": finite(info.diameter),
                "ratio": finite(info.ratio),
                "planar": info.planar,
                "simple": info.simple,
                "pinched": info.pinched,
                "rms": finite(info.rms),
                "min_angle": finite(info.min_angle),
                "difficulty": finite(info.difficulty),
                "estimated": info.estimated,
                "bbox": finite6(info.bbox),
                "centroid": finite3(info.centroid),
            })
        })
        .collect();
    lp.insert("items".into(), Value::Array(items));
    root.insert("loops".into(), Value::Object(lp));

    root.insert(
        "interior".into(),
        json!({
            "available": r.interior.available,
            "candidates": r.interior.candidates,
            "candidate_components": r.interior.candidate_components,
            "faces": r.interior.faces,
            "low_confidence": r.interior.low_confidence,
            "fold_faces": r.interior.fold_faces,
            "estimated": r.interior.estimated,
            "method": r.interior.method,
            "confidence": finite(r.interior.confidence),
        }),
    );
    root.insert(
        "thin_wall".into(),
        json!({
            "available": r.thin_wall.available,
            "samples": r.thin_wall.samples,
            "samples_hit": r.thin_wall.samples_hit,
            "min": finite(r.thin_wall.min),
            "p5": finite(r.thin_wall.p5),
            "median": finite(r.thin_wall.median),
            "min_norm": finite(r.thin_wall.min_norm),
            "p5_norm": finite(r.thin_wall.p5_norm),
            "median_norm": finite(r.thin_wall.median_norm),
            "thin_samples": r.thin_wall.thin_samples,
            "thin_faces": r.thin_wall.thin_faces,
            "thin_ratio": finite(r.thin_wall.thin_ratio),
            "threshold_norm": finite(r.thin_wall.threshold_norm),
            "threshold_mm": finite(r.thin_wall.threshold_mm),
            "units": r.thin_wall.units,
            "sample_stride": r.thin_wall.sample_stride,
            "method": r.thin_wall.method,
            "estimated": r.thin_wall.estimated,
            "confidence": finite(r.thin_wall.confidence),
        }),
    );
    root.insert(
        "noise".into(),
        json!({
            "available": r.noise.available,
            "noise_score": finite(r.noise.noise_score),
            "noise_ratio": finite(r.noise.noise_ratio),
            "flagged_vertices": r.noise.flagged_vertices,
            "eligible_vertices": r.noise.eligible_vertices,
            "dev_p50": finite(r.noise.dev_p50),
            "dev_p95": finite(r.noise.dev_p95),
            "valence_mean": finite(r.noise.valence_mean),
            "valence_std": finite(r.noise.valence_std),
            "edge_cv": finite(r.noise.edge_cv),
            "sample_stride": r.noise.sample_stride,
            "method": r.noise.method,
            "estimated": r.noise.estimated,
            "confidence": finite(r.noise.confidence),
        }),
    );

    if masks {
        root.insert("face_mask".into(), mask_value(&r.face_mask));
        root.insert("vertex_mask".into(), mask_value(&r.vertex_mask));
    }

    let regions: Vec<Value> = r
        .regions
        .iter()
        .map(|g| {
            json!({
                "channel": g.channel,
                "kind": g.kind,
                "bbox": finite6(g.bbox),
                "centroid": finite3(g.centroid),
                "faces": g.faces,
                "vertices": g.vertices,
                "components": g.components,
                "severity": finite(g.severity),
                "confidence": finite(g.confidence),
            })
        })
        .collect();
    root.insert("regions".into(), Value::Array(regions));

    let mut meta = Map::new();
    meta.insert("faces".into(), json!(r.faces));
    meta.insert("vertices".into(), json!(r.vertices));
    meta.insert("seconds".into(), json!(finite(seconds)));
    meta.insert("phase".into(), json!(3));
    meta.insert("fast_budget_s".into(), json!(1.0));
    meta.insert("fast_budget_met".into(), json!(seconds <= 1.0));
    let mut timings = Map::new();
    let mut slowest: Option<(&str, f64)> = None;
    for (name, sec) in &r.timings {
        timings.insert((*name).to_string(), json!(finite(*sec)));
        if slowest.map(|(_, s)| *sec > s).unwrap_or(true) {
            slowest = Some((name, *sec));
        }
    }
    meta.insert("channel_seconds".into(), Value::Object(timings));
    if seconds > 1.0 {
        if let Some((name, sec)) = slowest {
            meta.insert(
                "fast_budget_reason".into(),
                json!(format!("{} ({:.2} s)", name, finite(sec))),
            );
        }
    }
    meta.insert(
        "channels_available".into(),
        json!([
            "orientation",
            "nm_vertex",
            "duplicate",
            "degenerate",
            "parts",
            "self_intersection",
            "loops",
            "interior",
            "thin_wall",
            "noise",
        ]),
    );
    root.insert("meta".into(), Value::Object(meta));

    Value::Object(root)
}
