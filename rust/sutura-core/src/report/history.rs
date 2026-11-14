//! Usage-history record and summary formatting, matching `sutura/history.py`.

use serde_json::{json, Map, Value};

const SCHEMA_VERSION: i64 = 1;
const ALLOWED_FORMATS: [&str; 3] = ["stl", "obj", "3mf"];

fn get<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    v.get(k)
}
fn arr_len(v: Option<&Value>) -> usize {
    v.and_then(Value::as_array).map(Vec::len).unwrap_or(0)
}
fn mesh_format(result: &Value) -> Option<String> {
    let input = get(result, "input").and_then(Value::as_str).unwrap_or("");
    let ext = std::path::Path::new(input)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    ext.filter(|e| ALLOWED_FORMATS.contains(&e.as_str()))
}
fn stage2_status(result: &Value) -> &'static str {
    let Some(s2) = get(result, "stage2").filter(|x| x.is_object()) else {
        return "not_run";
    };
    if s2
        .get("ok")
        .map(|v| v.as_bool().unwrap_or(false))
        .unwrap_or(false)
    {
        return "ok";
    }
    if s2.get("error").is_some() {
        return "error";
    }
    "skipped"
}
fn adopted(v: Option<&Value>) -> bool {
    v.and_then(|x| x.get("adopted"))
        .map(|x| x.as_bool().unwrap_or(false))
        .unwrap_or(false)
}
fn null_or(v: Option<&Value>) -> Value {
    v.cloned().unwrap_or(Value::Null)
}

#[allow(clippy::too_many_arguments)]
pub fn build_record_value(
    result: &Value,
    fingerprint: &str,
    version: &str,
    elapsed_ms: i64,
    fmt: Option<&str>,
    ts: &str,
) -> Value {
    let s1 = get(result, "stage1").cloned().unwrap_or_else(|| json!({}));
    let defects = get(result, "defects").cloned().unwrap_or_else(|| json!({}));
    let mu = get(result, "method_used")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let tried = get(result, "methods_tried")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let methods_tried: Vec<Value> = tried
        .iter()
        .filter(|t| t.is_object())
        .map(|t| {
            json!({
                "num": null_or(t.get("num")),
                "id": null_or(t.get("id")),
                "outcome": null_or(t.get("outcome")),
                "template": null_or(t.get("template")),
                "elapsed_ms": null_or(t.get("elapsed_ms")),
            })
        })
        .collect();
    let metrics = get(result, "repair_fidelity_metrics")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut m = Map::new();
    m.insert("schema_version".into(), Value::from(SCHEMA_VERSION));
    m.insert("seq".into(), Value::from(0));
    m.insert("ts".into(), Value::from(ts));
    m.insert("app_version".into(), Value::from(version));
    m.insert(
        "format".into(),
        fmt.map(|f| Value::from(f))
            .or_else(|| mesh_format(result).map(Value::from))
            .unwrap_or(Value::Null),
    );
    m.insert("mesh_fingerprint".into(), Value::from(fingerprint));
    m.insert("elapsed_ms".into(), Value::from(elapsed_ms));
    for k in [
        "faces_before",
        "faces_after",
        "vertices_before",
        "vertices_after",
        "holes_closed",
        "holes_remaining",
        "non_manifold_edges_fixed",
        "non_manifold_edges_remaining",
        "volume_change_percent",
    ] {
        m.insert(k.into(), null_or(s1.get(k)));
    }
    m.insert(
        "defect_holes".into(),
        Value::from(arr_len(defects.get("holes"))),
    );
    m.insert(
        "defect_non_manifold".into(),
        Value::from(arr_len(defects.get("non_manifold"))),
    );
    for k in [
        "detected_type",
        "detected_confidence",
        "repair_mode",
        "tuning_applied",
        "category",
        "repair_confidence",
        "repair_confidence_label",
        "repair_fidelity",
        "repair_verdict_code",
        "repair_verdict_areas",
        "interior_walls_note",
        "extreme_passes_applied",
        "self_intersections_found",
        "self_intersections_removed",
    ] {
        m.insert(k.into(), null_or(result.get(k)));
    }
    m.insert("method_used_num".into(), null_or(mu.get("num")));
    m.insert("method_used_id".into(), null_or(mu.get("id")));
    m.insert("method_used_source".into(), null_or(mu.get("source")));
    m.insert("methods_tried".into(), Value::Array(methods_tried));
    m.insert("filters_applied".into(), null_or(s1.get("applied_filters")));
    m.insert(
        "filters_skipped".into(),
        Value::from(
            s1.get("skipped")
                .and_then(Value::as_object)
                .map(Map::len)
                .unwrap_or(0),
        ),
    );
    m.insert("stage2_status".into(), Value::from(stage2_status(result)));
    m.insert(
        "issues".into(),
        Value::Array(
            result
                .get("issues")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        ),
    );
    m.insert(
        "repair_verdict_low_confidence".into(),
        Value::Bool(
            result
                .get("repair_verdict_low_confidence")
                .map(|v| v.as_bool().unwrap_or(false))
                .unwrap_or(false),
        ),
    );
    for k in [
        "feature_recall",
        "feature_recall_all",
        "interior_walls_removed_faces",
        "interior_walls_removed_area_mm2",
        "interior_walls_buried_faces",
        "interior_walls_buried_area_mm2",
        "interior_walls_debris_faces",
        "interior_walls_cavity_faces",
    ] {
        m.insert(k.into(), null_or(metrics.get(k)));
    }
    m.insert(
        "autorefine_applied".into(),
        Value::Bool(adopted(get(result, "experimental_autorefine"))),
    );
    m.insert(
        "ftetwild_applied".into(),
        Value::Bool(adopted(get(result, "experimental_ftetwild"))),
    );
    m.insert(
        "indirect_autorefine_applied".into(),
        Value::Bool(adopted(get(result, "experimental_indirect_autorefine"))),
    );
    m.insert(
        "tamponade_applied".into(),
        Value::Bool(adopted(get(result, "tamponade"))),
    );
    Value::Object(m)
}

