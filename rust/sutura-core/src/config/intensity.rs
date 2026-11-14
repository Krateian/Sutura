//! Intensity presets and user profiles.
//!
//! Mirrors the preset/profile half of `sutura_engine/triage.py`:
//! `IntensitySpec`, the four built-in presets from the policy `[intensity]`
//! table, `~/.config/sutura/profiles.json`, the field coercions, the
//! `HAUSDORFF_FLOOR` clamp and the resolution priority
//! **CLI > `SUTURA_INTENSITY` > `~/.config/sutura/config.json`**.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::policy::{LoadedPolicy, PolicyValue};

pub const INTENSITIES: [&str; 4] = ["quick", "balanced", "thorough", "extreme"];
pub const INTENSITY_ENV: &str = "SUTURA_INTENSITY";
pub const DEFAULT_INTENSITY: &str = "balanced";
pub const DEEP_REPAIR_MODES: [&str; 3] = ["off", "local", "full"];
pub const PROFILES_SCHEMA_VERSION: i64 = 1;
pub const HAUSDORFF_FLOOR: i64 = 200_000;

/// Fields a profile may override (the `IntensitySpec` fields minus identity).
pub const PROFILE_FIELDS: [&str; 10] = [
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
];

/// Resolved post-Stage-1 knobs for one intensity preset.
#[derive(Clone, Debug, PartialEq)]
pub struct IntensitySpec {
    pub name: String,
    pub ftetwild_enabled: bool,
    pub ftetwild_max_faces: Option<i64>,
    pub ftetwild_timeout: f64,
    pub graft_max_faces: Option<i64>,
    pub graft_timeout: f64,
    pub ftetwild_optimize_retry_on_dense_fail: bool,
    pub deep_repair: String,
    pub dense_target_ladder: Vec<LadderEntry>,
    pub dense_ratio: i64,
    pub dense_min_faces: i64,
    pub ftetwild_hausdorff_samples: i64,
    pub base: Option<String>,
    pub overrides: BTreeMap<String, PolicyValue>,
}

/// `LadderEntry = Union[float, str]` — a positive number or `"threshold"`.
#[derive(Clone, Debug, PartialEq)]
pub enum LadderEntry {
    Number(f64),
    Threshold,
}

impl Default for IntensitySpec {
    fn default() -> Self {
        Self {
            name: DEFAULT_INTENSITY.to_string(),
            ftetwild_enabled: true,
            ftetwild_max_faces: Some(300_000),
            ftetwild_timeout: 180.0,
            graft_max_faces: Some(2_000_000),
            graft_timeout: 300.0,
            ftetwild_optimize_retry_on_dense_fail: false,
            deep_repair: "full".to_string(),
            dense_target_ladder: vec![LadderEntry::Number(1.5), LadderEntry::Number(3.0)],
            dense_ratio: 4,
            dense_min_faces: 20_000,
            ftetwild_hausdorff_samples: 200_000,
            base: None,
            overrides: BTreeMap::new(),
        }
    }
}

/// Build one preset from the schema defaults plus the policy overrides for
/// `name` (a nullable cap uses the strict `"none"` sentinel).
pub fn preset_from_policy(name: &str, intensity: &PolicyValue) -> IntensitySpec {
    let mut spec = IntensitySpec {
        name: name.to_string(),
        ..IntensitySpec::default()
    };
    let Some(table) = intensity.get(name).and_then(|v| v.as_table()) else {
        return spec;
    };
    for (key, value) in table {
        apply_field(&mut spec, key, value, false);
    }
    spec
}

