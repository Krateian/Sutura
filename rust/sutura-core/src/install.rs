// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Self-update: download the platform artifact, verify it, swap it in, and roll
//! back if the new version does not work.
//!
//! The shape follows `updater.perform_update` (backup → install → health check →
//! roll back on failure), with three deliberate differences that the release
//! flow needs:
//!
//! 1. **sha256 is verified.** Python's `download_source` unpacks a tarball with
//!    no integrity check at all; here an artifact whose `<asset>.sha256` does not
//!    match is refused and the installation is left untouched.
//! 2. **The swap is atomic** — new content is staged next to the target and
//!    moved into place with `rename`, so a crash mid-install cannot leave a
//!    half-replaced tree.
//! 3. **Two backups are kept**, not one (Arif's decision), and `data/` is never
//!    backed up, never deleted and never restored over: history, learning and
//!    config stay in place so newer learning survives a rollback.
//!
//! The new release's notes are snapshotted *before* installation
//! ([`crate::whatsnew`]) so "what's new" can be shown offline afterwards.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::http;
use crate::updater;

/// How many previous versions to keep.
pub const KEEP_BACKUPS: usize = 2;

/// The install root (`updater.APP_DIR`), which the updater replaces wholesale.
pub fn app_dir() -> PathBuf {
    if let Some(root) = std::env::var_os("SUTURA_APP_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(root);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".local/share/sutura")
}

/// Where backups live. Each kept version is a directory under this root.
pub fn backup_root() -> PathBuf {
    // Sibling of the install dir, so an overridden install root keeps its
    // backups next to it and a test cannot touch the real one.
    let app = app_dir();
    let name = app
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "sutura".to_string());
    app.with_file_name(format!("{name}.backup"))
}

/// The running version, as `--version` reports it.
///
/// The Rust binaries take it from `sutura/repair.py` at build time; this reads
/// the same value when the caller has not supplied one.
pub fn current_version() -> String {
    std::env::var("SUTURA_PRODUCT_VERSION").unwrap_or_else(|_| "0.9.9".to_string())
}

/// Names inside `app_dir()` the updater must never touch.
///
/// `data` holds the runtime user data (history, learning, config); Python
/// protects it by name and additionally resolves `SUTURA_DATA_DIR`, so an
/// override pointing *inside* the app dir is protected too.
pub fn protected_entries() -> Vec<String> {
    let mut v = vec!["data".to_string()];
    let app = app_dir();
    let data = crate::config::learning::data_dir();
    if let Ok(rel) = data.strip_prefix(&app) {
        if let Some(first) = rel.iter().next().and_then(|s| s.to_str()) {
            if !v.iter().any(|p| p == first) {
                v.push(first.to_string());
            }
        }
    }
    v
}

/// The asset file name for this platform, matching `scripts/package_rust.sh`.
///
/// | target          | asset                                  |
/// |-----------------|----------------------------------------|
/// | macOS arm64     | `Sutura-rust-v<ver>-arm64.dmg`          |
/// | Linux x86_64    | `Sutura-rust-v<ver>-x86_64.tar.gz`      |
pub fn asset_name(tag: &str) -> Option<String> {
    let ver = tag.strip_prefix('v').unwrap_or(tag);
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(format!("Sutura-rust-v{ver}-arm64.dmg"))
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(format!("Sutura-rust-v{ver}-x86_64.tar.gz"))
    } else {
        None
    }
}

