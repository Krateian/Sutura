// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! One-time migration from a Python install to the Rust distribution.
//!
//! The steps are ordered by what they protect:
//!
//! 1. **Detect** — write nothing.
//! 2. **Quarantine the Python-era backup** *first*. Python's `BACKUP_DIR` and
//!    Rust's `install::backup_root()` are the same path
//!    (`~/.local/share/sutura.backup`) with different layouts: Python writes a
//!    flat `copytree(APP_DIR, BACKUP_DIR)`, Rust writes `backup_root()/<ver>/`
//!    and `evict_old_backups` deletes every subdirectory beyond the newest two.
//!    A flat backup read as "versions" is silently deleted, so it is moved
//!    aside before anything can call the Rust updater.
//! 3. **Move the legacy data files** — the port of `sutura_engine/paths.py::
//!    migrate_data_dir`: never clobber, rename with a copy fallback, never
//!    raise.
//! 4. **Install the Rust tree** — `install::apply_artifact`, which keeps
//!    `data/` in place.
//! 5. **Health check — the gate.** Only a binary that reports the expected
//!    version may be adopted.
//! 6. **Remove the Python runtime**, only after step 5 passed, and only then.
//!
//! Every step is idempotent: a second run finds nothing to do and changes
//! nothing.

use std::path::{Path, PathBuf};

use crate::install;

/// The two files `paths.py` moves out of the legacy install dir.
pub const LEGACY_DATA_FILES: [&str; 2] = ["history.jsonl", "triage_learning.json"];

/// The venv directory names `install.sh` / `install-macos.sh` have used.
pub const VENV_DIRS: [&str; 3] = ["venv", "venv311", "python311"];

/// What the Python install looks like on disk, or `None` when there is nothing
/// to migrate.
#[derive(Clone, Debug, PartialEq)]
pub struct LegacyInstall {
    /// Venv directories found under the app dir.
    pub venvs: Vec<PathBuf>,
    /// Python source files/dirs found under the app dir.
    pub sources: Vec<PathBuf>,
}

/// The work [`detect`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// The install tree this plan was built for.
    ///
    /// Carried here so [`run`] writes only inside the tree it was handed,
    /// instead of re-reading the process-global `SUTURA_APP_DIR`. That read
    /// made the tests race with each other and write a marker into the real
    /// install directory.
    pub app: PathBuf,
    pub legacy: Option<LegacyInstall>,
    /// (source, destination) pairs for the legacy data files.
    pub data_moves: Vec<(PathBuf, PathBuf)>,
    /// Human-readable notes, printed by the caller.
    pub notes: Vec<String>,
}

/// What [`run`] actually did, and what [`rollback`] needs to undo it.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub quarantined: Option<PathBuf>,
    pub moved: Vec<(PathBuf, PathBuf)>,
    pub skipped: Vec<PathBuf>,
    /// True when the app tree was swapped for the Rust payload.
    pub installed: bool,
    pub removed: Vec<PathBuf>,
    pub notes: Vec<String>,
}

/// True when `app` already holds the Rust distribution.
///
/// This is the negative signal: a machine that already has `bin/sutura`
/// (macOS: `Sutura.app/Contents/MacOS/sutura`) has been migrated or installed
/// fresh, so there is nothing to do.
///
/// **Exact names only.** On case-insensitive APFS, `is_file()` on
/// `Contents/MacOS/sutura` matches the Python bundle's `Contents/MacOS/Sutura`
/// GUI launcher. A false positive here is the worst kind: the migration would
/// conclude "already Rust" and skip itself, leaving the Python install in place
/// while reporting success. `install::is_exact_file` compares the directory
/// entry's stored spelling, which is case-sensitive.
pub fn has_rust_layout(app: &Path) -> bool {
    crate::install::is_exact_file(&app.join("bin/sutura"))
        || crate::install::is_exact_file(&app.join("Contents/MacOS/sutura"))
        || crate::install::is_exact_file(&app.join("Sutura.app/Contents/MacOS/sutura"))
}