/// Apply one raw field to a spec, with the profile coercion rules.
/// Returns true when the value was accepted.
fn apply_field(spec: &mut IntensitySpec, field: &str, value: &PolicyValue, quiet: bool) -> bool {
    match coerce_field(field, value) {
        Coerced::Invalid => {
            if !quiet {
                eprintln!("sutura: warning: invalid value for {field} ignored");
            }
            false
        }
        Coerced::Field => {
            match field {
                "ftetwild_enabled" => {
                    spec.ftetwild_enabled = value.as_bool().unwrap_or(spec.ftetwild_enabled)
                }
                "ftetwild_max_faces" => spec.ftetwild_max_faces = max_faces(value),
                "ftetwild_timeout" => {
                    spec.ftetwild_timeout = value.as_f64().unwrap_or(spec.ftetwild_timeout)
                }
                "graft_max_faces" => spec.graft_max_faces = max_faces(value),
                "graft_timeout" => {
                    spec.graft_timeout = value.as_f64().unwrap_or(spec.graft_timeout)
                }
                "ftetwild_optimize_retry_on_dense_fail" => {
                    spec.ftetwild_optimize_retry_on_dense_fail =
                        value.as_bool().unwrap_or(spec.ftetwild_optimize_retry_on_dense_fail)
                }
                "deep_repair" => {
                    spec.deep_repair =
                        value.as_str().unwrap_or(&spec.deep_repair).to_string()
                }
                "dense_target_ladder" => {
                    spec.dense_target_ladder = coerce_ladder(value).unwrap_or_default()
                }
                "dense_ratio" => spec.dense_ratio = value.as_i64().unwrap_or(spec.dense_ratio),
                "dense_min_faces" => {
                    spec.dense_min_faces = value.as_i64().unwrap_or(spec.dense_min_faces)
                }
                "ftetwild_hausdorff_samples" => {
                    spec.ftetwild_hausdorff_samples =
                        value.as_i64().unwrap_or(spec.ftetwild_hausdorff_samples)
                }
                _ => return false,
            }
            true
        }
    }
}

fn max_faces(value: &PolicyValue) -> Option<i64> {
    match value {
        PolicyValue::Int(v) if *v > 0 => Some(*v),
        PolicyValue::Float(f) if *f > 0.0 => Some(*f as i64),
        _ => None,
    }
}

enum Coerced {
    Field,
    Invalid,
}

fn coerce_field(field: &str, value: &PolicyValue) -> Coerced {
    match field {
        "ftetwild_enabled" | "ftetwild_optimize_retry_on_dense_fail" => {
            if matches!(value, PolicyValue::Bool(_)) {
                Coerced::Field
            } else {
                Coerced::Invalid
            }
        }
        "ftetwild_max_faces" | "graft_max_faces" => {
            if value.as_str() == Some("none") || value.as_i64().map(|v| v > 0).unwrap_or(false) {
                Coerced::Field
            } else {
                Coerced::Invalid
            }
        }
        "ftetwild_timeout" | "graft_timeout" => {
            if value.as_f64().map(|v| v > 0.0).unwrap_or(false) {
                Coerced::Field
            } else {
                Coerced::Invalid
            }
        }
        "deep_repair" => {
            if value
                .as_str()
                .map(|s| DEEP_REPAIR_MODES.contains(&s))
                .unwrap_or(false)
            {
                Coerced::Field
            } else {
                Coerced::Invalid
            }
        }
        "dense_target_ladder" => {
            if coerce_ladder(value).is_some() {
                Coerced::Field
            } else {
                Coerced::Invalid
            }
        }
        "dense_ratio" => {
            if value.as_i64().map(|v| v >= 1).unwrap_or(false) {
                Coerced::Field
            } else {
                Coerced::Invalid
            }
        }
        "dense_min_faces" | "ftetwild_hausdorff_samples" => {
            if value.as_i64().map(|v| v >= 0).unwrap_or(false) {
                Coerced::Field
            } else {
                Coerced::Invalid
            }
        }
        _ => Coerced::Invalid,
    }
}

fn coerce_ladder(value: &PolicyValue) -> Option<Vec<LadderEntry>> {
    let items = value.as_array()?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            PolicyValue::Str(s) if s == "threshold" => out.push(LadderEntry::Threshold),
            PolicyValue::Int(v) if *v > 0 => out.push(LadderEntry::Number(*v as f64)),
            PolicyValue::Float(f) if *f > 0.0 => out.push(LadderEntry::Number(*f)),
            _ => return None,
        }
    }
    Some(out)
}

