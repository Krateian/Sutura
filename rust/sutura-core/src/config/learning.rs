// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Learning-triage aggregation and ranking bonus.
//!
//! Mirrors `sutura_engine/learning.py`: per-(template, method) success/time
//! counters in `triage_learning.json` under the Sutura data directory, turned
//! into a small bounded Bayesian ranking bonus.
//!
//! **Privacy rule (unchanged):** only template ids and method slugs are stored
//! — never a file name, path, user, IP or machine identifier.
//!
//! The Python module uses `numpy.median` for the speed factor; here the median
//! is a plain sorted-vector computation. `learning_bonus` is compared to 1e-9.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// Runtime user data lives in `<data>/`, never in the code/install dir.
pub const DATA_ENV: &str = "SUTURA_DATA_DIR";
pub const LEARNING_ENV: &str = "SUTURA_LEARNING_TRIAGE";
pub const LEARNING_FILE: &str = "triage_learning.json";
pub const LEARNING_SCHEMA_VERSION: i64 = 2;
pub const HISTORY_FILE: &str = "history.jsonl";

/// `[ranking].learning_max_bonus` and `[ranking].learning_prior`.
#[derive(Clone, Copy, Debug)]
pub struct LearningKnobs {
    pub max_bonus: f64,
    pub prior: f64,
}

impl Default for LearningKnobs {
    fn default() -> Self {
        Self {
            max_bonus: 0.05,
            prior: 2.0,
        }
    }
}

/// The classifier version stamped on every bucket; `None` disables the gate.
pub type ClassifierVersion = Option<i64>;

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The runtime user-data directory (never the code directory).
pub fn data_dir() -> PathBuf {
    match std::env::var_os(DATA_ENV).filter(|v| !v.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => home_dir().join(".local/share/sutura/data"),
    }
}

pub fn data_file(name: &str) -> PathBuf {
    data_dir().join(name)
}

pub fn learning_path() -> PathBuf {
    data_file(LEARNING_FILE)
}

fn config_intensity_path() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join("sutura/config.json");
    }
    home_dir().join(".config/sutura/config.json")
}

fn learning_enabled_from_config() -> bool {
    let text = std::fs::read_to_string(config_intensity_path()).ok();
    let Some(text) = text else { return true };
    let Ok(data) = serde_json::from_str::<Value>(&text) else {
        return true;
    };
    match data.get("learning_triage") {
        Some(Value::Bool(b)) => *b,
        Some(Value::Null) | None => true,
        Some(other) => other.as_bool().unwrap_or(true),
    }
}

/// Whether the local-history ranking bonus is active (env > config).
pub fn learning_enabled(environ: Option<&BTreeMap<String, String>>) -> bool {
    let env = match environ {
        Some(map) => map.get(LEARNING_ENV).cloned(),
        None => std::env::var(LEARNING_ENV).ok(),
    };
    if let Some(value) = env {
        return !matches!(
            value.trim().to_lowercase().as_str(),
            "0" | "false" | "no" | "off" | ""
        );
    }
    learning_enabled_from_config()
}

fn is_other_version(bucket: &Value, current: ClassifierVersion) -> bool {
    let Some(current) = current else { return false };
    match bucket.get("cv") {
        Some(Value::Number(n)) => n.as_i64().map(|cv| cv != current).unwrap_or(false),
        _ => false,
    }
}

fn merge_bucket(dest: &mut Map<String, Value>, src: &Value) {
    for key in ["n", "ok", "total_ms"] {
        let current = dest.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
        let extra = src.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
        dest.insert(key.to_string(), json!(current + extra));
    }
    if let Some(cv) = src.get("cv") {
        dest.insert("cv".to_string(), cv.clone());
    }
}

/// Counted buckets only: drops relocation keys and other-version buckets.
fn filter_stats(raw: &Value, current: ClassifierVersion) -> Map<String, Value> {
    let mut filtered = Map::new();
    let Some(templates) = raw.as_object() else {
        return filtered;
    };
    for (template, methods) in templates {
        let Some(methods) = methods.as_object() else {
            continue;
        };
        let mut kept = Map::new();
        for (method_id, bucket) in methods {
            if !bucket.is_object() || method_id.contains('@') {
                continue;
            }
            if is_other_version(bucket, current) {
                continue;
            }
            kept.insert(method_id.clone(), bucket.clone());
        }
        if !kept.is_empty() {
            filtered.insert(template.clone(), Value::Object(kept));
        }
    }
    filtered
}

