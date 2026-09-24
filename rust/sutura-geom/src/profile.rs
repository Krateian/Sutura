//! Optional profiling instrumentation for Phase C performance work.
//!
//! Everything here is a no-op unless the crate is built with the `profile`
//! feature.  Timers and counters use `std::sync::atomic` so they are safe to
//! update from multiple threads, although the Phase B pipeline is currently
//! serial.

#[cfg(feature = "profile")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "profile")]
use std::sync::Mutex;
#[cfg(feature = "profile")]
use std::time::Instant;

/// A profiling phase: elapsed wall time plus an event counter.
#[allow(dead_code)]
pub struct Phase {
    name: &'static str,
    #[cfg(feature = "profile")]
    elapsed_ns: AtomicU64,
    #[cfg(feature = "profile")]
    start: Mutex<Option<Instant>>,
    #[cfg(feature = "profile")]
    count: AtomicU64,
}

impl Phase {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            #[cfg(feature = "profile")]
            elapsed_ns: AtomicU64::new(0),
            #[cfg(feature = "profile")]
            start: Mutex::new(None),
            #[cfg(feature = "profile")]
            count: AtomicU64::new(0),
        }
    }

    /// Start timing this phase.  Nested starts without an intervening `end`
    /// overwrite the previous start (the earlier partial interval is lost).
    #[inline]
    pub fn begin(&self) {
        #[cfg(feature = "profile")]
        {
            *self.start.lock().unwrap() = Some(Instant::now());
        }
    }

    /// Stop timing and add the interval to the accumulated total.
    #[inline]
    pub fn end(&self) {
        #[cfg(feature = "profile")]
        {
            let mut s = self.start.lock().unwrap();
            if let Some(t) = s.take() {
                let ns = t.elapsed().as_nanos() as u64;
                self.elapsed_ns.fetch_add(ns, Ordering::Relaxed);
            }
        }
    }

    /// Add `n` to the event counter.
    #[inline]
    pub fn add_count(&self, _n: u64) {
        #[cfg(feature = "profile")]
        self.count.fetch_add(_n, Ordering::Relaxed);
    }

    #[cfg(feature = "profile")]
    fn seconds(&self) -> f64 {
        self.elapsed_ns.load(Ordering::Relaxed) as f64 / 1e9
    }

    #[cfg(feature = "profile")]
    fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// If this phase is currently running, add its in-flight interval to the
    /// accumulated total.  Call this before reading the report or on timeout.
    #[inline]
    pub fn flush_partial(&self) {
        #[cfg(feature = "profile")]
        {
            let mut s = self.start.lock().unwrap();
            if let Some(t) = s.take() {
                let ns = t.elapsed().as_nanos() as u64;
                self.elapsed_ns.fetch_add(ns, Ordering::Relaxed);
            }
        }
    }
}

macro_rules! declare_phase {
    ($name:ident, $label:expr) => {
        pub static $name: Phase = Phase::new($label);
    };
}

// Phases are declared unconditionally so macros can reference them whether or
// not the `profile` feature is enabled.  When the feature is off they are
// zero-sized no-ops.
declare_phase!(BROAD_PHASE, "broad_phase");
declare_phase!(CLASSIFY_PAIRS, "classify_pairs");
declare_phase!(IMPLICIT_CONSTRUCTION, "implicit_construction");
declare_phase!(CDT_PER_HOST, "cdt_per_host");
declare_phase!(WELD_AND_OUTPUT, "weld_and_output");
declare_phase!(PYO3_MARSHAL, "pyo3_marshal");

// Simple counters (time-less phases).
declare_phase!(ORIENT3D_EXPLICIT, "orient3d_explicit");
declare_phase!(ORIENT3D_IMPLICIT, "orient3d_implicit");
declare_phase!(ORIENT2D_CALLS, "orient2d_calls");
declare_phase!(INCIRCLE_CALLS, "incircle_calls");
declare_phase!(PROPER_SI_PAIRS, "proper_si_pairs");
declare_phase!(TOUCH_POINTS, "touch_points");
declare_phase!(TOUCH_SEGMENTS, "touch_segments");
declare_phase!(COPLANAR_OVERLAPS, "coplanar_overlaps");