fn format_ladder(entries: &[LadderEntry]) -> String {
    if entries.is_empty() {
        return "-".to_string();
    }
    entries
        .iter()
        .map(|e| match e {
            LadderEntry::Threshold => "threshold".to_string(),
            LadderEntry::Number(v) => super::policy::python_float_repr(*v),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Normalized `{base, overrides}` entry, or `None` for a non-table value.
#[derive(Clone, Debug)]
pub struct ProfileEntry {
    pub base: String,
    pub overrides: BTreeMap<String, PolicyValue>,
}

pub fn normalize_entry(name: &str, raw: &Value) -> Option<ProfileEntry> {
    let obj = raw.as_object()?;
    let (base_raw, overrides_raw) = if obj.contains_key("overrides") {
        (obj.get("base"), obj.get("overrides"))
    } else {
        (
            obj.get("base"),
            Some(&Value::Object(
                obj.iter()
                    .filter(|(k, _)| k.as_str() != "base")
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Map<String, Value>>(),
            )),
        )
    };
    let base = match base_raw.and_then(|v| v.as_str()) {
        Some(b) if INTENSITIES.contains(&b) => b.to_string(),
        other => {
            if other.is_some() {
                eprintln!(
                    "sutura: warning: profile '{name}': unknown base preset {other:?}; using '{DEFAULT_INTENSITY}'"
                );
            }
            DEFAULT_INTENSITY.to_string()
        }
    };
    let overrides = match overrides_raw.and_then(|v| v.as_object()) {
        Some(map) => map
            .iter()
            .map(|(k, v)| (k.clone(), json_to_policy(v)))
            .collect(),
        None => {
            if overrides_raw.is_some() {
                eprintln!("sutura: warning: profile '{name}': 'overrides' is not an object; ignored");
            }
            BTreeMap::new()
        }
    };
    Some(ProfileEntry { base, overrides })
}

fn json_to_policy(value: &Value) -> PolicyValue {
    match value {
        Value::Bool(b) => PolicyValue::Bool(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => PolicyValue::Int(i),
            None => PolicyValue::Float(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => PolicyValue::Str(s.clone()),
        Value::Array(items) => PolicyValue::Array(items.iter().map(json_to_policy).collect()),
        Value::Object(map) => PolicyValue::Table(
            map.iter()
                .map(|(k, v)| (k.clone(), json_to_policy(v)))
                .collect(),
        ),
        Value::Null => PolicyValue::Str(String::new()),
    }
}

/// Build a profile spec on top of its base preset (`_build_spec`).
pub fn build_spec(
    presets: &BTreeMap<String, IntensitySpec>,
    name: &str,
    base: &str,
    overrides: &BTreeMap<String, PolicyValue>,
    quiet: bool,
) -> (IntensitySpec, BTreeMap<String, PolicyValue>) {
    let base_name = if presets.contains_key(base) {
        base
    } else {
        DEFAULT_INTENSITY
    };
    let mut spec = presets
        .get(base_name)
        .cloned()
        .unwrap_or_else(IntensitySpec::default);
    let mut applied: BTreeMap<String, PolicyValue> = BTreeMap::new();
    for (key, value) in overrides {
        if !PROFILE_FIELDS.contains(&key.as_str()) {
            if !quiet {
                eprintln!("sutura: warning: profile '{name}': unknown field '{key}' ignored");
            }
            continue;
        }
        if !apply_field(&mut spec, key, value, true) {
            if !quiet {
                eprintln!("sutura: warning: profile '{name}': invalid value for {key} ignored");
            }
            continue;
        }
        applied.insert(key.clone(), value.clone());
    }
    if let Some(PolicyValue::Int(samples)) = applied.get("ftetwild_hausdorff_samples") {
        if *samples < HAUSDORFF_FLOOR {
            if !quiet {
                eprintln!(
                    "sutura: warning: profile '{name}': ftetwild_hausdorff_samples {samples} is below the {HAUSDORFF_FLOOR} floor; clamped"
                );
            }
            spec.ftetwild_hausdorff_samples = HAUSDORFF_FLOOR;
            applied.insert(
                "ftetwild_hausdorff_samples".to_string(),
                PolicyValue::Int(HAUSDORFF_FLOOR),
            );
        }
    }
    spec.name = name.to_string();
    spec.base = Some(base_name.to_string());
    spec.overrides = applied.clone();
    (spec, applied)
}

/// `name_error`: `empty`, `reserved`, `duplicate` or `None`.
pub fn name_error(name: &str, profiles: &BTreeMap<String, ProfileEntry>, exclude: Option<&str>) -> Option<&'static str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Some("empty");
    }
    if INTENSITIES.contains(&trimmed.to_lowercase().as_str()) {
        return Some("reserved");
    }
    for existing in profiles.keys() {
        if Some(existing.as_str()) == exclude {
            continue;
        }
        if existing.to_lowercase() == trimmed.to_lowercase() {
            return Some("duplicate");
        }
    }
    None
}

pub fn unique_profile_name(profiles: &BTreeMap<String, ProfileEntry>, base: &str) -> String {
    let base = {
        let trimmed = base.trim();
        if trimmed.is_empty() {
            "profile"
        } else {
            trimmed
        }
    };
    if name_error(base, profiles, None).is_none() {
        return base.to_string();
    }
    let mut index = 2;
    loop {
        let candidate = format!("{base} {index}");
        if name_error(&candidate, profiles, None).is_none() {
            return candidate;
        }
        index += 1;
    }
}

/// `~/.config/sutura/profiles.json`
pub fn profiles_path() -> PathBuf {
    home_config_dir().join("profiles.json")
}

fn home_config_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join("sutura");
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("sutura")
}

/// Load user profiles; a missing, malformed or wrong-schema file yields `{}`.
pub fn load_profiles(path: Option<&Path>) -> BTreeMap<String, ProfileEntry> {
    let target = path.map(|p| p.to_path_buf()).unwrap_or_else(profiles_path);
    let text = match std::fs::read_to_string(&target) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(e) => {
            eprintln!(
                "sutura: warning: cannot read intensity profiles from {} ({e}); using built-in presets only",
                target.display()
            );
            return BTreeMap::new();
        }
    };
    let data: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "sutura: warning: cannot read intensity profiles from {} ({e}); using built-in presets only",
                target.display()
            );
            return BTreeMap::new();
        }
    };
    let Some(obj) = data.as_object() else {
        eprintln!(
            "sutura: warning: intensity profiles file {} is not an object; using built-in presets only",
            target.display()
        );
        return BTreeMap::new();
    };
    let version = obj
        .get("schema_version")
        .or_else(|| obj.get("version"))
        .and_then(|v| v.as_i64());
    if version != Some(PROFILES_SCHEMA_VERSION) {
        eprintln!(
            "sutura: warning: intensity profiles file {} has schema_version {:?} (expected {}); using built-in presets only",
            target.display(),
            version,
            PROFILES_SCHEMA_VERSION
        );
        return BTreeMap::new();
    }
    let Some(raw) = obj.get("profiles").and_then(|v| v.as_object()) else {
        eprintln!(
            "sutura: warning: intensity profiles file {} has no \"profiles\" object; using built-in presets only",
            target.display()
        );
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for (name, entry) in raw {
        if name.trim().is_empty() {
            eprintln!("sutura: warning: ignoring a profile with an invalid name");
            continue;
        }
        let lower = name.to_lowercase();
        if let Some(first) = seen.get(&lower) {
            eprintln!(
                "sutura: warning: ignoring duplicate profile name '{name}' (case-insensitive clash with '{first}')"
            );
            continue;
        }
        if INTENSITIES.contains(&lower.as_str()) {
            eprintln!("sutura: warning: ignoring profile '{name}': it shadows a built-in preset");
            continue;
        }
        seen.insert(lower, name.clone());
        if let Some(normalized) = normalize_entry(name, entry) {
            out.insert(name.clone(), normalized);
        }
    }
    out
}

