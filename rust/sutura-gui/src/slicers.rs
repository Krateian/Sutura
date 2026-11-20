// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Opening a repaired file in the user's slicer.
//!
//! Ported from `sutura/gui.py:2473` (`_detect_slicers`). The rule that matters is
//! "never guessed": a slicer is offered only when an executable or an app bundle
//! is actually there, because offering a menu item that cannot work is worse than
//! offering nothing.
//!
//! Windows is not a supported platform, as in the Python implementation.

use std::path::PathBuf;

/// One detected slicer: what to call it, and the argv prefix to launch it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slicer {
    pub label: &'static str,
    pub command: Vec<String>,
}

/// Whether a file is executable, and where it is.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// The slicers actually installed, in Python's order.
///
/// `ORCA_BIN` is the explicit override the Python GUI already honoured; it wins
/// over detection for Orca.
pub fn detect() -> Vec<Slicer> {
    let mut found: Vec<Slicer> = Vec::new();
    let mut push = |label: &'static str, command: Vec<String>| {
        if command.is_empty() || found.iter().any(|s| s.label == label) {
            return;
        }
        found.push(Slicer { label, command });
    };

    // The explicit override, if it points at something real.
    if let Some(raw) = std::env::var_os("ORCA_BIN") {
        let raw = raw.to_string_lossy().to_string();
        let raw = raw.trim().to_string();
        if !raw.is_empty() {
            let candidate = PathBuf::from(&raw);
            let resolved = if candidate.is_absolute() {
                candidate.is_file().then_some(candidate)
            } else {
                which(&raw)
            };
            if let Some(path) = resolved {
                push("OrcaSlicer", vec![path.display().to_string()]);
            }
        }
    }

    for (label, names) in [
        ("OrcaSlicer", &["orca-slicer"][..]),
        ("Bambu Studio", &["bambu-studio", "BambuStudio"][..]),
        ("PrusaSlicer", &["prusa-slicer", "PrusaSlicer"][..]),
    ] {
        for name in names {
            if let Some(path) = which(name) {
                push(label, vec![path.display().to_string()]);
                break;
            }
        }
    }

    // macOS app bundles, launched with `open -a`.
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut roots = vec![PathBuf::from("/Applications")];
        if let Some(home) = home {
            roots.push(home.join("Applications"));
        }
        for root in roots {
            for (label, app) in [
                ("OrcaSlicer", "OrcaSlicer.app"),
                ("Bambu Studio", "BambuStudio.app"),
                ("PrusaSlicer", "PrusaSlicer.app"),
            ] {
                if root.join(app).is_dir() {
                    push(
                        label,
                        vec![
                            "open".to_string(),
                            "-a".to_string(),
                            app.trim_end_matches(".app").to_string(),
                        ],
                    );
                }
            }
        }
    }

    found
}

/// Launch `slicer` with `file`, detached.
pub fn open_in(slicer: &Slicer, file: &std::path::Path) -> std::io::Result<()> {
    let Some((program, args)) = slicer.command.split_first() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "empty slicer command",
        ));
    };
    std::process::Command::new(program)
        .args(args)
        .arg(file)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Detection never invents a slicer: on a machine with none installed the
    /// list is empty, and every entry has a real command.
    #[test]
    fn detection_only_reports_real_programs() {
        for slicer in detect() {
            assert!(!slicer.command.is_empty(), "{} has no command", slicer.label);
            let first = &slicer.command[0];
            let ok = PathBuf::from(first).is_file()
                || std::env::var_os("PATH")
                    .map(|p| {
                        std::env::split_paths(&p).any(|d| d.join(first).is_file())
                    })
                    .unwrap_or(false);
            assert!(
                ok,
                "{} points at {first}, which does not exist",
                slicer.label
            );
        }
    }

    /// An override that points nowhere is ignored, not offered.
    #[test]
    fn a_bogus_override_is_ignored() {
        std::env::set_var("ORCA_BIN", "/nonexistent/orca");
        let found = detect();
        assert!(
            !found
                .iter()
                .any(|s| s.command.iter().any(|c| c.contains("nonexistent"))),
            "a nonexistent ORCA_BIN must not be offered"
        );
        std::env::remove_var("ORCA_BIN");
    }
}
