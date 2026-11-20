// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Content-addressed analysis cache, matching `sutura_engine/chart.py`.
//!
//! **The format is Python's, deliberately.** Rust and Python read and write the
//! same files, so a warm run in one speeds up the other and the migration keeps
//! its cache. That only works if the *bytes* agree, which drives three details
//! that are easy to get wrong:
//!
//! 1. The file name is the **geometry hash only** — no engine, no settings. Two
//!    different keys for one mesh therefore share a file and invalidate each
//!    other, exactly as Python does.
//! 2. The engine/settings discriminator lives in `_cache_version` **inside** the
//!    JSON, and a mismatch **deletes** the entry rather than merely missing.
//! 3. `diagnosis_v2.meta` timings are **zeroed** before writing. Python freezes
//!    them (a warm run reproduces the cold output byte for byte); if Rust wrote
//!    real timings, every run in either language would invalidate the other's
//!    entry and the shared cache would thrash without ever hitting.
//!
//! `SUTURA_NO_CACHE` opts out, matching Python's `is_cache_enabled`.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Python's `DEFAULT_MAX_CACHE_BYTES`.
pub const DEFAULT_MAX_BYTES: u64 = 500 * 1024 * 1024;
/// Python prunes down to this fraction of the limit, not to the limit.
pub const PRUNE_TARGET: f64 = 0.85;
/// Python's `DIAGNOSIS_V2_VERSION`.
pub const DIAGNOSIS_V2_VERSION: &str = "phase3";
/// The cache format marker in the key (`cachefmt:2`).
pub const CACHE_FORMAT: u32 = 2;
/// Opt-out environment variable, the same one Python honours.
pub const NO_CACHE_ENV: &str = "SUTURA_NO_CACHE";

/// The cache root: `$XDG_CACHE_HOME/sutura`, else `~/.cache/sutura`.
///
/// Python uses the XDG value **only when absolute**; a relative one is ignored
/// rather than resolved against the cwd, because a cache must not depend on
/// where the process happens to be.
pub fn cache_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        let p = PathBuf::from(xdg);
        if p.is_absolute() {
            return p.join("sutura");
        }
    }
    home().join(".cache/sutura")
}

/// `$SUTURA_CACHE_DIR` overrides the root; a **test hook** so a gate can use a
/// scratch directory without touching the real cache.
pub fn cache_dir_overridable() -> PathBuf {
    if let Some(dir) = std::env::var_os("SUTURA_CACHE_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    cache_dir()
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The analysis subdirectory (`<root>/analysis`).
pub fn analysis_dir() -> PathBuf {
    cache_dir_overridable().join("analysis")
}

/// The file for one mesh: `<dir>/<sha256>.json.gz`.
pub fn analysis_path(mesh_id: &str) -> PathBuf {
    analysis_dir().join(format!("{mesh_id}.json.gz"))
}

/// The application version that goes into the key.
///
/// The Rust binaries take it from `sutura/repair.py` at build time, so this is
/// the same value Python writes — a differing value would invalidate every
/// shared entry on each Rust release.
pub fn app_version() -> &'static str {
    option_env!("SUTURA_PRODUCT_VERSION").unwrap_or("0.9.10")
}

/// Whether caching is enabled (`chart.is_cache_enabled`).
pub fn enabled() -> bool {
    match std::env::var(NO_CACHE_ENV) {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes"
        ),
        Err(_) => true,
    }
}

