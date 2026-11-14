//! Tamponade (#17) tier orchestration, ported from
//! `sutura_engine/pipeline/tiers/tamponade.py`.
//!
//! The Rust `sutura_geom::tamponade::tamponade_repair` core does the plug /
//! melt work.  This module owns the Python tier's acceptance logic: the shape
//! suitability card, the reload-honest damage gate, the exact-SI guard, the
//! plug leak test, the final bbox/leak safety net and the report fields.

use std::panic::{catch_unwind, AssertUnwindSafe};

use serde_json::{json, Map, Value};
use sutura_geom::diagnosis;
use sutura_geom::melt;
use sutura_geom::tamponade::{self, TamponadeConfig, TamponadeInfo};

use super::common::{self, json_b, json_f, json_i, json_opt_i, json_s, json_u, GuardPolicy};

fn thin_sample_fraction(v: &[[f64; 3]], t: &[[i64; 3]]) -> Option<f64> {
    let ut = common::to_usize_tris(t);
    let d = diagnosis::diagnose_core(v, &ut, "fast");
    if !d.thin_wall.available {
        None
    } else {
        Some(d.thin_wall.thin_ratio)
    }
}

fn solid_area_fraction(v: &[[f64; 3]], t: &[[i64; 3]], p: &Value) -> Option<f64> {
    let samples = json_u(p, "shape_side_samples", 512);
    let side_eps = json_f(p, "shape_side_eps", 1e-4);
    let winding_thresh = json_f(p, "shape_winding", 0.9);
    if v.is_empty() || t.is_empty() {
        return None;
    }
    let mut tt: Vec<[i64; 3]> = t.to_vec();
    if t.len() > samples {
        let mut sel: Vec<usize> = Vec::with_capacity(samples);
        if samples > 1 {
            let denom = (samples - 1) as f64;
            for i in 0..samples {
                let x = (t.len() - 1) as f64 * (i as f64) / denom;
                sel.push(x as usize);
            }
        } else {
            sel.push(0);
        }
        sel.sort_unstable();
        sel.dedup();
        tt = sel.iter().map(|&i| t[i]).collect();
    }
    let mut areas = Vec::with_capacity(tt.len());
    let mut normals = Vec::with_capacity(tt.len());
    let mut centers = Vec::with_capacity(tt.len());
    let mut total = 0.0f64;
    for tri in &tt {
        let a = v[tri[0] as usize];
        let b = v[tri[1] as usize];
        let c = v[tri[2] as usize];
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        let area = 0.5 * len;
        total += area;
        areas.push(area);
        let normal = if len > 1e-12 {
            [n[0] / len, n[1] / len, n[2] / len]
        } else {
            [0.0, 0.0, 0.0]
        };
        normals.push(normal);
        centers.push([
            (a[0] + b[0] + c[0]) / 3.0,
            (a[1] + b[1] + c[1]) / 3.0,
            (a[2] + b[2] + c[2]) / 3.0,
        ]);
    }
    if total <= 0.0 {
        return None;
    }
    let diag = common::bbox_diagonal(v);
    let eps = diag * side_eps;
    if !(eps > 0.0) {
        return None;
    }
    let mut pts = Vec::with_capacity(2 * centers.len());
    for i in 0..centers.len() {
        let n = normals[i];
        let c = centers[i];
        pts.push([c[0] + n[0] * eps, c[1] + n[1] * eps, c[2] + n[2] * eps]);
    }
    for i in 0..centers.len() {
        let n = normals[i];
        let c = centers[i];
        pts.push([c[0] - n[0] * eps, c[1] - n[1] * eps, c[2] - n[2] * eps]);
    }
    let w = common::winding_points(v, t, &pts);
    let n = tt.len();
    if w.len() < 2 * n {
        return None;
    }
    let mut hit = 0.0f64;
    for i in 0..n {
        let mx = w[i].abs().max(w[n + i].abs());
        if mx > winding_thresh {
            hit += areas[i];
        }
    }
    Some(hit / total)
}

fn interior_depth_norm(v: &[[f64; 3]], t: &[[i64; 3]], p: &Value) -> Option<f64> {
    let cells = json_u(p, "shape_depth_cells", 32768);
    let ut = common::to_usize_tris(t);
    melt::interior_depth_norm(v, &ut, cells)
}

