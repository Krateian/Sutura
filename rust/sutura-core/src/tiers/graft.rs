//! Graft (#13) tier admission logic, ported from
//! `sutura_engine/pipeline/tiers/graft.py`.
//!
//! The shell-wrap geometry itself stays in `sutura_engine.graft` (the approved
//! `pipeline/tiers/*` scope); this module owns the cap/report hand-off, the
//! reload-honest candidate check, the blow-up/SI guard and the adopt verdict.

use serde_json::{json, Map, Value};
use sutura_geom::topology::{
    remove_duplicate_faces_core, remove_unreferenced_vertices_core, repair_non_manifold_edges_core,
    repair_non_manifold_vertices_core,
};

use super::common::{self, GuardPolicy};

fn rec_base(params: &Value, grec: &Value) -> Map<String, Value> {
    let mut rec = Map::new();
    let warnings = grec.get("warnings").cloned().unwrap_or_else(|| json!([]));
    rec.insert("tier".into(), json!("graft"));
    rec.insert("ran".into(), json!(false));
    rec.insert("adopted".into(), json!(false));
    rec.insert("watertight".into(), json!(false));
    rec.insert("fidelity_ok".into(), Value::Null);
    rec.insert("input_coverage".into(), Value::Null);
    rec.insert("coverage_ok".into(), Value::Null);
    rec.insert("hausdorff_healthy".into(), Value::Null);
    rec.insert("detail_max_mm".into(), Value::Null);
    rec.insert("detail_area_moved".into(), Value::Null);
    rec.insert("r_used".into(), Value::Null);
    rec.insert("mode".into(), Value::Null);
    rec.insert("faces".into(), Value::Null);
    rec.insert("holes".into(), Value::Null);
    rec.insert("non_manifold".into(), Value::Null);
    rec.insert("warnings".into(), warnings);
    rec.insert("reason".into(), Value::Null);
    rec.insert("skipped_budget".into(), json!(false));
    rec.insert("timed_out".into(), json!(false));
    rec.insert("low_memory".into(), json!(false));
    rec.insert("aborted".into(), json!(false));
    rec.insert("grid_fallback".into(), Value::Null);
    rec.insert("budget_limited_by".into(), Value::Null);
    rec.insert("work_budget".into(), Value::Null);
    rec.insert("grid_work_units".into(), Value::Null);
    rec.insert("grid_work_raw".into(), Value::Null);
    rec.insert("hole_work_factor".into(), Value::Null);
    rec.insert("fallback_work_budget".into(), Value::Null);
    rec.insert("grid_cells".into(), Value::Null);
    rec.insert("reject_reason".into(), Value::Null);
    rec.insert("input_faces".into(), Value::Null);
    rec.insert("input_vertices".into(), Value::Null);
    rec.insert("candidate_faces".into(), Value::Null);
    rec.insert("candidate_vertices".into(), Value::Null);
    rec.insert("guard_rejected".into(), Value::Null);
    rec.insert("guard_reason".into(), Value::Null);
    rec.insert("baseline_self_intersections".into(), Value::Null);
    rec.insert("candidate_self_intersections".into(), Value::Null);
    rec.insert("si_policy".into(), Value::Null);
    rec.insert("si_checked".into(), json!(false));
    rec.insert("si_flagged".into(), json!(false));
    rec.insert(
        "max_faces".into(),
        params.get("max_faces").cloned().unwrap_or(Value::Null),
    );
    rec.insert(
        "time_budget".into(),
        params.get("time_budget").cloned().unwrap_or(Value::Null),
    );
    rec
}

fn policy_from(params: &Value) -> GuardPolicy {
    common::policy_from_json(params.get("guard").unwrap_or(&Value::Null))
}

