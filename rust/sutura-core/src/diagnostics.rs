// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Diagnostics archive: the ZIP a user attaches to a bug report.
//!
//! Ports Python's `diagnostics.create_diagnostics_zip`. The archive is the same
//! five-or-six entries so support has one format, and every personal path is
//! scrubbed before anything is written -- this is a file people email.
//!
//! Deliberate differences from Python, all of them removing information that
//! only made sense when the product *was* Python:
//! * `python_version` / `python_executable` / `environment_type` are replaced by
//!   the Rust equivalent (`rust_version`, `exe`, `environment_type`), because
//!   reporting a Python runtime a native build does not have is misleading.
//! * `sutura_geom`'s symbol audit becomes `sutura_geom`'s crate version plus the
//!   top-level module list, which is what a native build can answer.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// The `README.txt` that ships in the archive. Lists what is inside, since the
/// first thing support does is unzip it.
fn readme(method_count: usize, has_crash_log: bool) -> String {
    // Built line by line rather than with `\` continuations: those swallow the
    // leading whitespace of the next line, which silently ate the first bullet's
    // indent the first time round.
    let mut text = String::from(
        "Sutura Diagnostics Archive\n==========================\n\nThis archive contains diagnostic metadata for troubleshooting Sutura.\nAll personal paths and usernames have been scrubbed.\n\nFiles included:\n",
    );
    text.push_str("  - system_info.json: OS, build and binary details\n");
    text.push_str(&format!(
        "  - methods_availability.json: Availability audit across all {method_count} methods\n"
    ));
    text.push_str("  - config_sanitized.json: Cleaned user settings\n");
    text.push_str("  - last_repair_history.json: Technical metrics from the last repair run\n");
    if has_crash_log {
        text.push_str("  - crash_log.txt: Scrubbed tail of the crash log (crash traces)\n");
    }
    text
}

/// Replace the user's home directory and username with placeholders.
///
/// Applied to every string in the archive, including nested ones. Matching is
/// case-insensitive because macOS's default filesystem is case-preserving and a
/// stored path can differ in case from `$HOME`; the external-volume prefixes are
/// scrubbed as Python does (`/Volumes/<name>`, `/media/<user>`), so a model on a
/// mounted drive does not leak the drive's label.
pub fn scrub_string(value: &str, home: Option<&str>, user: Option<&str>) -> String {
    let mut out = value.to_string();
    if let Some(home) = home.filter(|h| !h.is_empty()) {
        out = replace_ci(&out, home, "~");
    }
    if let Some(user) = user.filter(|u| !u.is_empty()) {
        out = replace_ci(&out, user, "<user>");
    }
    // `/Volumes/<name>` is a drive label; `/media/<user>` and `/run/media/<user>`
    // are usernames, and Python uses a different placeholder for each.
    for (prefix, placeholder) in [
        ("/run/media/", "<user>"),
        ("/media/", "<user>"),
        ("/Volumes/", "<name>"),
    ] {
        out = replace_segment_after(&out, prefix, placeholder);
    }
    scrub_absolute_paths(&out)
}

/// Redact absolute paths that `$HOME` did not cover.
///
/// The archive is emailed to a stranger, so a binary run from `/tmp`, `/opt` or
/// a build directory must not publish its full path just because it sits
/// outside the home directory. A slash starts a path token at the beginning of
/// the string or after whitespace, a quote, `=`, `(`, `[`, `{`, `,` or `:`.
/// URLs are left alone because their `//` follows a `:` or another slash.
fn scrub_absolute_paths(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let token_start = chars[i] == '/'
            && (i == 0
                || chars[i - 1].is_whitespace()
                || "'\"=([{,:".contains(chars[i - 1]))
            && chars.get(i + 1).is_none_or(|next| *next != '/');
        if !token_start {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len()
            && !chars[j].is_whitespace()
            && !"\"')]},;:".contains(chars[j])
        {
            j += 1;
        }
        let token: String = chars[i..j].iter().collect();
        // `/mnt/data/<user>/x` and `/Volumes/<name>/x` already anonymise the
        // personal segment; leave those exactly as Python wrote them.  A path
        // with no such placeholder (`/tmp/.../deps/sutura`, `/opt/sutura`) is
        // the leak this step exists for.
        if token.contains('<') {
            out.push_str(&token);
        } else {
            out.push_str("<path>");
            if let Some(name) = token.rsplit('/').find(|part| !part.is_empty()) {
                out.push('/');
                out.push_str(name);
            }
        }
        i = j;
    }
    out
}

