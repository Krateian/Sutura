//! Method-health report assembly, matching `health.build_report`/`human_line`.
//! The runtime collector and guards stay Python; only the JSON assembly moves.

use serde_json::{json, Map, Value};

pub fn build_report_value(availability: &Value, errors: &[Value], skips: &[Value]) -> Value {
    let mut methods = Map::new();
    let mut errored: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for e in errors {
        if let Some(m) = e.get("method").and_then(Value::as_str) {
            errored.insert(m.to_string());
        }
    }
    if let Some(av) = availability.as_object() {
        for (mid, item) in av {
            let (status, reason) = if let Some(a) = item.as_array() {
                (
                    a.first()
                        .and_then(Value::as_str)
                        .unwrap_or("fail")
                        .to_string(),
                    a.get(1).cloned().unwrap_or(Value::Null),
                )
            } else if let Some(o) = item.as_object() {
                (
                    o.get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("fail")
                        .to_string(),
                    o.get("reason").cloned().unwrap_or(Value::Null),
                )
            } else {
                ("fail".to_string(), Value::Null)
            };
            let status = if errored.contains(mid) {
                "fail".to_string()
            } else {
                status
            };
            methods.insert(mid.clone(), json!({"status": status, "reason": reason}));
        }
    }
    let ok = methods.values().filter(|v| v["status"] == "ok").count();
    let failed = methods.values().filter(|v| v["status"] == "fail").count();
    let skipped = methods.values().filter(|v| v["status"] == "skip").count();
    json!({
        "total": methods.len(),
        "ok": ok,
        "failed": failed,
        "skipped": skipped,
        "methods": methods,
        "errors": errors,
        "skips": skips,
    })
}

pub fn build_report_json(availability: &str, errors: &str, skips: &str) -> Result<String, String> {
    let a: Value = serde_json::from_str(availability).map_err(|e| e.to_string())?;
    let e: Value = serde_json::from_str(errors).map_err(|e| e.to_string())?;
    let s: Value = serde_json::from_str(skips).map_err(|e| e.to_string())?;
    Ok(build_report_value(
        &a,
        e.as_array().map(Vec::as_slice).unwrap_or(&[]),
        s.as_array().map(Vec::as_slice).unwrap_or(&[]),
    )
    .to_string())
}

pub fn human_line(value: &Value) -> String {
    let total = value.get("total").and_then(Value::as_i64).unwrap_or(0);
    let ok = value.get("ok").and_then(Value::as_i64).unwrap_or(0);
    let failed = value.get("failed").and_then(Value::as_i64).unwrap_or(0);
    let skipped = value.get("skipped").and_then(Value::as_i64).unwrap_or(0);
    let mut line = format!("Methods: {ok}/{total} OK");
    let mut extra = Vec::new();
    if failed > 0 {
        extra.push(format!("{failed} failed"));
    }
    if skipped > 0 {
        extra.push(format!("{skipped} skipped"));
    }
    if !extra.is_empty() {
        line.push_str(&format!(" ({})", extra.join(", ")));
    }
    line
}

pub fn human_line_json(value: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(value).map_err(|e| e.to_string())?;
    Ok(human_line(&v))
}
