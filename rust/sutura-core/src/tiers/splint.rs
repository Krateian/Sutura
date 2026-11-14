//! Splint (#18) tier orchestration, ported from
//! `sutura_engine/pipeline/tiers/splint.py`.
//!
//! `sutura_geom::splint::splint_cut_core_opts` builds the exact cut and
//! `sutura_geom::manifold::stage2_rebuild_core` rebuilds a non-manifold cut as
//! a closed Manifold solid.  This module owns the cap/work budget, the
//! reload-honest one-part gate, the SI guard and the report.

use std::time::Instant;

use serde_json::{json, Map, Value};
use sutura_geom::manifold::stage2_rebuild_core;
use sutura_geom::splint::{splint_cut_core_opts, SplintAdaptive, SplintOpts, SplintReport};

use super::common::{self, json_i, json_opt_i, GuardPolicy};

fn rec_base(params: &Value) -> Map<String, Value> {
    let mut rec = Map::new();
    rec.insert("tier".into(), json!("splint"));
    rec.insert("ran".into(), json!(false));
    rec.insert("adopted".into(), json!(false));
    rec.insert("watertight".into(), json!(false));
    rec.insert("res".into(), json!(json_i(params, "res", 128)));
    rec.insert(
        "adaptive".into(),
        json!(params
            .get("adaptive")
            .and_then(Value::as_str)
            .unwrap_or("retry")),
    );
    rec.insert("input_faces".into(), Value::Null);
    rec.insert("output_faces".into(), Value::Null);
    rec.insert("output_vertices".into(), Value::Null);
    rec.insert("output_parts".into(), Value::Null);
    rec.insert("out_manifold".into(), Value::Null);
    rec.insert("holes".into(), Value::Null);
    rec.insert("non_manifold".into(), Value::Null);
    rec.insert("construct_status".into(), Value::Null);
    rec.insert("manifold_postprocessed".into(), json!(false));
    rec.insert("seconds".into(), Value::Null);
    rec.insert("skipped_budget".into(), json!(false));
    rec.insert("aborted".into(), json!(false));
    rec.insert("reject_reason".into(), Value::Null);
    rec.insert("reason".into(), Value::Null);
    rec.insert("warnings".into(), json!([]));
    rec.insert("guard_rejected".into(), Value::Null);
    rec.insert("guard_reason".into(), Value::Null);
    rec.insert("baseline_self_intersections".into(), Value::Null);
    rec.insert("candidate_self_intersections".into(), Value::Null);
    rec.insert("si_policy".into(), Value::Null);
    rec.insert("si_checked".into(), json!(false));
    rec.insert("si_flagged".into(), json!(false));
    rec.insert(
        "max_faces".into(),
        params.get("cap").cloned().unwrap_or(Value::Null),
    );
    rec.insert("work_units".into(), Value::Null);
    rec.insert("work_budget".into(), json!(json_i(params, "work_limit", 0)));
    rec.insert("budget_limited_by".into(), Value::Null);
    rec
}

fn policy_from(params: &Value) -> GuardPolicy {
    common::policy_from_json(params.get("guard").unwrap_or(&Value::Null))
}

fn core_json(r: &SplintReport) -> Value {
    json!({
        "ok": r.ok,
        "res": r.res,
        "input_vertices": r.input_vertices,
        "input_faces": r.input_faces,
        "proxy_vertices": r.proxy_vertices,
        "tets": r.tets,
        "internal_faces": r.internal_faces,
        "outer_faces": r.outer_faces,
        "out_tets": r.out_tets,
        "output_vertices": r.output_vertices,
        "output_faces": r.output_faces,
        "output_parts": r.output_parts,
        "out_manifold": r.out_manifold,
        "input_comps": r.input_comps,
        "inside_comps": r.inside_comps,
        "secs_adaptive": r.secs_adaptive,
        "secs_udf": r.secs_udf,
        "secs_proxy": r.secs_proxy,
        "secs_delaunay": r.secs_delaunay,
        "secs_cut": r.secs_cut,
        "seconds": r.seconds,
    })
}

fn stage1_counts(params: &Value) -> (Value, Value) {
    (
        params.get("stage1_holes").cloned().unwrap_or(Value::Null),
        params.get("stage1_nm").cloned().unwrap_or(Value::Null),
    )
}