/// Read the install-owner marker written by `scripts/install_owner.sh`.
///
/// Returns `(installer, conda_env_path)` when present. The marker is the
/// primary signal that a *Python* installer created this tree, but it is only a
/// hint: older installs predate it, so [`detect`] also looks for the venv.
pub fn read_owner_marker(app: &Path) -> Option<(String, String)> {
    let text = std::fs::read_to_string(app.join("data/.sutura-install-owner")).ok()?;
    let mut installer = String::new();
    let mut env_path = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("installer=") {
            installer = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("conda_env_path=") {
            env_path = v.trim().to_string();
        }
    }
    if installer.is_empty() {
        None
    } else {
        Some((installer, env_path))
    }
}

/// Decide what, if anything, needs migrating. Reads only; writes nothing.
pub fn detect(app: &Path, data: &Path) -> Plan {
    let mut plan = Plan {
        app: app.to_path_buf(),
        ..Plan::default()
    };

    if has_rust_layout(app) {
        plan.notes.push("already a Rust install".to_string());
        return plan;
    }

    let mut venvs = Vec::new();
    for name in VENV_DIRS {
        let p = app.join(name);
        if p.is_dir() {
            venvs.push(p);
        }
    }
    let mut sources = Vec::new();
    for name in ["repair.py", "gui.py", "sutura_engine", "sutura"] {
        let p = app.join(name);
        if p.exists() {
            sources.push(p);
        }
    }

    // A tree with neither a venv nor the Python source is not a Python install;
    // it may be a half-removed one, and guessing would be destructive.
    if venvs.is_empty() && sources.is_empty() {
        plan.notes
            .push("no Python install found; nothing to migrate".to_string());
        return plan;
    }

    if let Some((installer, env_path)) = read_owner_marker(app) {
        plan.notes
            .push(format!("installer: {installer} ({env_path})"));
    } else {
        plan.notes
            .push("no install-owner marker; using the venv layout".to_string());
    }

    for name in LEGACY_DATA_FILES {
        let src = app.join(name);
        if src.is_file() {
            plan.data_moves.push((src, data.join(name)));
        }
    }

    plan.legacy = Some(LegacyInstall { venvs, sources });
    plan
}

/// True when `dir` looks like a Python-era *flat* backup rather than the Rust
/// `backup_root()/<version>/` layout.
///
/// Rust's own backups are directories named like versions; anything else at
/// that path came from Python's `copytree(APP_DIR, BACKUP_DIR)`.
pub fn is_legacy_backup_layout(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.filter_map(Result::ok) {
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        // A Rust backup is a version string. `venv`, `sutura`, `sutura_engine`
        // and friends are not.
        let looks_like_version = name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-');
        if !looks_like_version {
            return true;
        }
    }
    false
}

/// Move the Python-era backup aside so the Rust updater cannot misread it.
///
/// One `rename` of the whole directory: nothing inside is deleted, so the
/// user's only pre-update snapshot survives under a name that says where it
/// came from.
pub fn quarantine_backup(root: &Path, stamp: &str) -> Result<Option<PathBuf>, String> {
    if !root.is_dir() || !is_legacy_backup_layout(root) {
        return Ok(None);
    }
    let dest = root.with_file_name(format!(
        "{}.python-{stamp}",
        root.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "sutura.backup".to_string())
    ));
    if dest.exists() {
        // Already quarantined by an earlier run; leave it alone.
        return Ok(Some(dest));
    }
    std::fs::rename(root, &dest).map_err(|e| {
        format!(
            "could not move {root:?} to {dest:?}: {e}. Move it by hand and re-run.",
            root = root,
            dest = dest
        )
    })?;
    Ok(Some(dest))
}

/// Move one legacy file, mirroring `paths.py::migrate_data_dir`.
///
/// Skip when the source is missing or the destination already exists (never
/// clobber). Try a rename first; fall back to a copy that leaves the source in
/// place, which is what a cross-device move needs.
pub fn move_one(src: &Path, dst: &Path) -> std::io::Result<bool> {
    if !src.exists() || dst.exists() {
        return Ok(false);
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(true),
        Err(_) => {
            std::fs::copy(src, dst)?;
            Ok(true)
        }
    }
}