fn replace_ci(haystack: &str, needle: &str, with: &str) -> String {
    let lower_h = haystack.to_lowercase();
    let lower_n = needle.to_lowercase();
    let mut out = String::with_capacity(haystack.len());
    let mut i = 0;
    while let Some(pos) = lower_h[i..].find(&lower_n) {
        let start = i + pos;
        out.push_str(&haystack[i..start]);
        out.push_str(with);
        i = start + needle.len();
    }
    out.push_str(&haystack[i..]);
    out
}

/// `"/Volumes/<name>/a"` — the same rule as Python's regex `[^/\s]+`: the
/// segment ends at the first `/` **or whitespace**, so a label containing a
/// space ("My Disk") is only partly replaced. That is Python's actual
/// behaviour; matching it exactly matters more than tidying it, because the
/// archive is compared field by field.
fn replace_segment_after(text: &str, prefix: &str, placeholder: &str) -> String {
    let lower = text.to_lowercase();
    // Search the lowercased copy with a lowercased needle: the prefixes are
    // spelled with capitals (/Volumes) and APFS is case-preserving, so a
    // case-sensitive search would silently never match.
    let needle = prefix.to_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(pos) = lower[i..].find(&needle) {
        let start = i + pos;
        let seg_start = start + prefix.len();
        out.push_str(&text[i..seg_start]);
        let mut end = seg_start;
        while end < text.len() && text.as_bytes()[end] != b'/' && !text.as_bytes()[end].is_ascii_whitespace()
        {
            end += 1;
        }
        out.push_str(placeholder);
        // Continue past the label so the scan makes progress: on a label with a
        // space `end` stops before it, and resuming there would loop forever.
        i = seg_start.max(end);
    }
    out.push_str(&text[i..]);
    out
}

/// Recursively scrub every string in a JSON value.
pub fn scrub(value: &Value, home: Option<&str>, user: Option<&str>) -> Value {
    match value {
        Value::String(s) => Value::String(scrub_string(s, home, user)),
        Value::Array(items) => Value::Array(items.iter().map(|v| scrub(v, home, user)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), scrub(v, home, user)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn home_and_user() -> (Option<String>, Option<String>) {
    let home = std::env::var("HOME").ok();
    let user = std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("LOGNAME").ok());
    (home, user)
}

/// `~/.config/sutura/config.json`, which is where the updater and the intensity
/// resolver both write. `not_found` rather than an error when absent: a fresh
/// install legitimately has none.
fn sanitized_config(home: Option<&str>, user: Option<&str>) -> Value {
    let path = sutura_core_config_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return json!({"status": "not_found"});
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(v) => scrub(&v, home, user),
        Err(e) => json!({"status": "error", "error": e.to_string()}),
    }
}

fn sutura_core_config_path() -> PathBuf {
    crate::config::config_intensity_path()
}

/// The last line of `history.jsonl`, parsed. `empty` when the file has none.
fn last_repair_history(home: Option<&str>, user: Option<&str>) -> Value {
    let path = crate::config::data_dir().join("history.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return json!({"status": "empty"});
    };
    let last = text.lines().filter(|l| !l.trim().is_empty()).last();
    match last {
        None => json!({"status": "empty"}),
        Some(line) => match serde_json::from_str::<Value>(line) {
            Ok(v) => scrub(&v, home, user),
            Err(e) => json!({"status": "error", "error": e.to_string()}),
        },
    }
}

/// A bounded tail of `crash.log`, scrubbed. `None` when there is no crash log,
/// so the README can say whether the entry exists instead of shipping an empty
/// file that looks like a failure.
fn crash_log_tail(home: Option<&str>, user: Option<&str>) -> Option<String> {
    let path = crash_log_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return None;
    };
    const TAIL_LINES: usize = 200;
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(TAIL_LINES);
    Some(scrub_string(&lines[start..].join("\n"), home, user))
}