fn repair_welded_topology(v: &[[f32; 3]], t: &[[i64; 3]]) -> (Vec<[f32; 3]>, Vec<[i64; 3]>) {
    let mut vv = v.to_vec();
    let mut tt = t.to_vec();
    tt = repair_non_manifold_edges_core(&vv, &tt);
    tt = remove_duplicate_faces_core(&tt);
    let (nv, nt) = repair_non_manifold_vertices_core(&vv, &tt);
    vv = nv;
    tt = nt;
    let (nv, nt) = remove_unreferenced_vertices_core(&vv, &tt);
    (nv, nt)
}

fn stage1_counts(params: &Value) -> (Value, Value) {
    (
        params.get("stage1_holes").cloned().unwrap_or(Value::Null),
        params.get("stage1_nm").cloned().unwrap_or(Value::Null),
    )
}

pub fn decide(
    v: &[[f64; 3]],
    t: &[[i64; 3]],
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    cand_v: &[[f64; 3]],
    cand_t: &[[i64; 3]],
    grec: &Value,
    params: &Value,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let mut rec = rec_base(params, grec);
    rec.insert("input_faces".into(), json!(input_t.len()));
    rec.insert("input_vertices".into(), json!(input_v.len()));

    for (k, name) in [
        ("budget_exceeded", "budget_exceeded"),
        ("budget_limited_by", "budget_limited_by"),
        ("work_budget", "work_budget"),
        ("grid_work_units", "fallback_work_units"),
        ("grid_work_raw", "grid_work_raw"),
        ("hole_work_factor", "hole_work_factor"),
        ("fallback_work_budget", "fallback_work_budget"),
        ("grid_cells", "grid_cells"),
        ("grid_fallback", "grid_fallback"),
    ] {
        if let Some(val) = grec.get(name) {
            rec.insert(k.into(), val.clone());
        }
    }
    if grec.get("budget_exceeded").and_then(Value::as_bool) == Some(true) {
        rec.insert("budget_exceeded".into(), json!(true));
        rec.insert("reject_reason".into(), json!("budget"));
    }
    if grec.get("precheck_skipped").and_then(Value::as_bool) == Some(true) {
        rec.insert("ran".into(), json!(true));
        rec.insert("skipped_budget".into(), json!(true));
        rec.insert("reject_reason".into(), json!("budget"));
        rec.insert(
            "reason".into(),
            grec.get("reason").cloned().unwrap_or(Value::Null),
        );
        let (h, n) = stage1_counts(params);
        rec.insert("holes".into(), h);
        rec.insert("non_manifold".into(), n);
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }
    if grec.get("aborted").and_then(Value::as_bool) == Some(true) {
        rec.insert("ran".into(), json!(true));
        rec.insert("aborted".into(), json!(true));
        rec.insert("timed_out".into(), json!(true));
        rec.insert("reject_reason".into(), json!("aborted_time"));
        rec.insert(
            "reason".into(),
            grec.get("reason").cloned().unwrap_or(Value::Null),
        );
        let (h, n) = stage1_counts(params);
        rec.insert("holes".into(), h);
        rec.insert("non_manifold".into(), n);
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }
    if grec.get("low_memory").and_then(Value::as_bool) == Some(true) {
        rec.insert("ran".into(), json!(true));
        rec.insert("low_memory".into(), json!(true));
        rec.insert("reject_reason".into(), json!("low_memory"));
        rec.insert(
            "reason".into(),
            grec.get("reason").cloned().unwrap_or(Value::Null),
        );
        let (h, n) = stage1_counts(params);
        rec.insert("holes".into(), h);
        rec.insert("non_manifold".into(), n);
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }

    rec.insert("ran".into(), json!(true));
    for (k, name) in [
        ("fidelity_ok", "fidelity_ok"),
        ("input_coverage", "input_coverage"),
        ("coverage_ok", "coverage_ok"),
        ("hausdorff_healthy", "hausdorff_healthy"),
        ("detail_max_mm", "detail_max_mm"),
        ("detail_area_moved", "detail_area_moved"),
        ("r_used", "r_used"),
        ("mode", "mode"),
        ("seconds", "seconds"),
    ] {
        if let Some(val) = grec.get(name) {
            rec.insert(k.into(), val.clone());
        }
    }

    if cand_t.is_empty() {
        rec.insert("reason".into(), json!("graft returned no geometry"));
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }

    let (wv32, wt) = match common::weld_reload_equivalent(cand_v, cand_t) {
        Ok(x) => x,
        Err(_) => {
            rec.insert(
                "reason".into(),
                json!("candidate weld failed; stage-1 result kept"),
            );
            return (v.to_vec(), t.to_vec(), Value::Object(rec));
        }
    };
    let (wv32, wt) = repair_welded_topology(&wv32, &wt);
    let wt32: Vec<[i32; 3]> = wt
        .iter()
        .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
        .collect();
    let det = crate::report::detect_defects(&wv32, &wt32, false);
    let cand_holes = det.holes.len();
    let cand_nm = det.non_manifold.len();
    rec.insert("holes".into(), json!(cand_holes));
    rec.insert("non_manifold".into(), json!(cand_nm));
    rec.insert("candidate_faces".into(), json!(wt.len()));
    rec.insert("candidate_vertices".into(), json!(wv32.len()));
    rec.insert("faces".into(), json!(wt.len()));

    if cand_t.is_empty() || (cand_holes != 0 || cand_nm != 0) {
        rec.insert(
            "reason".into(),
            json!(format!(
                "candidate not strict-watertight (holes={} non-manifold={})",
                cand_holes, cand_nm
            )),
        );
        if rec.get("budget_exceeded").and_then(Value::as_bool) == Some(true) {
            rec.insert("skipped_budget".into(), json!(true));
            if rec.get("holes").is_none() {
                let (h, n) = stage1_counts(params);
                rec.insert("holes".into(), h);
                rec.insert("non_manifold".into(), n);
            }
            rec.insert(
                "reason".into(),
                json!(grec
                    .get("reason")
                    .cloned()
                    .unwrap_or_else(|| json!("Graft hit its deterministic work budget"))),
            );
        }
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }

    let v32: Vec<[f32; 3]> = v
        .iter()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    let t32: Vec<[i32; 3]> = t
        .iter()
        .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
        .collect();
    let in_det = crate::report::detect_defects(&v32, &t32, false);
    let improved = !in_det.holes.is_empty() || !in_det.non_manifold.is_empty();
    let wv_f = common::to_f64_verts(&wv32);
    let base_si = common::exact_si_count(v, t);
    let cand_si = common::exact_si_count(&wv_f, &wt);
    let policy = policy_from(params);
    let guard = common::guard_fields(
        input_t.len(),
        input_v.len(),
        wt.len(),
        wv32.len(),
        0,
        base_si,
        cand_si,
        &policy,
        improved,
    );
    let guard_ok = guard
        .get("guard_ok")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    for (k, val) in guard {
        if rec.contains_key(&k) && k != "guard_ok" {
            rec.insert(k, val);
        }
    }
    if !guard_ok {
        rec.insert(
            "reason".into(),
            json!(format!(
                "rejected: {}",
                rec.get("guard_reason")
                    .and_then(Value::as_str)
                    .unwrap_or("guard")
            )),
        );
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }

    let adopted_v = common::to_f64_verts(&wv32);
    rec.insert("adopted".into(), json!(true));
    rec.insert("watertight".into(), json!(false));
    rec.insert("candidate_watertight".into(), json!(true));
    rec.insert(
        "reason".into(),
        json!("strict-watertight candidate (stage 2 pending)"),
    );
    if grec.get("budget_exceeded").and_then(Value::as_bool) == Some(true) {
        rec.insert("timed_out".into(), json!(false));
        rec.insert("reject_reason".into(), Value::Null);
    }
    (adopted_v, wt, Value::Object(rec))
}