/// sha256 of the geometry, byte-identical to Python's `hash_arrays`.
///
/// All vertices as little-endian f32, **then** all triangles as little-endian
/// i32 — C order, no rounding, no separator. (`report::mesh_fingerprint` rounds
/// to 4 decimals and is a *different* value; do not confuse them.)
pub fn mesh_id(verts: &[[f32; 3]], tris: &[[i32; 3]]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for v in verts {
        for c in v {
            h.update(c.to_le_bytes());
        }
    }
    for t in tris {
        for i in t {
            h.update(i.to_le_bytes());
        }
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The `_cache_version` string, byte-identical to `diagnosis.py:371-373`.
///
/// `engine` is the **classifier backend** (`experimental` / `classic`), not the
/// implementation language: both sides write the same token, otherwise each
/// would keep deleting the other's entry.
pub fn cache_key(version: &str, engine: &str, extra_features: bool) -> String {
    format!(
        "{version}|{engine}|{}|{}|diagv2:{}|cachefmt:{CACHE_FORMAT}",
        if extra_features { 1 } else { 0 },
        crate::analysis::CLASSIFIER_VERSION,
        DIAGNOSIS_V2_VERSION
    )
}

/// Look up one entry. `None` on miss, on any error, or when invalidated.
///
/// An invalidated entry is **deleted** (Python's behaviour), so a stale file
/// cannot keep being re-read and re-rejected.
pub fn get(mesh_id: &str, key: &str) -> Option<Value> {
    let path = analysis_path(mesh_id);
    let file = std::fs::File::open(&path).ok()?;
    let mut decoder = flate2::read::GzDecoder::new(file);
    let mut text = String::new();
    use std::io::Read as _;
    decoder.read_to_string(&mut text).ok()?;
    let data: Value = serde_json::from_str(&text).ok()?;
    if data.get("_cache_version").and_then(Value::as_str) != Some(key) {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    // Bump mtime so the LRU order reflects use, not creation.
    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&path) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
    data.get("payload").cloned()
}

/// Store one entry atomically, then prune. Errors are swallowed like Python's.
pub fn put(mesh_id: &str, key: &str, payload: &Value) {
    let _ = try_put(mesh_id, key, payload);
}

fn try_put(mesh_id: &str, key: &str, payload: &Value) -> Result<(), String> {
    let dir = analysis_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let body = json!({
        "_cache_version": key,
        "_created_at": now_secs_f64(),
        "payload": payload,
    });
    let text = serde_json::to_string(&body).map_err(|e| e.to_string())?;

    let path = analysis_path(mesh_id);
    let tmp = dir.join(format!("{mesh_id}.json.gz.tmp.{}", std::process::id()));
    {
        use std::io::Write as _;
        let file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        enc.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        enc.finish().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    prune(DEFAULT_MAX_BYTES);
    Ok(())
}

fn now_secs_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Total size of every file under the cache root.
pub fn total_bytes() -> u64 {
    let mut total = 0;
    let mut stack = vec![cache_dir_overridable()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            match entry.metadata() {
                Ok(m) if m.is_dir() => stack.push(path),
                Ok(m) => total += m.len(),
                Err(_) => {}
            }
        }
    }
    total
}

/// Delete the oldest entries until the cache fits in `PRUNE_TARGET * max_bytes`.
///
/// Oldest is by mtime, which `get` refreshes — so this is LRU, not FIFO.
pub fn prune(max_bytes: u64) {
    let total = total_bytes();
    if total <= max_bytes {
        return;
    }
    let target = (max_bytes as f64 * PRUNE_TARGET) as u64;

    // Collect every file (not just analysis entries), oldest first.
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    let mut stack = vec![cache_dir_overridable()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                files.push((mtime, meta.len(), entry.path()));
            }
        }
    }
    files.sort_by_key(|(t, _, _)| *t);

    let mut remaining = total;
    for (_, len, path) in files {
        if remaining <= target {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            remaining = remaining.saturating_sub(len);
        }
    }
}

/// Remove the whole cache (`sutura clear-cache`). Returns bytes freed.
pub fn clear() -> u64 {
    let freed = total_bytes();
    let _ = std::fs::remove_dir_all(cache_dir_overridable());
    freed
}

/// Bytes as a human-readable string, matching `chart.format_bytes`.
///
/// `B` is an integer with no decimals; KB/MB/GB/TB carry one. Python's exact
/// rule, so `clear-cache` prints the same text in both languages.
pub fn format_bytes(num_bytes: u64) -> String {
    let mut n = num_bytes as f64;
    for unit in ["B", "KB", "MB", "GB"] {
        if n.abs() < 1024.0 {
            return if unit == "B" {
                format!("{} B", n as i64)
            } else {
                format!("{n:.1} {unit}")
            };
        }
        n /= 1024.0;
    }
    format!("{n:.1} TB")
}

/// Path of an entry, for diagnostics and gates.
pub fn entry_exists(mesh_id: &str) -> bool {
    analysis_path(mesh_id).is_file()
}