/// Write user profiles atomically (`save_profiles`).
pub fn save_profiles(
    profiles: &BTreeMap<String, ProfileEntry>,
    path: Option<&Path>,
) -> Result<(), String> {
    let target = path.map(|p| p.to_path_buf()).unwrap_or_else(profiles_path);
    let directory = target.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let mut body = Map::new();
    body.insert(
        "schema_version".to_string(),
        Value::from(PROFILES_SCHEMA_VERSION),
    );
    let mut out_profiles = Map::new();
    for (name, entry) in profiles {
        let mut item = Map::new();
        item.insert("base".to_string(), Value::from(entry.base.clone()));
        let mut over = Map::new();
        for (k, v) in &entry.overrides {
            over.insert(k.clone(), policy_to_json(v));
        }
        item.insert("overrides".to_string(), Value::Object(over));
        out_profiles.insert(name.clone(), Value::Object(item));
    }
    body.insert("profiles".to_string(), Value::Object(out_profiles));
    let text = serde_json::to_string_pretty(&Value::Object(body)).map_err(|e| e.to_string())?;
    let tmp = directory.join(format!(".profiles-{}.tmp", std::process::id()));
    std::fs::write(&tmp, text.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &target).map_err(|e| e.to_string())
}

fn policy_to_json(value: &PolicyValue) -> Value {
    match value {
        PolicyValue::Bool(b) => Value::Bool(*b),
        PolicyValue::Int(i) => Value::from(*i),
        PolicyValue::Float(f) => Value::from(*f),
        PolicyValue::Str(s) => Value::from(s.clone()),
        PolicyValue::Array(items) => Value::Array(items.iter().map(policy_to_json).collect()),
        PolicyValue::Table(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), policy_to_json(v)))
                .collect(),
        ),
    }
}