pub fn build_record_json(
    result: &str,
    fingerprint: &str,
    version: &str,
    elapsed_ms: i64,
    fmt: Option<&str>,
    ts: &str,
) -> Result<String, String> {
    let v: Value = serde_json::from_str(result).map_err(|e| e.to_string())?;
    Ok(build_record_value(&v, fingerprint, version, elapsed_ms, fmt, ts).to_string())
}

pub fn summary_text_value(records: &[Value]) -> String {
    use std::collections::BTreeMap;
    let mut lines = vec![format!(
        "Sutura usage history: {} repair record(s)",
        records.len()
    )];
    let ts: Vec<&str> = records
        .iter()
        .filter_map(|r| r.get("ts").and_then(Value::as_str))
        .collect();
    if !ts.is_empty() {
        lines.push(format!(
            "time range: {} .. {}",
            ts.iter().min().unwrap(),
            ts.iter().max().unwrap()
        ));
    }
    for (label, key) in [
        ("category", "category"),
        ("repair_mode", "repair_mode"),
        ("detected_type", "detected_type"),
        ("stage2_status", "stage2_status"),
    ] {
        let mut c: BTreeMap<String, usize> = BTreeMap::new();
        for r in records {
            let v = r.get(key).cloned().unwrap_or(Value::Null);
            let s = match v {
                Value::String(s) => s,
                Value::Null => "None".to_string(),
                other => other.to_string(),
            };
            *c.entry(s).or_insert(0) += 1;
        }
        if !c.is_empty() {
            lines.push(format!(
                "{}: {}",
                label,
                c.iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    let conf: Vec<f64> = records
        .iter()
        .filter_map(|r| r.get("repair_confidence").and_then(Value::as_f64))
        .collect();
    if !conf.is_empty() {
        let avg = conf.iter().sum::<f64>() / conf.len() as f64;
        let min = conf.iter().cloned().fold(f64::INFINITY, f64::min) as i64;
        let max = conf.iter().cloned().fold(f64::NEG_INFINITY, f64::max) as i64;
        lines.push(format!(
            "repair_confidence: avg={avg:.1} min={min} max={max}"
        ));
    }
    let el: Vec<f64> = records
        .iter()
        .filter_map(|r| r.get("elapsed_ms").and_then(Value::as_f64))
        .collect();
    if !el.is_empty() {
        let avg = el.iter().sum::<f64>() / el.len() as f64;
        let total = el.iter().sum::<f64>() / 1000.0;
        lines.push(format!("elapsed_ms: avg={avg:.0} total={total:.1}s"));
    }
    let mut fps = std::collections::BTreeSet::new();
    for r in records {
        if let Some(f) = r.get("mesh_fingerprint").and_then(Value::as_str) {
            fps.insert(f.to_string());
        }
    }
    lines.push(format!(
        "distinct meshes (geometry fingerprint): {}",
        fps.len()
    ));
    let ext = records
        .iter()
        .filter(|r| {
            r.get("extreme_passes_applied")
                .map(|v| v.as_bool().unwrap_or(false))
                .unwrap_or(false)
        })
        .count();
    lines.push(format!("extreme extra passes applied: {ext}"));
    lines.join("\n")
}

pub fn summary_text_json(records: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(records).map_err(|e| e.to_string())?;
    let arr = v
        .as_array()
        .ok_or_else(|| "records must be an array".to_string())?;
    Ok(summary_text_value(arr))
}
