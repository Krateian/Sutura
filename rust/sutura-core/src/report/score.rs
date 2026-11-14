//! Repair Health / Risk / Status scoring, matching `sutura/repair_score.py`.

use serde_json::{json, Map, Value};

fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(|x| x.as_f64())
}

fn is_num(v: Option<&Value>) -> bool {
    v.map(Value::is_number).unwrap_or(false)
}

fn as_int(v: Option<&Value>) -> Option<i64> {
    v.and_then(|x| x.as_i64())
}

fn weights(cfg: &Value, section: &str) -> Map<String, Value> {
    cfg.get(section)
        .and_then(|s| s.get("weights"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn health_factors(report: &Value) -> Map<String, Value> {
    let s1 = report.get("stage1").and_then(Value::as_object);
    let mut out = Map::new();
    if let Some(s1) = s1 {
        if s1.contains_key("two_manifold") {
            let s2 = report.get("stage2").and_then(Value::as_object);
            let s2_ok = s2
                .map(|x| x.contains_key("ok") && !x.contains_key("error"))
                .unwrap_or(false);
            let watertight_s1 = s1
                .get("two_manifold")
                .map(|v| v.as_bool().unwrap_or(false))
                .unwrap_or(false)
                && as_int(s1.get("holes_remaining")).unwrap_or(0) == 0;
            out.insert("watertight".into(), Value::Bool(watertight_s1 && s2_ok));
        }
        if is_num(s1.get("non_manifold_edges_remaining")) {
            out.insert(
                "no_non_manifold".into(),
                Value::Bool(as_int(s1.get("non_manifold_edges_remaining")).unwrap_or(0) == 0),
            );
        }
        if is_num(s1.get("self_intersections_remaining")) {
            out.insert(
                "no_self_intersections".into(),
                Value::Bool(as_int(s1.get("self_intersections_remaining")).unwrap_or(0) == 0),
            );
        }
        if is_num(s1.get("holes_remaining")) {
            out.insert(
                "no_holes".into(),
                Value::Bool(as_int(s1.get("holes_remaining")).unwrap_or(0) == 0),
            );
        }
    }
    out
}

fn health(report: &Value, cfg: &Value) -> (Option<i64>, Map<String, Value>) {
    let w = weights(cfg, "health");
    if w.is_empty() {
        return (None, Map::new());
    }
    let ok = health_factors(report);
    if ok.is_empty() {
        return (None, Map::new());
    }
    let total_w: f64 = w
        .values()
        .filter_map(|v| v.as_f64())
        .map(|x| x.max(0.0))
        .sum();
    if total_w <= 0.0 {
        return (None, Map::new());
    }
    let mut used = 0.0;
    let mut acc = 0.0;
    let mut factors = Map::new();
    for (name, weight) in &w {
        let Some(okv) = ok.get(name).and_then(Value::as_bool) else {
            continue;
        };
        let wt = weight.as_f64().unwrap_or(0.0).max(0.0);
        used += wt;
        acc += if okv { wt } else { 0.0 };
        factors.insert(
            name.clone(),
            json!({"ok": okv, "weight": (wt * 100.0).round_ties_even() / 100.0}),
        );
    }
    if used <= 0.0 {
        return (None, Map::new());
    }
    let score = (acc / used * 100.0).round_ties_even() as i64;
    (Some(score.clamp(0, 100)), factors)
}

fn pct_delta(before: Option<&Value>, after: Option<&Value>) -> Option<f64> {
    let b = num(before)?;
    let a = num(after)?;
    if !(b.abs() > 1e-12) {
        return None;
    }
    Some((a - b) / b.abs() * 100.0)
}

fn risk(report: &Value, cfg: &Value) -> (Option<i64>, Map<String, Value>) {
    let w = weights(cfg, "risk");
    if w.is_empty() {
        return (None, Map::new());
    }
    let rcfg = cfg.get("risk");
    let mut scale = rcfg
        .and_then(|r| r.get("delta_scale_percent"))
        .and_then(Value::as_f64)
        .unwrap_or(25.0);
    let comp_pen = rcfg
        .and_then(|r| r.get("component_loss_penalty"))
        .and_then(Value::as_f64)
        .unwrap_or(30.0);
    if scale <= 0.0 {
        scale = 25.0;
    }
    let s1 = report.get("stage1").and_then(Value::as_object);
    let mut factors = Map::new();
    let add = |name: &str, risk01: f64, detail: String, factors: &mut Map<String, Value>| {
        factors.insert(
            name.into(),
            json!({"risk": (risk01.clamp(0.0, 1.0) * 1000.0).round_ties_even() / 1000.0,
                   "detail": detail}),
        );
    };
    if let Some(s1) = s1 {
        if let Some(face) = pct_delta(s1.get("faces_before"), s1.get("faces_after")) {
            add(
                "face_delta",
                face.abs() / scale,
                format!("{face:.1}%"),
                &mut factors,
            );
        }
        if let Some(vert) = pct_delta(s1.get("vertices_before"), s1.get("vertices_after")) {
            add(
                "vertex_delta",
                vert.abs() / scale,
                format!("{vert:.1}%"),
                &mut factors,
            );
        }
        let cb = s1.get("components_before");
        let ca = s1.get("components");
        let mut lost = false;
        if is_num(cb) && is_num(ca) {
            let b = as_int(cb).unwrap_or(0);
            let a = as_int(ca).unwrap_or(0);
            lost = b > a;
            add(
                "component_change",
                if a != b { 1.0 } else { 0.0 },
                format!("{b} -> {a}"),
                &mut factors,
            );
        }
        if is_num(s1.get("volume_change_percent")) {
            let vol = num(s1.get("volume_change_percent")).unwrap_or(0.0);
            add(
                "volume_delta",
                vol.abs() / scale,
                format!("{vol:.1}%"),
                &mut factors,
            );
        }
        if factors.is_empty() {
            return (None, Map::new());
        }
        let mut used = 0.0;
        let mut acc = 0.0;
        for (name, weight) in &w {
            let Some(f) = factors
                .get(name)
                .and_then(|x| x.get("risk"))
                .and_then(Value::as_f64)
            else {
                continue;
            };
            let wt = weight.as_f64().unwrap_or(0.0).max(0.0);
            used += wt;
            acc += f * wt;
        }
        if used <= 0.0 {
            return (None, Map::new());
        }
        let mut score = acc / used * 100.0;
        if lost {
            score += comp_pen;
        }
        return (
            Some((score.round_ties_even() as i64).clamp(0, 100)),
            factors,
        );
    }
    (None, Map::new())
}

fn tier(value: i64, boundaries: Option<&Value>) -> String {
    let mut pairs: Vec<(String, f64)> = boundaries
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_f64().map(|x| (k.clone(), x)))
                .collect()
        })
        .unwrap_or_default();
    pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (name, lower) in pairs {
        if value as f64 >= lower {
            return name;
        }
    }
    "low".into()
}

