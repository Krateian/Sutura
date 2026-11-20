// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! GUI side of the updater: check, confirm, download, install, roll back.
//!
//! The rules this encodes are the ones the CLI and Python share:
//!
//! * **Nothing is installed without consent.** The download only starts after the
//!   user accepts; until then not a byte is written (`gui.py:5939` asks with a
//!   default of No).
//! * A licence-boundary release is never auto-installed — the user is shown the
//!   new terms and pointed at the releases page.
//! * An AppImage cannot update itself.
//! * Notes are snapshotted **before** installing, so "what's new" works offline.
//!
//! Work happens on a thread and reports through a channel, matching the repair
//! worker, so the UI never blocks.

use std::sync::mpsc::{self, Receiver, Sender};

use sutura_core::{install, updater, whatsnew};

/// What the panel is currently doing.
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// No check has run in this session.
    Idle,
    Checking,
    /// A newer release exists and is waiting for the user's decision.
    Available { tag: String },
    /// Newer, but crosses the licence boundary: manual install only.
    License { tag: String },
    Downloading { tag: String },
    /// Downloaded and verified; the swap is in progress.
    Installing { tag: String },
    Done { tag: String, has_notes: bool },
    Failed(String),
}

/// Events from the worker thread.
#[derive(Debug)]
pub enum UpdateEvent {
    Checked {
        status: &'static str,
        tag: Option<String>,
    },
    /// The artifact was verified and the notes are on disk; the swap is next.
    Installing { tag: String },
    /// The swap succeeded. This is the terminal success event.
    Ready { tag: String, has_notes: bool },
    /// Any step failed; the message is shown to the user as-is.
    Failed(String),
}

/// The panel's state.
pub struct Updater {
    pub phase: Phase,
    rx: Option<Receiver<UpdateEvent>>,
    /// Set once a check has produced a decision, so the panel can render it.
    pub current_version: String,
    pub releases_url: &'static str,
}

impl Updater {
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle,
            rx: None,
            current_version: install::current_version(),
            releases_url: updater::RELEASES_URL,
        }
    }

    /// Run the update check on a worker thread.
    pub fn start_check(&mut self, force: bool) {
        if matches!(self.phase, Phase::Checking | Phase::Downloading { .. } | Phase::Installing { .. }) {
            return;
        }
        self.phase = Phase::Checking;
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let current = self.current_version.clone();
        std::thread::spawn(move || {
            let status = updater::check_for_update(&current, force);
            let _ = tx.send(UpdateEvent::Checked {
                status: status.status,
                tag: status.tag,
            });
        });
    }

    /// Download, verify and install `tag`, after the user has agreed.
    ///
    /// Notes are snapshotted before the install so they survive a rollback and
    /// are available with no network afterwards.
    pub fn start_install(&mut self, tag: String) {
        self.phase = Phase::Downloading { tag: tag.clone() };
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let current = self.current_version.clone();
        std::thread::spawn(move || run_install(tx, tag, current));
    }

    /// "Repair installation": re-install the version already running.
    ///
    /// Same worker and the same download/verify/swap/rollback path as an update
    /// -- only the tag differs, and it is the *current* one. Python's
    /// `self_repair.py` did this by rewriting its install tree; a static binary
    /// has no tree to rewrite, so the honest equivalent is a verified reinstall.
    pub fn start_repair_install(&mut self) {
        let tag = format!("v{}", self.current_version);
        self.start_install(tag);
    }

    /// Drain worker events. Returns true when the UI should repaint.
    pub fn poll(&mut self) -> bool {
        let mut events = Vec::new();
        if let Some(rx) = &self.rx {
            while let Ok(ev) = rx.try_recv() {
                events.push(ev);
            }
        }
        if events.is_empty() {
            return false;
        }
        for ev in events {
            if let Some(phase) = phase_for(&ev) {
                self.phase = phase;
            }
        }
        true
    }

    /// Whether the panel needs a repaint of the window.
    pub fn is_busy(&self) -> bool {
        matches!(
            self.phase,
            Phase::Checking | Phase::Downloading { .. } | Phase::Installing { .. }
        )
    }
}

