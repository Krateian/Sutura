//! Flap tier orchestration, ported from
//! `sutura_engine/pipeline/tiers/flap.py`.
//!
//! `sutura_geom::flap::flap_fill` does the surface fill.  This module owns the
//! reload-honest damage gate, the blow-up guard, the SI flag, the debris
//! cleanup and the final flap-ON/OFF safety-net measurements.

use std::time::Instant;

use serde_json::{json, Map, Value};
use sutura_geom::flap::{flap_fill, FlapParams};

use super::common::{self, json_f, json_i, GuardPolicy};

const FLAP_SKIP_KEYS: [&str; 8] = [
    "loops_skipped_nm",
    "loops_skipped_flat",
    "loops_skipped_sheet",
    "loops_skipped_coplanar",
    "loops_skipped_sliver",
    "loops_skipped_safety",
    "loops_skipped_work",
    "loops_fan_fallback",
];

fn base_record(params: &Value) -> Map<String, Value> {
    let mut rec = Map::new();
    rec.insert("ran".into(), json!(false));
    rec.insert("adopted".into(), json!(false));
    rec.insert("reason".into(), Value::Null);
    rec.insert("baseline_holes".into(), Value::Null);
    rec.insert("baseline_non_manifold".into(), Value::Null);
    rec.insert("baseline_damage".into(), Value::Null);
    rec.insert("candidate_holes".into(), Value::Null);
    rec.insert("candidate_non_manifold".into(), Value::Null);
    rec.insert("candidate_damage".into(), Value::Null);
    rec.insert("loops_found".into(), Value::Null);
    rec.insert("loops_filled".into(), Value::Null);
    rec.insert("loops_skipped".into(), Value::Null);
    rec.insert("loops_skipped_by_reason".into(), Value::Null);
    rec.insert("patch_faces".into(), Value::Null);
    rec.insert("new_vertices".into(), Value::Null);
    rec.insert("time_s".into(), Value::Null);
    rec.insert("cache_hit".into(), json!(false));
    rec.insert("work_units".into(), Value::Null);
    rec.insert(
        "work_budget".into(),
        json!(json_i(params, "work_budget", 0)),
    );
    rec.insert("max_orig_vertex_move_mm".into(), Value::Null);
    rec.insert("input_faces".into(), Value::Null);
    rec.insert("input_vertices".into(), Value::Null);
    rec.insert("candidate_faces".into(), Value::Null);
    rec.insert("candidate_vertices".into(), Value::Null);
    rec.insert("loop_vertex_total".into(), Value::Null);
    rec.insert("slack_units".into(), Value::Null);
    rec.insert("baseline_self_intersections".into(), Value::Null);
    rec.insert("candidate_self_intersections".into(), Value::Null);
    rec.insert("si_flagged".into(), json!(false));
    rec.insert("si_checked".into(), json!(false));
    rec.insert("si_policy".into(), json!("flag"));
    rec.insert("guard_rejected".into(), Value::Null);
    rec.insert("guard_reason".into(), Value::Null);
    rec
}

fn loop_vertex_total(report: &sutura_geom::flap::Report) -> usize {
    let total: usize = report.loop_log.iter().map(|e| e.m).sum();
    if total > 0 {
        total
    } else {
        3 * report.loops_found
    }
}

fn max_orig_move(report: &sutura_geom::flap::Report) -> f64 {
    let mut max = 0.0f64;
    if let Some(info) = &report.boundary_normalize {
        for s in &info.weld_sites {
            if s.is_orig && s.gap > max {
                max = s.gap;
            }
        }
    }
    (max * 1_000_000.0).round() / 1_000_000.0
}

fn policy_from(p: &Value) -> GuardPolicy {
    common::policy_from_json(p.get("guard").unwrap_or(&Value::Null))
}