/// `triage_learning.json` contents; a missing/broken file yields the empty doc.
pub fn load_learning(path: Option<&Path>) -> Value {
    let target = path.map(|p| p.to_path_buf()).unwrap_or_else(learning_path);
    if let Ok(text) = std::fs::read_to_string(&target) {
        if let Ok(Value::Object(mut map)) = serde_json::from_str::<Value>(&text) {
            if !map.get("templates").map(|t| t.is_object()).unwrap_or(false) {
                map.insert("templates".to_string(), Value::Object(Map::new()));
            }
            return Value::Object(map);
        }
    }
    json!({"schema_version": LEARNING_SCHEMA_VERSION, "templates": {}})
}

fn save_learning(data: &Value, path: Option<&Path>) -> Result<(), String> {
    let target = path.map(|p| p.to_path_buf()).unwrap_or_else(learning_path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut data = data.clone();
    if let Some(map) = data.as_object_mut() {
        map.insert(
            "schema_version".to_string(),
            Value::from(LEARNING_SCHEMA_VERSION),
        );
    }
    let text = serde_json::to_string(&data).map_err(|e| e.to_string())?;
    let dir = target
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let tmp = dir.join(format!(".learning-{}.tmp", std::process::id()));
    std::fs::write(&tmp, text.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &target).map_err(|e| e.to_string())
}

/// One `methods_tried` entry the learning pass consumes.
#[derive(Clone, Debug)]
pub struct MethodTrial {
    pub id: i64,
    pub outcome: String,
    pub template: Option<String>,
    pub elapsed_ms: Option<f64>,
}

/// Accumulate per-(template, method) success/time counts. Never fails.
pub fn record_learning(
    trials: &[MethodTrial],
    current: ClassifierVersion,
    path: Option<&Path>,
) -> Result<(), String> {
    if trials.is_empty() {
        return Ok(());
    }
    let mut data = load_learning(path);
    let root = data
        .as_object_mut()
        .ok_or_else(|| "learning file is not an object".to_string())?;
    let templates = root
        .entry("templates")
        .or_insert_with(|| Value::Object(Map::new()));
    let templates = templates
        .as_object_mut()
        .ok_or_else(|| "templates is not an object".to_string())?;
    for trial in trials {
        if !matches!(trial.outcome.as_str(), "accepted" | "rejected" | "best") {
            continue;
        }
        let template = trial
            .template
            .clone()
            .unwrap_or_else(|| "_none_".to_string());
        let methods = templates
            .entry(template)
            .or_insert_with(|| Value::Object(Map::new()));
        let Some(methods) = methods.as_object_mut() else {
            continue;
        };
        let method_key = trial.id.to_string();
        let mut bucket_value = methods.get(&method_key).cloned();
        if let Some(existing) = &bucket_value {
            if is_other_version(existing, current) {
                let cv = existing
                    .get("cv")
                    .and_then(|v| v.as_i64())
                    .unwrap_or_default();
                let stale_key = format!("{method_key}@{cv}");
                let stale = methods
                    .get(&stale_key)
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Map::new()));
                let mut stale_map = match stale {
                    Value::Object(map) => map,
                    _ => Map::new(),
                };
                merge_bucket(&mut stale_map, existing);
                methods.insert(stale_key, Value::Object(stale_map));
                bucket_value = None;
            }
        }
        let mut bucket = match bucket_value {
            Some(Value::Object(map)) => map,
            _ => {
                let mut map = Map::new();
                map.insert("n".to_string(), Value::from(0));
                map.insert("ok".to_string(), Value::from(0));
                map.insert("total_ms".to_string(), Value::from(0));
                map
            }
        };
        if let Some(cv) = current {
            bucket.insert("cv".to_string(), Value::from(cv));
        }
        let bump = |map: &mut Map<String, Value>, key: &str, by: i64| {
            let value = map.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
            map.insert(key.to_string(), Value::from(value + by));
        };
        bump(&mut bucket, "n", 1);
        if matches!(trial.outcome.as_str(), "accepted" | "best") {
            bump(&mut bucket, "ok", 1);
        }
        if let Some(ms) = trial.elapsed_ms {
            if ms >= 0.0 {
                bump(&mut bucket, "total_ms", ms as i64);
            }
        }
        methods.insert(method_key, Value::Object(bucket));
    }
    save_learning(&data, path)
}