pub fn run(
    v: &[[f64; 3]],
    t: &[[i64; 3]],
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    params: &Value,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let t0 = Instant::now();
    let mut rec = rec_base(params);
    rec.insert("input_faces".into(), json!(input_t.len()));
    let cap = json_opt_i(params, "cap");
    if let Some(cap) = cap {
        if input_t.len() > cap as usize {
            rec.insert("skipped_budget".into(), json!(true));
            rec.insert("reject_reason".into(), json!("too_large"));
            let (h, n) = stage1_counts(params);
            rec.insert("holes".into(), h);
            rec.insert("non_manifold".into(), n);
            rec.insert(
                "reason".into(),
                json!(format!(
                    "input has {} faces, above the Splint cap of {}; the ladder result was kept",
                    input_t.len(),
                    cap
                )),
            );
            return (v.to_vec(), t.to_vec(), Value::Object(rec));
        }
    }
    let res = json_i(params, "res", 128).max(0) as usize;
    let mode = params
        .get("adaptive")
        .and_then(Value::as_str)
        .unwrap_or("retry");
    let adaptive = if mode == "retry" {
        SplintAdaptive::Retry
    } else {
        SplintAdaptive::None
    };
    let ut: Vec<[usize; 3]> = input_t
        .iter()
        .map(|r| [r[0] as usize, r[1] as usize, r[2] as usize])
        .collect();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        splint_cut_core_opts(input_v, &ut, res, adaptive, true, SplintOpts::default())
    }));
    let out = match outcome {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => {
            rec.insert("reject_reason".into(), json!("error"));
            rec.insert("reason".into(), json!(format!("splint error: {}", e)));
            return (v.to_vec(), t.to_vec(), Value::Object(rec));
        }
        Err(_) => {
            rec.insert("reject_reason".into(), json!("error"));
            rec.insert("reason".into(), json!("splint panicked"));
            return (v.to_vec(), t.to_vec(), Value::Object(rec));
        }
    };
    let cand_v = out.verts;
    let cand_t_usize = out.tris;
    let srec = out.report;
    rec.insert("ran".into(), json!(true));
    rec.insert("seconds".into(), json!(srec.seconds));
    rec.insert("output_faces".into(), json!(srec.output_faces));
    rec.insert("output_vertices".into(), json!(srec.output_vertices));
    rec.insert("output_parts".into(), json!(srec.output_parts));
    rec.insert("out_manifold".into(), json!(srec.out_manifold));
    rec.insert("core".into(), core_json(&srec));
    let work_units = srec.tets as i64;
    rec.insert("work_units".into(), json!(work_units));
    let work_limit = json_i(params, "work_limit", 0);
    if work_limit > 0 && work_units > work_limit {
        rec.insert("aborted".into(), json!(true));
        rec.insert("budget_limited_by".into(), json!("work"));
        rec.insert("reject_reason".into(), json!("work"));
        let (h, n) = stage1_counts(params);
        rec.insert("holes".into(), h);
        rec.insert("non_manifold".into(), n);
        rec.insert(
            "reason".into(),
            json!(format!(
                "Splint used {} work units > {} at intensity {:?}; the ladder result was kept",
                work_units,
                work_limit,
                params
                    .get("intensity")
                    .and_then(Value::as_str)
                    .unwrap_or("balanced")
            )),
        );
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }

    let cand_t: Vec<[i64; 3]> = cand_t_usize
        .iter()
        .map(|r| [r[0] as i64, r[1] as i64, r[2] as i64])
        .collect();
    if cand_t.is_empty() {
        rec.insert("reason".into(), json!("splint returned no geometry"));
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }
    let (mut wv32, mut wt) = match common::weld_reload_equivalent(&cand_v, &cand_t) {
        Ok(x) => x,
        Err(_) => {
            rec.insert("reject_reason".into(), json!("error"));
            rec.insert("reason".into(), json!("candidate weld failed"));
            return (v.to_vec(), t.to_vec(), Value::Object(rec));
        }
    };
    let mut cand_holes;
    let mut cand_nm;
    {
        let wt32: Vec<[i32; 3]> = wt
            .iter()
            .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
            .collect();
        let det = crate::report::detect_defects(&wv32, &wt32, false);
        cand_holes = det.holes.len();
        cand_nm = det.non_manifold.len();
    }
    let mut two_manifold = common::is_two_manifold(wv32.len(), &wt);
    if !(cand_holes == 0 && cand_nm == 0 && two_manifold) {
        let vf: Vec<[f64; 3]> = common::to_f64_verts(&wv32);
        let ti32: Vec<[i32; 3]> = wt
            .iter()
            .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
            .collect();
        let (m3_v, m3_t, m3rep) = stage2_rebuild_core(&vf, &ti32);
        if m3rep.error.is_none() && !m3_t.is_empty() {
            rec.insert("construct_status".into(), json!(m3rep.construct_status));
            let m3_v64: Vec<[f64; 3]> = m3_v
                .iter()
                .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
                .collect();
            let m3_t64: Vec<[i64; 3]> = m3_t
                .iter()
                .map(|r| [r[0] as i64, r[1] as i64, r[2] as i64])
                .collect();
            if let Ok((m3w32, m3wt)) = common::weld_reload_equivalent(&m3_v64, &m3_t64) {
                let m3wt32: Vec<[i32; 3]> = m3wt
                    .iter()
                    .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
                    .collect();
                let det = crate::report::detect_defects(&m3w32, &m3wt32, false);
                let m3_holes = det.holes.len();
                let m3_nm = det.non_manifold.len();
                if m3_holes <= cand_holes && m3_nm <= cand_nm {
                    wv32 = m3w32;
                    wt = m3wt;
                    cand_holes = m3_holes;
                    cand_nm = m3_nm;
                    two_manifold = common::is_two_manifold(wv32.len(), &wt);
                    rec.insert("manifold_postprocessed".into(), json!(true));
                }
            }
        }
    }
    rec.insert("holes".into(), json!(cand_holes));
    rec.insert("non_manifold".into(), json!(cand_nm));
    rec.insert("faces".into(), json!(wt.len()));

    if cand_holes == 0 && cand_nm == 0 {
        let wf = common::to_f64_verts(&wv32);
        let base_si = common::exact_si_count(v, t);
        let cand_si = common::exact_si_count(&wf, &wt);
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
        rec.insert("adopted".into(), json!(true));
        rec.insert("candidate_watertight".into(), json!(true));
        rec.insert(
            "reason".into(),
            json!("strict-watertight candidate (stage 2 pending)"),
        );
        let out_v = common::to_f64_verts(&wv32);
        return (out_v, wt, Value::Object(rec));
    }
    rec.insert(
        "reason".into(),
        json!(format!(
            "candidate not strict-watertight (holes={} non-manifold={})",
            cand_holes, cand_nm
        )),
    );
    let _ = t0;
    (v.to_vec(), t.to_vec(), Value::Object(rec))
}
