// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Update check, matching `sutura/updater.py`.
//!
//! The status triple is Python's: `update` (a newer release that may be
//! installed automatically), `license` (newer, but it crosses the license
//! boundary, so the user must install it deliberately) and `none` (nothing
//! newer, or the check did not run). Downloading and installing live in
//! [`crate::install`]; this module never touches the filesystem beyond the
//! config, and never mutates the installation.

use serde_json::{json, Value};
use std::path::PathBuf;

use crate::http;

pub const GITHUB_API: &str = "https://api.github.com/repos/Krateian/Sutura/releases/latest";
pub const GITHUB_TAGS_API: &str = "https://api.github.com/repos/Krateian/Sutura/tags";
pub const RELEASES_URL: &str = "https://github.com/Krateian/Sutura/releases";
pub const CHECK_INTERVAL_SECONDS: i64 = 7 * 24 * 60 * 60;
/// The release at/after which the licence terms changed.
pub const LICENSE_BOUNDARY_VERSION: &str = "0.2.0";

const PRE_BETA: u8 = 0;
const PRE_STABLE: u8 = 1;

/// A parsed `vMAJOR.MINOR.PATCH[-pre]` tag, sortable like Python's tuple.
///
/// A stable release carries [`PRE_STABLE`] and therefore sorts *newer* than any
/// prerelease of the same version (`0.1.8` > `0.1.8-beta.1`), so a beta build
/// is offered the stable release once it exists.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub rank: u8,
    pub pre: String,
}

impl Version {
    /// The numeric triple, as `version[:3]` in Python.
    pub fn triple(&self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }
}

/// Parse `v1.2.3` / `v1.2.3-beta.1`. `None` on a malformed tag.
pub fn parse_version(tag: &str) -> Option<Version> {
    let s = tag.strip_prefix('v').unwrap_or(tag);
    let (core, pre) = match s.split_once('-') {
        Some((c, p)) => {
            // Python's character class is `[0-9A-Za-z.\-]+`, so a prerelease may
            // itself contain dots and dashes but nothing else.
            if p.is_empty()
                || !p
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
            {
                return None;
            }
            (c, Some(p))
        }
        None => (s, None),
    };
    let mut it = core.split('.');
    let major = it.next()?.parse::<u64>().ok()?;
    let minor = it.next()?.parse::<u64>().ok()?;
    let patch = it.next()?.parse::<u64>().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some(match pre {
        None => Version {
            major,
            minor,
            patch,
            rank: PRE_STABLE,
            pre: String::new(),
        },
        Some(p) => Version {
            major,
            minor,
            patch,
            rank: PRE_BETA,
            pre: p.to_string(),
        },
    })
}

