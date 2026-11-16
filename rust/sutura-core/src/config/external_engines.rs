// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! External third-party engine configuration, read from
//! `${XDG_CONFIG_HOME:-~/.config}/sutura/engines/*.toml`.
//!
//! Port of `sutura_engine/adapters/external_adapter.py`'s loader half, living
//! beside `policy` and `intensity` as the same kind of validated config reader.
//! Sutura never downloads engines: the user installs and manages the binaries,
//! and only the post-run watertight/Hausdorff guards decide whether a result is
//! kept.
//!
//! Validation mirrors Python message-for-message -- a malformed file is skipped
//! with a warning, never a crash, and the warning text is the same so the
//! `--analyze` payload matches.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `BUILTIN_STAGES`.
pub const BUILTIN_STAGES: [&str; 4] = ["stage1", "stage2", "deep_repair", "ftetwild"];
/// `ALLOWED_FORMATS`.
pub const ALLOWED_FORMATS: [&str; 3] = ["stl", "obj", "ply"];
/// `ALLOWED_PLACEMENTS`.
pub const ALLOWED_PLACEMENTS: [&str; 5] = [
    "before_stage1",
    "after_stage1",
    "after_stage2",
    "replace_ftetwild",
    "final_fallback",
];
/// `ALLOWED_PLACEHOLDERS`.
pub const ALLOWED_PLACEHOLDERS: [&str; 3] = ["input", "output", "tmpdir"];

/// One validated engine configuration.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub name: String,
    pub command: Vec<String>,
    pub input_format: String,
    pub output_format: String,
    pub timeout: f64,
    pub placement: String,
    pub enabled: bool,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
    pub source_path: Option<PathBuf>,
}

/// `get_engines_dir`: `$XDG_CONFIG_HOME/sutura/engines` or `~/.config/...`.
pub fn engines_dir() -> PathBuf {
    let base = match std::env::var("XDG_CONFIG_HOME") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        }
    };
    base.join("sutura").join("engines")
}

/// `resolve_executable`: absolute/relative path or `which`.
///
/// A path containing a separator is expanded and must be an executable FILE;
/// the result is `abspath` WITHOUT resolving symlinks, because resolving a
/// virtualenv's `bin/python` would run the base interpreter and lose the venv's
/// site-packages.
pub fn resolve_executable(cmd0: &str) -> Option<PathBuf> {
    if cmd0.is_empty() {
        return None;
    }
    if cmd0.contains(std::path::MAIN_SEPARATOR) || cmd0.contains('/') {
        let expanded = expand_home(cmd0);
        let p = Path::new(&expanded);
        if p.is_file() && is_executable(p) {
            // `os.path.abspath`, NOT `canonicalize`: resolving symlinks would
            // turn a virtualenv's bin/python into the base interpreter and lose
            // the venv's site-packages.
            return Some(abspath(p));
        }
        return None;
    }
    which(cmd0)
}

/// `os.path.abspath`: make absolute and collapse `.`/`..` WITHOUT resolving
/// symlinks.
fn abspath(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut out = PathBuf::new();
    for comp in abs.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_default();
        return format!("{}/{}", home, rest);
    }
    path.to_string()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Minimal `shutil.which`: search `PATH` for an executable file.