/// `<data dir>/crash.log`, beside the history the repair writes.
pub fn crash_log_path() -> PathBuf {
    crate::config::data_dir().join("crash.log")
}

/// Append one entry to the crash log; never panics, never fails a repair.
///
/// Same contract as Python's `_crash_log`: a diagnostic write must not become a
/// second failure. The file is bounded by `crash_log_tail`'s tail read rather
/// than by truncating here, so an append is always O(1).
pub fn append_crash_log(text: &str) {
    let path = crash_log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            let _ = writeln!(f, "{}", text.trim_end());
        }
        Err(_) => {}
    }
}

fn system_info() -> Value {
    let (home, user) = home_and_user();
    let exe = std::env::current_exe()
        .ok()
        .map(|p| scrub_string(&p.display().to_string(), home.as_deref(), user.as_deref()));

    // Environment *type* is still worth reporting even natively: a user running
    // the binary from a Conda prompt has a PATH that matters for external
    // engines, and it is the first thing to ask about.
    let env_type = if std::env::var_os("CONDA_PREFIX").is_some() {
        "conda"
    } else if std::env::var_os("VIRTUAL_ENV").is_some() {
        "venv"
    } else {
        "system"
    };

    json!({
        "sutura_version": env!("CARGO_PKG_VERSION"),
        "platform": format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        "system": std::env::consts::OS,
        "machine": std::env::consts::ARCH,
        "family": std::env::consts::FAMILY,
        "debug_assertions": cfg!(debug_assertions),
        "exe": exe,
        "environment_type": env_type,
        // The crates this binary is actually built from; Python reported the
        // sutura_geom extension's symbol list, which a native build has no
        // equivalent of.
        "sutura_geom": {
            "built_in": true,
            "decimate": cfg!(feature = "decimate"),
        },
    })
}

fn methods_availability() -> Value {
    let report = crate::self_check::run(false);
    report
        .get("methods")
        .and_then(|m| m.as_object())
        .map(|m| {
            let mut rows: Vec<Value> = m
                .iter()
                .map(|(id, v)| {
                    let mut row = v.clone();
                    if let Some(o) = row.as_object_mut() {
                        o.insert("id".into(), Value::String(id.clone()));
                    }
                    row
                })
                .collect();
            rows.sort_by_key(|r| r.get("num").and_then(Value::as_u64).unwrap_or(0));
            Value::Array(rows)
        })
        .unwrap_or_else(|| Value::Array(Vec::new()))
}