/// The four built-in presets from the loaded policy.
pub fn presets(loaded: &LoadedPolicy) -> BTreeMap<String, IntensitySpec> {
    let intensity = loaded.get("intensity").cloned().unwrap_or(PolicyValue::Table(BTreeMap::new()));
    INTENSITIES
        .iter()
        .map(|name| (name.to_string(), preset_from_policy(name, &intensity)))
        .collect()
}

/// `IntensitySpec` for a preset or user profile, or `None`.
pub fn effective_spec(
    loaded: &LoadedPolicy,
    name: &str,
    profiles: Option<&BTreeMap<String, ProfileEntry>>,
) -> Option<IntensitySpec> {
    let presets = presets(loaded);
    if let Some(spec) = presets.get(name) {
        return Some(spec.clone());
    }
    let owned = profiles.is_none();
    let loaded_profiles = profiles.cloned().unwrap_or_else(|| load_profiles(None));
    let entry = loaded_profiles.get(name)?;
    let _ = owned;
    Some(build_spec(&presets, name, &entry.base, &entry.overrides, false).0)
}

/// `(spec, applied)` for one name, prior-free (used by the resolvers).
fn spec_for(
    presets: &BTreeMap<String, IntensitySpec>,
    value: &str,
    profiles: &BTreeMap<String, ProfileEntry>,
) -> Option<(IntensitySpec, BTreeMap<String, PolicyValue>)> {
    if let Some(spec) = presets.get(value) {
        return Some((spec.clone(), BTreeMap::new()));
    }
    let entry = profiles.get(value)?;
    Some(build_spec(presets, value, &entry.base, &entry.overrides, false))
}

