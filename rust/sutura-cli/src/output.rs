// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Stdout helper shared by the CLI and its submodules.

/// Write a line to stdout, ending the process quietly if the reader has gone.
///
/// Rust ignores SIGPIPE, so a closed pipe makes `println!` panic with "failed
/// printing to stdout: Broken pipe". Every other Unix filter, and Python's
/// `repair.py`, instead just stop: `sutura --list-methods | head` must print two
/// lines and exit 0, and under `pipefail` `| grep -q` must not turn a successful
/// run into a failure. Measured: Python exits 0 here; a plain SIG_DFL restore
/// gives 141, which still fails the gate, so BrokenPipe is treated as success.
#[macro_export]
macro_rules! outln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let mut out = std::io::stdout().lock();
        if let Err(e) = writeln!(out, $($arg)*) {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                std::process::exit(0);
            }
            // Any other write error is a real failure; report it the same way a
            // failed println! would have, without panicking.
            eprintln!("sutura: write failed: {e}");
            std::process::exit(1);
        }
    }};
}

