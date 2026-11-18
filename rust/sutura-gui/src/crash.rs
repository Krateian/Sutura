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

/// Install the panic hook. Idempotent-ish: calling it twice replaces the hook,
/// which is harmless because the new one chains to the default handler the same
/// way the old one did.
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
        let thread = std::thread::current();
        sutura_core::diagnostics::append_crash_log(&format!(
            "panic at {location} (thread {:?}): {message}",
            thread.name().unwrap_or("<unnamed>")
        ));

        // The GUI shows this; a headless run has no window, so the flag just
        // stays set and the default handler prints the usual message.
        SHOWN.store(true, Ordering::SeqCst);
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
    fn hook_records_a_panic_and_sets_the_flag() {
        assert!(!panic_recorded(), "no panic yet in this process");
        // The hook writes to `data_dir()`, which defaults to the **real**
        // ~/.local/share/sutura/data. The first version of this test did exactly
        // that -- it appended two synthetic panics to the developer's own crash
        // log. `SUTURA_DATA_DIR` is the supported override (learning.rs:21), and
        // it is the only sandboxing a crash hook needs.
        let dir = std::env::temp_dir().join(format!("sutura-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SUTURA_DATA_DIR", &dir);

        install_hooks();
        let _ = std::panic::catch_unwind(|| panic!("synthetic crash for the test"));
        assert!(panic_recorded(), "the hook must record the panic");

        // And it stays set: a crash is not a transient state, and the dialog
        // must not disappear if the app survives a second panic.
        let _ = std::panic::catch_unwind(|| panic!("another synthetic crash"));
        assert!(panic_recorded(), "sticky after any panic");

        // The write really went to the sandbox, and is readable there.
        let log = dir.join("crash.log");
        let text = std::fs::read_to_string(&log).expect("the hook must write crash.log");
        assert!(text.contains("synthetic crash"), "the entry is in the log");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