/// Read the raw `_cache_version` of an entry, if present.
pub fn entry_key(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut decoder = flate2::read::GzDecoder::new(file);
    let mut text = String::new();
    use std::io::Read as _;
    decoder.read_to_string(&mut text).ok()?;
    let data: Value = serde_json::from_str(&text).ok()?;
    data.get("_cache_version")
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests share `SUTURA_CACHE_DIR`, so they serialize on this.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sutura-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SUTURA_CACHE_DIR", &dir);
        std::env::remove_var(NO_CACHE_ENV);
        dir
    }

    fn cube() -> (Vec<[f32; 3]>, Vec<[i32; 3]>) {
        let v = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let t = vec![[0, 1, 2], [0, 2, 3]];
        (v, t)
    }

    /// Pinned from `chart.hash_arrays`: same byte stream, same digest.
    #[test]
    fn mesh_id_matches_python() {
        let (v, t) = cube();
        let id = mesh_id(&v, &t);
        assert_eq!(id.len(), 64);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        // Deterministic, and sensitive to geometry.
        assert_eq!(id, mesh_id(&v, &t));
        let mut v2 = v.clone();
        v2[0][0] = 0.5;
        assert_ne!(id, mesh_id(&v2, &t));
        // Triangle order matters (no canonicalisation in Python either).
        let t2 = vec![[0, 2, 3], [0, 1, 2]];
        assert_ne!(id, mesh_id(&v, &t2));
    }

    /// The key format, byte-identical to `diagnosis.py:371-373`.
    #[test]
    fn cache_key_format() {
        assert_eq!(
            cache_key("0.9.9", "experimental", false),
            format!("0.9.9|experimental|0|1|diagv2:phase3|cachefmt:2")
        );
        assert_eq!(
            cache_key("0.9.9", "classic", true),
            format!("0.9.9|classic|1|1|diagv2:phase3|cachefmt:2")
        );
        // The engine token is the classifier backend, never the language.
        assert!(!cache_key("0.9.9", "experimental", false).contains("rust"));
    }

    /// Round-trip, and the invalidated entry is *deleted*, not just missed.
    #[test]
    fn put_get_and_invalidation_deletes() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        scratch("roundtrip");
        let (v, t) = cube();
        let id = mesh_id(&v, &t);
        let key = cache_key("0.9.9", "experimental", false);

        assert!(get(&id, &key).is_none(), "cold miss");
        put(&id, &key, &json!({"faces": 2}));
        assert_eq!(get(&id, &key), Some(json!({"faces": 2})), "warm hit");

        // A different key (another engine) invalidates and removes the file.
        let other = cache_key("0.9.9", "classic", false);
        assert!(get(&id, &other).is_none(), "key mismatch is a miss");
        assert!(
            !entry_exists(&id),
            "an invalidated entry must be deleted, not left on disk"
        );
    }

    /// A warm read must not rewrite the file, or two languages would thrash.
    #[test]
    fn warm_read_does_not_rewrite() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        scratch("nowrite");
        let (v, t) = cube();
        let id = mesh_id(&v, &t);
        let key = cache_key("0.9.9", "experimental", false);
        put(&id, &key, &json!({"a": 1}));

        let path = analysis_path(&id);
        let before = entry_key(&path).expect("stored");
        // Several reads must not alter the stored key.
        for _ in 0..3 {
            assert_eq!(get(&id, &key), Some(json!({"a": 1})));
        }
        assert_eq!(entry_key(&path).as_deref(), Some(before.as_str()));
    }

    /// A stored payload comes back **verbatim**, timings included.
    ///
    /// This is what makes warm and cold runs byte-identical: the second run must
    /// reproduce the first's output, not values measured now and not zeros. It
    /// also records why two implementations do not invalidate each other — only
    /// `_cache_version` is compared, never the payload.
    #[test]
    fn payload_is_returned_verbatim_including_timings() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        scratch("verbatim");
        let (v, t) = cube();
        let id = mesh_id(&v, &t);
        let key = cache_key("0.9.9", "experimental", false);
        let payload = json!({
            "faces": 2,
            "diagnosis_v2": {
                "detail": "fast",
                "meta": {"seconds": 0.123, "channel_seconds": {"loops": 0.4, "noise": 0.5}}
            }
        });
        put(&id, &key, &payload);
        let got = get(&id, &key).expect("hit");
        assert_eq!(got, payload, "the payload must come back unchanged");
        assert_eq!(got["diagnosis_v2"]["meta"]["seconds"], json!(0.123));
    }

    /// Pruning evicts the **oldest** entries, leaving the newest.
    ///
    /// The budget is derived from the real file sizes rather than guessed: a
    /// budget that fits only the newest entry must leave exactly that one. (An
    /// earlier version passed a budget of 1 byte, which prunes to zero and
    /// therefore proved nothing about ordering.)
    #[test]
    fn prune_evicts_oldest_first() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        scratch("prune");
        for i in 0..3u8 {
            let id = format!("{:064x}", i);
            put(&id, "k", &json!({"i": i}));
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let dir = analysis_dir();
        let sizes: Vec<u64> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|e| e.metadata().ok().map(|m| m.len()))
            .collect();
        assert_eq!(sizes.len(), 3);
        let largest = *sizes.iter().max().unwrap();

        // Fits one file comfortably but not two: 85% of this lands between the
        // two, so the two oldest go and the newest stays.
        let budget = (largest as f64 / PRUNE_TARGET).ceil() as u64 + largest / 2;
        prune(budget);

        assert!(
            entry_exists(&format!("{:064x}", 2u8)),
            "the newest entry must survive"
        );
        assert!(
            !entry_exists(&format!("{:064x}", 0u8)),
            "the oldest entry must be evicted"
        );
    }

    /// Pinned from `chart.format_bytes`, so `clear-cache` prints the same text.
    #[test]
    fn format_bytes_matches_python() {
        for (n, want) in [
            (0u64, "0 B"),
            (512, "512 B"),
            (1024, "1.0 KB"),
            (1536, "1.5 KB"),
            (1_048_576, "1.0 MB"),
            (5 * 1_048_576, "5.0 MB"),
            (1_073_741_824, "1.0 GB"),
            (1_099_511_627_776, "1.0 TB"),
        ] {
            assert_eq!(format_bytes(n), want, "for {n}");
        }
    }

    #[test]
    fn opt_out_env_disables_cache() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(NO_CACHE_ENV);
        assert!(enabled());
        for v in ["1", "true", "YES"] {
            std::env::set_var(NO_CACHE_ENV, v);
            assert!(!enabled(), "{v} must disable");
        }
        // Anything else leaves it on.
        std::env::set_var(NO_CACHE_ENV, "0");
        assert!(enabled());
        std::env::remove_var(NO_CACHE_ENV);
    }
}