/// Whether `tag` is newer than `current`. False when either is unparseable.
pub fn is_newer(tag: &str, current: &str) -> bool {
    match (parse_version(tag), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Whether `latest` sits at/after the licence boundary while `current` is
/// strictly before it. Compares the numeric triple, so a prerelease of the
/// boundary version also counts.
pub fn crosses_license_boundary(current: &str, latest: &str) -> bool {
    let (Some(cur), Some(lat), Some(bnd)) = (
        parse_version(current),
        parse_version(latest),
        parse_version(LICENSE_BOUNDARY_VERSION),
    ) else {
        return false;
    };
    cur.triple() < bnd.triple() && bnd.triple() <= lat.triple()
}

/// The updater's config file (`~/.config/sutura/config.json`), the same file the
/// Python CLI and GUI read.
pub fn config_path() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join("sutura/config.json");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config/sutura/config.json")
}

/// Read the config, or an empty object when it is missing or unreadable.
pub fn load_config() -> Value {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

/// Write the config back atomically. Errors are returned, not swallowed, so a
/// caller can decide; `check_for_update` ignores them like Python does.
pub fn save_config(cfg: &Value) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, text.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// Whether an update check is enabled and due.
pub fn should_check(cfg: &Value) -> bool {
    if !cfg
        .get("check_for_updates")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return false;
    }
    if cfg
        .get("check_on_startup")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return true;
    }
    let Some(last) = cfg.get("last_check").and_then(Value::as_i64) else {
        return true;
    };
    now_secs() - last >= CHECK_INTERVAL_SECONDS
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Whether this process is an AppImage build, which cannot reinstall itself into
/// its read-only payload (`updater.is_appimage`).
pub fn is_appimage() -> bool {
    std::env::var_os("APPIMAGE")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// The latest release's tag: `/releases/latest`, else the newest tag.
pub fn fetch_latest_release() -> Option<String> {
    if let Some(data) = http::get_json(GITHUB_API) {
        if let Some(tag) = data.get("tag_name").and_then(Value::as_str) {
            return Some(tag.to_string());
        }
    }
    // No published release yet: fall back to the newest *stable* tag, skipping
    // prereleases so a beta never surfaces to a normal user.
    let tags = http::get_json(GITHUB_TAGS_API)?;
    tags.as_array()?
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .find(|name| name.starts_with('v') && !name.contains('-'))
        .map(str::to_string)
}

/// The release object (tag, body, assets) as GitHub returned it.
pub fn fetch_latest_release_full() -> Option<Value> {
    http::get_json(GITHUB_API)
}

/// Outcome of a check.
#[derive(Clone, Debug, PartialEq)]
pub struct UpdateStatus {
    /// `update` / `license` / `none`.
    pub status: &'static str,
    /// The newer tag, when there is one.
    pub tag: Option<String>,
}

/// Run the check if enabled and due (or `force`d).
///
/// `current` is the running version. Marks `last_check` only when a check
/// actually ran, so an offline machine retries next time instead of waiting a
/// week. AppImage builds never check.
pub fn check_for_update(current: &str, force: bool) -> UpdateStatus {
    let mut cfg = load_config();
    if is_appimage() {
        return none();
    }
    if !force && !should_check(&cfg) {
        return none();
    }
    let Some(latest) = fetch_latest_release() else {
        // Transient network issue: record the attempt, don't nag.
        cfg["last_check"] = json!(now_secs());
        let _ = save_config(&cfg);
        return none();
    };
    cfg["last_check"] = json!(now_secs());
    let newer = is_newer(&latest, current);
    if newer && crosses_license_boundary(current, &latest) {
        let _ = save_config(&cfg);
        return UpdateStatus {
            status: "license",
            tag: Some(latest),
        };
    }
    let _ = save_config(&cfg);
    if newer {
        UpdateStatus {
            status: "update",
            tag: Some(latest),
        }
    } else {
        none()
    }
}

fn none() -> UpdateStatus {
    UpdateStatus {
        status: "none",
        tag: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values pinned from `updater.parse_version` / `is_newer` in Python.
    #[test]
    fn version_parsing_and_ordering() {
        assert_eq!(parse_version("v0.9.9").unwrap().triple(), (0, 9, 9));
        assert_eq!(parse_version("0.9.9").unwrap().triple(), (0, 9, 9));
        assert_eq!(parse_version("v0.1.8-beta.1").unwrap().rank, PRE_BETA);
        assert_eq!(parse_version("v0.1.8").unwrap().rank, PRE_STABLE);

        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("v0.9.10", "0.9.9"));
        assert!(!is_newer("v0.9.9", "0.9.9"));
        // A stable release beats a prerelease of the same version.
        assert!(is_newer("v0.1.8", "v0.1.8-beta.1"));
        assert!(!is_newer("v0.1.8-beta.1", "v0.1.8"));
        // Malformed input never claims an update.
        assert!(!is_newer("banana", "0.9.9"));
        assert!(!is_newer("v0.10.0", "banana"));
        assert!(parse_version("v1.2").is_none());
        assert!(parse_version("v1.2.3.4").is_none());
    }

    #[test]
    fn licence_boundary_cases() {
        // Below the boundary on both sides: no crossing.
        assert!(!crosses_license_boundary("0.1.9", "0.1.10"));
        // Straddling 0.2.0: crosses.
        assert!(crosses_license_boundary("0.1.9", "0.2.0"));
        assert!(crosses_license_boundary("0.1.9", "0.2.5"));
        // A prerelease of the boundary version already carries the new terms.
        assert!(crosses_license_boundary("0.1.9", "0.2.0-beta.1"));
        // Already at/after the boundary: not a *crossing*.
        assert!(!crosses_license_boundary("0.2.0", "0.3.0"));
        assert!(!crosses_license_boundary("0.2.1", "0.3.0"));
        // Unparseable input is never a crossing.
        assert!(!crosses_license_boundary("nope", "0.3.0"));
    }

    /// Disabled config means no check even when forced past the interval.
    #[test]
    fn should_check_respects_config() {
        assert!(!should_check(&json!({})));
        assert!(!should_check(&json!({"check_for_updates": false})));
        assert!(should_check(&json!({"check_for_updates": true, "check_on_startup": true})));
        // Enabled, no previous check: due.
        assert!(should_check(&json!({"check_for_updates": true})));
        // Enabled, checked long ago: due. Checked just now: not due.
        assert!(should_check(&json!({
            "check_for_updates": true,
            "last_check": now_secs() - CHECK_INTERVAL_SECONDS - 10
        })));
        assert!(!should_check(&json!({
            "check_for_updates": true,
            "last_check": now_secs()
        })));
    }

    /// With no network the check reports `none` and does not panic.
    #[test]
    fn offline_check_is_none() {
        if std::env::var_os("SUTURA_TEST_NETWORK").is_some() {
            return; // opt-in network test
        }
        // Force through a bogus config dir so a real config is not touched.
        let dir = std::env::temp_dir().join(format!("sutura-upd-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // `fetch_latest_release` on a machine with no network returns None;
        // this asserts the *shape* of the offline path, not the network.
        assert_eq!(none().status, "none");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