fn shape_card_full(
    v: &[[f64; 3]],
    t: &[[i64; 3]],
    holes: &[Vec<i64>],
    genus: Option<i64>,
    p: &Value,
) -> Value {
    let area = common::surface_area(v, t);
    let vol = common::signed_volume(v, t).abs();
    let compactness = if area > 0.0 {
        36.0 * std::f64::consts::PI * vol * vol / (area * area * area)
    } else {
        0.0
    };
    let mut open_area = 0.0f64;
    for loop_idx in holes {
        open_area += common::loop_spanned_area(v, loop_idx);
    }
    let open_ratio = if area > 0.0 { open_area / area } else { 0.0 };
    let diag = common::bbox_diagonal(v);
    let thickness = if area > 0.0 && diag > 0.0 {
        2.0 * vol / area / diag
    } else {
        0.0
    };
    let compact_th = json_f(p, "shape_compactness", 0.05);
    let open_th = json_f(p, "shape_open_area", 0.2);
    let thin_th = json_f(p, "shape_thin", 0.5);
    let thickness_th = json_f(p, "shape_thickness", 0.01);
    let perim_th = json_f(p, "shape_perimeter_ratio", 2.0);
    let solid_th = json_f(p, "shape_solid_area", 0.2);
    let thin = if compactness <= compact_th && open_ratio >= open_th {
        thin_sample_fraction(v, t)
    } else {
        None
    };
    let thin_shell = compactness <= compact_th
        && open_ratio >= open_th
        && thin.is_some()
        && thin.unwrap_or(0.0) >= thin_th;
    let no_volume = thickness <= thickness_th && open_ratio >= open_th;
    let perimeter = common::boundary_perimeter(v, t);
    let perimeter_ratio = if area > 0.0 {
        perimeter / area.sqrt()
    } else {
        0.0
    };
    let solid_area = if !thin_shell && !no_volume && perimeter_ratio >= perim_th {
        solid_area_fraction(v, t, p)
    } else {
        None
    };
    let open_shell = perimeter_ratio >= perim_th
        && solid_area.is_some()
        && solid_area.unwrap_or(1.0) <= solid_th;
    let (skip, reason) = if thin_shell {
        (true, Some("thin_shell"))
    } else if no_volume {
        (true, Some("no_volume"))
    } else if open_shell {
        (true, Some("open_shell"))
    } else {
        (false, None)
    };
    json!({
        "genus": genus,
        "compactness": round6(compactness),
        "open_area_ratio": round6(open_ratio),
        "effective_thickness_norm": round6(thickness),
        "thin_sample_fraction": thin.map(round6),
        "interior_depth_norm": interior_depth_norm(v, t, p),
        "boundary_perimeter": round6(perimeter),
        "perimeter_ratio": round6(perimeter_ratio),
        "solid_area_fraction": solid_area.map(round6),
        "thresholds": {
            "compactness": compact_th,
            "open_area_ratio": open_th,
            "thin_sample_fraction": thin_th,
            "effective_thickness_norm": thickness_th,
            "perimeter_ratio": perim_th,
            "solid_area_fraction": solid_th,
        },
        "skip": skip,
        "reason": reason,
    })
}

fn round6(x: f64) -> f64 {
    (x * 1_000_000.0).round() / 1_000_000.0
}

/// Outside-the-input leak: added result vertices farther than the shape
/// tolerance and with winding < 0.5 (classic and, when enabled, antipodal).
fn shape_leak(
    in_v: &[[f64; 3]],
    in_t: &[[i64; 3]],
    out_v: &[[f64; 3]],
    out_t: &[[i64; 3]],
    in_diag: f64,
    leak_max_faces: usize,
) -> (f64, usize) {
    if in_t.is_empty() || out_t.is_empty() || in_t.len() > leak_max_faces {
        return (0.0, 0);
    }
    let mut in_keys = std::collections::HashSet::new();
    for t in in_t {
        let mut k = [t[0], t[1], t[2]];
        k.sort_unstable();
        in_keys.insert(k);
    }
    let mut add_faces = Vec::new();
    for t in out_t {
        let mut k = [t[0], t[1], t[2]];
        k.sort_unstable();
        if !in_keys.contains(&k) {
            add_faces.push(*t);
        }
    }
    if add_faces.is_empty() {
        return (0.0, 0);
    }
    let mut add_idx: Vec<i64> = add_faces.iter().flat_map(|t| t.iter().copied()).collect();
    add_idx.sort_unstable();
    add_idx.dedup();
    let add_v: Vec<[f64; 3]> = add_idx.iter().map(|&i| out_v[i as usize]).collect();
    let cp = common::closest_points(in_v, in_t, &add_v);
    let tol = (in_diag / 10000.0).max(0.0125).min(0.05);
    let mut far_v = Vec::new();
    let mut far_d = Vec::new();
    for (i, (_q, d, _f)) in cp.iter().enumerate() {
        if *d > tol {
            far_v.push(add_v[i]);
            far_d.push(*d);
        }
    }
    if far_v.is_empty() {
        return (0.0, 0);
    }
    let w = common::winding_points(in_v, in_t, &far_v);
    let mut bad: Vec<usize> = w
        .iter()
        .enumerate()
        .filter(|(_, v)| **v < 0.5)
        .map(|(i, _)| i)
        .collect();
    if sutura_geom::antipodal::antipodal_enabled_by_default() {
        let wa = common::antipodal_winding(in_v, in_t, &far_v);
        for (i, v) in wa.iter().enumerate() {
            if *v < 0.5 && !bad.contains(&i) {
                bad.push(i);
            }
        }
    }
    if bad.is_empty() {
        return (0.0, 0);
    }
    let max = bad.iter().map(|&i| far_d[i]).fold(0.0f64, f64::max);
    (max, bad.len())
}