impl Default for Updater {
    fn default() -> Self {
        Self::new()
    }
}

/// The worker body: preflight, fetch, find the asset for this platform,
/// snapshot the notes, download and verify, install, health check, and roll
/// back if the new build does not run.
fn run_install(tx: Sender<UpdateEvent>, tag: String, current: String) {
    if let Err(e) = install::preflight(&current, &tag) {
        let _ = tx.send(UpdateEvent::Failed(e));
        return;
    }
    // The release for the tag we were asked to install -- not `/releases/latest`.
    // For an update those are the same; for "repair installation" the tag is the
    // running version, and fetching latest would find no asset for it (or
    // silently upgrade, which is not what a repair promises).
    let Some(release) = updater::fetch_release_by_tag(&tag) else {
        let _ = tx.send(UpdateEvent::Failed("could not read the release".into()));
        return;
    };
    let Some((url, sha_url)) = install::find_asset(&release, &tag) else {
        let _ = tx.send(UpdateEvent::Failed(format!(
            "no artifact for this platform in {tag}"
        )));
        return;
    };

    // Notes are fetched first, so the "what's new" view never waits on a
    // download — but they are written only after the health check below. A
    // failed or rolled-back install must leave the previous notes in place
    // rather than describing a version that is not installed.
    let notes = release
        .get("body")
        .and_then(|b| b.as_str())
        .map(|body| whatsnew::prepare(&tag, body, &sutura_core::http::get_bytes));

    // Verified bytes, or nothing happens at all.
    let bytes = match install::download_verified(&url, &sha_url) {
        Ok(b) => b,
        Err(e) => {
            let _ = tx.send(UpdateEvent::Failed(e.to_string()));
            return;
        }
    };

    // Tell the UI the swap is starting, so "installing" is visible rather than
    // the panel sitting on "downloading" through the whole install.
    let _ = tx.send(UpdateEvent::Installing { tag: tag.clone() });
    if let Err(e) = install::backup_install(&current)
        .and_then(|_| install::apply_artifact(&bytes, &tag))
    {
        let _ = tx.send(UpdateEvent::Failed(format!("install failed: {e}")));
        return;
    }

    // Health check, then roll back if the new build does not run. Without this
    // the module's promise ("roll back if the new version does not work") was
    // only true of the tests: a bad release was installed and kept, and the
    // only way out was a manual reinstall. Both `health_check` and `rollback`
    // already existed; nothing called them.
    let binary = install::installed_binary();
    if let Err(why) = install::health_check(&tag, &run_binary, &binary) {
        match install::rollback() {
            Ok(restored) => {
                let _ = tx.send(UpdateEvent::Failed(format!(
                    "{tag} did not start ({why}); restored {}",
                    restored.display()
                )));
            }
            Err(e) => {
                // Both the new build and the restore failed. Say exactly where
                // the backup is rather than leaving the user with a dead
                // install and no pointer.
                let where_ = install::newest_backup()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| install::backup_root().display().to_string());
                let _ = tx.send(UpdateEvent::Failed(format!(
                    "{tag} did not start ({why}) and the rollback failed ({e}); \
                     your previous version is at {where_}"
                )));
            }
        }
        return;
    }

    // Installed and running: now the notes may replace the stored copy. Not
    // fatal — the update itself already succeeded.
    if let Some(pending) = &notes {
        if let Err(e) = whatsnew::commit(pending) {
            eprintln!("what's-new snapshot failed: {e}");
        }
    }
    let has_notes = whatsnew::current_tag().is_some();

    let _ = tx.send(UpdateEvent::Ready { tag, has_notes });
}