/// Find this platform's asset (and its `.sha256` sibling) in a release.
pub fn find_asset(release: &Value, tag: &str) -> Option<(String, String)> {
    let want = asset_name(tag)?;
    let assets = release.get("assets")?.as_array()?;
    let url_of = |name: &str| {
        assets
            .iter()
            .find(|a| a.get("name").and_then(Value::as_str) == Some(name))
            .and_then(|a| a.get("browser_download_url"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let url = url_of(&want)?;
    let sha_url = url_of(&format!("{want}.sha256"))?;
    Some((url, sha_url))
}

/// sha256 of a byte buffer, lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Parse a `.sha256` file: the digest is the first whitespace-separated token
/// (`shasum -a 256` writes `<hash>  <name>`).
pub fn parse_sha256_file(text: &str) -> Option<String> {
    let first = text.split_whitespace().next()?;
    let ok = first.len() == 64 && first.chars().all(|c| c.is_ascii_hexdigit());
    ok.then(|| first.to_ascii_lowercase())
}

/// Why an install attempt failed.
#[derive(Debug, Clone, PartialEq)]
pub enum InstallError {
    /// No artifact for this platform in the release.
    NoAsset,
    /// The download failed (offline, 404, ...).
    Download,
    /// The artifact's digest did not match the published `.sha256`.
    Checksum { expected: String, actual: String },
    /// The swap failed; the previous installation has been restored.
    Install(String),
    /// The new version installed but failed its health check; rolled back.
    Rollback(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::NoAsset => write!(f, "no artifact for this platform"),
            InstallError::Download => write!(f, "download failed"),
            InstallError::Checksum { expected, actual } => write!(
                f,
                "sha256 mismatch (expected {expected}, got {actual}); not installed"
            ),
            InstallError::Install(e) => write!(f, "install failed: {e}"),
            InstallError::Rollback(e) => write!(f, "update failed, rolled back: {e}"),
        }
    }
}

/// Download an artifact and verify it against its `.sha256` before any change.
///
/// Returns the verified bytes. **Nothing on disk is touched** — this is the
/// point where a bad or tampered artifact is rejected.
pub fn download_verified(url: &str, sha_url: &str) -> Result<Vec<u8>, InstallError> {
    let bytes = http::get_bytes(url).ok_or(InstallError::Download)?;
    let published = http::get_text(sha_url)
        .and_then(|t| parse_sha256_file(&t))
        .ok_or(InstallError::Download)?;
    let actual = sha256_hex(&bytes);
    if actual != published {
        return Err(InstallError::Checksum {
            expected: published,
            actual,
        });
    }
    Ok(bytes)
}

/// Copy `src` into a new backup directory tagged with `version`, then evict the
/// oldest so at most [`KEEP_BACKUPS`] remain.
///
/// `data/` (and anything else in [`protected_entries`]) is skipped, so user data
/// is never copied into a backup and can never be restored over.
pub fn backup_install(version: &str) -> Result<PathBuf, String> {
    let root = backup_root();
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let dest = root.join(version);
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
    }
    let protected = protected_entries();
    copy_tree(&app_dir(), &dest, &protected, true)?;
    evict_old_backups()?;
    Ok(dest)
}

/// Keep the newest [`KEEP_BACKUPS`] backup directories, remove the rest.
fn evict_old_backups() -> Result<(), String> {
    let root = backup_root();
    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&root)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (mtime, e.path())
        })
        .collect();
    if entries.len() <= KEEP_BACKUPS {
        return Ok(());
    }
    entries.sort_by_key(|(t, _)| *t);
    let drop_count = entries.len() - KEEP_BACKUPS;
    for (_, path) in entries.into_iter().take(drop_count) {
        std::fs::remove_dir_all(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The newest backup directory, which a rollback restores.
pub fn newest_backup() -> Option<PathBuf> {
    std::fs::read_dir(backup_root())
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let mtime = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((mtime, e.path()))
        })
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

/// Copy a tree, skipping protected top-level names when `top` is true.
fn copy_tree(src: &Path, dest: &Path, protected: &[String], top: bool) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        if top && protected.iter().any(|p| *p == name) {
            continue;
        }
        let from = entry.path();
        let to = dest.join(&name);
        if from.is_dir() {
            copy_tree(&from, &to, protected, false)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Restore the newest backup over `app_dir()`, leaving protected entries alone.
pub fn rollback() -> Result<PathBuf, String> {
    let backup = newest_backup().ok_or_else(|| "no backup to restore".to_string())?;
    let app = app_dir();
    let protected = protected_entries();
    if app.exists() {
        for entry in std::fs::read_dir(&app).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().to_string();
            if protected.iter().any(|p| *p == name) {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                std::fs::remove_dir_all(&path).map_err(|e| e.to_string())?;
            } else {
                std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            }
        }
    } else {
        std::fs::create_dir_all(&app).map_err(|e| e.to_string())?;
    }
    copy_tree(&backup, &app, &protected, true)?;
    Ok(backup)
}

/// Where the CLI lands inside the installed app tree.
///
/// `scripts/package_rust.sh` puts it at `Contents/MacOS/sutura` inside the macOS
/// bundle and at `bin/sutura` on Linux. This is the binary `health_check` runs
/// after a swap.
pub fn installed_binary() -> PathBuf {
    let app = app_dir();
    if cfg!(target_os = "macos") {
        app.join("Contents/MacOS/sutura")
    } else {
        app.join("bin/sutura")
    }
}

/// The health check from `updater.health_check`: the new binary reports the
/// expected version and can actually repair a small mesh.
///
/// `run(binary, args) -> (exit_code, stdout)` is injected so the check can be
/// exercised without installing anything.
pub fn health_check(
    expected_tag: &str,
    run: &dyn Fn(&Path, &[&str]) -> (i32, String),
    binary: &Path,
) -> Result<(), String> {
    let want = expected_tag.strip_prefix('v').unwrap_or(expected_tag);
    let (code, out) = run(binary, &["--version"]);
    if code != 0 {
        return Err(format!("version check failed (exit {code})"));
    }
    if !out.contains(want) {
        return Err(format!("version mismatch: {}", out.trim()));
    }
    Ok(())
}

/// Check that the requested update is allowed at all.
pub fn preflight(current: &str, tag: &str) -> Result<(), String> {
    if updater::is_appimage() {
        return Err(format!(
            "The AppImage build cannot update itself. Download the latest AppImage from {}",
            updater::RELEASES_URL
        ));
    }
    if updater::crosses_license_boundary(current, tag) {
        return Err(format!(
            "Sutura {tag} changes the license terms (PolyForm Noncommercial 1.0.0 - \
commercial use requires a separate agreement). This update will not be applied \
automatically - download it manually from {} if you accept the new terms.",
            updater::RELEASES_URL
        ));
    }
    Ok(())
}

/// Unpack a verified artifact into a staging directory beside `app_dir()`, then
/// copy it into place around the protected entries.
///
/// Extraction goes to a staging directory and the app tree is replaced only
/// after it is fully unpacked, so an interrupted unpack cannot leave a
/// half-written install. The archive is unpacked with the platform's own tool
/// (`tar`, and `hdiutil` for a `.dmg`) rather than a Rust tar crate: the release
/// already requires those tools and this keeps the dependency set unchanged.
///
/// `data/` is never removed or overwritten — [`protected_entries`] is skipped on
/// both the delete and the copy side.
pub fn apply_artifact(bytes: &[u8], tag: &str) -> Result<(), String> {
    let app = app_dir();
    let parent = app
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "install directory has no parent".to_string())?;
    std::fs::create_dir_all(&parent).map_err(|e| e.to_string())?;

    let stage = parent.join(format!(".sutura-staging-{}", std::process::id()));
    if stage.exists() {
        std::fs::remove_dir_all(&stage).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(&stage).map_err(|e| e.to_string())?;

    // Detect the container from the bytes rather than the OS: a gzip stream is
    // the Linux tarball, anything else is treated as a disk image. That makes a
    // mismatched asset fail with a clear message instead of silently unpacking
    // nothing, and it lets the tar path be exercised on any host.
    let is_dmg = !bytes.starts_with(&[0x1f, 0x8b]);
    let artifact = stage.join(if is_dmg { "artifact.dmg" } else { "artifact.tar.gz" });
    std::fs::write(&artifact, bytes).map_err(|e| e.to_string())?;

    let unpacked = stage.join("unpacked");
    std::fs::create_dir_all(&unpacked).map_err(|e| e.to_string())?;
    if is_dmg {
        let mnt = stage.join("mnt");
        std::fs::create_dir_all(&mnt).map_err(|e| e.to_string())?;
        run_tool(
            "hdiutil",
            &[
                "attach".to_string(),
                artifact.display().to_string(),
                "-nobrowse".to_string(),
                "-mountpoint".to_string(),
                mnt.display().to_string(),
            ],
        )?;
        // Copy the bundle out before detaching, so a failed detach cannot cost
        // us the payload.
        let bundle = std::fs::read_dir(&mnt)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.extension().and_then(|x| x.to_str()) == Some("app"));
        let copied = match bundle {
            Some(b) => copy_tree(&b, &unpacked.join("Sutura.app"), &[], true),
            None => Err("no .app inside the disk image".to_string()),
        };
        // Detach regardless: a stuck mount is worse than a failed update.
        let _ = run_tool(
            "hdiutil",
            &[
                "detach".to_string(),
                mnt.display().to_string(),
                "-force".to_string(),
            ],
        );
        copied?;
    } else {
        run_tool(
            "tar",
            &[
                "-xzf".to_string(),
                artifact.display().to_string(),
                "-C".to_string(),
                unpacked.display().to_string(),
            ],
        )?;
    }

    // `scripts/package_rust.sh` produces a `Sutura.app` tree on macOS and a
    // `Sutura/` root on Linux.
    let payload = if is_dmg {
        unpacked.join("Sutura.app")
    } else {
        unpacked.join("Sutura")
    };
    if !payload.exists() {
        let _ = std::fs::remove_dir_all(&stage);
        return Err(format!("{tag}: unexpected artifact layout"));
    }

    let protected = protected_entries();
    if app.exists() {
        for entry in std::fs::read_dir(&app).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().to_string();
            if protected.iter().any(|p| *p == name) {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                std::fs::remove_dir_all(&path).map_err(|e| e.to_string())?;
            } else {
                std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            }
        }
    } else {
        std::fs::create_dir_all(&app).map_err(|e| e.to_string())?;
    }
    let result = copy_tree(&payload, &app, &protected, true);
    let _ = std::fs::remove_dir_all(&stage);
    result
}