/// Reset all timers and counters.  Useful when running multiple inputs in one
/// process.
pub fn reset() {
    #[cfg(feature = "profile")]
    {
        for p in [
            &BROAD_PHASE,
            &CLASSIFY_PAIRS,
            &IMPLICIT_CONSTRUCTION,
            &CDT_PER_HOST,
            &WELD_AND_OUTPUT,
            &PYO3_MARSHAL,
            &ORIENT3D_EXPLICIT,
            &ORIENT3D_IMPLICIT,
            &ORIENT2D_CALLS,
            &INCIRCLE_CALLS,
            &PROPER_SI_PAIRS,
            &TOUCH_POINTS,
            &TOUCH_SEGMENTS,
            &COPLANAR_OVERLAPS,
        ] {
            p.elapsed_ns.store(0, Ordering::Relaxed);
            p.count.store(0, Ordering::Relaxed);
            let mut s = p.start.lock().unwrap();
            *s = None;
        }
    }
}

/// Flush any running phase timers so the report reflects partial progress.
pub fn flush_partial_all() {
    #[cfg(feature = "profile")]
    {
        for p in [
            &BROAD_PHASE,
            &CLASSIFY_PAIRS,
            &IMPLICIT_CONSTRUCTION,
            &CDT_PER_HOST,
            &WELD_AND_OUTPUT,
            &PYO3_MARSHAL,
        ] {
            p.flush_partial();
        }
    }
}

/// Return a one-line snapshot of the current timers and counters for stderr.
#[cfg(feature = "profile")]
pub fn heartbeat_line() -> String {
    use std::fmt::Write;
    flush_partial_all();
    let mut out = String::new();
    write!(
        out,
        "PROFILE broad_phase={:.2}s classify_pairs={:.2}s implicit_construction={:.2}s cdt_per_host={:.2}s weld_and_output={:.2}s pyo3_marshal={:.2}s",
        BROAD_PHASE.seconds(),
        CLASSIFY_PAIRS.seconds(),
        IMPLICIT_CONSTRUCTION.seconds(),
        CDT_PER_HOST.seconds(),
        WELD_AND_OUTPUT.seconds(),
        PYO3_MARSHAL.seconds(),
    )
    .unwrap();
    out
}

#[cfg(not(feature = "profile"))]
pub fn heartbeat_line() -> String {
    "PROFILE disabled".to_string()
}

/// Return a markdown table of the current timers and counters.
#[cfg(feature = "profile")]
pub fn report() -> String {
    use std::fmt::Write;
    flush_partial_all();
    let total = BROAD_PHASE.seconds()
        + CLASSIFY_PAIRS.seconds()
        + IMPLICIT_CONSTRUCTION.seconds()
        + CDT_PER_HOST.seconds()
        + WELD_AND_OUTPUT.seconds()
        + PYO3_MARSHAL.seconds();
    let mut out = String::new();
    writeln!(
        out,
        "| Phase | Time (s) | % of total | Count |\n|---|---|---|---|"
    )
    .unwrap();
    let phases = [
        (&BROAD_PHASE, BROAD_PHASE.count()),
        (&CLASSIFY_PAIRS, CLASSIFY_PAIRS.count()),
        (&IMPLICIT_CONSTRUCTION, IMPLICIT_CONSTRUCTION.count()),
        (&CDT_PER_HOST, CDT_PER_HOST.count()),
        (&WELD_AND_OUTPUT, WELD_AND_OUTPUT.count()),
        (&PYO3_MARSHAL, PYO3_MARSHAL.count()),
    ];
    for (p, c) in phases {
        let pct = if total > 0.0 {
            100.0 * p.seconds() / total
        } else {
            0.0
        };
        writeln!(
            out,
            "| {} | {:.4} | {:.1} | {} |",
            p.name,
            p.seconds(),
            pct,
            c
        )
        .unwrap();
    }
    writeln!(
        out,
        "\n**Counters:** orient3d_explicit={}, orient3d_implicit={}, orient2d={}, incircle={}, proper_si={}, touch_point={}, touch_segment={}, coplanar_overlap={}",
        ORIENT3D_EXPLICIT.count(),
        ORIENT3D_IMPLICIT.count(),
        ORIENT2D_CALLS.count(),
        INCIRCLE_CALLS.count(),
        PROPER_SI_PAIRS.count(),
        TOUCH_POINTS.count(),
        TOUCH_SEGMENTS.count(),
        COPLANAR_OVERLAPS.count(),
    ).unwrap();
    writeln!(out, "\n**Total instrumented time:** {:.4} s", total).unwrap();
    out
}

#[cfg(not(feature = "profile"))]
pub fn report() -> String {
    "Profiling disabled: build with --features profile\n".to_string()
}

#[macro_export]
macro_rules! profile_time {
    ($phase:ident, $expr:expr) => {{
        $crate::profile::$phase.begin();
        let __result = $expr;
        $crate::profile::$phase.end();
        __result
    }};
}

#[macro_export]
macro_rules! profile_count {
    ($phase:ident, $n:expr) => {
        $crate::profile::$phase.add_count($n)
    };
}