/// `(cli, env, config.json)` in that priority order.
pub fn resolve_intensity_detail(
    loaded: &LoadedPolicy,
    cli_value: Option<&str>,
    environ: Option<&BTreeMap<String, String>>,
    config_path: Option<&Path>,
    user_profiles: Option<&BTreeMap<String, ProfileEntry>>,
    profiles_path_override: Option<&Path>,
) -> (IntensitySpec, BTreeMap<String, PolicyValue>) {
    let presets = presets(loaded);
    let profiles = match user_profiles {
        Some(p) => p.clone(),
        None => load_profiles(profiles_path_override),
    };
    let config_value = read_config_intensity(config_path);
    let env_value = match environ {
        Some(map) => map.get(INTENSITY_ENV).cloned(),
        None => std::env::var(INTENSITY_ENV).ok(),
    };
    for value in [cli_value.map(|s| s.to_string()), env_value, config_value]
        .into_iter()
        .flatten()
    {
        if let Some(hit) = spec_for(&presets, &value, &profiles) {
            return hit;
        }
    }
    let default = loaded
        .get("intensity")
        .and_then(|i| i.get("default"))
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_INTENSITY);
    let spec = presets
        .get(default)
        .cloned()
        .unwrap_or_else(IntensitySpec::default);
    (spec, BTreeMap::new())
}

/// `resolve_intensity`: the spec only.
pub fn resolve_intensity(
    loaded: &LoadedPolicy,
    cli_value: Option<&str>,
    environ: Option<&BTreeMap<String, String>>,
    config_path: Option<&Path>,
    user_profiles: Option<&BTreeMap<String, ProfileEntry>>,
    profiles_path_override: Option<&Path>,
) -> IntensitySpec {
    resolve_intensity_detail(
        loaded,
        cli_value,
        environ,
        config_path,
        user_profiles,
        profiles_path_override,
    )
    .0
}

/// `~/.config/sutura/config.json` → `intensity`.
pub fn config_intensity_path() -> PathBuf {
    home_config_dir().join("config.json")
}

fn read_config_intensity(path: Option<&Path>) -> Option<String> {
    let target = path.map(|p| p.to_path_buf()).unwrap_or_else(config_intensity_path);
    let text = std::fs::read_to_string(target).ok()?;
    let data: Value = serde_json::from_str(&text).ok()?;
    data.get("intensity")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Report fields for one spec (`triage_report_fields`).
pub fn triage_report_fields(spec: Option<&IntensitySpec>) -> PolicyValue {
    let mut out = BTreeMap::new();
    match spec {
        None => {
            out.insert(
                "triage_intensity".into(),
                PolicyValue::Str(DEFAULT_INTENSITY.to_string()),
            );
        }
        Some(spec) => {
            out.insert(
                "triage_intensity".into(),
                PolicyValue::Str(spec.name.clone()),
            );
            if let Some(base) = &spec.base {
                out.insert("triage_profile_base".into(), PolicyValue::Str(base.clone()));
                out.insert(
                    "triage_overrides".into(),
                    PolicyValue::Table(spec.overrides.clone()),
                );
            }
        }
    }
    PolicyValue::Table(out)
}

/// One formatted row for `--list-intensities`.
pub fn format_row(name: &str, spec: &IntensitySpec) -> String {
    let cap = match spec.ftetwild_max_faces {
        None => "no-limit".to_string(),
        Some(v) => v.to_string(),
    };
    let ftetwild = if spec.ftetwild_enabled {
        format!(
            "on(cap={cap},timeout={}s,optretry={})",
            format_g(spec.ftetwild_timeout),
            if spec.ftetwild_optimize_retry_on_dense_fail {
                "yes"
            } else {
                "no"
            }
        )
    } else {
        "off".to_string()
    };
    let ladder = format_ladder(&spec.dense_target_ladder);
    let gcap = match spec.graft_max_faces {
        None => "no-limit".to_string(),
        Some(v) => v.to_string(),
    };
    let graft = format!("graft(cap={gcap},timeout={}s)", format_g(spec.graft_timeout));
    let base = match &spec.base {
        Some(b) => format!(" [base={b}]"),
        None => String::new(),
    };
    let mut out = String::new();
    let _ = write!(
        out,
        "{:<20} ftetwild={ftetwild} deep_repair={} ladder={ladder} ratio={} min_faces={} hausdorff={} {graft}{base}",
        name,
        spec.deep_repair,
        spec.dense_ratio,
        spec.dense_min_faces,
        spec.ftetwild_hausdorff_samples
    );
    out
}

/// Python's `%g` for the timeouts used in the listing.
fn format_g(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        let mut s = format!("{value}");
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        s
    }
}

