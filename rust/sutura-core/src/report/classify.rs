//! Result classification and confidence scoring, matching
//! `sutura/classification.py` and `sutura/confidence.py`.

use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct Confidence {
    pub score: i64,
    pub label: String,
    pub factors: Map<String, Value>,
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|x| x != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.get(key)
}

fn has(v: &Value, key: &str) -> bool {
    v.get(key).is_some()
}

fn int_of(v: Option<&Value>) -> i64 {
    v.and_then(|x| x.as_i64())
        .or_else(|| v.and_then(|x| x.as_f64()).map(|x| x as i64))
        .unwrap_or(0)
}

fn obj_or_empty(v: &Value) -> &Value {
    static NULL: Value = Value::Null;
    if v.is_object() {
        v
    } else {
        &NULL
    }
}

fn closed(r: &Value) -> bool {
    let s1 = get(r, "stage1").map(obj_or_empty).unwrap_or(&Value::Null);
    truthy(get(s1, "two_manifold")) && int_of(get(s1, "holes_remaining")) == 0
}

fn classify_objects(reports: &[Value], issues: &mut Vec<String>) -> (String, String) {
    let n = reports.len();
    if reports.iter().any(|r| truthy(get(r, "shape_changed"))) {
        issues.push("shape_changed".into());
    }
    if reports.iter().any(|r| {
        let g = get(r, "graft");
        truthy(g.and_then(|x| x.get("adopted"))) && truthy(g.and_then(|x| x.get("warnings")))
    }) {
        issues.push("graft_detail_loss".into());
    }
    if reports.iter().any(|r| {
        let g = get(r, "dressing");
        truthy(g.and_then(|x| x.get("adopted"))) && truthy(g.and_then(|x| x.get("warnings")))
    }) {
        issues.push("dressing_detail_loss".into());
    }
    let closed_flags: Vec<bool> = reports.iter().map(closed).collect();
    let all_closed = n > 0 && closed_flags.iter().all(|&x| x);
    let mut outcomes = Vec::new();
    for r in reports {
        let s2 = get(r, "stage2").map(obj_or_empty).unwrap_or(&Value::Null);
        if !has(s2, "error") {
            outcomes.push(0);
        } else if get(s2, "error")
            .and_then(Value::as_str)
            .map(|s| s.starts_with("Stage 2 skipped"))
            .unwrap_or(false)
        {
            outcomes.push(1);
        } else {
            outcomes.push(2);
        }
    }
    let all_s2_ok = n > 0
        && outcomes.iter().all(|&o| o == 0)
        && reports.iter().all(|r| {
            let s2 = get(r, "stage2").map(obj_or_empty).unwrap_or(&Value::Null);
            truthy(get(s2, "ok"))
        });
    let any_error = outcomes.iter().any(|&o| o == 2);
    if all_closed && all_s2_ok {
        return ("watertight".into(), "watertight".into());
    }
    if all_closed {
        let code = if any_error {
            "stage2_error"
        } else {
            "stage2_skipped"
        };
        issues.push(code.into());
        return ("warning".into(), code.into());
    }
    issues.push("partial".into());
    if reports.iter().any(|r| {
        let s1 = get(r, "stage1").map(obj_or_empty).unwrap_or(&Value::Null);
        truthy(get(s1, "volume_warning"))
    }) {
        issues.push("volume_warning".into());
    }
    ("warning".into(), "partial".into())
}

