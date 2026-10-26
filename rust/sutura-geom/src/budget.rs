// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cooperative wall-time budget for the CDT3D pipeline stages.
//!
//! The budget is installed on the calling thread only by `run_cdt3d` and is
//! inert when no timeout is configured.  Long loops call [`tick`] every
//! [`CHECK_EVERY`] iterations so a stage-boundary budget cannot be overshot by
//! a whole stage.  Checks never change the computed output: they only return
//! the typed budget error once the deadline has passed and are otherwise a
//! no-op.

use std::cell::Cell;
use std::time::Instant;

/// How many loop iterations pass between two deadline checks.
pub(crate) const CHECK_EVERY: usize = 64;

#[derive(Clone, Copy)]
struct WallBudget {
    start: Instant,
    timeout_ms: u64,
}

thread_local! {
    static WALL_BUDGET: Cell<Option<WallBudget>> = const { Cell::new(None) };
}

/// Installs a deadline for the current thread and clears it on drop.
pub(crate) struct BudgetGuard;

impl BudgetGuard {
    pub(crate) fn install(start: Instant, timeout_ms: u64) -> Self {
        if timeout_ms > 0 {
            WALL_BUDGET.with(|c| c.set(Some(WallBudget { start, timeout_ms })));
        }
        BudgetGuard
    }
}

impl Drop for BudgetGuard {
    fn drop(&mut self) {
        WALL_BUDGET.with(|c| c.set(None));
    }
}

/// Returns the same typed budget error used at the stage boundaries when the
/// installed deadline has passed; `Ok(())` when no budget is installed.
#[inline]
pub(crate) fn check(stage: &str) -> Result<(), String> {
    WALL_BUDGET.with(|c| match c.get() {
        None => Ok(()),
        Some(b) => {
            let ms = b.start.elapsed().as_millis() as u64;
            if ms > b.timeout_ms {
                Err(format!(
                    "cdt3d wall-time budget exceeded after {stage}: {ms} ms > {} ms",
                    b.timeout_ms
                ))
            } else {
                Ok(())
            }
        }
    })
}

/// Checks the deadline once every [`CHECK_EVERY`] iterations of a loop.
#[inline]
pub(crate) fn tick(i: usize, stage: &str) -> Result<(), String> {
    if i % CHECK_EVERY == 0 {
        check(stage)
    } else {
        Ok(())
    }
}