/// `list_intensities`: preset rows then profile rows.
pub fn list_intensities(
    loaded: &LoadedPolicy,
    profiles: Option<&BTreeMap<String, ProfileEntry>>,
) -> Vec<(String, &'static str, Option<String>, IntensitySpec)> {
    let presets = presets(loaded);
    let mut rows: Vec<(String, &'static str, Option<String>, IntensitySpec)> = INTENSITIES
        .iter()
        .map(|name| {
            (
                name.to_string(),
                "preset",
                None,
                presets.get(*name).cloned().unwrap_or_default(),
            )
        })
        .collect();
    let loaded_profiles = match profiles {
        Some(p) => p.clone(),
        None => load_profiles(None),
    };
    for (name, entry) in &loaded_profiles {
        let (spec, _) = build_spec(&presets, name, &entry.base, &entry.overrides, false);
        rows.push((name.clone(), "profile", Some(entry.base.clone()), spec));
    }
    rows
}

/// `format_intensities`: the `--list-intensities` text.
pub fn format_intensities(
    loaded: &LoadedPolicy,
    profiles: Option<&BTreeMap<String, ProfileEntry>>,
) -> String {
    let rows = list_intensities(loaded, profiles);
    let mut lines = vec!["Intensity presets (built-in, read-only):".to_string()];
    for (name, kind, _base, spec) in &rows {
        if *kind == "preset" {
            lines.push(format!("  {}", format_row(name, spec)));
        }
    }
    lines.push("User profiles (base + effective values):".to_string());
    let profile_rows: Vec<_> = rows.iter().filter(|r| r.1 == "profile").collect();
    if profile_rows.is_empty() {
        lines.push("  (none)".to_string());
    }
    for (name, _kind, _base, spec) in profile_rows {
        lines.push(format!("  {}", format_row(name, spec)));
    }
    lines.join("\n")
}

/// `profile_overrides`: fields whose value differs from the base spec.
pub fn profile_overrides(
    base_spec: &IntensitySpec,
    values: &BTreeMap<String, PolicyValue>,
) -> BTreeMap<String, PolicyValue> {
    let mut out = BTreeMap::new();
    for (field, value) in values {
        if !PROFILE_FIELDS.contains(&field.as_str()) {
            continue;
        }
        let mut probe = base_spec.clone();
        if !apply_field(&mut probe, field, value, true) {
            continue;
        }
        // Compare the effective value, like `new != getattr(base_spec, field)`.
        let changed = match field.as_str() {
            "ftetwild_enabled" => probe.ftetwild_enabled != base_spec.ftetwild_enabled,
            "ftetwild_max_faces" => probe.ftetwild_max_faces != base_spec.ftetwild_max_faces,
            "ftetwild_timeout" => probe.ftetwild_timeout != base_spec.ftetwild_timeout,
            "graft_max_faces" => probe.graft_max_faces != base_spec.graft_max_faces,
            "graft_timeout" => probe.graft_timeout != base_spec.graft_timeout,
            "ftetwild_optimize_retry_on_dense_fail" => {
                probe.ftetwild_optimize_retry_on_dense_fail
                    != base_spec.ftetwild_optimize_retry_on_dense_fail
            }
            "deep_repair" => probe.deep_repair != base_spec.deep_repair,
            "dense_target_ladder" => probe.dense_target_ladder != base_spec.dense_target_ladder,
            "dense_ratio" => probe.dense_ratio != base_spec.dense_ratio,
            "dense_min_faces" => probe.dense_min_faces != base_spec.dense_min_faces,
            "ftetwild_hausdorff_samples" => {
                probe.ftetwild_hausdorff_samples != base_spec.ftetwild_hausdorff_samples
            }
            _ => false,
        };
        if changed {
            out.insert(field.clone(), value.clone());
        }
    }
    out
}