pub fn classify_value(data: &Value) -> (String, Vec<String>, String) {
    let mut issues = Vec::new();
    if truthy(get(data, "extreme_removed_object")) {
        issues.push("extreme_removed_object".into());
        return ("error".into(), issues, "extreme_removed_object".into());
    }
    if get(data, "status").and_then(Value::as_str) == Some("budget_declined") {
        issues.push("budget_exceeded".into());
        return ("error".into(), issues, "budget_declined".into());
    }
    if truthy(get(data, "error")) && !has(data, "stage1") {
        issues.push("malformed".into());
        return ("error".into(), issues, "error".into());
    }
    if let Some(Value::Array(reports)) = get(data, "object_reports") {
        if !reports.is_empty() {
            let (cat, key) = classify_objects(reports, &mut issues);
            return (cat, issues, key);
        }
    }
    let s1 = get(data, "stage1")
        .map(obj_or_empty)
        .unwrap_or(&Value::Null);
    let watertight_s1 = truthy(get(s1, "two_manifold"))
        && int_of(get(s1, "holes_remaining")) == 0
        && !truthy(get(s1, "reload_non_manifold_vertices"));
    if truthy(get(s1, "reload_non_manifold_vertices")) {
        issues.push("reload_non_manifold_vertices".into());
    }
    if truthy(get(data, "shape_changed")) {
        issues.push("shape_changed".into());
    }
    if let Some(g) = get(data, "graft") {
        if truthy(get(g, "adopted")) && truthy(get(g, "warnings")) {
            issues.push("graft_detail_loss".into());
        }
    }
    if let Some(g) = get(data, "dressing") {
        if truthy(get(g, "adopted")) && truthy(get(g, "warnings")) {
            issues.push("dressing_detail_loss".into());
        }
    }
    let s2 = get(data, "stage2");
    let s2_skipped = s2
        .filter(|x| has(x, "error"))
        .and_then(|x| x.get("error"))
        .and_then(Value::as_str)
        .map(|s| s.starts_with("Stage 2 skipped"))
        .unwrap_or(false);
    let s2_error = s2
        .filter(|x| has(x, "error"))
        .map(|_| !s2_skipped)
        .unwrap_or(false);
    let s2_ok = s2
        .filter(|x| has(x, "ok"))
        .map(|_| !s2_error && !s2_skipped)
        .unwrap_or(false);
    if watertight_s1 && s2_ok {
        return ("watertight".into(), issues, "watertight".into());
    }
    if watertight_s1 && (s2_skipped || s2_error || s2.is_none()) {
        let code = if s2_error {
            "stage2_error"
        } else {
            "stage2_skipped"
        };
        issues.push(code.into());
        return ("warning".into(), issues, code.into());
    }
    if truthy(get(s1, "volume_warning")) {
        issues.push("volume_warning".into());
    }
    if !watertight_s1 {
        issues.push("partial".into());
        if truthy(get(s1, "two_manifold")) && int_of(get(s1, "holes_remaining")) > 0 {
            return ("warning".into(), issues, "holes".into());
        }
        if !truthy(get(s1, "two_manifold")) {
            return ("warning".into(), issues, "partial".into());
        }
    }
    ("warning".into(), issues, "partial".into())
}

pub fn summary_args_value(data: &Value) -> Vec<i64> {
    let s1 = get(data, "stage1")
        .map(obj_or_empty)
        .unwrap_or(&Value::Null);
    if truthy(get(s1, "two_manifold")) {
        vec![int_of(get(s1, "holes_remaining"))]
    } else {
        Vec::new()
    }
}

fn clamp(score: i64) -> i64 {
    score.clamp(0, 100)
}

fn label(score: i64) -> &'static str {
    if score >= 80 {
        "high"
    } else if score >= 50 {
        "medium"
    } else {
        "low"
    }
}

fn factor(map: &mut Map<String, Value>, key: &str, n: i64) {
    map.insert(key.into(), Value::from(n));
}

