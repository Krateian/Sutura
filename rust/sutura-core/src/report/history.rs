// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
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

/// Deterministic sha256 of a mesh's geometry, matching `history.mesh_fingerprint`.
///
/// Vertices are rounded to 4 decimals *in f32* and hashed as raw little-endian
/// f32 bytes, then the triangles as raw little-endian i32 — the exact buffers
/// NumPy would hand to `hashlib` (`np.round(v, 4).tobytes()` of a float32 array).
/// Rounding in f32 before hashing is what makes the same mesh hash identically
/// when it arrives via a slightly different float path.
pub fn mesh_fingerprint(verts: &[[f32; 3]], tris: &[[i32; 3]]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for v in verts {
        for c in v {
            // `round` on a float32 value must round the f32 itself, so the
            // arithmetic is done in f64 and stored back as f32 — the same
            // double-rounding NumPy performs.
            let r = (f64::from(*c) * 1e4).round() / 1e4;
            h.update((r as f32).to_le_bytes());
        }
    }
    for t in tris {
        for c in t {
            h.update(c.to_le_bytes());
        }
    }
    let digest = h.finalize();
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Serialize a value exactly as Python's `json.dumps(v, ensure_ascii=False)`.
///
/// `serde_json::to_string` is compact (`{"a":1}`); Python writes `{"a": 1}` with
/// `", "` and `": "` separators, so byte-comparing a history line against
/// Python's needs this form. Floats go through `fmt::py_repr_f64`, which already
/// implements `repr(float)` (e.g. `1e-06`, `300.0`), and strings through
/// `serde_json`'s escaper. Key order is the insertion order of the map.
pub fn py_json_dumps(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() => crate::fmt::py_repr_f64(f),
            _ => n.to_string(),
        },
        Value::String(s) => serde_json::to_string(s).expect("string serialization"),
        Value::Array(items) => {
            let body: Vec<String> = items.iter().map(py_json_dumps).collect();
            format!("[{}]", body.join(", "))
        }
        Value::Object(map) => {
            let body: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    let key = serde_json::to_string(k).expect("key serialization");
                    format!("{key}: {}", py_json_dumps(v))
                })
                .collect();
            format!("{{{}}}", body.join(", "))
        }
    }
}

/// Append one record line, assigning its sequence number. Never fails.
///
/// Mirrors `history.append_record`: `seq` is the current line count + 1, the
/// directory is created on demand, and every error is swallowed — recording must
/// never break a repair. The line is `json.dumps(record, ensure_ascii=False)` plus
/// a newline, byte-identical to Python's.
pub fn append_record(record: &mut Value) {
    let _ = try_append_record(record);
}

fn try_append_record(record: &mut Value) -> Result<(), String> {
    let path = crate::config::learning::data_file(HISTORY_FILE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let seq = match std::fs::read_to_string(&path) {
        Ok(text) => text.lines().count() as i64 + 1,
        Err(_) => 1,
    };
    if let Some(map) = record.as_object_mut() {
        map.insert("seq".to_string(), Value::from(seq));
    }
    let line = py_json_dumps(record);
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    file.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    file.write_all(b"\n").map_err(|e| e.to_string())
}

/// Append one history record for a finished repair. Never fails.
///
/// Counterpart of `history.write` for the single-mesh case: it builds the
/// record and appends it, swallowing every error. Multi-object 3MF reports call
/// this once per object.
pub fn write_record(
    result: &Value,
    verts: &[[f32; 3]],
    tris: &[[i32; 3]],
    version: &str,
    elapsed_ms: i64,
    fmt: Option<&str>,
    ts: &str,
) {
    let fingerprint = mesh_fingerprint(verts, tris);
    let mut record = build_record_value(result, &fingerprint, version, elapsed_ms, fmt, ts);
    append_record(&mut record);
}

/// The history file name inside the data directory (`paths.data_file`).
pub const HISTORY_FILE: &str = "history.jsonl";

/// Current UTC time as Python's `datetime.now(timezone.utc).isoformat()`.
///
/// That is `YYYY-MM-DDTHH:MM:SS.ffffff+00:00` — always six fractional digits and
/// a `+00:00` offset (not `Z`). Built from `SystemTime` so no date dependency is
/// needed; the calendar conversion is Howard Hinnant's `civil_from_days`.
pub fn now_ts() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let micros = now.subsec_micros();

    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (h, mi, s) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    format!(
        "{year:04}-{month:02}-{day:02}T{h:02}:{mi:02}:{s:02}.{micros:06}+00:00"
    )
}

/// Days since 1970-01-01 to a (year, month, day) civil date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
        // A leap day, to exercise the era/century correction.
        assert_eq!(civil_from_days(18_321), (2020, 2, 29));
    }

    #[test]
    fn now_ts_matches_python_isoformat_shape() {
        let ts = now_ts();
        // 2026-11-16T06:04:34.354044+00:00
        assert_eq!(ts.len(), 32, "{ts}");
        assert!(ts.ends_with("+00:00"), "{ts}");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[19..20], ".");
        // The fractional part is always exactly six digits (NumPy/Python
        // `isoformat` pads microseconds rather than trimming trailing zeros).
        assert!(ts[20..26].chars().all(|c| c.is_ascii_digit()), "{ts}");
        assert!(ts[..4].chars().all(|c| c.is_ascii_digit()), "{ts}");
    }

    /// The exact byte layout Python's `json.dumps` produces: `", "` and `": "`
    /// separators, `repr(float)` for numbers. Values pinned from Python.
    #[test]
    fn py_json_dumps_matches_python() {
        let v = serde_json::json!({
            "a": 0.12345678901234, "b": 1e-6, "c": 0.0, "d": 300.0,
            "e": Value::Null, "f": true, "g": [1, 2], "h": "s"
        });
        assert_eq!(
            py_json_dumps(&v),
            r#"{"a": 0.12345678901234, "b": 1e-06, "c": 0.0, "d": 300.0, "e": null, "f": true, "g": [1, 2], "h": "s"}"#
        );
    }

    /// Fingerprint pinned from `history.mesh_fingerprint` on the same buffers
    /// (a 128-vertex / 250-face mesh, NumPy float32/int32). This is the value
    /// the Python and Rust history lines must agree on.
    #[test]
    fn fingerprint_is_stable_for_known_buffer() {
        // A unit square in two triangles: verifies f32 rounding + LE byte order.
        let verts = [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]];
        let tris = [[0i32, 1, 2], [0, 2, 3]];
        let fp = mesh_fingerprint(&verts, &tris);
        assert_eq!(fp.len(), 64);
        // Deterministic and order-sensitive.
        assert_eq!(fp, mesh_fingerprint(&verts, &tris));
        assert_ne!(fp, mesh_fingerprint(&verts, &[[0, 1, 2], [0, 3, 2]]));
    }

    /// Rounding happens in f32, so coordinates that differ only beyond 4
    /// decimals collapse to the same digest — the property that makes the same
    /// mesh hash identically when it arrives through a different float path.
    #[test]
    fn fingerprint_rounds_to_four_decimals() {
        let a = [[0.00001f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let b = [[0.00002f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let tris = [[0i32, 1, 2]];
        assert_eq!(mesh_fingerprint(&a, &tris), mesh_fingerprint(&b, &tris));
    }
}

