// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Repair policy: packaged defaults, user override merge, content hash.
//!
//! Mirrors `sutura_engine/policy/__init__.py`. The packaged
//! `default_policy.toml` is embedded at build time; the user file
//! (`$SUTURA_POLICY_USER` or `~/.config/sutura/policy.toml`) is deep-merged on
//! top, locked gate subtrees and unknown keys are dropped and recorded, and the
//! effective policy is hashed.
//!
//! **Byte contract:** `content_hash` is `sha256` of the policy serialized
//! exactly like Python's
//! `json.dumps(data, sort_keys=True, separators=(',', ':'))`, first 16 hex
//! characters. `json_python` below reproduces that serialization, including
//! Python's float repr, because the hash is compared across languages.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The packaged policy, embedded so the core has no runtime asset lookup.
pub const DEFAULT_POLICY_TOML: &str = include_str!("../../assets/default_policy.toml");

pub const USER_POLICY_ENV: &str = "SUTURA_POLICY_USER";
pub const POLICY_ENV: &str = "SUTURA_POLICY";
pub const SCHEMA_VERSION: i64 = 1;

const REQUIRED: &[&str] = &[
    "policy",
    "order",
    "auto",
    "si_escalation",
    "ranking",
    "templates",
    "intensity",
    "modes",
    "stage1_profiles",
    "stage1_types",
    "stage1_tuning",
];
const ORDER_KEYS: &[&str] = &[
    "deep_tier_order",
    "final_tier_precedence",
    "graft",
    "ftetwild",
    "deep_tier_method_num",
];
const AUTO_NUM_KEYS: &[&str] = &[
    "max_attempts",
    "fallback_watchdog_s",
    "fallback_budget_factor",
    "fallback_min_s",
    "fallback_max_s",
    "invent_min_score",
    "generative_max_hausdorff_rel",
];
const SI_NUM_KEYS: &[&str] = &[
    "method_num",
    "vol_rel_tol",
    "dist_tol_min_mm",
    "dist_tol_max_mm",
    "dist_tol_div",
];
const TEMPLATE_NUM_KEYS: &[&str] = &[
    "open_area_scan",
    "open_area_relief",
    "large_loop_ratio",
    "many_loops",
    "si_moderate",
    "si_heavy",
];
const TEMPLATE_INT_KEYS: &[&str] = &["many_loops", "si_moderate", "si_heavy"];
const TEMPLATE_IDS: &[&str] = &[
    "mechanical",
    "organic",
    "single_side_scan",
    "relief",
    "repeated_pattern",
    "dense_scan_heavy_si",
];
const INTENSITY_FIELDS: &[&str] = &[
    "ftetwild_enabled",
    "ftetwild_max_faces",
    "ftetwild_timeout",
    "graft_max_faces",
    "graft_timeout",
    "ftetwild_optimize_retry_on_dense_fail",
    "deep_repair",
    "dense_target_ladder",
    "dense_ratio",
    "dense_min_faces",
    "ftetwild_hausdorff_samples",
];
const INTENSITY_CAP: &[&str] = &["ftetwild_max_faces", "graft_max_faces"];
const INTENSITY_BOOL: &[&str] = &[
    "ftetwild_enabled",
    "ftetwild_optimize_retry_on_dense_fail",
];
const INTENSITY_INT: &[&str] = &[
    "dense_ratio",
    "dense_min_faces",
    "ftetwild_hausdorff_samples",
];
const EXTRA_GATE_KEYS: &[&str] = &["auto.generative_max_hausdorff_rel"];
const EXTRA_GATE_SECTIONS: &[&str] = &["si_escalation"];

/// A policy value in TOML's data model.
#[derive(Clone, Debug, PartialEq)]
pub enum PolicyValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<PolicyValue>),
    Table(BTreeMap<String, PolicyValue>),
}