fn final_needs_off(
    in_v: &[[f64; 3]],
    in_t: &[[i64; 3]],
    out_v: &[[f64; 3]],
    out_t: &[[i64; 3]],
    force_metrics: bool,
    p: &Value,
) -> (Option<String>, Option<Value>) {
    let in_diag = common::bbox_diagonal(in_v);
    let out_diag = common::bbox_diagonal(out_v);
    let coverage = if in_diag != 0.0 {
        out_diag / in_diag
    } else {
        1.0
    };
    let leak_max = json_u(p, "leak_max_faces", 60000);
    let (leak_mm, leak_pts) = shape_leak(in_v, in_t, out_v, out_t, in_diag, leak_max);
    let collapse_frac = json_f(p, "collapse_bbox_frac", 0.5);
    let leak_frac = json_f(p, "leak_trigger_frac", 0.01);
    let mut reason = None;
    if in_diag != 0.0 && coverage < collapse_frac {
        reason = Some("bbox_collapse".to_string());
    } else if in_diag != 0.0 && (leak_mm / in_diag) > leak_frac {
        reason = Some("shape_leak".to_string());
    }
    if reason.is_none() && !force_metrics {
        return (None, None);
    }
    let (holes, nm) = common::reload_strict_holes_nm(out_v, out_t);
    let (wv, wt) = common::weld_reload_equivalent(out_v, out_t).unwrap_or_default();
    let parts = common::count_vertex_components(wv.len(), &wt);
    let metrics = json!({
        "input_diag_mm": round4(in_diag),
        "output_diag_mm": round4(out_diag),
        "coverage": round4(coverage),
        "leak_mm": round4(leak_mm),
        "leak_pts": leak_pts,
        "holes": holes,
        "non_manifold": nm,
        "parts": parts,
    });
    (reason, Some(metrics))
}

fn round4(x: f64) -> f64 {
    (x * 10000.0).round() / 10000.0
}

fn off_preferred(on: &Value, off: &Value) -> bool {
    let damage = |m: &Value| -> Option<i64> {
        let h = m.get("holes").and_then(Value::as_i64);
        let n = m.get("non_manifold").and_then(Value::as_i64);
        match (h, n) {
            (Some(h), Some(n)) => Some(h + n),
            _ => None,
        }
    };
    let parts = |m: &Value| m.get("parts").and_then(Value::as_i64);
    if let (Some(a), Some(b)) = (damage(on), damage(off)) {
        if b > a {
            return false;
        }
    }
    if let (Some(a), Some(b)) = (parts(on), parts(off)) {
        if b > a {
            return false;
        }
    }
    let leak_on = on.get("leak_mm").and_then(Value::as_f64).unwrap_or(0.0);
    let leak_off = off.get("leak_mm").and_then(Value::as_f64).unwrap_or(0.0);
    if leak_off < leak_on {
        return true;
    }
    let cov_on = on.get("coverage").and_then(Value::as_f64).unwrap_or(0.0);
    let cov_off = off.get("coverage").and_then(Value::as_f64).unwrap_or(0.0);
    cov_off > cov_on
}

fn guard_from(p: &Value) -> GuardPolicy {
    common::policy_from_json(p.get("guard").unwrap_or(&Value::Null))
}