pub fn repair_confidence_value(report: &Value) -> Confidence {
    let mut factors = Map::new();
    let (category, _issues, _key) = classify_value(report);
    if category == "error" {
        factor(&mut factors, "category_error", -100);
        return Confidence {
            score: 0,
            label: "low".into(),
            factors,
        };
    }
    let s1 = get(report, "stage1")
        .map(obj_or_empty)
        .unwrap_or(&Value::Null);
    let s2 = get(report, "stage2");
    let mut score = 60;
    if category == "watertight" {
        score += 25;
        factor(&mut factors, "stage2_watertight", 25);
    } else if category == "warning" {
        factor(&mut factors, "warning_category", 0);
    } else {
        score -= 20;
        factor(&mut factors, "unknown_category", -20);
    }
    let s2_skipped = s2
        .filter(|x| has(x, "error"))
        .and_then(|x| x.get("error"))
        .and_then(Value::as_str)
        .map(|s| s.starts_with("Stage 2 skipped"))
        .unwrap_or(false);
    let s2_error = s2
        .filter(|x| has(x, "error"))
        .map(|_| !s2_skipped)
        .unwrap_or(false);
    if s2_error {
        score -= 35;
        factor(&mut factors, "stage2_error", -35);
    } else if s2_skipped {
        score -= 25;
        factor(&mut factors, "stage2_skipped", -25);
    } else if !s2.map(|x| has(x, "ok")).unwrap_or(false) {
        let wt1 = truthy(get(s1, "two_manifold")) && int_of(get(s1, "holes_remaining")) == 0;
        if wt1 {
            score -= 25;
            factor(&mut factors, "stage2_unconfirmed", -25);
        } else {
            score -= 5;
            factor(&mut factors, "stage2_not_applicable", -5);
        }
    }
    let holes = int_of(get(s1, "holes_remaining"));
    if holes != 0 {
        let pen = (holes * 8).min(24);
        score -= pen;
        factor(&mut factors, "holes_remaining", -pen);
    } else {
        score += 5;
        factor(&mut factors, "no_holes_remaining", 5);
    }
    let in_holes = get(report, "defects")
        .and_then(|d| d.get("holes"))
        .and_then(Value::as_array)
        .map(|a| a.len() as i64)
        .unwrap_or(0);
    if in_holes > 0 && holes == 0 {
        score += 5;
        factor(&mut factors, "all_input_holes_closed", 5);
    } else if in_holes > 0 {
        score -= 5;
        factor(&mut factors, "input_holes_remaining", -5);
    }
    let mtype = get(report, "detected_type").and_then(Value::as_str);
    if mtype == Some("unknown") {
        score -= 10;
        factor(&mut factors, "unknown_type", -10);
    } else if let Some(conf) = get(report, "detected_confidence").and_then(Value::as_f64) {
        let conf = conf.clamp(0.0, 1.0);
        let pen = ((1.0 - conf) * 10.0).round_ties_even() as i64;
        if pen != 0 {
            score -= pen;
            factor(&mut factors, "classifier_confidence", -pen);
        }
    }
    if get(report, "tuning_applied").and_then(Value::as_bool) == Some(true) {
        score += 3;
        factor(&mut factors, "tuning_applied", 3);
    } else if get(report, "tuning_applied").and_then(Value::as_bool) == Some(false) {
        score -= 5;
        factor(&mut factors, "tuning_not_applied", -5);
    }
    match get(report, "repair_mode").and_then(Value::as_str) {
        Some("extreme") => {
            score -= 10;
            factor(&mut factors, "extreme_mode", -10);
        }
        Some("aggressive") => {
            score -= 5;
            factor(&mut factors, "aggressive_mode", -5);
        }
        _ => {}
    }
    if truthy(get(report, "self_intersections_removed")) {
        score -= 3;
        factor(&mut factors, "self_intersections_removed", -3);
    }
    if truthy(get(s1, "volume_warning")) {
        score -= 15;
        factor(&mut factors, "volume_warning", -15);
    }
    let score = clamp(score);
    Confidence {
        score,
        label: label(score).into(),
        factors,
    }
}