/// Per-(template, method) counts with version filtering.
pub fn triage_stats(
    current: ClassifierVersion,
    path: Option<&Path>,
) -> Map<String, Value> {
    filter_stats(&load_learning(path)["templates"], current)
}

/// Median of a sorted slice (numpy's default for an even count: mean of the
/// two middle values).
fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// Bounded `[0, max_bonus]` Bayesian bonus for `(template, method_id)`.
///
/// The posterior success mean under a `Beta(prior, prior)` prior is scaled to a
/// small bonus (0 at 0.5, `max_bonus` at 1.0) and damped by a speed factor in
/// `[0.5, 1.0]`. No data gives exactly 0.
pub fn learning_bonus(
    template: Option<&str>,
    method_id: Option<&str>,
    stats: Option<&Map<String, Value>>,
    current: ClassifierVersion,
    knobs: LearningKnobs,
) -> f64 {
    let (Some(template), Some(method_id)) = (template, method_id) else {
        return 0.0;
    };
    if template.is_empty() || method_id.is_empty() {
        return 0.0;
    }
    let owned;
    let stats = match stats {
        Some(s) => s,
        None => {
            owned = triage_stats(current, None);
            &owned
        }
    };
    // Filter even an externally supplied map.
    let stats = filter_stats(&Value::Object(stats.clone()), current);
    let bucket = stats
        .get(template)
        .and_then(|v| v.as_object())
        .and_then(|m| m.get(method_id));
    let Some(bucket) = bucket else { return 0.0 };
    let n = bucket.get("n").and_then(|v| v.as_i64()).unwrap_or(0);
    let ok = bucket.get("ok").and_then(|v| v.as_i64()).unwrap_or(0);
    if n <= 0 {
        return 0.0;
    }
    let n = n as f64;
    let ok = ok as f64;
    let posterior = (ok + knobs.prior) / (n + 2.0 * knobs.prior);
    let base = ((posterior - 0.5) * 2.0).clamp(0.0, 1.0);
    if base <= 0.0 {
        return 0.0;
    }
    let avg_ms = bucket.get("total_ms").and_then(|v| v.as_i64()).unwrap_or(0) as f64 / n;
    let mut all_ms: Vec<f64> = Vec::new();
    for template in stats.values() {
        if let Some(methods) = template.as_object() {
            for bucket in methods.values() {
                let n = bucket.get("n").and_then(|v| v.as_i64()).unwrap_or(0);
                if n > 0 {
                    let total = bucket.get("total_ms").and_then(|v| v.as_i64()).unwrap_or(0);
                    all_ms.push(total as f64 / n as f64);
                }
            }
        }
    }
    let median_ms = if all_ms.is_empty() { 0.0 } else { median(&mut all_ms) };
    let mut speed = 1.0;
    if median_ms > 0.0 && avg_ms > 0.0 {
        let ratio = avg_ms / median_ms;
        speed = (1.0 / (0.5 + 0.5 * ratio)).clamp(0.5, 1.0);
    }
    round6(knobs.max_bonus * base * speed)
}

/// Python's `round(value, 6)` (banker's rounding on the decimal digits).
pub fn round6(value: f64) -> f64 {
    let scaled = value * 1e6;
    let floor = scaled.floor();
    let frac = scaled - floor;
    let rounded = if (frac - 0.5).abs() < f64::EPSILON {
        // Half-way: round to even, like Python.
        if (floor as i64) % 2 == 0 {
            floor
        } else {
            floor + 1.0
        }
    } else {
        scaled.round()
    };
    rounded / 1e6
}

/// Delete the learning file. Returns true when something was removed.
pub fn clear_learning(path: Option<&Path>) -> bool {
    let target = path.map(|p| p.to_path_buf()).unwrap_or_else(learning_path);
    std::fs::remove_file(target).is_ok()
}