impl PolicyValue {
    pub fn as_table(&self) -> Option<&BTreeMap<String, PolicyValue>> {
        match self {
            PolicyValue::Table(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_table_mut(&mut self) -> Option<&mut BTreeMap<String, PolicyValue>> {
        match self {
            PolicyValue::Table(t) => Some(t),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&PolicyValue> {
        self.as_table().and_then(|t| t.get(key))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            PolicyValue::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            PolicyValue::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            PolicyValue::Int(i) => Some(*i as f64),
            PolicyValue::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            PolicyValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[PolicyValue]> {
        match self {
            PolicyValue::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn is_number(&self) -> bool {
        matches!(self, PolicyValue::Int(_) | PolicyValue::Float(_))
    }

    pub fn is_int(&self) -> bool {
        matches!(self, PolicyValue::Int(_))
    }

    pub fn is_integer_valued(&self) -> bool {
        match self {
            PolicyValue::Int(_) => true,
            PolicyValue::Float(f) => f.is_finite() && f.fract() == 0.0,
            _ => false,
        }
    }
}

/// A policy load/validate failure, mirroring Python's `ValueError` messages.
#[derive(Clone, Debug, PartialEq)]
pub struct PolicyError(pub String);

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PolicyError {}

impl From<std::io::Error> for PolicyError {
    fn from(e: std::io::Error) -> Self {
        PolicyError(format!("{}: {}", "IoError", e))
    }
}

fn err<T>(message: String) -> Result<T, PolicyError> {
    Err(PolicyError(message))
}

/// Parse TOML text into the policy value model.
pub fn parse_toml(text: &str) -> Result<PolicyValue, PolicyError> {
    let raw: toml::Value = text
        .parse::<toml::Value>()
        .map_err(|e| PolicyError(format!("TomlDecodeError: {e}")))?;
    Ok(from_toml(raw))
}

fn from_toml(value: toml::Value) -> PolicyValue {
    match value {
        toml::Value::String(s) => PolicyValue::Str(s),
        toml::Value::Integer(i) => PolicyValue::Int(i),
        toml::Value::Float(f) => PolicyValue::Float(f),
        toml::Value::Boolean(b) => PolicyValue::Bool(b),
        toml::Value::Datetime(d) => PolicyValue::Str(d.to_string()),
        toml::Value::Array(items) => {
            PolicyValue::Array(items.into_iter().map(from_toml).collect())
        }
        toml::Value::Table(table) => PolicyValue::Table(
            table
                .into_iter()
                .map(|(k, v)| (k, from_toml(v)))
                .collect::<BTreeMap<_, _>>(),
        ),
    }
}

/// Python's `repr(float)`: shortest round-trip, decimal notation for
/// `1e-4 <= |x| < 1e16`, otherwise scientific with a signed two-digit exponent.
pub fn python_float_repr(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    // Shortest round-trip digits, e.g. "1.2345e2" or "0e0".
    let sci = format!("{:e}", value);
    let (mantissa, exp) = match sci.split_once('e') {
        Some((m, e)) => (m.to_string(), e.parse::<i32>().unwrap_or(0)),
        None => (sci, 0),
    };
    let negative = mantissa.starts_with('-');
    let mantissa = mantissa.trim_start_matches('-');
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let ndigits = digits.len() as i32;
    // Decimal exponent: value = 0.digits * 10^dexp
    let dexp = exp + 1;
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if ndigits <= dexp && dexp <= 16 {
        out.push_str(digits);
        for _ in 0..(dexp - ndigits) {
            out.push('0');
        }
        out.push_str(".0");
    } else if 0 < dexp && dexp <= 16 {
        out.push_str(&digits[..dexp as usize]);
        out.push('.');
        out.push_str(&digits[dexp as usize..]);
    } else if -4 < dexp && dexp <= 0 {
        out.push_str("0.");
        for _ in 0..(-dexp) {
            out.push('0');
        }
        out.push_str(digits);
    } else {
        out.push_str(&digits[..1]);
        if ndigits > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let e = dexp - 1;
        out.push('e');
        out.push(if e < 0 { '-' } else { '+' });
        let abs = e.abs();
        if abs < 10 {
            out.push('0');
        }
        let _ = write!(out, "{abs}");
    }
    out
}

fn json_escape(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c if (c as u32) > 0x7e && (c as u32) < 0x10000 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c if (c as u32) >= 0x10000 => {
                let cp = c as u32 - 0x10000;
                let hi = 0xd800 + (cp >> 10);
                let lo = 0xdc00 + (cp & 0x3ff);
                let _ = write!(out, "\\u{:04x}\\u{:04x}", hi, lo);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn json_write(value: &PolicyValue, out: &mut String) {
    match value {
        PolicyValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        PolicyValue::Int(i) => {
            let _ = write!(out, "{i}");
        }
        PolicyValue::Float(f) => out.push_str(&python_float_repr(*f)),
        PolicyValue::Str(s) => json_escape(s, out),
        PolicyValue::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_write(item, out);
            }
            out.push(']');
        }
        PolicyValue::Table(table) => {
            out.push('{');
            for (i, (key, item)) in table.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_escape(key, out);
                out.push(':');
                json_write(item, out);
            }
            out.push('}');
        }
    }
}

/// `json.dumps(value, sort_keys=True, separators=(',', ':'))`.
pub fn json_python(value: &PolicyValue) -> String {
    let mut out = String::new();
    json_write(value, &mut out);
    out
}

/// Short content hash of a policy dict (deterministic, order-insensitive).
pub fn policy_hash(policy: &PolicyValue) -> String {
    let blob = json_python(policy);
    let digest = Sha256::digest(blob.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex.chars().take(16).collect()
}

/// Everything a caller needs from one policy load.
#[derive(Clone, Debug)]
pub struct LoadedPolicy {
    pub policy: PolicyValue,
    pub schema_version: i64,
    pub content_hash: String,
    pub user_override: bool,
    pub policy_error: Option<String>,
    pub ignored_keys: Vec<String>,
}

impl LoadedPolicy {
    pub fn get(&self, key: &str) -> Option<&PolicyValue> {
        self.policy.get(key)
    }

    pub fn gate_paths(&self) -> Vec<String> {
        let mut paths = Vec::new();
        if let Some(gates) = self.policy.get("gates").and_then(|g| g.as_table()) {
            paths.extend(gates.keys().map(|k| format!("gates.{k}")));
        }
        if let Some(tiers) = self.policy.get("tiers").and_then(|t| t.as_table()) {
            paths.extend(tiers.keys().map(|k| format!("tiers.{k}.gates")));
        }
        paths.extend(EXTRA_GATE_KEYS.iter().map(|s| (*s).to_string()));
        paths.extend(EXTRA_GATE_SECTIONS.iter().map(|s| (*s).to_string()));
        paths.sort();
        paths
    }

    /// The report's `policy` block (no filesystem paths).
    pub fn report_block(&self) -> PolicyValue {
        let mut block = BTreeMap::new();
        block.insert("schema_version".into(), PolicyValue::Int(self.schema_version));
        block.insert("content_hash".into(), PolicyValue::Str(self.content_hash.clone()));
        block.insert("user_override".into(), PolicyValue::Bool(self.user_override));
        block.insert(
            "policy_error".into(),
            match &self.policy_error {
                Some(e) => PolicyValue::Str(e.clone()),
                None => PolicyValue::Str(String::new()),
            },
        );
        block.insert(
            "ignored_keys".into(),
            PolicyValue::Array(
                self.ignored_keys
                    .iter()
                    .map(|k| PolicyValue::Str(k.clone()))
                    .collect(),
            ),
        );
        PolicyValue::Table(block)
    }

    /// One `--human` line, only when a user override or an error is present.
    pub fn human_line(&self) -> Option<String> {
        if let Some(e) = &self.policy_error {
            return Some(format!("policy: {e} (packaged default used)"));
        }
        if self.user_override {
            return Some(format!(
                "policy: user override applied (schema {}, {} ignored key(s))",
                self.schema_version,
                self.ignored_keys.len()
            ));
        }
        None
    }
}

fn table_of<'a>(
    value: &'a PolicyValue,
    section: &str,
) -> Result<&'a BTreeMap<String, PolicyValue>, PolicyError> {
    value
        .as_table()
        .ok_or_else(|| PolicyError(format!("policy: [{section}] must be a table")))
}

fn validate_threshold_table(name: &str, value: &PolicyValue) -> Result<(), PolicyError> {
    let table = value
        .as_table()
        .ok_or_else(|| PolicyError(format!("policy: [{name}] must be a table")))?;
    if table.is_empty() {
        return err(format!("policy: [{name}] must be a non-empty table"));
    }
    for (key, spec) in table {
        let spec = spec
            .as_table()
            .ok_or_else(|| PolicyError(format!("policy: [{name}.{key}] must be a table")))?;
        for field in ["mincomponentsize", "maxholesize"] {
            match spec.get(field) {
                Some(PolicyValue::Int(v)) if *v >= 0 => {}
                _ => {
                    return err(format!(
                        "policy: [{name}.{key}].{field} must be a non-negative integer"
                    ))
                }
            }
        }
        for extra in spec.keys() {
            if extra != "mincomponentsize" && extra != "maxholesize" {
                let mut sorted: Vec<&String> = spec
                    .keys()
                    .filter(|k| k.as_str() != "mincomponentsize" && k.as_str() != "maxholesize")
                    .collect();
                sorted.sort();
                let list = sorted
                    .iter()
                    .map(|k| format!("'{k}'"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return err(format!(
                    "policy: [{name}.{key}] unknown keys [{list}]"
                ));
            }
        }
    }
    Ok(())
}

fn validate_tiers(tiers: &PolicyValue) -> Result<(), PolicyError> {
    let Some(tiers) = tiers.as_table() else {
        return Ok(());
    };
    for (name, sec) in tiers {
        if let Some(gates) = sec.get("gates") {
            let gates = gates
                .as_table()
                .ok_or_else(|| PolicyError(format!("policy: [tiers.{name}.gates] must be a table")))?;
            for (key, value) in gates {
                if !value.is_number() {
                    return err(format!(
                        "policy: [tiers.{name}.gates].{key} must be a number"
                    ));
                }
            }
        }
        if let Some(v) = sec.get("default_enabled") {
            if !matches!(v, PolicyValue::Bool(_)) {
                return err(format!(
                    "policy: [tiers.{name}].default_enabled must be a boolean"
                ));
            }
        }
        if let Some(wbi) = sec.get("work_by_intensity") {
            let table = wbi.as_table().ok_or_else(|| {
                PolicyError(format!(
                    "policy: [tiers.{name}].work_by_intensity must map names to ints"
                ))
            })?;
            if table.is_empty() || table.values().any(|v| !v.is_int()) {
                return err(format!(
                    "policy: [tiers.{name}].work_by_intensity must map names to ints"
                ));
            }
        }
        for key in ["default_work", "res", "seam_ulp_k"] {
            if let Some(value) = sec.get(key) {
                match value.as_i64() {
                    Some(v) if v >= 0 => {}
                    _ => {
                        return err(format!(
                            "policy: [tiers.{name}].{key} must be a non-negative integer"
                        ))
                    }
                }
            }
        }
    }
    if let Some(flap) = tiers.get("flap") {
        if !flap.get("default_work").map(PolicyValue::is_int).unwrap_or(false) {
            return err("policy: [tiers.flap].default_work must be an integer".to_string());
        }
    }
    if let Some(tamp) = tiers.get("tamponade") {
        if !tamp
            .get("leak_max_faces")
            .map(PolicyValue::is_int)
            .unwrap_or(false)
        {
            return err("policy: [tiers.tamponade].leak_max_faces must be an integer".to_string());
        }
        let gates = tamp.get("gates");
        for key in ["shape_depth_cells", "shape_side_samples"] {
            let ok = gates
                .and_then(|g| g.get(key))
                .map(PolicyValue::is_int)
                .unwrap_or(false);
            if !ok {
                return err(format!(
                    "policy: [tiers.tamponade.gates].{key} must be an integer"
                ));
            }
        }
    }
    if let Some(dress) = tiers.get("dressing") {
        if !matches!(
            dress.get("default_enabled"),
            Some(PolicyValue::Bool(_))
        ) {
            return err("policy: [tiers.dressing].default_enabled must be a boolean".to_string());
        }
        let ok = dress
            .get("gates")
            .and_then(|g| g.get("max_parts_slack"))
            .map(PolicyValue::is_int)
            .unwrap_or(false);
        if !ok {
            return err(
                "policy: [tiers.dressing.gates].max_parts_slack must be an integer".to_string(),
            );
        }
    }
    Ok(())
}

/// Validate a policy document, mirroring the Python `_validate` messages.
pub fn validate(data: &PolicyValue) -> Result<(), PolicyError> {
    let root = data
        .as_table()
        .ok_or_else(|| PolicyError("policy: top level is not a table".to_string()))?;
    for section in REQUIRED {
        if !root.contains_key(*section) {
            return err(format!("policy: missing section [{section}]"));
        }
    }
    if !root["policy"]
        .get("schema_version")
        .map(PolicyValue::is_int)
        .unwrap_or(false)
    {
        return err("policy: [policy].schema_version must be an integer".to_string());
    }
    let order = table_of(&root["order"], "order")?;
    for key in ORDER_KEYS {
        if !order.contains_key(*key) {
            return err(format!("policy: missing [order].{key}"));
        }
    }
    let method_num = table_of(&order["deep_tier_method_num"], "order.deep_tier_method_num")?;
    let deep_order = order["deep_tier_order"]
        .as_array()
        .ok_or_else(|| PolicyError("policy: [order].deep_tier_order must be a list".into()))?;
    for tier in deep_order {
        let tier = tier
            .as_str()
            .ok_or_else(|| PolicyError("policy: [order].deep_tier_order must be strings".into()))?;
        if !method_num.contains_key(tier) {
            return err(format!(
                "policy: no method number for tier '{tier}'"
            ));
        }
    }
    let auto = table_of(&root["auto"], "auto")?;
    for key in AUTO_NUM_KEYS {
        if !auto.get(*key).map(PolicyValue::is_number).unwrap_or(false) {
            return err(format!("policy: [auto].{key} must be a number"));
        }
    }
    let si = table_of(&root["si_escalation"], "si_escalation")?;
    for key in SI_NUM_KEYS {
        if !si.get(*key).map(PolicyValue::is_number).unwrap_or(false) {
            return err(format!("policy: [si_escalation].{key} must be a number"));
        }
    }
    if !si.get("method_num").map(PolicyValue::is_int).unwrap_or(false) {
        return err("policy: [si_escalation].method_num must be an integer".to_string());
    }
    validate_tiers(root.get("tiers").unwrap_or(&PolicyValue::Table(BTreeMap::new())))?;

    let rank = table_of(&root["ranking"], "ranking")?;
    for key in [
        "preference_boost",
        "top_n",
        "learning_max_bonus",
        "learning_prior",
    ] {
        if !rank.get(key).map(PolicyValue::is_number).unwrap_or(false) {
            return err(format!("policy: [ranking].{key} must be a number"));
        }
    }
    if !rank.get("top_n").map(PolicyValue::is_int).unwrap_or(false) {
        return err("policy: [ranking].top_n must be an integer".to_string());
    }
    let tpl = table_of(&root["templates"], "templates")?;
    for key in TEMPLATE_NUM_KEYS {
        if !tpl.get(*key).map(PolicyValue::is_number).unwrap_or(false) {
            return err(format!("policy: [templates].{key} must be a number"));
        }
    }
    for key in TEMPLATE_INT_KEYS {
        if !tpl.get(*key).map(PolicyValue::is_int).unwrap_or(false) {
            return err(format!("policy: [templates].{key} must be an integer"));
        }
    }
    let pref = tpl.get("preferred").cloned().unwrap_or(PolicyValue::Table(BTreeMap::new()));
    let pref_table = table_of(&pref, "templates.preferred")?;
    for tid in TEMPLATE_IDS {
        let seq = pref_table
            .get(*tid)
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                PolicyError(format!(
                    "policy: [templates.preferred].{tid} must be a list of ints"
                ))
            })?;
        if seq.is_empty() || seq.iter().any(|v| !v.is_int()) {
            return err(format!(
                "policy: [templates.preferred].{tid} must be a list of ints"
            ));
        }
    }
    if let Some(gates) = root.get("gates") {
        if let Some(adoption) = gates.get("adoption") {
            let adoption = table_of(adoption, "gates.adoption")?;
            for key in [
                "face_ratio",
                "vertex_ratio",
                "min_slack",
                "loop_face_slack",
                "loop_vertex_slack",
                "ftetwild_abs_floor",
            ] {
                if !adoption.get(key).map(PolicyValue::is_number).unwrap_or(false) {
                    return err(format!("policy: [gates.adoption].{key} must be a number"));
                }
            }
            for key in [
                "min_slack",
                "loop_face_slack",
                "loop_vertex_slack",
                "ftetwild_abs_floor",
            ] {
                if !adoption.get(key).map(PolicyValue::is_int).unwrap_or(false) {
                    return err(format!("policy: [gates.adoption].{key} must be an integer"));
                }
            }
        }
        if let Some(tuning) = gates.get("tuning") {
            let tuning = table_of(tuning, "gates.tuning")?;
            for key in [
                "hausdorff_floor",
                "decimate_hd_margin",
                "ftetwild_max_hausdorff_rel",
            ] {
                if !tuning.get(key).map(PolicyValue::is_number).unwrap_or(false) {
                    return err(format!("policy: [gates.tuning].{key} must be a number"));
                }
            }
            if !tuning
                .get("hausdorff_floor")
                .map(PolicyValue::is_int)
                .unwrap_or(false)
            {
                return err(
                    "policy: [gates.tuning].hausdorff_floor must be an integer".to_string(),
                );
            }
        }
    }
    let sec = table_of(&root["intensity"], "intensity")?;
    let default = sec
        .get("default")
        .and_then(|v| v.as_str())
        .ok_or_else(|| PolicyError("policy: [intensity].default must name a preset".into()))?;
    let presets: Vec<&String> = sec.keys().filter(|k| k.as_str() != "default").collect();
    if !presets.iter().any(|k| k.as_str() == default) {
        return err("policy: [intensity].default must name a preset".to_string());
    }
    for (name, spec) in sec.iter().filter(|(k, _)| k.as_str() != "default") {
        let spec = spec
            .as_table()
            .ok_or_else(|| PolicyError(format!("policy: [intensity.{name}] must be a table")))?;
        for (key, value) in spec {
            if !INTENSITY_FIELDS.contains(&key.as_str()) {
                return err(format!(
                    "policy: [intensity.{name}].{key} is not a known field"
                ));
            }
            if INTENSITY_CAP.contains(&key.as_str()) {
                if value.as_str() == Some("none") {
                    continue;
                }
                match value.as_i64() {
                    Some(v) if v > 0 => {}
                    _ => {
                        return err(format!(
                            "policy: [intensity.{name}].{key} must be a positive integer or \"none\""
                        ))
                    }
                }
            } else if INTENSITY_BOOL.contains(&key.as_str()) {
                if !matches!(value, PolicyValue::Bool(_)) {
                    return err(format!(
                        "policy: [intensity.{name}].{key} must be a boolean"
                    ));
                }
            } else if INTENSITY_INT.contains(&key.as_str()) {
                match value.as_i64() {
                    Some(v) if v >= 0 => {}
                    _ => {
                        return err(format!(
                            "policy: [intensity.{name}].{key} must be a non-negative integer"
                        ))
                    }
                }
            } else if key == "ftetwild_timeout" || key == "graft_timeout" {
                match value.as_f64() {
                    Some(v) if v > 0.0 => {}
                    _ => {
                        return err(format!(
                            "policy: [intensity.{name}].{key} must be positive"
                        ))
                    }
                }
            } else if key == "deep_repair" {
                let ok = matches!(value.as_str(), Some("off") | Some("local") | Some("full"));
                if !ok {
                    return err(format!(
                        "policy: [intensity.{name}].deep_repair must be off/local/full"
                    ));
                }
            } else if key == "dense_target_ladder" {
                let ok = value
                    .as_array()
                    .map(|items| {
                        items.iter().all(|x| {
                            x.as_str() == Some("threshold")
                                || matches!(x, PolicyValue::Int(1..))
                                || matches!(x, PolicyValue::Float(f) if *f > 0.0)
                        })
                    })
                    .unwrap_or(false);
                if !ok {
                    return err(format!(
                        "policy: [intensity.{name}].dense_target_ladder must be positive numbers and/or \"threshold\""
                    ));
                }
            }
        }
    }
    for section in ["modes", "stage1_profiles", "stage1_types"] {
        validate_threshold_table(section, &root[section])?;
    }
    let tun = table_of(&root["stage1_tuning"], "stage1_tuning")?;
    for key in ["mech_gate", "org_gate"] {
        if !tun.get(key).map(PolicyValue::is_number).unwrap_or(false) {
            return err(format!("policy: [stage1_tuning].{key} must be a number"));
        }
    }
    Ok(())
}

fn gate_paths_for(data: &PolicyValue) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(gates) = data.get("gates").and_then(|g| g.as_table()) {
        paths.extend(gates.keys().map(|k| format!("gates.{k}")));
    }
    if let Some(tiers) = data.get("tiers").and_then(|t| t.as_table()) {
        paths.extend(tiers.keys().map(|k| format!("tiers.{k}.gates")));
    }
    paths.extend(EXTRA_GATE_KEYS.iter().map(|s| (*s).to_string()));
    paths.extend(EXTRA_GATE_SECTIONS.iter().map(|s| (*s).to_string()));
    paths
}

fn is_gate(dotted: &str, gates: &[String]) -> bool {
    gates
        .iter()
        .any(|gate| dotted == gate || dotted.starts_with(&format!("{gate}.")))
}

fn merge_known(
    base: &PolicyValue,
    override_value: &PolicyValue,
    prefix: &[String],
    ignored: &mut Vec<String>,
    gates: &[String],
) -> PolicyValue {
    let mut out = base.clone();
    let base_table = match base.as_table() {
        Some(t) => t,
        None => return out,
    };
    let Some(over_table) = override_value.as_table() else {
        return out;
    };
    let out_table = out.as_table_mut().expect("table clone");
    for (key, value) in over_table {
        let dotted = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{}.{key}", prefix.join("."))
        };
        if is_gate(&dotted, gates) || !base_table.contains_key(key) {
            ignored.push(dotted);
            continue;
        }
        if base_table[key].as_table().is_some() {
            if value.as_table().is_none() {
                ignored.push(dotted);
            } else {
                let mut next_prefix = prefix.to_vec();
                next_prefix.push(key.clone());
                out_table.insert(
                    key.clone(),
                    merge_known(&base_table[key], value, &next_prefix, ignored, gates),
                );
            }
        } else {
            out_table.insert(key.clone(), value.clone());
        }
    }
    out
}

/// Load the packaged policy and (once) merge the user override.
///
/// An explicit `path` loads only that file (tests/tooling; no user merge).
/// The user file is `$SUTURA_POLICY_USER` or `~/.config/sutura/policy.toml`;
/// a malformed user file falls back to the packaged default and records
/// `policy_error`.
pub fn load_policy(path: Option<&Path>) -> Result<LoadedPolicy, PolicyError> {
    let explicit = path.map(|p| p.to_path_buf());
    let source = match &explicit {
        Some(p) => p.to_path_buf(),
        None => std::env::var_os(POLICY_ENV)
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(default_policy_path),
    };
    let text = std::fs::read_to_string(&source).map_err(|e| {
        PolicyError(format!(
            "policy file {} unreadable: {e}",
            source.display()
        ))
    })?;
    let base = parse_toml(&text)?;
    validate(&base)?;
    if explicit.is_some() {
        return Ok(finish(base, false, None, Vec::new()));
    }

    let user_path = std::env::var_os(USER_POLICY_ENV)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(user_policy_path);
    if !user_path.exists() {
        return Ok(finish(base, false, None, Vec::new()));
    }
    // Python marks the override before reading, so a malformed user file still
    // reports `user_override = true` together with `policy_error`.
    let user_text = match std::fs::read_to_string(&user_path) {
        Ok(t) => t,
        Err(e) => {
            return Ok(finish(
                base,
                true,
                Some(format!("{}: {e}", "IoError")),
                Vec::new(),
            ))
        }
    };
    let user = match parse_toml(&user_text) {
        Ok(v) => v,
        Err(e) => {
            return Ok(finish(base, true, Some(e.0), Vec::new()));
        }
    };
    if user.as_table().is_none() {
        return Ok(finish(
            base,
            true,
            Some("top level is not a table".to_string()),
            Vec::new(),
        ));
    }
    let gates = gate_paths_for(&base);
    let mut ignored = Vec::new();
    let merged = merge_known(&base, &user, &[], &mut ignored, &gates);
    if let Err(e) = validate(&merged) {
        return Ok(finish(base, true, Some(e.0), Vec::new()));
    }
    Ok(finish(merged, true, None, ignored))
}

fn finish(
    policy: PolicyValue,
    user_override: bool,
    policy_error: Option<String>,
    ignored_keys: Vec<String>,
) -> LoadedPolicy {
    let schema_version = policy
        .get("policy")
        .and_then(|p| p.get("schema_version"))
        .and_then(|v| v.as_i64())
        .unwrap_or(SCHEMA_VERSION);
    LoadedPolicy {
        content_hash: policy_hash(&policy),
        policy,
        schema_version,
        user_override,
        policy_error,
        ignored_keys,
    }
}

/// `~/.config/sutura/policy.toml`.
pub fn user_policy_path() -> PathBuf {
    home_config_dir().join("policy.toml")
}

/// The packaged default as a filesystem path (for tests/tooling that need it).
pub fn default_policy_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("default_policy.toml")
}

fn home_config_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join("sutura");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("sutura")
}

/// The packaged default, validated.
pub fn default_policy() -> Result<LoadedPolicy, PolicyError> {
    let base = parse_toml(DEFAULT_POLICY_TOML)?;
    validate(&base)?;
    Ok(finish(base, false, None, Vec::new()))
}
