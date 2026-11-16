// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Wall-thicken (#15) tier gate, ported from
//! `sutura_engine/pipeline/tiers/wall.py`.
//!
//! The SDF / ray-march / marching-tetrahedra builder stays in
//! `sutura_engine.wall` (the approved `pipeline/tiers/*` scope); this module
//! owns the strict-watertight gate and the report.

use serde_json::{json, Map, Value};

use super::common;

fn rec_base(params: &Value) -> Map<String, Value> {
    let mut rec = Map::new();
    rec.insert("tier".into(), json!("wall_thicken"));
    rec.insert("ran".into(), json!(true));
    rec.insert("adopted".into(), json!(false));
    rec.insert("watertight".into(), json!(false));
    rec.insert("min_before".into(), Value::Null);
    rec.insert("min_after".into(), Value::Null);
    rec.insert("min_thickness".into(), Value::Null);
    rec.insert("delta".into(), Value::Null);
    rec.insert("thin_before".into(), Value::Null);
    rec.insert("thin_after".into(), Value::Null);
    rec.insert("faces".into(), Value::Null);
    rec.insert("notes".into(), json!([]));
    rec.insert("holes".into(), Value::Null);
    rec.insert("non_manifold".into(), Value::Null);
    rec.insert("reason".into(), Value::Null);
    let _ = params;
    rec
}

pub fn decide(
    cur_v: &[[f64; 3]],
    cur_t: &[[i64; 3]],
    _input_v: &[[f64; 3]],
    _input_t: &[[i64; 3]],
    cand_v: &[[f64; 3]],
    cand_t: &[[i64; 3]],
    rep: &Value,
    params: &Value,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let mut rec = rec_base(params);
    for (k, name) in [
        ("min_thickness", "min_thickness"),
        ("min_before", "min_before"),
        ("thin_before", "thin_before"),
        ("delta", "delta"),
        ("min_after", "min_after"),
        ("thin_after", "thin_after"),
        ("notes", "notes"),
    ] {
        if let Some(v) = rep.get(name) {
            rec.insert(k.into(), v.clone());
        }
    }
    if let Some(err) = rep.get("error").and_then(Value::as_str) {
        rec.insert("reason".into(), json!(format!("module error: {}", err)));
    }
    if cand_t.is_empty() {
        if rec.get("reason").and_then(Value::as_str).is_none() {
            rec.insert("reason".into(), json!("module returned no geometry"));
        }
        return (cur_v.to_vec(), cur_t.to_vec(), Value::Object(rec));
    }
    let base_holes = common::boundary_loop_count(cur_t);
    let base_nm = common::non_two_manifold_edges(cur_t);
    let v32: Vec<[f32; 3]> = cand_v
        .iter()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    let t32: Vec<[i32; 3]> = cand_t
        .iter()
        .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
        .collect();
    let det = crate::report::detect_defects(&v32, &t32, false);
    let cand_holes = det.holes.len();
    let cand_nm = det.non_manifold.len();
    rec.insert("holes".into(), json!(cand_holes));
    rec.insert("non_manifold".into(), json!(cand_nm));
    rec.insert("faces".into(), json!(cand_t.len()));
    if cand_holes == 0 && cand_nm == 0 && cand_holes + cand_nm <= base_holes + base_nm {
        rec.insert("adopted".into(), json!(true));
        rec.insert("candidate_watertight".into(), json!(true));
        rec.insert("reason".into(), json!("walls thickened (stage 2 pending)"));
        return (cand_v.to_vec(), cand_t.to_vec(), Value::Object(rec));
    }
    if rec.get("reason").and_then(Value::as_str).is_none() {
        rec.insert(
            "reason".into(),
            json!(format!(
                "candidate not strict-watertight (holes={} non-manifold={})",
                cand_holes, cand_nm
            )),
        );
    }
    (cur_v.to_vec(), cur_t.to_vec(), Value::Object(rec))
}
