//! Scan-closing / proxy-template / mirror-closing tier, ported from
//! `sutura_engine/pipeline/tiers/closing.py`.
//!
//! The geometry algorithms stay in `sutura_engine.methods.closing`,
//! `methods.proxy` and `mirror` (the approved `pipeline/tiers/*` scope); this
//! module owns the reload-honest strict-watertight gate and the report.

use serde_json::{json, Map, Value};

use super::common;

fn rec_base(params: &Value) -> Map<String, Value> {
    let mut rec = Map::new();
    rec.insert(
        "tier".into(),
        json!(params
            .get("tier")
            .and_then(Value::as_str)
            .unwrap_or("closing")),
    );
    rec.insert("ran".into(), json!(true));
    rec.insert("adopted".into(), json!(false));
    rec.insert("watertight".into(), json!(false));
    rec.insert("holes".into(), Value::Null);
    rec.insert("non_manifold".into(), Value::Null);
    rec.insert("faces".into(), Value::Null);
    rec.insert("notes".into(), json!([]));
    rec.insert("reason".into(), Value::Null);
    rec
}

pub fn decide(
    v: &[[f64; 3]],
    t: &[[i64; 3]],
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    cand_v: &[[f64; 3]],
    cand_t: &[[i64; 3]],
    rep: &Value,
    params: &Value,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let mut rec = rec_base(params);
    let base_holes = common::boundary_loop_count(t);
    let base_nm = common::non_two_manifold_edges(t);
    if let Some(notes) = rep.get("notes") {
        rec.insert("notes".into(), notes.clone());
    }
    if let Some(err) = rep.get("error").and_then(Value::as_str) {
        rec.insert("reason".into(), json!(format!("module error: {}", err)));
    }
    if cand_t.is_empty() {
        if rec.get("reason").and_then(Value::as_str).is_none() {
            rec.insert("reason".into(), json!("module returned no geometry"));
        }
        rec.insert("ran".into(), json!(true));
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }
    let (wv32, wt) = match common::weld_reload_equivalent(cand_v, cand_t) {
        Ok(x) => x,
        Err(_) => {
            rec.insert("reason".into(), json!("candidate weld failed"));
            return (v.to_vec(), t.to_vec(), Value::Object(rec));
        }
    };
    let (wv32, wt) = common::repair_welded_topology(&wv32, &wt);
    let wt32: Vec<[i32; 3]> = wt
        .iter()
        .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
        .collect();
    let det = crate::report::detect_defects(&wv32, &wt32, false);
    let cand_holes = det.holes.len();
    let cand_nm = det.non_manifold.len();
    rec.insert("holes".into(), json!(cand_holes));
    rec.insert("non_manifold".into(), json!(cand_nm));
    rec.insert("faces".into(), json!(wt.len()));
    let _ = (input_v, input_t);
    if cand_holes == 0 && cand_nm == 0 {
        rec.insert("adopted".into(), json!(true));
        rec.insert("watertight".into(), json!(false));
        rec.insert("candidate_watertight".into(), json!(true));
        rec.insert(
            "reason".into(),
            json!("strict-watertight candidate (stage 2 pending)"),
        );
        return (common::to_f64_verts(&wv32), wt, Value::Object(rec));
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
    let _ = (base_holes, base_nm);
    (v.to_vec(), t.to_vec(), Value::Object(rec))
}