fn which(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() && is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// `is_engine_available`.
pub fn is_engine_available(engine: &EngineConfig) -> bool {
    match engine.command.first() {
        Some(cmd0) => resolve_executable(cmd0).is_some(),
        None => false,
    }
}

/// `engine.to_json()` as `external_engines` reports it.
///
/// Deliberately only these five fields: the payload reports what a user needs to
/// see, and the env/cwd/timeout belong to the runner, not the report.
pub fn engine_json(engine: &EngineConfig) -> serde_json::Value {
    serde_json::json!({
        "name": engine.name,
        "placement": engine.placement,
        "enabled": engine.enabled,
        "available": is_engine_available(engine),
        "command": engine.command,
    })
}

/// `load_engine_file`: parse and validate one TOML file.
///
/// Returns `Ok(None)` when the root is not a table (Python returns a message
/// for that case too, so it is `Err` there -- see the caller).
pub fn load_engine_file(path: &Path) -> Result<EngineConfig, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("TOML parse error: {}", e))?;
    let data: toml::Value = text
        .parse::<toml::Value>()
        .map_err(|e| format!("TOML parse error: {}", e))?;
    let table = match data.as_table() {
        Some(t) => t,
        None => return Err("Root element of TOML must be a table/dictionary".into()),
    };

    // name
    let name = match table.get("name").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return Err("Missing or empty 'name' field".into()),
    };

    // command
    let cmd_val = match table.get("command").and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => a,
        _ => return Err("Field 'command' must be a non-empty list of argument strings".into()),
    };
    let mut command = Vec::with_capacity(cmd_val.len());
    for (i, arg) in cmd_val.iter().enumerate() {
        match arg.as_str() {
            Some(s) => command.push(s.to_string()),
            None => {
                return Err(format!(
                    "Command argument at index {} must be a string, got {}",
                    i,
                    toml_type_name(arg)
                ))
            }
        }
    }

    // placeholders
    let joined = command.join(" ");
    let found = placeholders(&joined);
    let unknown: Vec<&String> = found
        .iter()
        .filter(|p| !ALLOWED_PLACEHOLDERS.contains(&p.as_str()))
        .collect();
    if !unknown.is_empty() {
        let mut names: Vec<&str> = unknown.iter().map(|s| s.as_str()).collect();
        names.sort_unstable();
        let mut allowed = ALLOWED_PLACEHOLDERS.to_vec();
        allowed.sort_unstable();
        return Err(format!(
            "Unknown placeholder(s) in command: {}. Allowed placeholders: {}",
            names.join(", "),
            allowed.join(", ")
        ));
    }
    if !found.contains(&"input".to_string()) {
        return Err("Command template must contain '{input}' placeholder".into());
    }
    if !found.contains(&"output".to_string()) {
        return Err("Command template must contain '{output}' placeholder".into());
    }

    // formats
    let in_fmt = match table.get("input_format") {
        None => "stl".to_string(),
        Some(v) => match v.as_str() {
            Some(s) if ALLOWED_FORMATS.contains(&s.to_lowercase().as_str()) => s.to_lowercase(),
            _ => {
                return Err(format!(
                    "Invalid 'input_format': {}. Allowed: {}",
                    toml_repr(table.get("input_format")),
                    ALLOWED_FORMATS.join(", ")
                ))
            }
        },
    };
    let out_fmt = match table.get("output_format") {
        None => "stl".to_string(),
        Some(v) => match v.as_str() {
            Some(s) if ALLOWED_FORMATS.contains(&s.to_lowercase().as_str()) => s.to_lowercase(),
            _ => {
                return Err(format!(
                    "Invalid 'output_format': {}. Allowed: {}",
                    toml_repr(table.get("output_format")),
                    ALLOWED_FORMATS.join(", ")
                ))
            }
        },
    };

    // timeout
    let timeout = match table.get("timeout") {
        None => 120.0,
        Some(v) => {
            let num = v.as_float().or_else(|| v.as_integer().map(|i| i as f64));
            match num {
                Some(t) if t > 0.0 => t,
                _ => {
                    return Err(format!(
                        "Invalid 'timeout': {}. Must be a positive number of seconds",
                        toml_repr(table.get("timeout"))
                    ))
                }
            }
        }
    };

    // placement
    let placement = match table.get("placement") {
        None => "after_stage1".to_string(),
        Some(v) => match v.as_str() {
            Some(s) if ALLOWED_PLACEMENTS.contains(&s) => s.to_string(),
            _ => {
                return Err(format!(
                    "Invalid 'placement': {}. Allowed: {}",
                    toml_repr(table.get("placement")),
                    ALLOWED_PLACEMENTS.join(", ")
                ))
            }
        },
    };

    // enabled
    let enabled = match table.get("enabled") {
        None => true,
        Some(v) => match v.as_bool() {
            Some(b) => b,
            None => {
                return Err(format!(
                    "Field 'enabled' must be boolean, got {}",
                    toml_type_name(v)
                ))
            }
        },
    };

    // env
    let mut env = BTreeMap::new();
    if let Some(v) = table.get("env") {
        let env_table = match v.as_table() {
            Some(t) => t,
            None => {
                return Err(format!(
                    "Field 'env' must be a dictionary/table, got {}",
                    toml_type_name(v)
                ))
            }
        };
        for (k, val) in env_table {
            match val.as_str() {
                Some(s) => {
                    env.insert(k.clone(), s.to_string());
                }
                None => {
                    return Err(format!(
                        "All env keys and values must be strings, got '{}'={}",
                        k,
                        toml_repr(Some(val))
                    ))
                }
            }
        }
    }

    // cwd
    let cwd = match table.get("cwd") {
        None => None,
        Some(v) => match v.as_str() {
            Some(s) => Some(s.to_string()),
            None => {
                return Err(format!(
                    "Field 'cwd' must be a string or null, got {}",
                    toml_type_name(v)
                ))
            }
        },
    };

    Ok(EngineConfig {
        name,
        command,
        input_format: in_fmt,
        output_format: out_fmt,
        timeout,
        placement,
        enabled,
        env,
        cwd,
        source_path: Some(path.to_path_buf()),
    })
}

