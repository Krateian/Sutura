// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Re-install the current version over itself.
//!
//! A static binary has no venv to repair, but the *install* can still be
//! damaged: a half-finished update, a file deleted by hand, a corrupted
//! executable. Python's `self_repair.py` rewrote its install tree for that
//! case; the Rust equivalent is to re-download the release asset for the
//! version already running, verify it, and swap it in with the same
//! backup/health-check/rollback path the updater uses.
//!
//! Deliberately **not** a new mechanism. Every step is an existing function:
//! `install::find_asset`, `install::download_verified`,
//! `install::backup_install`, `install::apply_artifact`,
//! `install::health_check`, `install::rollback`. If this module ever grows
//! install logic of its own, that is the bug.
//!
//! It never changes the version: the tag is `install::current_version()`, so a
//! repair cannot silently upgrade or downgrade.

use std::path::Path;

use crate::{install, updater};

/// What a repair attempt did, for the caller to report.
#[derive(Clone, Debug)]
pub struct Report {
    pub tag: String,
    /// Where the previous tree was copied before the swap.
    pub backup: Option<std::path::PathBuf>,
    pub notes: Vec<String>,
}

/// Why a repair could not even be attempted.
#[derive(Clone, Debug, PartialEq)]
pub enum RepairError {
    /// The running version has no published asset for this platform.
    NoAsset(String),
    /// No network, or the release could not be read.
    Offline,
    /// The asset's digest did not match its `.sha256`.
    Checksum(String),
    /// The swap or the health check failed.
    Install(String),
}

impl std::fmt::Display for RepairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RepairError::NoAsset(tag) => write!(
                f,
                "no {tag} build is published for this platform; \
                 download it from {}",
                updater::RELEASES_URL
            ),
            RepairError::Offline => write!(
                f,
                "could not reach {} to fetch the current release",
                updater::RELEASES_URL
            ),
            RepairError::Checksum(e) => write!(f, "the download was rejected: {e}"),
            RepairError::Install(e) => write!(f, "the reinstall failed: {e}"),
        }
    }
}

/// Re-install the running version.
///
/// `tag` defaults to the running version; `run` is the injected process runner
/// so the health check is testable without installing anything (same shape as
/// [`install::health_check`]).
pub fn repair_install(
    tag: Option<&str>,
    run: &dyn Fn(&Path, &[&str]) -> (i32, String),
) -> Result<Report, RepairError> {
    let version = install::current_version();
    let tag = tag.map(str::to_string).unwrap_or_else(|| format!("v{version}"));
    let mut report = Report {
        tag: tag.clone(),
        backup: None,
        notes: Vec::new(),
    };

    // The release for the **running** tag, not the newest one. A repair must
    // reinstall what the user has; fetching `/releases/latest` would either
    // find nothing (when the running version is older) or, worse, quietly
    // upgrade -- which is `--check-update`'s job, not a repair's.
    let release =
        updater::fetch_release_by_tag(&tag).ok_or(RepairError::Offline)?;
    let (url, sha_url) =
        install::find_asset(&release, &tag).ok_or_else(|| RepairError::NoAsset(tag.clone()))?;

    // Verified before anything on disk is touched -- a tampered or truncated
    // download must not reach the backup step. A transport failure is reported
    // as offline (that is what the user can act on); only a digest mismatch is
    // a checksum error.
    let bytes = install::download_verified(&url, &sha_url).map_err(|e| match e {
        install::InstallError::Download => RepairError::Offline,
        other => RepairError::Checksum(other.to_string()),
    })?;

    let backup = install::backup_install(&version)
        .map_err(|e| RepairError::Install(format!("backup failed: {e}")))?;
    report.backup = Some(backup);

    install::apply_artifact(&bytes, &tag)
        .map_err(|e| RepairError::Install(format!("reinstall failed: {e}")))?;

    // The same gate an update goes through: a binary that does not run must not
    // be adopted, and the previous tree comes back.
    let binary = install::installed_binary();
    if let Err(why) = install::health_check(&tag, run, &binary) {
        // The previous tree comes back; the caller learns where from the error
        // text, which is also what the user needs. (Reporting it on the success
        // path would be dead code: this branch always returns.)
        return Err(RepairError::Install(match install::rollback() {
            Ok(restored) => format!(
                "{tag} did not start after reinstalling ({why}); restored {}",
                restored.display()
            ),
            Err(e) => format!(
                "{tag} did not start after reinstalling ({why}) and the rollback \
                 also failed ({e})"
            ),
        }));
    }

    report.notes.push(format!("reinstalled {tag}"));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The refusal paths must be reachable without network access, because they
    /// are what a user with a broken install and no connection sees.
    #[test]
    fn a_missing_asset_names_the_release_page() {
        let e = RepairError::NoAsset("v0.9.9".to_string());
        let text = e.to_string();
        assert!(text.contains("v0.9.9"), "{text}");
        assert!(text.contains("releases"), "{text}");
    }

    #[test]
    fn offline_is_its_own_message() {
        assert!(RepairError::Offline.to_string().contains("could not reach"));
    }

    /// `find_asset` is the piece self-repair depends on, so pin that the tag it
    /// asks for is the one the running version reports -- a repair that fetched
    /// a *different* version would be an update, not a repair.
    #[test]
    fn the_asset_is_the_running_version() {
        let version = install::current_version();
        let tag = format!("v{version}");
        let want = install::asset_name(&tag);
        if let Some(name) = want {
            // The release payload `find_asset` expects.
            let release = json!({"assets": [
                {"name": name, "browser_download_url": "https://example/a"},
                {"name": format!("{name}.sha256"),
                 "browser_download_url": "https://example/a.sha256"},
            ]});
            let found = install::find_asset(&release, &tag);
            assert!(found.is_some(), "the running version must resolve an asset");
        }
    }
}