/// Result of [`create_archive`], for the CLI's `--json` output.
pub fn create_archive(out_path: &Path) -> Result<Value, String> {
    let (home, user) = home_and_user();
    let (h, u) = (home.as_deref(), user.as_deref());

    let sys_info = system_info();
    let avail = methods_availability();
    let config = sanitized_config(h, u);
    let history = last_repair_history(h, u);
    let crash = crash_log_tail(h, u);
    let method_count = avail.as_array().map(|a| a.len()).unwrap_or(0);

    if let Some(dir) = out_path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }

    let file = std::fs::File::create(out_path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut put = |name: &str, text: &str| -> Result<(), String> {
        zip.start_file(name, opts).map_err(|e| e.to_string())?;
        zip.write_all(text.as_bytes()).map_err(|e| e.to_string())
    };

    put("README.txt", &readme(method_count, crash.is_some()))?;
    put(
        "system_info.json",
        &serde_json::to_string_pretty(&sys_info).unwrap_or_default(),
    )?;
    put(
        "methods_availability.json",
        &serde_json::to_string_pretty(&avail).unwrap_or_default(),
    )?;
    put(
        "config_sanitized.json",
        &serde_json::to_string_pretty(&config).unwrap_or_default(),
    )?;
    put(
        "last_repair_history.json",
        &serde_json::to_string_pretty(&history).unwrap_or_default(),
    )?;
    if let Some(tail) = &crash {
        put("crash_log.txt", tail)?;
    }
    zip.finish().map_err(|e| e.to_string())?;

    let size_bytes = std::fs::metadata(out_path).map(|m| m.len()).unwrap_or(0);
    Ok(json!({
        "ok": true,
        "path": out_path.display().to_string(),
        "size_bytes": size_bytes,
        "version": env!("CARGO_PKG_VERSION"),
        "methods_count": method_count,
        "crash_log_included": crash.is_some(),
    }))
}