/// Run the migration.
///
/// `payload` is the verified artifact; `tag` is what the health check expects.
/// `run` is the injected process runner (see [`install::health_check`]) so this
/// is testable without installing anything.
pub fn run(
    plan: &Plan,
    tag: &str,
    payload: &[u8],
    stamp: &str,
    run_bin: &dyn Fn(&Path, &[&str]) -> (i32, String),
) -> Result<Report, String> {
    let mut report = Report::default();

    if plan.legacy.is_none() {
        report.notes.push("nothing to migrate".to_string());
        return Ok(report);
    }

    // 1. Quarantine first: eviction would otherwise delete the user's snapshot.
    let root = install::backup_root();
    report.quarantined = quarantine_backup(&root, stamp)?;
    if let Some(q) = &report.quarantined {
        report
            .notes
            .push(format!("Python-era backup kept at {}", q.display()));
    }

    // 2. Legacy data files.
    for (src, dst) in &plan.data_moves {
        match move_one(src, dst) {
            Ok(true) => report.moved.push((src.clone(), dst.clone())),
            Ok(false) => report.skipped.push(src.clone()),
            Err(e) => report
                .notes
                .push(format!("could not move {}: {e}", src.display())),
        }
    }
    for (_, dst) in &report.moved {
        report
            .notes
            .push(format!("history copied into {}", dst.display()));
    }

    // 3. Swap in the Rust tree, keeping `data/` in place.
    //
    // This already removes the Python venv and source from `app_dir`, because
    // `protected_entries()` protects `data/` and nothing else. That is safe
    // only because step 3 is preceded by a backup that contains them: if the
    // health check below fails, `rollback` puts the whole previous tree back,
    // venv included. The explicit deletion in step 5 is therefore not the thing
    // that removes the runtime -- it is what makes the removal permanent once
    // the new build has proved it works.
    let backup = install::backup_install(tag)?;
    report.notes.push(format!("backup at {}", backup.display()));
    // Release notes are fetched before the swap and written only after the
    // health gate, the same ordering every other install path uses: the "what's
    // new" view never waits on a download, and a migration that rolls back
    // leaves the previous snapshot alone.
    let release_notes = crate::whatsnew::fetch_for_tag(tag);
    install::apply_artifact(payload, tag)?;
    report.installed = true;

    // 4. The gate. A build that does not run is rolled back, restoring the
    // Python tree that step 3 displaced.
    let binary = install::installed_binary();
    if let Err(why) = install::health_check(tag, run_bin, &binary) {
        let restored = install::rollback();
        return Err(format!(
            "the new version did not start ({why}); {}",
            match restored {
                Ok(p) => format!("restored {}", p.display()),
                Err(e) => format!("rollback also failed ({e})"),
            }
        ));
    }

    // Installed and running: now the notes may replace the stored copy. Best
    // effort -- a notes failure must never fail a successful migration.
    if let Some(pending) = &release_notes {
        if let Err(e) = crate::whatsnew::commit(pending) {
            report.notes.push(format!("release notes not saved: {e}"));
        }
    }

    // 5. Only now is the removal permanent: drop anything the payload did not
    // overwrite (a Python file the new tree does not ship), so a `python` on
    // this tree cannot silently keep being `sutura`.
    if let Some(legacy) = &plan.legacy {
        for path in legacy.venvs.iter().chain(legacy.sources.iter()) {
            match std::fs::remove_dir_all(path) {
                Ok(()) => report.removed.push(path.clone()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => report
                    .notes
                    .push(format!("could not remove {}: {e}", path.display())),
            }
        }
    }

    // 6. Mark the tree as ours so a second run is a no-op.
    //
    // The marker path comes from the detected tree (`plan`), not from
    // `install::app_dir()`, so this function's writes are exactly the tree it
    // was handed. Reading the process-global here made the tests race: another
    // test's `set_var` could land between `scratch` and this line, and the
    // marker was written into the real install.
    let marker = plan
        .app
        .join("data/.sutura-install-owner");
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        &marker,
        format!("owner=sutura\ninstaller=install-rust.sh\nmigrated_at={stamp}\n"),
    );

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The **same** lock `install.rs` uses, not a second one.
    ///
    /// `SUTURA_APP_DIR` is process-global, so two independent mutexes are no
    /// mutex at all: this module's tests and `install.rs`'s ran concurrently,
    /// one restored the real default while the other was mid-test, and a test
    /// write landed in the user's actual `~/.local/share/sutura`. Sharing the
    /// guard is what makes `set_var` safe here.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::install::tests_env_lock()
    }

    fn scratch(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "sutura-migrate-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("app/data")).unwrap();
        std::env::set_var("SUTURA_APP_DIR", base.join("app"));
        base
    }

    /// A Python install is recognised by its venv, and a Rust layout is not
    /// treated as a migration at all.
    #[test]
    fn detect_finds_a_python_install_and_ignores_a_rust_one() {
        let _g = env_lock();
        let base = scratch("detect");
        let app = base.join("app");

        // Empty tree: not a Python install.
        assert_eq!(detect(&app, &app.join("data")).legacy, None);

        std::fs::create_dir_all(app.join("venv/bin")).unwrap();
        std::fs::write(app.join("repair.py"), "").unwrap();
        std::fs::write(app.join("history.jsonl"), "H\n").unwrap();
        let plan = detect(&app, &app.join("data"));
        let legacy = plan.legacy.expect("venv is a Python install");
        assert_eq!(legacy.venvs.len(), 1);
        assert_eq!(plan.data_moves.len(), 1, "history.jsonl is queued");

        // The Rust layout wins over everything else.
        std::fs::create_dir_all(app.join("bin")).unwrap();
        std::fs::write(app.join("bin/sutura"), "").unwrap();
        assert_eq!(detect(&app, &app.join("data")).legacy, None);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The flat Python backup is moved aside whole; nothing inside is deleted.
    ///
    /// Without this, Rust's `evict_old_backups` reads `venv/`, `sutura/` and
    /// `sutura_engine/` as "versions" and deletes all but the newest two.
    #[test]
    fn quarantine_moves_the_flat_backup_and_deletes_nothing() {
        let _g = env_lock();
        let base = scratch("quarantine");
        let root = base.join("sutura.backup");
        for name in ["venv", "sutura", "sutura_engine"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("f"), name).unwrap();
        }
        assert!(is_legacy_backup_layout(&root));

        let moved = quarantine_backup(&root, "20260101-0000")
            .unwrap()
            .expect("flat backup is quarantined");
        assert!(!root.exists(), "the original path is free for Rust backups");
        for name in ["venv", "sutura", "sutura_engine"] {
            assert_eq!(
                std::fs::read_to_string(moved.join(name).join("f")).unwrap(),
                name,
                "{name} must survive the move"
            );
        }

        // A Rust-style backup (version directories) is left alone.
        let rust_root = base.join("rust.backup");
        std::fs::create_dir_all(rust_root.join("0.9.9")).unwrap();
        assert!(!is_legacy_backup_layout(&rust_root));
        assert_eq!(quarantine_backup(&rust_root, "x").unwrap(), None);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A new version that does not run must leave a working Python install
    /// behind.
    ///
    /// The invariant is subtle and worth pinning: `apply_artifact` does **not**
    /// protect the venv, so the swap itself removes it. What saves the user is
    /// that the swap is preceded by a backup containing it, and the failing
    /// health check triggers `rollback`. So this asserts the venv is back after
    /// a failed check -- which fails if either the backup or the rollback is
    /// dropped.
    #[test]
    fn a_failed_health_check_restores_the_python_tree() {
        let _g = env_lock();
        let base = scratch("gate");
        let app = base.join("app");
        let venv = app.join("venv/bin");
        std::fs::create_dir_all(&venv).unwrap();
        std::fs::write(venv.join("python"), "PYTHON").unwrap();
        std::fs::write(app.join("repair.py"), "OLD").unwrap();
        std::fs::write(app.join("history.jsonl"), "H\n").unwrap();

        // A valid payload, so `apply_artifact` succeeds and the run reaches the
        // health check rather than failing earlier for an unrelated reason.
        let work = base.join("work/Sutura");
        std::fs::create_dir_all(work.join("bin")).unwrap();
        std::fs::write(work.join("bin/sutura"), "#!/bin/sh\n").unwrap();
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
        let payload = std::fs::read(&tarball).unwrap();

        let plan = detect(&app, &app.join("data"));
        assert!(plan.legacy.is_some());

        // The new binary reports the old version, so the gate rejects it.
        let stale = |_: &Path, _: &[&str]| (0, "sutura 0.0.1".to_string());
        let err = run(&plan, "v9.9.9", &payload, "t", &stale).unwrap_err();
        assert!(err.contains("did not start"), "{err}");

        // The rollback restored the tree the swap displaced.
        assert!(
            app.join("venv/bin/python").is_file(),
            "the venv must come back: it is in the backup, not protected"
        );
        assert_eq!(
            std::fs::read_to_string(app.join("repair.py")).unwrap(),
            "OLD",
            "the old source is restored too"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A build that passes the gate keeps the Rust tree and loses the venv.
    #[test]
    fn a_passing_health_check_commits_the_migration() {
        let _g = env_lock();
        let base = scratch("commit");
        let app = base.join("app");
        std::fs::create_dir_all(app.join("venv/bin")).unwrap();
        std::fs::write(app.join("venv/bin/python"), "").unwrap();
        std::fs::write(app.join("repair.py"), "").unwrap();

        let work = base.join("work/Sutura");
        std::fs::create_dir_all(work.join("bin")).unwrap();
        std::fs::write(work.join("bin/sutura"), "#!/bin/sh\n").unwrap();
        let tarball = base.join("asset.tar.gz");
        assert!(std::process::Command::new("tar")
            .arg("-C")
            .arg(base.join("work"))
            .arg("-czf")
            .arg(&tarball)
            .arg("Sutura")
            .status()
            .expect("tar")
            .success());
        let payload = std::fs::read(&tarball).unwrap();

        let plan = detect(&app, &app.join("data"));
        // The installed binary reports the expected version.
        let good = |_: &Path, _: &[&str]| (0, "sutura 9.9.9".to_string());
        let report = run(&plan, "v9.9.9", &payload, "t", &good).expect("commits");

        assert!(report.installed);
        assert!(!app.join("venv").exists(), "the Python runtime is gone");
        assert!(!app.join("repair.py").exists(), "the Python source is gone");
        let marker =
            std::fs::read_to_string(app.join("data/.sutura-install-owner")).unwrap();
        assert!(
            marker.contains("installer=install-rust.sh"),
            "the tree is marked as ours so a re-run is a no-op: {marker}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A second run finds nothing to do and changes nothing.
    #[test]
    fn a_second_run_is_a_no_op() {
        let _g = env_lock();
        let base = scratch("idempotent");
        let app = base.join("app");

        // Already migrated: the Rust layout is present.
        std::fs::create_dir_all(app.join("bin")).unwrap();
        std::fs::write(app.join("bin/sutura"), "").unwrap();
        let plan = detect(&app, &app.join("data"));
        assert_eq!(plan.legacy, None);

        let report = run(&plan, "v9.9.9", b"", "t", &|_, _| (0, String::new()))
            .expect("a no-op run succeeds");
        assert!(!report.installed, "nothing is installed");
        assert!(report.removed.is_empty(), "nothing is removed");
        assert!(report.moved.is_empty(), "nothing is moved");

        let _ = std::fs::remove_dir_all(&base);
    }
}