/// Every `{...}` name in the command template, in first-seen order.
fn placeholders(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(end) = text[i + 1..].find('}') {
                let name = &text[i + 1..i + 1 + end];
                if !name.contains('{') {
                    out.push(name.to_string());
                    i += end + 2;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

fn toml_type_name(v: &toml::Value) -> &'static str {
    match v {
        toml::Value::String(_) => "str",
        toml::Value::Integer(_) => "int",
        toml::Value::Float(_) => "float",
        toml::Value::Boolean(_) => "bool",
        toml::Value::Datetime(_) => "datetime",
        toml::Value::Array(_) => "list",
        toml::Value::Table(_) => "dict",
    }
}

/// Python's `repr()` of a TOML scalar, used in the validation messages.
fn toml_repr(v: Option<&toml::Value>) -> String {
    match v {
        None => "None".to_string(),
        Some(toml::Value::String(s)) => format!("'{}'", s),
        Some(toml::Value::Integer(i)) => i.to_string(),
        Some(toml::Value::Float(f)) => f.to_string(),
        Some(toml::Value::Boolean(b)) => {
            if *b {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        Some(other) => other.to_string(),
    }
}

/// `load_all_engines`: discover and load every `*.toml` except `chain.toml`.
///
/// Bad files and duplicate names are skipped with a warning, never fatal.
pub fn load_all_engines(config_dir: Option<&Path>) -> (BTreeMap<String, EngineConfig>, Vec<String>) {
    let dir = match config_dir {
        Some(d) => d.to_path_buf(),
        None => engines_dir(),
    };
    let mut engines = BTreeMap::new();
    let mut warnings = Vec::new();
    if !dir.is_dir() {
        return (engines, warnings);
    }
    let mut paths: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "toml").unwrap_or(false))
            .collect(),
        Err(_) => return (engines, warnings),
    };
    paths.sort();
    for path in paths {
        if path.file_name().map(|n| n == "chain.toml").unwrap_or(false) {
            continue;
        }
        match load_engine_file(&path) {
            Err(err) => warnings.push(format!(
                "Failed to load engine from {}: {}",
                path.display(),
                err
            )),
            Ok(cfg) => {
                if let Some(prev) = engines.get(&cfg.name) {
                    warnings.push(format!(
                        "Duplicate engine name '{}' in {} (already defined in {}); skipping.",
                        cfg.name,
                        path.display(),
                        prev.source_path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "None".to_string())
                    ));
                    continue;
                }
                engines.insert(cfg.name.clone(), cfg);
            }
        }
    }
    (engines, warnings)
}