/// Run the Flap pre-pass tier.  Returns `(out_verts, out_tris, report_json)`.
pub fn run(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    params: &Value,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let mut rec = base_record(params);
    let v = verts.to_vec();
    let t = tris.to_vec();

    let (base_holes, base_nm) = common::reload_strict_holes_nm(&v, &t);
    rec.insert("baseline_holes".into(), json!(base_holes));
    rec.insert("baseline_non_manifold".into(), json!(base_nm));
    if base_holes == 0 && base_nm == 0 {
        rec.insert(
            "reason".into(),
            json!("no residual boundary loops or non-manifold edges"),
        );
        return (v, t, Value::Object(rec));
    }

    let budget = json_i(params, "work_budget", 0);
    if budget <= 0 {
        let intensity = params
            .get("intensity")
            .and_then(Value::as_str)
            .unwrap_or("balanced");
        rec.insert(
            "reason".into(),
            json!(format!(
                "budget: disabled by intensity preset '{}'",
                intensity
            )),
        );
        return (v, t, Value::Object(rec));
    }

    let t0 = Instant::now();
    let t_u32: Vec<[u32; 3]> = t
        .iter()
        .map(|r| [r[0] as u32, r[1] as u32, r[2] as u32])
        .collect();
    let prm = FlapParams {
        separate_stl: false,
        work_budget: Some(budget as u64),
        ..Default::default()
    };
    let (cand_v, cand_t, frep) = flap_fill(&v, &t_u32, &prm);
    rec.insert("ran".into(), json!(true));
    rec.insert(
        "time_s".into(),
        json!((t0.elapsed().as_secs_f64() * 1000.0).round() / 1000.0),
    );
    rec.insert("work_units".into(), json!(frep.work_units));
    rec.insert("work_budget".into(), json!(budget));
    rec.insert("loops_found".into(), json!(frep.loops_found));
    rec.insert("loops_filled".into(), json!(frep.loops_filled));
    rec.insert("loops_skipped".into(), json!(frep.loops_skipped));
    let mut skipped = Map::new();
    let values = [
        frep.loops_skipped_nm,
        frep.loops_skipped_flat,
        frep.loops_skipped_sheet,
        frep.loops_skipped_coplanar,
        frep.loops_skipped_sliver,
        frep.loops_skipped_safety,
        frep.loops_skipped_work,
        frep.loops_fan_fallback,
    ];
    for (k, v) in FLAP_SKIP_KEYS.iter().zip(values.iter()) {
        skipped.insert((*k).to_string(), json!(*v));
    }
    rec.insert("loops_skipped_by_reason".into(), Value::Object(skipped));
    rec.insert("patch_faces".into(), json!(frep.patch_faces));
    rec.insert("new_vertices".into(), json!(frep.new_vertices));
    rec.insert(
        "max_orig_vertex_move_mm".into(),
        json!(max_orig_move(&frep)),
    );

    let cand_i = common::to_i64_tris(&cand_t);
    if cand_i.is_empty() {
        rec.insert("reason".into(), json!("flap returned no geometry"));
        return (v, t, Value::Object(rec));
    }
    let (cand_holes, cand_nm) = common::reload_strict_holes_nm(&cand_v, &cand_i);
    rec.insert("candidate_holes".into(), json!(cand_holes));
    rec.insert("candidate_non_manifold".into(), json!(cand_nm));
    let base_damage = base_holes + base_nm;
    let cand_damage = cand_holes + cand_nm;
    rec.insert("baseline_damage".into(), json!(base_damage));
    rec.insert("candidate_damage".into(), json!(cand_damage));

    if frep.work_units > budget as u64 {
        rec.insert("budget_exceeded".into(), json!(true));
    }

    let would_adopt = cand_damage <= base_damage;
    if would_adopt {
        let loop_vertices = loop_vertex_total(&frep);
        rec.insert("loop_vertex_total".into(), json!(loop_vertices));
        rec.insert("slack_units".into(), json!(loop_vertices));
        let base_si = common::exact_si_count(&v, &t);
        let cand_si = common::exact_si_count(&cand_v, &cand_i);
        rec.insert(
            "baseline_self_intersections".into(),
            base_si.map(Value::from).unwrap_or(Value::Null),
        );
        rec.insert(
            "candidate_self_intersections".into(),
            cand_si.map(Value::from).unwrap_or(Value::Null),
        );
        let policy = policy_from(params);
        let guard = common::guard_fields(
            t.len(),
            v.len(),
            cand_i.len(),
            cand_v.len(),
            loop_vertices as i64,
            base_si,
            cand_si,
            &policy,
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
            return (v, t, Value::Object(rec));
        }
        rec.insert("adopted".into(), json!(true));
        rec.insert(
            "reason".into(),
            json!(format!(
                "adopted (holes {}->{}, non-manifold {}->{}; damage {}->{})",
                base_holes, cand_holes, base_nm, cand_nm, base_damage, cand_damage
            )),
        );
        return (cand_v, cand_i, Value::Object(rec));
    }

    rec.insert(
        "reason".into(),
        json!(format!(
            "rejected: reload damage {}->{} would worsen (holes {}->{}, non-manifold {}->{})",
            base_damage, cand_damage, base_holes, cand_holes, base_nm, cand_nm
        )),
    );
    (v, t, Value::Object(rec))
}