fn annotation_json(info: &TamponadeInfo) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("plugs".into(), json!(info.plugs));
    m.insert("plugs_rejected".into(), json!(info.plugs_rejected));
    m.insert("plugs_inconclusive".into(), json!(info.plugs_inconclusive));
    m.insert(
        "plugs_rejected_size".into(),
        json!(info.plugs_rejected_size),
    );
    m.insert("plugs_rejected_dev".into(), json!(info.plugs_rejected_dev));
    m.insert("plug_faces".into(), json!(info.plug_faces));
    m.insert("melt_layers_tried".into(), json!(info.melt_layers_tried));
    m.insert(
        "melt_layers_accepted".into(),
        json!(info.melt_layers_accepted),
    );
    m.insert(
        "melt_layers_rolled_back".into(),
        json!(info.melt_layers_rolled_back),
    );
    m.insert("micro_welds_ok".into(), json!(info.micro_welds_ok));
    m.insert("micro_welds_failed".into(), json!(info.micro_welds_failed));
    m.insert("excised_faces".into(), json!(info.excised_faces));
    m.insert("healthy_faces_kept".into(), json!(info.healthy_faces_kept));
    m.insert("reference_used".into(), json!(info.reference_used));
    m.insert("reference_fallback".into(), json!(info.reference_fallback));
    m.insert("reference_rbf".into(), json!(info.reference_rbf));
    m.insert("reference_skirt".into(), json!(info.reference_skirt));
    m.insert("stent_used".into(), json!(info.stent_used));
    m.insert("stent_rejected".into(), json!(info.stent_rejected));
    m.insert("stent_subplugs".into(), json!(info.stent_subplugs));
    if info.plug_metrics_json.is_empty() {
        m.insert("plug_metrics".into(), Value::Null);
    } else {
        m.insert(
            "plug_metrics".into(),
            serde_json::from_str(&info.plug_metrics_json).unwrap_or(Value::Null),
        );
    }
    m.insert("trimmed_vertices".into(), json!(info.trimmed_vertices));
    m.insert("patch_vertices".into(), json!(info.patch_vertices));
    m.insert("plugs_rolled_back".into(), json!(info.plugs_rolled_back));
    m.insert("plugs_leaking".into(), json!(info.plugs_leaking));
    m.insert("plugs_double_wall".into(), json!(info.plugs_double_wall));
    m.insert("plugs_floating".into(), json!(info.plugs_floating));
    m.insert("components_before".into(), json!(info.components_before));
    m.insert("components_after".into(), json!(info.components_after));
    m.insert(
        "closed_shells_before".into(),
        json!(info.closed_shells_before),
    );
    m.insert(
        "closed_shells_after".into(),
        json!(info.closed_shells_after),
    );
    m.insert("seconds".into(), json!(info.seconds));
    m.insert("work_units".into(), json!(info.work_units));
    m
}

fn base_record(params: &Value) -> Map<String, Value> {
    let mut rec = Map::new();
    let prepass = json_b(params, "prepass", false);
    rec.insert("tier".into(), json!("tamponade"));
    rec.insert("ran".into(), json!(false));
    rec.insert("adopted".into(), json!(false));
    rec.insert(
        "placement".into(),
        json!(if prepass { "prepass" } else { "post_stage1" }),
    );
    rec.insert("watertight".into(), json!(false));
    rec.insert("reload_watertight".into(), json!(false));
    rec.insert("reason".into(), Value::Null);
    rec.insert("reject_reason".into(), Value::Null);
    rec.insert(
        "intensity".into(),
        params.get("intensity").cloned().unwrap_or(Value::Null),
    );
    rec.insert("work_budget".into(), Value::Null);
    rec.insert("work_units".into(), Value::Null);
    rec.insert("h_min".into(), json!(json_f(params, "h_min", 0.0)));
    rec.insert(
        "input_faces".into(),
        json!(json_i(params, "input_faces", 0)),
    );
    rec.insert("output_faces".into(), Value::Null);
    rec.insert("seconds".into(), Value::Null);
    rec.insert("input_holes".into(), Value::Null);
    rec.insert("input_non_manifold".into(), Value::Null);
    rec.insert("input_parts".into(), Value::Null);
    rec.insert("candidate_holes".into(), Value::Null);
    rec.insert("candidate_non_manifold".into(), Value::Null);
    rec.insert("candidate_parts".into(), Value::Null);
    for k in [
        "plugs",
        "plug_faces",
        "plugs_rejected",
        "plugs_inconclusive",
        "plugs_rejected_size",
        "plugs_rejected_dev",
        "melt_layers_tried",
        "melt_layers_accepted",
        "melt_layers_rolled_back",
        "micro_welds_ok",
        "micro_welds_failed",
        "excised_faces",
        "healthy_faces_kept",
        "healthy_fraction",
        "plugs_rolled_back",
        "plugs_leaking",
        "plugs_double_wall",
        "plugs_floating",
        "components_before",
        "components_after",
        "closed_shells_before",
        "closed_shells_after",
        "input_self_intersections",
        "candidate_self_intersections",
        "si_fall",
        "si_ok",
        "melt_enabled",
        "si_method",
        "input_internal_walls",
        "candidate_internal_walls",
        "prepass_seconds",
        "guard_rejected",
        "guard_reason",
        "baseline_self_intersections",
        "si_policy",
        "si_checked",
        "si_flagged",
        "gate",
        "shape",
        "reason_key",
    ] {
        rec.insert(k.into(), Value::Null);
    }
    rec
}

fn exact_si(v: &[[f64; 3]], t: &[[i64; 3]]) -> Option<i64> {
    common::exact_si_count(v, t)
}

