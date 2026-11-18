// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Crash reporting for the GUI.
//!
//! Ports the intent of Python's `_install_crash_hooks` (`gui.py:7684`): a panic
//! must leave a trace and the user must be told, once, that a diagnostics
//! archive would help. Python chains `faulthandler` plus excepthooks; the Rust
//! equivalent of an uncaught exception is a panic, so one hook covers it.
//!
//! The log is bounded by the *reader* (`diagnostics::crash_log_tail` keeps the
//! last 200 lines), not by truncating here, so the append stays O(1) and a
//! repeated failure cannot make the file grow without limit during one run.

use std::sync::atomic::{AtomicBool, Ordering};

/// Once, per process: a panic loop would otherwise stack a dialog per iteration.
static SHOWN: AtomicBool = AtomicBool::new(false);

/// Record one panic: the text, and the flag the dialog reads.
///
/// Split out of the hook so a test can exercise the *recording* without
/// installing a process-global hook. That matters: `install_hooks` cannot be
/// uninstalled, so a test that installs it makes every later panic in the same
/// test process -- including a panic in an unrelated, parallel test -- write to
/// `data_dir()`, which is the real `~/.local/share/sutura/data`. That is exactly
/// how this file's first version scribbled on the developer's own crash log.
pub fn record(location: &str, message: &str, thread: &str) {
    sutura_core::diagnostics::append_crash_log(&format!(
        "panic at {location} (thread {thread}): {message}"
    ));
    SHOWN.store(true, Ordering::SeqCst);
}

/// Install the panic hook. Call once, first thing in `main`.
pub fn install_hooks() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Record first: whatever happens next, the log exists.
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".to_string());
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".to_string());
        let current = std::thread::current();
        record(
            &location,
            &message,
            &format!("{:?}", current.name().unwrap_or("<unnamed>")),
        );

        // The default handler still prints the usual message and unwinds; the
        // GUI reads `panic_recorded` to decide whether to show its dialog.
        default(info);
    }));
}

/// True once a panic has been recorded in this process.
pub fn panic_recorded() -> bool {
    SHOWN.load(Ordering::SeqCst)
}

/// Whether there is a crash log to look at at all, from a previous run too.
pub fn crash_log_present() -> bool {
    sutura_core::diagnostics::crash_log_path().is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A caught panic must set the flag the dialog reads.
    ///
    /// ONE test on purpose: the hook and the flag are process-global, so two
    /// tests would race under the parallel harness -- and a panic hook cannot be
    /// uninstalled, so the second test would be observing the first one's state
    /// rather than its own.
    ///
    /// The flag is what the GUI uses to decide whether to show the dialog, so
    /// this is the part worth pinning; that the *file* gets an entry is
    /// covered by `diagnostics::archive_contains_no_personal_paths`, which
    /// checks the crash entry end to end.
    #[test]
    fn record_writes_the_log_and_sets_the_flag() {
        assert!(!panic_recorded(), "nothing recorded yet in this process");

        // A temp data dir, so nothing can reach the real ~/.local. No hook is
        // installed: `record` is the piece under test, and installing the global
        // hook in a test is what wrote to the developer's own crash log.
        let dir = std::env::temp_dir().join(format!("sutura-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var(sutura_core::config::learning::DATA_ENV, &dir);

        record("src/x.rs:1:1", "synthetic crash", "main");
        assert!(panic_recorded(), "recording must set the flag the dialog reads");

        let log = dir.join("crash.log");
        let text = std::fs::read_to_string(&log).expect("the entry must be written");
        assert!(text.contains("synthetic crash"), "message recorded: {text}");
        assert!(text.contains("src/x.rs:1:1"), "location recorded: {text}");

        // A second recording appends rather than replacing.
        record("src/y.rs:2:2", "second crash", "main");
        let text = std::fs::read_to_string(&log).unwrap();
        assert_eq!(text.lines().count(), 2, "both entries are kept");
        assert!(panic_recorded(), "the flag is sticky");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
