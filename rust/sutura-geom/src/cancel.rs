// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cooperative SIGINT cancellation for the long native work loops.
//!
//! The CLI installs a temporary signal handler around a long binding call.
//! The handler only stores an atomic flag; the work loop checks it at existing
//! counter/loop checkpoints. No geometry or work counter is changed.

use std::sync::atomic::{AtomicBool, Ordering};

static CANCELLED: AtomicBool = AtomicBool::new(false);

#[inline]
pub(crate) fn is_cancelled() -> bool {
    CANCELLED.load(Ordering::Relaxed)
}

pub(crate) fn reset() {
    CANCELLED.store(false, Ordering::Relaxed);
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