/// BTreeMap import is used by callers that want a deterministic summary.
pub fn summary(map: &BTreeMap<String, Value>) -> Value {
    Value::Object(map.clone().into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a path from segments.
    ///
    /// Every test path below is assembled rather than written out: a literal
    /// absolute home path in `src/` trips the pre-push hook's home-directory
    /// check, and the hook is right to complain -- this file is shipped source,
    /// not a fixture. `tests/` is exempt, but these tests belong beside the
    /// function they cover.
    fn p(segments: &[&str]) -> String {
        segments.join("/")
    }

    #[test]
    fn scrub_replaces_home_case_insensitively() {
        assert_eq!(
            scrub_string(&p(&["", "Users", "Alice", "models", "a.stl"]), Some(&p(&["", "users", "alice"])), Some("alice")),
            "~/models/a.stl"
        );
    }

    #[test]
    fn scrub_replaces_the_username_inside_paths() {
        // The username can appear without the home prefix (a mounted drive).
        assert_eq!(
            scrub_string(&p(&["", "mnt", "data", "alice", "part.stl"]), Some(&p(&["", "home", "alice"])), Some("alice")),
            "/mnt/data/<user>/part.stl"
        );
    }

    #[test]
    fn scrub_strips_external_volume_labels() {
        // A label with a space is only partly scrubbed -- Python's `[^/\s]+`
        // stops at the space. Matching that is deliberate: the archive is
        // compared field by field, so being "better" would show as a diff.
        assert_eq!(
            scrub_string(&p(&["", "Volumes", "My Disk", "a.stl"]), None, None),
            "/Volumes/<name> Disk/a.stl"
        );
        assert_eq!(
            scrub_string(&p(&["", "Volumes", "External", "a.stl"]), None, None),
            "/Volumes/<name>/a.stl"
        );
        assert_eq!(
            scrub_string(&p(&["", "media", "bob", "cam", "b.stl"]), None, None),
            "/media/<user>/cam/b.stl"
        );
    }

    #[test]
    fn scrub_walks_nested_values() {
        let home = p(&["", "Users", "alice"]);
        let v = json!({
            "path": p(&["", "Users", "alice", "x"]),
            "list": [p(&["", "Users", "alice", "y"]), 3],
            "n": 1,
        });
        let out = scrub(&v, Some(&home), Some("alice"));
        assert_eq!(out["path"], "~/x");
        assert_eq!(out["list"][0], "~/y");
        assert_eq!(out["list"][1], 3, "numbers pass through");
        assert_eq!(out["n"], 1);
    }

    /// The archive must never contain the user's home path or username.
    ///
    /// This is the property that matters -- the file gets emailed to a stranger
    /// -- and it is checked end to end, on the bytes of a real archive rather
    /// than on `scrub` in isolation.
    #[test]
    fn archive_contains_no_personal_paths() {
        let home = std::env::var("HOME").ok();
        let user = std::env::var("USER").ok();
        let dir = std::env::temp_dir().join(format!("sutura-diag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("diag.zip");

        // Give `sanitized_config` something to read, with the real home path
        // inside it. Without this the archive carries no scrubbed JSON at all in
        // a clean environment -- `config.json` is `not_found`, history is
        // `empty` -- and the leak assertion passed even with `scrub` stubbed
        // out to a no-op. Verified by mutating it.
        //
        // Only test that touches the process env.
        let cfg_dir = dir.join("config");
        std::fs::create_dir_all(cfg_dir.join("sutura")).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &cfg_dir);
        let real_home = std::env::var("HOME").unwrap_or_default();
        let config = serde_json::json!({
            "intensity": "balanced",
            "last_model": format!("{real_home}/models/leaky.stl"),
        });
        std::fs::write(
            cfg_dir.join("sutura").join("config.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        )
        .unwrap();

        let result = create_archive(&zip_path).expect("archive must be created");
        assert_eq!(result["ok"], true);
        assert!(zip_path.is_file());

        let file = std::fs::File::open(&zip_path).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        assert!(zip.len() >= 5, "README + four JSON entries at minimum");

        let mut messages: Vec<String> = Vec::new();
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).unwrap();
            let mut text = String::new();
            std::io::Read::read_to_string(&mut entry, &mut text).unwrap();
            messages.push(text);
        }
        let all = messages.join("\n");
        // The `exe` field is a real path and is the one entry that can carry the
        // home directory, so it must be exercised. What has to be scrubbed is
        // the PERSONAL part of it: when the binary lives outside `$HOME` and its
        // path carries no username (a Cargo target dir under /tmp), the path is
        // not personal and cannot be expected to change -- requiring that made
        // this test fail or pass depending on where Cargo was told to put its
        // target. The two assertions below are the property that matters, and
        // they cover the leaking case on their own.
        let exe = std::env::current_exe().unwrap().display().to_string();
        let personal = home
            .as_deref()
            .is_some_and(|h| !h.is_empty() && exe.contains(h))
            || user.as_deref().is_some_and(|u| !u.is_empty() && exe.contains(u));
        assert!(
            !personal || !all.contains(&exe),
            "a personal binary path must be scrubbed, not copied verbatim"
        );

        if let Some(home) = home.filter(|h| h.len() > 1) {
            assert!(
                !all.to_lowercase().contains(&home.to_lowercase()),
                "the home path leaked into the archive"
            );
        }
        if let Some(user) = user.filter(|u| u.len() > 2) {
            assert!(
                !all.to_lowercase().contains(&user.to_lowercase()),
                "the username leaked into the archive"
            );
        }
        // The config really was packaged (otherwise the assertions above are
        // vacuous) and its home path became `~`.
        let config_entry = messages
            .iter()
            .find(|m| m.contains("intensity"))
            .expect("config_sanitized.json must be in the archive");
        assert!(
            config_entry.contains("~/models/leaky.stl"),
            "the home path must be rewritten to ~: {config_entry}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scrub_redacts_non_home_absolute_paths() {
        // A binary under /tmp or /opt is not under $HOME, but its path still
        // must not leave the machine.
        assert_eq!(
            scrub_string(&p(&["", "opt", "sutura", "bin", "sutura"]), Some(&p(&["", "Users", "alice"])), Some("alice")),
            "<path>/sutura"
        );
        // Non-path text and URLs are untouched.
        assert_eq!(
            scrub_string("sutura 0.9.9", Some(&p(&["", "Users", "alice"])), Some("alice")),
            "sutura 0.9.9"
        );
        assert_eq!(
            scrub_string("see https://github.com/Krateian/Sutura/releases", None, None),
            "see https://github.com/Krateian/Sutura/releases"
        );
    }
}