pub fn estimate_confidence_value(signals: &Value, mech_gate: f64, org_gate: f64) -> Confidence {
    let mut factors = Map::new();
    let mut score = 70i64;
    let mtype = get(signals, "detected_type").and_then(Value::as_str);
    if mtype == Some("unknown") {
        score -= 10;
        factor(&mut factors, "unknown_type", -10);
    } else if matches!(mtype, Some("mechanical") | Some("organic")) {
        score += 5;
        factor(&mut factors, "type_known", 5);
    }
    if let Some(conf) = get(signals, "detected_confidence").and_then(Value::as_f64) {
        let c = conf.clamp(0.0, 1.0);
        let gate = match mtype {
            Some("mechanical") => Some(mech_gate),
            Some("organic") => Some(org_gate),
            _ => None,
        };
        if let Some(g) = gate {
            if c < g {
                score -= 5;
                factor(&mut factors, "below_confidence_gate", -5);
            }
        }
    }
    match get(signals, "tuning_applied").and_then(Value::as_bool) {
        Some(true) => {
            score += 5;
            factor(&mut factors, "tuning_applied", 5);
        }
        Some(false) => {
            score -= 5;
            factor(&mut factors, "tuning_not_applied", -5);
        }
        None => {}
    }
    match get(signals, "mode").and_then(Value::as_str) {
        Some("extreme") => {
            score -= 10;
            factor(&mut factors, "extreme_mode", -10);
        }
        Some("aggressive") => {
            score -= 5;
            factor(&mut factors, "aggressive_mode", -5);
        }
        _ => {}
    }
    if let Some(h) = get(signals, "holes").and_then(Value::as_i64) {
        if h == 0 {
            score += 10;
            factor(&mut factors, "no_input_holes", 10);
        } else if h > 3 {
            score -= 10;
            factor(&mut factors, "many_input_holes", -10);
        }
    }
    if truthy(get(signals, "non_manifold")) {
        score -= 5;
        factor(&mut factors, "non_manifold", -5);
    }
    if truthy(get(signals, "self_intersections")) {
        score -= 8;
        factor(&mut factors, "self_intersections", -8);
    }
    match get(signals, "stage2_bridge_available").and_then(Value::as_bool) {
        Some(false) => {
            score -= 15;
            factor(&mut factors, "stage2_unavailable", -15);
        }
        Some(true) => {
            score += 5;
            factor(&mut factors, "stage2_available", 5);
        }
        None => {}
    }
    let score = clamp(score);
    Confidence {
        score,
        label: label(score).into(),
        factors,
    }
}

pub fn classify_json(data: &str) -> Result<(String, Vec<String>, String), String> {
    let v: Value = serde_json::from_str(data).map_err(|e| e.to_string())?;
    Ok(classify_value(&v))
}

pub fn summary_args_json(data: &str) -> Result<Vec<i64>, String> {
    let v: Value = serde_json::from_str(data).map_err(|e| e.to_string())?;
    Ok(summary_args_value(&v))
}

pub fn repair_confidence_json(data: &str) -> Result<Confidence, String> {
    let v: Value = serde_json::from_str(data).map_err(|e| e.to_string())?;
    Ok(repair_confidence_value(&v))
}

pub fn estimate_confidence_json(
    signals: &str,
    mech_gate: f64,
    org_gate: f64,
) -> Result<Confidence, String> {
    let v: Value = serde_json::from_str(signals).map_err(|e| e.to_string())?;
    Ok(estimate_confidence_value(&v, mech_gate, org_gate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn watertight_needs_stage2_confirmation() {
        let r = json!({
            "stage1": {"two_manifold": true, "holes_remaining": 0},
            "stage2": {"ok": true},
            "detected_type": "mechanical", "detected_confidence": 0.9,
            "tuning_applied": true, "defects": {"holes": []}
        });
        assert_eq!(classify_value(&r).0, "watertight");
        let c = repair_confidence_value(&r);
        assert_eq!(c.label, "high");
        assert!(c.score >= 80);
    }

    #[test]
    fn stage2_skip_is_a_warning_not_watertight() {
        let r = json!({
            "stage1": {"two_manifold": true, "holes_remaining": 0},
            "stage2": {"error": "Stage 2 skipped: no bridge"}
        });
        assert_eq!(classify_value(&r).0, "warning");
        assert_eq!(classify_value(&r).2, "stage2_skipped");
    }

    #[test]
    fn error_forces_zero_confidence() {
        let r = json!({"error": "boom"});
        let c = repair_confidence_value(&r);
        assert_eq!((c.score, c.label.as_str()), (0, "low"));
        assert_eq!(c.factors.get("category_error").unwrap(), &json!(-100));
    }

    #[test]
    fn pre_repair_estimate_uses_default_gates() {
        let s = json!({"detected_type": "organic", "detected_confidence": 0.2,
                       "tuning_applied": false, "mode": "aggressive",
                       "holes": 5, "non_manifold": 2, "stage2_bridge_available": false});
        let c = estimate_confidence_value(&s, 0.7, 0.7);
        assert_eq!(c.label, "low");
        assert!(c.factors.contains_key("below_confidence_gate"));
        assert!(c.factors.contains_key("stage2_unavailable"));
    }
}