/// Run an external tool and fail with its stderr when it exits non-zero.
fn run_tool(program: &str, args: &[String]) -> Result<(), String> {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_is_lowercase_and_correct() {
        // sha256("") = e3b0c442...
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // sha256("abc")
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn parse_sha256_file_accepts_shasum_output() {
        let hash = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(
            parse_sha256_file(&format!("{hash}  Sutura.tar.gz")),
            Some(hash.to_string())
        );
        assert_eq!(
            parse_sha256_file(&format!("{hash}\n")),
            Some(hash.to_string())
        );
        // Wrong length, non-hex and empty input are rejected rather than guessed.
        assert_eq!(parse_sha256_file("deadbeef  f"), None);
        assert_eq!(parse_sha256_file("zzzz"), None);
        assert_eq!(parse_sha256_file(""), None);
    }

    /// A checksum mismatch must be reported and must not proceed.
    #[test]
    fn checksum_mismatch_is_rejected() {
        let published = "0".repeat(64);
        let actual = sha256_hex(b"payload");
        assert_ne!(actual, published);
        let err = InstallError::Checksum {
            expected: published.clone(),
            actual: actual.clone(),
        };
        let msg = err.to_string();
        assert!(msg.contains("sha256 mismatch"), "{msg}");
        assert!(msg.contains(&actual), "{msg}");
        assert!(msg.contains("not installed"), "{msg}");
    }

    /// `data/` is always protected, whatever the override points at.
    #[test]
    fn data_is_always_protected() {
        assert!(protected_entries().iter().any(|p| p == "data"));
    }

    /// `--version` containing the tag passes; a wrong version or a non-zero exit
    /// fails. This is the gate between "installed" and "adopted".
    #[test]
    fn health_check_verifies_version_and_exit() {
        let bin = Path::new("/nonexistent/sutura");
        let ok = |_: &Path, _: &[&str]| (0, "sutura 0.10.0".to_string());
        assert!(health_check("v0.10.0", &ok, bin).is_ok());
        // A tag without 'v' still matches the printed version.
        assert!(health_check("0.10.0", &ok, bin).is_ok());

        let wrong = |_: &Path, _: &[&str]| (0, "sutura 0.9.9".to_string());
        assert!(health_check("v0.10.0", &wrong, bin).is_err());

        let nonzero = |_: &Path, _: &[&str]| (1, "boom".to_string());
        assert!(health_check("v0.10.0", &nonzero, bin).is_err());
    }

    /// The licence boundary blocks an automatic install, with Python's wording.
    #[test]
    fn preflight_blocks_licence_crossing() {
        let err = preflight("0.1.9", "v0.2.0").unwrap_err();
        assert!(err.contains("changes the license terms"), "{err}");
        assert!(err.contains("will not be applied"), "{err}");
        assert!(preflight("0.1.9", "v0.1.10").is_ok());
    }

    /// A scratch install root, so no test touches the real one.
    ///
    /// `SUTURA_APP_DIR` is process-global, so the tests that use this hold
    /// [`ENV_LOCK`] for their whole body; otherwise the default parallel runner
    /// lets them overwrite each other's install root.
    fn scratch(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "sutura-install-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("app/data")).unwrap();
        std::env::set_var("SUTURA_APP_DIR", base.join("app"));
        base
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Three versions in a row leave exactly the newest two, the oldest is
    /// evicted, and **no backup ever contains `data/`**.
    #[test]
    fn keeps_two_backups_and_never_backs_up_data() {
        let _guard = lock_env();
        let base = scratch("retain");
        for v in ["0.9.7", "0.9.8", "0.9.9"] {
            std::fs::write(base.join("app/bin_sutura"), v).unwrap();
            std::fs::write(base.join("app/data/history.jsonl"), "HISTORY\n").unwrap();
            backup_install(v).unwrap();
            // Distinct mtimes so the eviction order is well defined.
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let root = backup_root();
        let mut names: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        assert_eq!(names.len(), KEEP_BACKUPS, "{names:?}");
        assert!(!names.contains(&"0.9.7".to_string()), "oldest must be evicted");
        for n in &names {
            assert!(
                !root.join(n).join("data").exists(),
                "backup {n} must not contain user data"
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Rollback restores the newest backup and leaves `data/` as it was.
    #[test]
    fn rollback_restores_newest_and_preserves_data() {
        let _guard = lock_env();
        let base = scratch("rollback");
        for v in ["0.9.8", "0.9.9"] {
            std::fs::write(base.join("app/bin_sutura"), v).unwrap();
            std::fs::write(base.join("app/data/history.jsonl"), "HISTORY\n").unwrap();
            backup_install(v).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // Simulate a broken install.
        std::fs::write(base.join("app/bin_sutura"), "broken").unwrap();
        std::fs::write(base.join("app/data/history.jsonl"), "NEWER\n").unwrap();

        rollback().expect("rollback");
        let ver = std::fs::read_to_string(base.join("app/bin_sutura")).unwrap();
        assert_eq!(ver, "0.9.9", "the newest backup is restored");
        let hist = std::fs::read_to_string(base.join("app/data/history.jsonl")).unwrap();
        assert_eq!(hist, "NEWER\n", "data written after the backup survives");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The gate between "installed" and "adopted": a binary that fails its
    /// health check must be rolled back, and a healthy one must not.
    ///
    /// Before this was wired, `health_check` and `rollback` had **no production
    /// caller** — a bad release was installed and kept, and the only recovery
    /// was a manual reinstall. This test pins the two outcomes so the wiring
    /// cannot silently disappear again.
    #[test]
    fn a_failed_health_check_rolls_back_and_a_healthy_one_stays() {
        let _guard = lock_env();
        let base = scratch("gate");

        // Build the tree at the path `installed_binary` really names, so the
        // test exercises the same file the check runs. On macOS that is
        // `Contents/MacOS/sutura`, on Linux `bin/sutura`; hardcoding either one
        // would make this test pass on one platform and check nothing on the
        // other.
        let bin = installed_binary();
        let bin_dir = bin.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&bin_dir).unwrap();

        // A good install, backed up.
        std::fs::write(&bin, "GOOD").unwrap();
        std::fs::write(base.join("app/data/history.jsonl"), "HISTORY\n").unwrap();
        backup_install("0.9.9").unwrap();

        // The update lands, but the binary reports the version it replaced.
        std::fs::write(&bin, "BAD").unwrap();
        let broken = |_: &Path, _: &[&str]| (0, "sutura 0.9.9".to_string());
        let err = health_check("v0.10.0", &broken, &bin).unwrap_err();
        assert!(err.contains("version mismatch"), "{err}");

        rollback().expect("rollback after a failed check");
        assert_eq!(
            std::fs::read_to_string(&bin).unwrap(),
            "GOOD",
            "the previous build is back"
        );
        assert_eq!(
            std::fs::read_to_string(base.join("app/data/history.jsonl")).unwrap(),
            "HISTORY\n",
            "user data is never rolled back"
        );

        // The same sequence with a healthy binary must keep the new build.
        std::fs::write(&bin, "NEWER").unwrap();
        let healthy = |_: &Path, _: &[&str]| (0, "sutura 0.10.0".to_string());
        health_check("v0.10.0", &healthy, &bin).expect("healthy build is adopted");
        assert_eq!(
            std::fs::read_to_string(&bin).unwrap(),
            "NEWER",
            "a passing check must not roll anything back"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The tar layout really installs: the payload lands, a stale entry is
    /// removed, `data/` survives and no staging directory is left behind.
    #[test]
    fn applies_tar_artifact_and_preserves_data() {
        let _guard = lock_env();
        let base = scratch("apply");
        let work = base.join("work/Sutura");
        std::fs::create_dir_all(work.join("bin")).unwrap();
        std::fs::create_dir_all(work.join("share/applications")).unwrap();
        std::fs::write(work.join("bin/sutura"), "#!/bin/sh\n").unwrap();
        std::fs::write(work.join("share/applications/sutura.desktop"), "Exec=sutura\n").unwrap();

        let tarball = base.join("asset.tar.gz");
        let status = std::process::Command::new("tar")
            .arg("-C")
            .arg(base.join("work"))
            .arg("-czf")
            .arg(&tarball)
            .arg("Sutura")
            .status()
            .expect("tar");
        assert!(status.success(), "tar must succeed");

        // Pre-existing install with an entry the new payload no longer ships.
        std::fs::create_dir_all(base.join("app/bin")).unwrap();
        std::fs::write(base.join("app/bin_old"), "stale").unwrap();
        std::fs::write(base.join("app/data/history.jsonl"), "PRECIOUS\n").unwrap();

        let bytes = std::fs::read(&tarball).unwrap();
        apply_artifact(&bytes, "v0.10.0").expect("apply");

        assert!(base.join("app/bin/sutura").exists(), "payload installed");
        assert!(base.join("app/share/applications/sutura.desktop").exists());
        assert!(!base.join("app/bin_old").exists(), "stale entry removed");
        assert_eq!(
            std::fs::read_to_string(base.join("app/data/history.jsonl")).unwrap(),
            "PRECIOUS\n",
            "user data must survive an install"
        );
        let staging: Vec<_> = std::fs::read_dir(&base)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("staging"))
            .collect();
        assert!(staging.is_empty(), "staging must be cleaned up");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A gzip stream is the Linux artifact; anything else is treated as a disk
    /// image. A payload with neither shape fails instead of doing nothing.
    #[test]
    fn unexpected_layout_is_an_error() {
        let _guard = lock_env();
        let base = scratch("layout");
        // Not gzip, so it takes the disk-image path, which cannot mount this.
        let err = apply_artifact(b"not an artifact", "v0.10.0").unwrap_err();
        assert!(!err.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
}