fn reload_component_count(v: &[[f64; 3]], t: &[[i64; 3]]) -> usize {
    let (wv, wt) = common::weld_reload_equivalent(v, t).unwrap_or_default();
    if wt.is_empty() {
        return 0;
    }
    common::count_vertex_components(wv.len(), &wt)
}

pub fn si_rose(rec: &Value) -> bool {
    let base = rec
        .get("baseline_self_intersections")
        .and_then(Value::as_i64);
    let cand = rec
        .get("candidate_self_intersections")
        .and_then(Value::as_i64);
    match (base, cand) {
        (Some(a), Some(b)) => b > a,
        _ => false,
    }
}

/// Flap final safety net: returns `(reason, metrics)`.
pub fn final_off(
    in_v: &[[f64; 3]],
    in_t: &[[i64; 3]],
    out_v: &[[f64; 3]],
    out_t: &[[i64; 3]],
    measure_si: bool,
    si_flagged: bool,
) -> (Option<String>, Option<Value>) {
    let (holes, nm) = common::reload_strict_holes_nm(out_v, out_t);
    let parts_in = reload_component_count(in_v, in_t);
    let parts_out = reload_component_count(out_v, out_t);
    let si = if measure_si {
        common::exact_si_count(out_v, out_t)
    } else {
        None
    };
    let metrics = json!({
        "holes": holes,
        "non_manifold": nm,
        "input_parts": parts_in,
        "output_parts": parts_out,
        "si": si,
        "watertight": holes == 0 && nm == 0,
    });
    let reason = if holes != 0 || nm != 0 {
        Some("not_watertight".to_string())
    } else if parts_out > parts_in {
        Some("more_parts".to_string())
    } else if si_flagged {
        Some("si_candidate_rise".to_string())
    } else {
        None
    };
    (reason, Some(metrics))
}

/// Whether the flap-OFF result should replace the flap-ON one.
pub fn off_preferred(on_reason: &str, on: &Value, off: &Value) -> bool {
    let damage = |m: &Value| -> i64 {
        m.get("holes").and_then(Value::as_i64).unwrap_or(0)
            + m.get("non_manifold").and_then(Value::as_i64).unwrap_or(0)
    };
    let on_damage = damage(on);
    let off_damage = damage(off);
    if off_damage != on_damage {
        return off_damage < on_damage;
    }
    let on_parts = on.get("output_parts").and_then(Value::as_i64).unwrap_or(0);
    let off_parts = off.get("output_parts").and_then(Value::as_i64).unwrap_or(0);
    if off_parts != on_parts {
        if on_reason == "si_candidate_rise" {
            return false;
        }
        return off_parts < on_parts;
    }
    match (
        on.get("si").and_then(Value::as_i64),
        off.get("si").and_then(Value::as_i64),
    ) {
        (Some(a), Some(b)) => b < a,
        _ => false,
    }
}

