// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Cooperative SIGINT cancellation for the long native work loops.
//!
//! The CLI installs a temporary signal handler around a long binding call.
//! The handler only stores an atomic flag; the work loop checks it at existing
//! counter/loop checkpoints. No geometry or work counter is changed.

use std::sync::atomic::{AtomicBool, Ordering};

static CANCELLED: AtomicBool = AtomicBool::new(false);

/// Public read for crates that cannot take a `&dyn Cancel` (the geometry
/// kernels use this global instead).
#[inline]
pub fn is_cancelled() -> bool {
    CANCELLED.load(Ordering::Relaxed)
}

/// Clear the global flag before a new repair run in the same process.
pub fn reset() {
    CANCELLED.store(false, Ordering::Relaxed);
}

/// Set the global flag from the caller's cancel token, so geometry kernels
/// that cannot take `&dyn Cancel` observe the same request.
pub fn request() {
    CANCELLED.store(true, Ordering::Relaxed);
}

#[cfg(unix)]
unsafe extern "C" fn on_sigint(_sig: libc::c_int) {
    CANCELLED.store(true, Ordering::Relaxed);
}

#[cfg(unix)]
pub(crate) struct SignalGuard {
    old: libc::sigaction,
}

#[cfg(unix)]
impl SignalGuard {
    pub(crate) fn install() -> Self {
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = on_sigint as *const () as usize;
            libc::sigemptyset(&mut act.sa_mask);
            act.sa_flags = 0;
            libc::sigaction(libc::SIGINT, &act, &mut old);
            SignalGuard { old }
        }
    }
}

#[cfg(unix)]
impl Drop for SignalGuard {
    fn drop(&mut self) {
        unsafe {
            libc::sigaction(libc::SIGINT, &self.old, std::ptr::null_mut());
        }
    }
}

#[cfg(not(unix))]
pub(crate) struct SignalGuard;
#[cfg(not(unix))]
impl SignalGuard {
    pub(crate) fn install() -> Self { SignalGuard }
}

/// Read-only cancellation probe shared with `sutura-core`.
///
/// The pipeline owns the concrete token (`CancelToken`); geometry loops only
/// need a cheap read.  `None` preserves the historical uncancellable behaviour
/// for bridge and unit-test call sites.
pub trait Cancel: Sync {
    fn is_cancelled(&self) -> bool;
}

/// A probe that never cancels, for call sites without a token.
pub struct NeverCancel;

impl Cancel for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[inline]
pub fn cancelled(cancel: Option<&dyn Cancel>) -> bool {
    cancel.map(|c| c.is_cancelled()).unwrap_or(false)
}