/// Run the Tamponade tier.  Returns `(out_verts, out_tris, report_json)`.
/// The input arrays are returned when the candidate is not adopted.
pub fn run(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    params: &Value,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let mut rec = base_record(params);
    let v = verts.to_vec();
    let t = tris.to_vec();
    rec.insert("input_faces".into(), json!(t.len()));

    let prepass = json_b(params, "prepass", false);
    let tamp_force = json_b(params, "tamponade_force", false);
    let si_mode = json_s(params, "si_mode", "report");
    let melt_on = json_b(params, "melt_si", false);
    rec.insert("melt_enabled".into(), json!(melt_on));

    let budget = json_opt_i(params, "budget").unwrap_or(0);
    rec.insert("work_budget".into(), json!(budget));
    if prepass && budget <= 0 {
        rec.insert(
            "reason".into(),
            json!(json_s(
                params,
                "reason_override",
                "intensity skips Tamponade (work budget 0)"
            )),
        );
        return (v, t, Value::Object(rec));
    }

    let (wb_v, wb_t) = match common::weld_reload_equivalent(&v, &t) {
        Ok(x) => x,
        Err(_) => {
            rec.insert("reason".into(), json!("weld_reload_equivalent failed"));
            return (v, t, Value::Object(rec));
        }
    };
    let wb_f: Vec<[f64; 3]> = common::to_f64_verts(&wb_v);
    let wb_t32: Vec<[i32; 3]> = wb_t
        .iter()
        .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
        .collect();
    let defects = crate::report::detect_defects(&wb_v, &wb_t32, true);
    let base_holes = defects.holes.len();
    let base_nm = defects.non_manifold.len();
    let base_parts = common::count_vertex_components(wb_v.len(), &wb_t);
    rec.insert("input_holes".into(), json!(base_holes));
    rec.insert("input_non_manifold".into(), json!(base_nm));
    rec.insert("input_parts".into(), json!(base_parts));

    let base_si_exact =
        if !prepass && si_mode != "off" && (melt_on || base_holes > 0 || base_nm > 0) {
            exact_si(&wb_f, &wb_t)
        } else {
            None
        };
    rec.insert(
        "input_self_intersections".into(),
        base_si_exact.map(Value::from).unwrap_or(Value::Null),
    );
    let base_si = base_si_exact.unwrap_or(0);

    if base_holes == 0 && base_nm == 0 && !melt_on {
        rec.insert(
            "reason".into(),
            json!("no holes/non-manifold to plug; melt disabled at this intensity; tier skipped"),
        );
        return (v, t, Value::Object(rec));
    }
    if !prepass && base_holes == 0 && base_nm == 0 && base_si == 0 && base_si_exact.is_none() {
        rec.insert(
            "reason".into(),
            json!("no holes/non-manifold/self-intersections to plug; tier skipped"),
        );
        return (v, t, Value::Object(rec));
    }

    let hole_loops: Vec<Vec<i64>> = defects
        .holes
        .iter()
        .map(|h| h.verts_idx.clone().unwrap_or_default())
        .collect();
    let genus = json_opt_i(params, "genus");
    let shape = shape_card_full(&wb_f, &wb_t, &hole_loops, genus, params);
    rec.insert("shape".into(), shape.clone());
    let shape_skip = shape.get("skip").and_then(Value::as_bool).unwrap_or(false);
    let shape_reason = shape.get("reason").and_then(Value::as_str).unwrap_or("");
    if !tamp_force && json_b(params, "shape_gate_enabled", true) && shape_skip {
        rec.insert("gate".into(), json!("shape_card"));
        let (key, reason) = match shape_reason {
            "no_volume" => (
                "tamponade_skip_no_volume",
                "not suitable: no interior volume (open sheet); left to Flap",
            ),
            "open_shell" => (
                "tamponade_skip_open_shell",
                "not suitable: open shell / no solid interior; left to Flap",
            ),
            _ => (
                "tamponade_skip_no_interior",
                "not suitable: thin shell / no interior volume; left to Flap",
            ),
        };
        rec.insert("reason_key".into(), json!(key));
        rec.insert("reason".into(), json!(reason));
        return (v, t, Value::Object(rec));
    }
    if tamp_force && shape_skip {
        rec.insert("gate".into(), json!("user_forced"));
    }

    if budget <= 0 {
        rec.insert(
            "reason".into(),
            json!(json_s(
                params,
                "reason_override",
                "intensity skips Tamponade (work budget 0)"
            )),
        );
        return (v, t, Value::Object(rec));
    }
    let h_min = json_f(params, "h_min", 0.0);
    let config = TamponadeConfig {
        h_min: Some(h_min),
        melt_si: melt_on,
        max_nodes: None,
        work_budget_cells: Some(budget as u64),
        openings_gate: None,
        fwn: std::env::var("SUTURA_TAMP_FWN")
            .map(|x| x == "1" || x.eq_ignore_ascii_case("true"))
            .unwrap_or(false),
    };
    let t_u32: Vec<[u32; 3]> = t
        .iter()
        .map(|r| [r[0] as u32, r[1] as u32, r[2] as u32])
        .collect();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        tamponade::tamponade_repair(&v, &t_u32, &config)
    }));
    let (out_v, out_t, info) = match outcome {
        Ok(x) => x,
        Err(_) => {
            let mut info = TamponadeInfo::default();
            info.healthy_faces_kept = t.len();
            info.reason = "internal error".to_string();
            (v.clone(), t_u32.clone(), info)
        }
    };
    for (k, val) in annotation_json(&info) {
        rec.insert(k, val);
    }
    rec.insert("reason".into(), json!(info.reason));
    rec.insert("ran".into(), json!(true));

    let out_t_i64 = common::to_i64_tris(&out_t);
    let cand_f = out_v.clone();
    let cand_t = out_t_i64.clone();
    rec.insert("output_faces".into(), json!(cand_t.len()));
    let (cand_holes, cand_nm) = common::reload_strict_holes_nm(&cand_f, &cand_t);
    let (wv, wt) = common::weld_reload_equivalent(&cand_f, &cand_t).unwrap_or_default();
    let wv_f = common::to_f64_verts(&wv);
    let cand_parts = common::count_vertex_components(wv.len(), &wt);
    rec.insert("candidate_holes".into(), json!(cand_holes));
    rec.insert("candidate_non_manifold".into(), json!(cand_nm));
    rec.insert("candidate_parts".into(), json!(cand_parts));
    if !t.is_empty() {
        rec.insert(
            "healthy_fraction".into(),
            json!(info.healthy_faces_kept as f64 / t.len() as f64),
        );
    }

    if info.plugs > 0 && json_b(params, "tap_internal_walls", false) {
        let iw_in = internal_wall_count(&wb_f, &wb_t);
        let iw_out = internal_wall_count(&cand_f, &cand_t);
        rec.insert("input_internal_walls".into(), json!(iw_in));
        rec.insert("candidate_internal_walls".into(), json!(iw_out));
    }

    let no_rise = cand_holes <= base_holes && cand_nm <= base_nm;
    let strict_fall = (cand_holes + cand_nm) < (base_holes + base_nm);
    let parts_ok = cand_parts <= base_parts;
    let healthy_ok = t.is_empty() || info.healthy_faces_kept as f64 >= 0.99 * t.len() as f64;

    let mut cand_si_exact = None;
    let mut base_local: Option<i64> = None;
    let mut cand_local: Option<i64> = None;
    let si_fall;
    let si_ok;
    if prepass {
        let (b, c, rise, used) = local_si_rise(&wb_f, &wb_t, &wv_f, &wt);
        base_local = b;
        cand_local = c;
        if used {
            rec.insert("si_method".into(), json!("local"));
            rec.insert("base_local_si".into(), json!(b));
            rec.insert("cand_local_si".into(), json!(c));
            si_fall = c.unwrap_or(0) < b.unwrap_or(0);
            si_ok = !rise;
        } else if base_holes == 0 && base_nm == 0 {
            rec.insert("si_method".into(), json!("unavailable"));
            si_fall = false;
            si_ok = false;
        } else {
            let bs = exact_si(&wb_f, &wb_t);
            rec.insert(
                "input_self_intersections".into(),
                bs.map(Value::from).unwrap_or(Value::Null),
            );
            let cs = if bs.is_some() {
                exact_si(&wv_f, &wt)
            } else {
                None
            };
            rec.insert(
                "candidate_self_intersections".into(),
                cs.map(Value::from).unwrap_or(Value::Null),
            );
            si_fall = match (bs, cs) {
                (Some(a), Some(b)) => b < a,
                _ => false,
            };
            si_ok = match (bs, cs) {
                (Some(a), Some(b)) => b <= a,
                _ => true,
            };
            rec.insert("si_method".into(), json!("full"));
        }
    } else {
        if base_si_exact.is_some() {
            cand_si_exact = exact_si(&wv_f, &wt);
            rec.insert(
                "candidate_self_intersections".into(),
                cand_si_exact.map(Value::from).unwrap_or(Value::Null),
            );
        }
        si_fall = match (base_si_exact, cand_si_exact) {
            (Some(a), Some(b)) => b < a,
            _ => false,
        };
        si_ok = match (base_si_exact, cand_si_exact) {
            (Some(a), Some(b)) => b <= a,
            _ => true,
        };
    }
    rec.insert("si_fall".into(), json!(si_fall));
    rec.insert("si_ok".into(), json!(si_ok));

    let policy = guard_from(params);
    let guard = common::guard_fields(
        t.len(),
        v.len(),
        cand_t.len(),
        cand_f.len(),
        info.plug_faces as i64,
        if prepass { None } else { base_si_exact },
        if prepass { None } else { cand_si_exact },
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

    let geometry_fall;
    if prepass {
        geometry_fall = cand_holes < base_holes || si_fall;
        let mut si_guard = si_ok;
        if let Some(pb) = json_opt_i(params, "prepass_budget") {
            if (info.work_units as i64) > pb {
                si_guard = false;
                rec.insert(
                    "reject_reason".into(),
                    json!("prepass work budget exceeded"),
                );
                rec.insert("budget_limited_by".into(), json!("work"));
            }
        }
        let cand_leak = if info.plugs > 0 {
            shape_leak(
                &v,
                &t,
                &cand_f,
                &cand_t,
                common::bbox_diagonal(&v),
                json_u(params, "leak_max_faces", 60000),
            )
        } else {
            (0.0, 0)
        };
        let leak_ok = cand_leak.1 == 0;
        if guard_ok
            && no_rise
            && geometry_fall
            && parts_ok
            && healthy_ok
            && si_guard
            && leak_ok
            && !cand_t.is_empty()
        {
            rec.insert("adopted".into(), json!(true));
            let wet = cand_holes == 0 && cand_nm == 0;
            rec.insert("watertight".into(), json!(wet));
            rec.insert("reload_watertight".into(), json!(wet));
            let si_desc = if rec.get("si_method").and_then(Value::as_str) == Some("local") {
                format!(
                    "{} -> {} (local)",
                    base_local
                        .map(|x| x.to_string())
                        .unwrap_or_else(|| "None".into()),
                    cand_local
                        .map(|x| x.to_string())
                        .unwrap_or_else(|| "None".into())
                )
            } else {
                format!(
                    "{} -> {}",
                    base_si_exact
                        .map(|x| x.to_string())
                        .unwrap_or_else(|| "None".into()),
                    cand_si_exact
                        .map(|x| x.to_string())
                        .unwrap_or_else(|| "None".into())
                )
            };
            rec.insert(
                "reason".into(),
                json!(format!(
                    "adopted (holes {} -> {}, non-manifold {} -> {}, SI {})",
                    base_holes, cand_holes, base_nm, cand_nm, si_desc
                )),
            );
            return (cand_f, cand_t, Value::Object(rec));
        }
        let mut reasons: Vec<String> = Vec::new();
        if !no_rise {
            reasons.push("holes/non-manifold rose".to_string());
        }
        if !geometry_fall {
            reasons.push("holes+non-manifold/SI did not strictly fall".to_string());
        }
        if !si_guard {
            if rec.get("reject_reason").and_then(Value::as_str)
                == Some("prepass work budget exceeded")
            {
                reasons.push("prepass budget exceeded".to_string());
            } else {
                reasons.push("self-intersections rose".to_string());
            }
        }
        if !parts_ok {
            reasons.push("component count rose".to_string());
        }
        if !healthy_ok {
            reasons.push("healthy faces below 99%".to_string());
        }
        if !leak_ok {
            reasons.push("plug leaks outside solid".to_string());
        }
        if cand_t.is_empty() {
            reasons.push("no geometry".to_string());
        }
        rec.insert(
            "reason".into(),
            json!(format!("not adopted ({})", reasons.join("; "))),
        );
        rec.insert("reject_reason".into(), json!(reasons.join(", ")));
        return (v, t, Value::Object(rec));
    }

    let cand_leak = if info.plugs > 0 {
        shape_leak(
            &v,
            &t,
            &cand_f,
            &cand_t,
            common::bbox_diagonal(&v),
            json_u(params, "leak_max_faces", 60000),
        )
    } else {
        (0.0, 0)
    };
    let leak_ok = cand_leak.1 == 0;
    let si_guard = true;
    geometry_fall = strict_fall || si_fall;
    if guard_ok
        && no_rise
        && geometry_fall
        && parts_ok
        && healthy_ok
        && si_guard
        && leak_ok
        && !cand_t.is_empty()
    {
        rec.insert("adopted".into(), json!(true));
        let wet = cand_holes == 0 && cand_nm == 0;
        rec.insert("watertight".into(), json!(wet));
        rec.insert("reload_watertight".into(), json!(wet));
        rec.insert(
            "reason".into(),
            json!(format!(
                "adopted (holes {} -> {}, non-manifold {} -> {}, SI {} -> {})",
                base_holes,
                cand_holes,
                base_nm,
                cand_nm,
                base_si_exact
                    .map(|x| x.to_string())
                    .unwrap_or_else(|| "None".into()),
                cand_si_exact
                    .map(|x| x.to_string())
                    .unwrap_or_else(|| "None".into())
            )),
        );
        return (cand_f, cand_t, Value::Object(rec));
    }
    let mut reasons: Vec<String> = Vec::new();
    if !no_rise {
        reasons.push("holes/non-manifold rose".to_string());
    }
    if !geometry_fall {
        reasons.push("holes+non-manifold/SI did not strictly fall".to_string());
    }
    if !parts_ok {
        reasons.push("component count rose".to_string());
    }
    if !healthy_ok {
        reasons.push("healthy faces below 99%".to_string());
    }
    if !leak_ok {
        reasons.push(format!(
            "plug leaks outside solid ({:.4} mm, {} pts)",
            cand_leak.0, cand_leak.1
        ));
    }
    if cand_t.is_empty() {
        reasons.push("no geometry".to_string());
    }
    rec.insert(
        "reason".into(),
        json!(format!("not adopted ({})", reasons.join("; "))),
    );
    rec.insert("reject_reason".into(), json!(reasons.join(", ")));
    (v, t, Value::Object(rec))
}

