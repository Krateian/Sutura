//! CDT solid-engine tier, ported from `sutura_engine/pipeline/tiers/cdt.py`.
//!
//! The exact constrained tetrahedralization lives in
//! `sutura_geom::cdt3d_solid_core`.  This module owns the tier cap, the
//! reload-honest one-part/strict-watertight gate, the SI guard and the report.

use std::time::Instant;

use serde_json::{json, Map, Value};
use sutura_geom::{cdt3d_solid_core, Cdt3dCoreOptions};

use super::common::{self, json_i, json_opt_i, GuardPolicy};

fn rec_base(params: &Value) -> Map<String, Value> {
    let mut rec = Map::new();
    rec.insert("tier".into(), json!("cdt"));
    rec.insert("engine".into(), json!("cdt"));
    rec.insert("ran".into(), json!(false));
    rec.insert("adopted".into(), json!(false));
    rec.insert("watertight".into(), json!(false));
    rec.insert("candidate_watertight".into(), json!(false));
    rec.insert("seconds".into(), Value::Null);
    rec.insert("wall_seconds".into(), Value::Null);
    rec.insert(
        "input_faces".into(),
        json!(json_i(params, "input_faces", 0)),
    );
    rec.insert("output_faces".into(), Value::Null);
    rec.insert("output_vertices".into(), Value::Null);
    rec.insert("output_parts".into(), Value::Null);
    rec.insert("holes".into(), Value::Null);
    rec.insert("non_manifold".into(), Value::Null);
    rec.insert("parts".into(), Value::Null);
    rec.insert("bbox_ok".into(), Value::Null);
    rec.insert("hausdorff".into(), Value::Null);
    rec.insert("verify_coverage".into(), json!(false));
    rec.insert("coverage_certified".into(), json!(false));
    rec.insert("reject_reason".into(), Value::Null);
    rec.insert("reason".into(), Value::Null);
    rec.insert("wall_timeout_ms".into(), Value::Null);
    rec.insert(
        "max_tets".into(),
        json!(json_i(params, "max_tets", 250_000)),
    );
    rec.insert("budget_limited_by".into(), Value::Null);
    rec
}

fn policy_from(params: &Value) -> GuardPolicy {
    common::policy_from_json(params.get("guard").unwrap_or(&Value::Null))
}