fn status(health: Option<i64>, risk: Option<i64>, cfg: &Value) -> (String, String) {
    let st = cfg.get("status");
    let labels = st.and_then(|s| s.get("labels"));
    let table = st.and_then(|s| s.get("table"));
    let label = |code: &str| -> String {
        labels
            .and_then(|l| l.get(code))
            .and_then(Value::as_str)
            .unwrap_or(code)
            .to_string()
    };
    let (Some(h), Some(r)) = (health, risk) else {
        return ("unavailable".into(), label("unavailable"));
    };
    let ht = tier(h, st.and_then(|s| s.get("health_tiers")));
    let rt = tier(r, st.and_then(|s| s.get("risk_tiers")));
    let key = format!("{ht}_{rt}");
    let code = table
        .and_then(|t| t.get(&key))
        .and_then(Value::as_str)
        .unwrap_or("unavailable");
    (code.into(), label(code))
}

pub fn compute_scores_value(report: &Value, cfg: &Value) -> Value {
    let (h, hf) = health(report, cfg);
    let (r, rf) = risk(report, cfg);
    let (code, label) = status(h, r, cfg);
    json!({
        "health": h,
        "health_factors": hf,
        "risk": r,
        "risk_factors": rf,
        "status": label,
        "status_code": code,
    })
}

pub fn compute_scores_json(report: &str, cfg: &str) -> Result<String, String> {
    let report: Value = serde_json::from_str(report).map_err(|e| e.to_string())?;
    let cfg: Value = serde_json::from_str(cfg).map_err(|e| e.to_string())?;
    Ok(compute_scores_value(&report, &cfg).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg() -> Value {
        json!({
            "health": {"weights": {"watertight": 39, "no_non_manifold": 22,
                                   "no_self_intersections": 22, "no_holes": 17}},
            "risk": {"weights": {"face_delta": 30, "vertex_delta": 20,
                                 "component_change": 25, "volume_delta": 25},
                     "delta_scale_percent": 25, "component_loss_penalty": 30},
            "status": {"health_tiers": {"high": 80, "mid": 60},
                       "risk_tiers": {"high": 70, "mid": 40},
                       "labels": {"safe": "Safe to inspect", "review": "Review recommended",
                                  "caution": "Caution advised", "failed": "Failed / inspect",
                                  "unavailable": "Scoring unavailable"},
                       "table": {"high_low": "safe", "high_mid": "review",
                                 "high_high": "review", "mid_low": "review",
                                 "mid_mid": "review", "mid_high": "caution",
                                 "low_low": "failed", "low_mid": "failed",
                                 "low_high": "failed"}}
        })
    }

    #[test]
    fn watertight_low_risk_is_safe() {
        let r = json!({"stage1": {"two_manifold": true, "holes_remaining": 0,
                                  "non_manifold_edges_remaining": 0,
                                  "self_intersections_remaining": 0,
                                  "faces_before": 100, "faces_after": 100,
                                  "vertices_before": 50, "vertices_after": 50,
                                  "components_before": 1, "components": 1,
                                  "volume_change_percent": 0.0},
                       "stage2": {"ok": true}});
        let s = compute_scores_value(&r, &cfg());
        assert_eq!(s["health"], 100);
        assert_eq!(s["risk"], 0);
        assert_eq!(s["status_code"], "safe");
    }

    #[test]
    fn missing_evidence_is_unavailable() {
        let s = compute_scores_value(&json!({}), &cfg());
        assert!(s["health"].is_null());
        assert!(s["risk"].is_null());
        assert_eq!(s["status_code"], "unavailable");
    }
}