/// Flap debris cleanup (`drop_flap_debris`).  Returns `(verts, tris, info)`.
pub fn debris(
    in_v: &[[f64; 3]],
    in_t: &[[i64; 3]],
    out_v: &[[f64; 3]],
    out_t: &[[i64; 3]],
    params: &Value,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Option<Value>) {
    let (iv, _it) = match common::weld_reload_equivalent(in_v, in_t) {
        Ok(x) => x,
        Err(_) => return (out_v.to_vec(), out_t.to_vec(), None),
    };
    let (wv, wt) = match common::weld_reload_equivalent(out_v, out_t) {
        Ok(x) => x,
        Err(_) => return (out_v.to_vec(), out_t.to_vec(), None),
    };
    if iv.is_empty() || wt.is_empty() {
        return (out_v.to_vec(), out_t.to_vec(), None);
    }
    let wv_f = common::to_f64_verts(&wv);
    let n = wv.len();
    let (ncomp, labels) = common::component_labels(n, &wt);
    let face_label: Vec<i64> = wt.iter().map(|t| labels[t[0] as usize]).collect();
    let mut face_counts = vec![0i64; ncomp];
    for &l in &face_label {
        face_counts[l as usize] += 1;
    }
    let in_diag = common::bbox_diagonal(&common::to_f64_verts(&iv));
    let eps = json_f(params, "debris_volume_eps", 1e-9);
    let rel = json_f(params, "debris_volume_rel", 1e-9);
    let flat_th = json_f(params, "debris_flatness", 0.01);
    let vol_eps = eps.max(in_diag.powi(3) * rel);
    let main = face_counts
        .iter()
        .enumerate()
        .max_by_key(|(_, c)| **c)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let mut keep = vec![true; ncomp];
    let mut dropped = Vec::new();
    for c in 0..ncomp {
        if c == main || face_counts[c] == 0 {
            continue;
        }
        let tri: Vec<[i64; 3]> = wt
            .iter()
            .zip(face_label.iter())
            .filter(|(_, l)| **l == c as i64)
            .map(|(t, _)| *t)
            .collect();
        let p0: Vec<[f64; 3]> = tri.iter().map(|t| wv_f[t[0] as usize]).collect();
        let p1: Vec<[f64; 3]> = tri.iter().map(|t| wv_f[t[1] as usize]).collect();
        let p2: Vec<[f64; 3]> = tri.iter().map(|t| wv_f[t[2] as usize]).collect();
        let mut vol = 0.0f64;
        let mut area = 0.0f64;
        for i in 0..tri.len() {
            let a = p0[i];
            let b = p1[i];
            let c2 = p2[i];
            let cx = b[1] * c2[2] - b[2] * c2[1];
            let cy = b[2] * c2[0] - b[0] * c2[2];
            let cz = b[0] * c2[1] - b[1] * c2[0];
            vol += a[0] * cx + a[1] * cy + a[2] * cz;
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let w = [c2[0] - a[0], c2[1] - a[1], c2[2] - a[2]];
            let nx = u[1] * w[2] - u[2] * w[1];
            let ny = u[2] * w[0] - u[0] * w[2];
            let nz = u[0] * w[1] - u[1] * w[0];
            area += 0.5 * (nx * nx + ny * ny + nz * nz).sqrt();
        }
        vol = (vol / 6.0).abs();
        let mut verts: Vec<usize> = tri
            .iter()
            .flat_map(|t| t.iter().map(|x| *x as usize))
            .collect();
        verts.sort_unstable();
        verts.dedup();
        let mut lo = wv_f[verts[0]];
        let mut hi = lo;
        for &vi in &verts {
            let p = wv_f[vi];
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let dx = hi[0] - lo[0];
        let dy = hi[1] - lo[1];
        let dz = hi[2] - lo[2];
        let diag = (dx * dx + dy * dy + dz * dz).sqrt();
        let thickness = if area > 0.0 { 2.0 * vol / area } else { 0.0 };
        let flat = area <= 0.0 || diag <= 0.0 || vol <= 0.0 || thickness <= flat_th * diag;
        if flat && vol <= vol_eps {
            keep[c] = false;
            dropped.push(json!({
                "faces": face_counts[c],
                "volume": vol,
                "thickness": thickness,
                "diag": diag,
                "centroid": [(lo[0]+hi[0])/2.0, (lo[1]+hi[1])/2.0, (lo[2]+hi[2])/2.0],
            }));
        }
    }
    if dropped.is_empty() {
        return (out_v.to_vec(), out_t.to_vec(), None);
    }
    let fkeep: Vec<bool> = face_label.iter().map(|l| keep[*l as usize]).collect();
    if !fkeep.iter().any(|x| *x) {
        return (out_v.to_vec(), out_t.to_vec(), None);
    }
    let kept: Vec<[i64; 3]> = wt
        .iter()
        .zip(fkeep.iter())
        .filter(|(_, k)| **k)
        .map(|(t, _)| *t)
        .collect();
    let (cv, ct) = match common::weld_reload_equivalent(&wv_f, &kept) {
        Ok(x) => x,
        Err(_) => return (out_v.to_vec(), out_t.to_vec(), None),
    };
    let ct_i = ct;
    let mut after_labels = vec![false; ncomp];
    for (l, k) in face_label.iter().zip(fkeep.iter()) {
        if *k {
            after_labels[*l as usize] = true;
        }
    }
    let info = json!({
        "components_before": face_counts.iter().filter(|c| **c > 0).count(),
        "components_after": after_labels.iter().filter(|x| **x).count(),
        "dropped_faces": fkeep.iter().filter(|x| !**x).count(),
        "dropped_volume": dropped.iter().filter_map(|d| d.get("volume").and_then(Value::as_f64)).sum::<f64>(),
        "volume_eps": vol_eps,
        "dropped": dropped,
    });
    (common::to_f64_verts(&cv), ct_i, Some(info))
}

pub fn final_off_json(
    inv: &[[f64; 3]],
    int: &[[i64; 3]],
    outv: &[[f64; 3]],
    outt: &[[i64; 3]],
    measure_si: bool,
    si_flagged: bool,
) -> (Option<String>, Option<Value>) {
    final_off(inv, int, outv, outt, measure_si, si_flagged)
}

pub fn off_preferred_json(reason: &str, on: &Value, off: &Value) -> bool {
    off_preferred(reason, on, off)
}

pub fn si_rose_json(rec: &Value) -> bool {
    si_rose(rec)
}