fn internal_wall_count(v: &[[f64; 3]], t: &[[i64; 3]]) -> i64 {
    if v.is_empty() || t.is_empty() {
        return 0;
    }
    let mut centers = Vec::with_capacity(t.len());
    let mut normals = Vec::with_capacity(t.len());
    for tri in t {
        let a = v[tri[0] as usize];
        let b = v[tri[1] as usize];
        let c = v[tri[2] as usize];
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1.0);
        centers.push([
            (a[0] + b[0] + c[0]) / 3.0,
            (a[1] + b[1] + c[1]) / 3.0,
            (a[2] + b[2] + c[2]) / 3.0,
        ]);
        normals.push([n[0] / len, n[1] / len, n[2] / len]);
    }
    let diag = common::bbox_diagonal(v);
    let eps = (diag * 1e-4).max(1e-6);
    let mut pts = Vec::with_capacity(2 * centers.len());
    for i in 0..centers.len() {
        let c = centers[i];
        let n = normals[i];
        pts.push([c[0] + n[0] * eps, c[1] + n[1] * eps, c[2] + n[2] * eps]);
    }
    for i in 0..centers.len() {
        let c = centers[i];
        let n = normals[i];
        pts.push([c[0] - n[0] * eps, c[1] - n[1] * eps, c[2] - n[2] * eps]);
    }
    let w = common::winding_points(v, t, &pts);
    let n = centers.len();
    let mut count = 0;
    for i in 0..n {
        if w[i] >= 1.5 && w[n + i] >= 1.5 {
            count += 1;
        }
    }
    count
}