/// Run the freshly installed binary for [`install::health_check`].
///
/// Kept as a named function rather than a closure so the argument shape matches
/// the injected runner the unit tests use.
fn run_binary(binary: &std::path::Path, args: &[&str]) -> (i32, String) {
    match std::process::Command::new(binary).args(args).output() {
        Ok(out) => (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
        ),
        // A missing or non-executable binary is a failed check, not a panic.
        Err(_) => (-1, String::new()),
    }
}

/// The panel's phase for a worker event, kept separate so the download →
/// install → done transition is testable without network or disk.
fn phase_for(ev: &UpdateEvent) -> Option<Phase> {
    match ev {
        UpdateEvent::Checked { status, tag } => Some(match (status, tag) {
            (&"update", Some(tag)) => Phase::Available { tag: tag.clone() },
            (&"license", Some(tag)) => Phase::License { tag: tag.clone() },
            _ => Phase::Idle,
        }),
        UpdateEvent::Ready { tag, has_notes } => Some(Phase::Done {
            tag: tag.clone(),
            has_notes: *has_notes,
        }),
        UpdateEvent::Failed(msg) => Some(Phase::Failed(msg.clone())),
        UpdateEvent::Installing { tag } => Some(Phase::Installing { tag: tag.clone() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_updater_is_idle_and_has_urls() {
        let u = Updater::new();
        assert_eq!(u.phase, Phase::Idle);
        assert!(u.releases_url.starts_with("https://"));
        assert!(!u.is_busy());
    }

    /// A licence-boundary release must never reach the download step, and the
    /// message must say why. This is checked before any network write.
    #[test]
    fn licence_boundary_fails_before_downloading() {
        let (tx, rx) = mpsc::channel();
        run_install(tx, "v0.2.0".to_string(), "0.1.9".to_string());
        match rx.recv().unwrap() {
            UpdateEvent::Failed(msg) => {
                assert!(msg.contains("changes the license terms"), "{msg}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// The polling loop is a no-op with no worker, so the UI can call it always.
    #[test]
    fn poll_without_worker_is_false() {
        let mut u = Updater::new();
        assert!(!u.poll());
        assert_eq!(u.phase, Phase::Idle);
    }

    /// A second check while one is in flight is ignored rather than spawning a
    /// second worker.
    #[test]
    fn check_is_not_restarted_while_busy() {
        let mut u = Updater::new();
        u.phase = Phase::Checking;
        u.start_check(true);
        assert_eq!(u.phase, Phase::Checking);
    }

    /// The check result maps onto exactly one panel phase, including the licence
    /// case, which must not look installable.
    #[test]
    fn check_results_map_to_phases() {
        assert_eq!(
            phase_for(&UpdateEvent::Checked {
                status: "update",
                tag: Some("v0.10.0".into())
            }),
            Some(Phase::Available {
                tag: "v0.10.0".into()
            })
        );
        assert_eq!(
            phase_for(&UpdateEvent::Checked {
                status: "license",
                tag: Some("v0.2.0".into())
            }),
            Some(Phase::License {
                tag: "v0.2.0".into()
            })
        );
        assert_eq!(
            phase_for(&UpdateEvent::Checked {
                status: "none",
                tag: None
            }),
            Some(Phase::Idle)
        );
    }

    /// The install sequence ends at Done and carries whether notes exist, which
    /// is what decides if "What's new" is offered.
    #[test]
    fn install_events_end_at_done() {
        assert_eq!(
            phase_for(&UpdateEvent::Installing {
                tag: "v0.10.0".into()
            }),
            Some(Phase::Installing {
                tag: "v0.10.0".into()
            })
        );
        assert_eq!(
            phase_for(&UpdateEvent::Ready {
                tag: "v0.10.0".into(),
                has_notes: true
            }),
            Some(Phase::Done {
                tag: "v0.10.0".into(),
                has_notes: true
            })
        );
    }
}