pub fn run(
    v: &[[f64; 3]],
    t: &[[i64; 3]],
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    params: &Value,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let mut rec = rec_base(params);
    let cap = json_opt_i(params, "cap");
    if let Some(cap) = cap {
        if input_t.len() > cap as usize {
            rec.insert("reject_reason".into(), json!("too_large"));
            rec.insert(
                "reason".into(),
                json!(format!(
                    "input has {} faces, above the CDT cap of {}; the ladder result was kept",
                    input_t.len(),
                    cap
                )),
            );
            return (v.to_vec(), t.to_vec(), Value::Object(rec));
        }
    }
    let wall_timeout_ms = json_i(params, "wall_timeout_ms", 0).max(0) as u64;
    rec.insert("wall_timeout_ms".into(), json!(wall_timeout_ms));
    let max_tets = json_i(params, "max_tets", 250_000).max(0) as usize;
    let t0 = Instant::now();
    let opts = Cdt3dCoreOptions {
        verify_coverage: false,
        max_tets,
        wall_timeout_ms,
        label_mode: "gwn_threshold",
        lambda: 0.5,
        constraint_fraction: 0.05,
        proxy_res: 128,
    };
    let t32: Vec<[i32; 3]> = input_t
        .iter()
        .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
        .collect();
    let (cand_v, cand_t32, srec) = match cdt3d_solid_core(input_v, &t32, &opts) {
        Ok(x) => x,
        Err(e) => {
            rec.insert("reject_reason".into(), json!("error"));
            rec.insert("reason".into(), json!(format!("error: cdt3d: {}", e)));
            rec.insert("wall_seconds".into(), json!(t0.elapsed().as_secs_f64()));
            return (v.to_vec(), t.to_vec(), Value::Object(rec));
        }
    };
    rec.insert("ran".into(), json!(true));
    rec.insert("wall_seconds".into(), json!(t0.elapsed().as_secs_f64()));
    rec.insert("seconds".into(), json!(srec.seconds));
    rec.insert("output_faces".into(), json!(srec.output_faces));
    rec.insert("output_vertices".into(), json!(cand_v.len()));
    rec.insert("output_parts".into(), json!(srec.parts));
    rec.insert("parts".into(), json!(srec.parts));
    rec.insert("holes".into(), json!(srec.holes));
    rec.insert("non_manifold".into(), json!(srec.nonmanifold));
    rec.insert("cap_faces".into(), json!(srec.cap_faces));
    rec.insert("cap_area".into(), json!(srec.cap_area));
    rec.insert("bbox_ok".into(), json!(srec.bbox_ok));
    rec.insert("hausdorff".into(), json!(srec.hausdorff));
    rec.insert("valid".into(), json!(srec.valid));
    rec.insert("tets".into(), json!(srec.tets));
    rec.insert(
        "constraint_cut_faces".into(),
        json!(srec.constraint_cut_faces),
    );
    rec.insert("free_cut_faces".into(), json!(srec.free_cut_faces));
    rec.insert("inside_cells".into(), json!(srec.inside_cells));
    rec.insert("orientation_used".into(), json!(srec.orientation_used));
    rec.insert(
        "orientation_components".into(),
        json!(srec.orientation_components),
    );
    rec.insert(
        "orientation_flipped".into(),
        json!(srec.orientation_flipped),
    );
    rec.insert("w_hist".into(), json!(srec.w_hist));
    rec.insert("coverage_certified".into(), json!(srec.coverage_certified));
    rec.insert("verify_coverage".into(), json!(srec.verify_coverage));
    rec.insert("max_tets".into(), json!(srec.max_tets));
    rec.insert("wall_timeout_ms".into(), json!(srec.wall_timeout_ms));
    rec.insert("hash".into(), json!(srec.hash));

    let cand_t: Vec<[i64; 3]> = cand_t32
        .iter()
        .map(|r| [r[0] as i64, r[1] as i64, r[2] as i64])
        .collect();
    if cand_t.is_empty() {
        rec.insert("reject_reason".into(), json!("empty"));
        rec.insert("reason".into(), json!("cdt3d_solid returned no geometry"));
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }
    let (wv32, wt) = match common::weld_reload_equivalent(&cand_v, &cand_t) {
        Ok(x) => x,
        Err(_) => {
            rec.insert("reject_reason".into(), json!("error"));
            rec.insert("reason".into(), json!("candidate weld failed"));
            return (v.to_vec(), t.to_vec(), Value::Object(rec));
        }
    };
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
    if cand_holes != 0 || cand_nm != 0 || srec.parts != 1 {
        rec.insert("reject_reason".into(), json!("not_watertight"));
        rec.insert(
            "reason".into(),
            json!(format!(
                "candidate not strict-watertight one-part (parts={} holes={} non-manifold={})",
                srec.parts, cand_holes, cand_nm
            )),
        );
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }
    let wv_f = common::to_f64_verts(&wv32);
    let base_si = common::exact_si_count(v, t);
    let cand_si = common::exact_si_count(&wv_f, &wt);
    let guard = common::guard_fields(
        input_t.len(),
        input_v.len(),
        wt.len(),
        wv32.len(),
        0,
        base_si,
        cand_si,
        &policy_from(params),
        false,
    );
    let guard_ok = guard
        .get("guard_ok")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if !guard_ok {
        rec.insert("reject_reason".into(), json!("guard"));
        rec.insert(
            "reason".into(),
            json!(format!(
                "rejected: {}",
                guard
                    .get("guard_reason")
                    .and_then(Value::as_str)
                    .unwrap_or("guard")
            )),
        );
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }
    rec.insert("adopted".into(), json!(true));
    rec.insert("candidate_watertight".into(), json!(true));
    rec.insert(
        "reason".into(),
        json!("strict-watertight one-part CDT candidate"),
    );
    (wv_f, wt, Value::Object(rec))
}