fn local_si_rise(
    base_v: &[[f64; 3]],
    base_t: &[[i64; 3]],
    cand_v: &[[f64; 3]],
    cand_t: &[[i64; 3]],
) -> (Option<i64>, Option<i64>, bool, bool) {
    if base_t.is_empty() || cand_t.is_empty() {
        return (None, None, false, false);
    }
    let bk = common::canonical_face_keys(base_v, base_t);
    let ck = common::canonical_face_keys(cand_v, cand_t);
    let bset: std::collections::HashSet<[u8; 36]> = bk.iter().copied().collect();
    let cset: std::collections::HashSet<[u8; 36]> = ck.iter().copied().collect();
    let base_changed: Vec<bool> = bk.iter().map(|k| !cset.contains(k)).collect();
    let cand_changed: Vec<bool> = ck.iter().map(|k| !bset.contains(k)).collect();
    let base_local = if base_changed.iter().any(|b| *b) {
        common::si_subset_count(base_v, base_t, &base_changed)
    } else {
        0
    };
    let cand_local = if cand_changed.iter().any(|b| *b) {
        common::si_subset_count(cand_v, cand_t, &cand_changed)
    } else {
        0
    };
    (
        Some(base_local),
        Some(cand_local),
        cand_local > base_local,
        true,
    )
}

/// Public helper used by the Python final safety net.
pub fn final_off(
    in_v: &[[f64; 3]],
    in_t: &[[i64; 3]],
    out_v: &[[f64; 3]],
    out_t: &[[i64; 3]],
    force_metrics: bool,
    params: &Value,
) -> (Option<String>, Option<Value>) {
    final_needs_off(in_v, in_t, out_v, out_t, force_metrics, params)
}

pub fn off_preferred_json(on: &Value, off: &Value) -> bool {
    off_preferred(on, off)
}

/// Exposed for the Python wrapper's shape card call (report only).
pub fn shape_json(
    v: &[[f64; 3]],
    t: &[[i64; 3]],
    hole_loops: &[Vec<i64>],
    genus: Option<i64>,
    params: &Value,
) -> Value {
    shape_card_full(v, t, hole_loops, genus, params)
}