/// `external_engines()`: the `--analyze` payload block.
///
/// Reported separately from the ranking, sorted by name, and empty when the
/// config dir is missing or unreadable.
pub fn external_engines() -> serde_json::Value {
    let (engines, _warnings) = load_all_engines(None);
    let out: Vec<serde_json::Value> = engines.values().map(engine_json).collect();
    serde_json::Value::Array(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).expect("write");
        p
    }

    #[test]
    fn missing_dir_is_empty_not_an_error() {
        let (engines, warnings) = load_all_engines(Some(Path::new("/nonexistent/dir")));
        assert!(engines.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn a_valid_file_loads_with_defaults() {
        let tmp = std::env::temp_dir().join(format!("sutura-eng-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("mkdir");
        write(
            &tmp,
            "a.toml",
            "name = \"cleaner\"\ncommand = [\"mybin\", \"{input}\", \"{output}\"]\n",
        );
        let (engines, warnings) = load_all_engines(Some(&tmp));
        assert!(warnings.is_empty(), "{warnings:?}");
        let cfg = engines.get("cleaner").expect("engine");
        assert_eq!(cfg.input_format, "stl");
        assert_eq!(cfg.output_format, "stl");
        assert_eq!(cfg.timeout, 120.0);
        assert_eq!(cfg.placement, "after_stage1");
        assert!(cfg.enabled);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_placeholders_are_rejected() {
        let tmp = std::env::temp_dir().join(format!("sutura-eng2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("mkdir");
        let p = write(&tmp, "bad.toml", "name = \"x\"\ncommand = [\"bin\"]\n");
        let err = load_engine_file(&p).expect_err("must reject");
        assert!(err.contains("{input}"), "{err}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn unknown_placeholder_is_rejected_with_a_sorted_list() {
        let tmp = std::env::temp_dir().join(format!("sutura-eng3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("mkdir");
        let p = write(
            &tmp,
            "bad.toml",
            "name = \"x\"\ncommand = [\"b\", \"{input}\", \"{output}\", \"{zzz}\"]\n",
        );
        let err = load_engine_file(&p).expect_err("must reject");
        assert!(err.contains("zzz"), "{err}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn chain_toml_is_skipped() {
        let tmp = std::env::temp_dir().join(format!("sutura-eng4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("mkdir");
        write(&tmp, "chain.toml", "stages = [\"stage1\"]\n");
        let (engines, warnings) = load_all_engines(Some(&tmp));
        assert!(engines.is_empty());
        assert!(warnings.is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn duplicate_names_keep_the_first_and_warn() {
        let tmp = std::env::temp_dir().join(format!("sutura-eng5-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("mkdir");
        let body = "name = \"dup\"\ncommand = [\"b\", \"{input}\", \"{output}\"]\n";
        write(&tmp, "a.toml", body);
        write(&tmp, "b.toml", body);
        let (engines, warnings) = load_all_engines(Some(&tmp));
        assert_eq!(engines.len(), 1);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("Duplicate engine name"), "{warnings:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn placeholders_are_found_in_order() {
        assert_eq!(
            placeholders("a {input} b {output} c {tmpdir}"),
            vec!["input", "output", "tmpdir"]
        );
        assert!(placeholders("no braces here").is_empty());
    }

    /// Golden fixture also checked against Python's `external_engines()`: the
    /// reported JSON is exactly these five fields, with `available` from
    /// actually resolving the command.
    #[test]
    fn engine_json_reports_the_five_fields() {
        let tmp = std::env::temp_dir().join(format!("sutura-eng6-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("mkdir");
        write(
            &tmp,
            "probe.toml",
            "name = \"probe\"\ncommand = [\"/bin/echo\", \"{input}\", \"{output}\"]\nplacement = \"after_stage2\"\ntimeout = 30.0\n",
        );
        let (engines, warnings) = load_all_engines(Some(&tmp));
        assert!(warnings.is_empty(), "{warnings:?}");
        let cfg = engines.get("probe").expect("probe");
        assert_eq!(cfg.placement, "after_stage2");
        assert!((cfg.timeout - 30.0).abs() < 1e-12);
        let json = engine_json(cfg);
        assert_eq!(json["name"], "probe");
        assert_eq!(json["placement"], "after_stage2");
        assert_eq!(json["enabled"], true);
        assert_eq!(json["available"], true, "/bin/echo must resolve");
        assert_eq!(json["command"][0], "/bin/echo");
        assert_eq!(json.as_object().map(|o| o.len()), Some(5));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_binary_reports_unavailable() {
        let tmp = std::env::temp_dir().join(format!("sutura-eng7-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("mkdir");
        write(
            &tmp,
            "gone.toml",
            "name = \"gone\"\ncommand = [\"definitely-not-a-real-binary-xyz\", \"{input}\", \"{output}\"]\n",
        );
        let (engines, _w) = load_all_engines(Some(&tmp));
        let cfg = engines.get("gone").expect("gone");
        assert!(!engine_json(cfg)["available"].as_bool().unwrap());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
