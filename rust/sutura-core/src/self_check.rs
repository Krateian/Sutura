// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! `--self-check`: deterministic health and method verification.
//!
//! Mirrors `sutura/self_check.py`. Every registered method is classified as
//! `ok`, `skip` or `fail`, with the same `mode` vocabulary Python uses:
//!
//! * `availability` — the method cannot run at all on this machine (a missing
//!   external binary). No fixture is attempted.
//! * `fixture` — the method actually ran on the deterministic fixture.
//! * `availability_only` — the method is compiled in but is too slow for the
//!   self-check budget (iterative solvers, subprocess tetrahedralization) or
//!   needs manual input, so its presence is asserted without running it.
//! * `skipped` — an optional addon that is not installed.
//!
//! The accounting fields (`total`/`ok`/`failed`/`skipped`) feed
//! `report::health::human_line` unchanged, so the summary line is identical.

use serde_json::{json, Map, Value};
use std::time::Instant;

use crate::ranking::{MethodInfo, METHODS};
use crate::repair::RepairOptions;

/// The deterministic fixture: a 6-vertex, 6-triangle open shell (an octahedron
/// missing two adjacent faces, leaving one boundary loop).
///
/// Values are pinned from `self_check.get_self_check_fixture`; the arrays are
/// written out rather than generated so both sides cannot drift.
pub fn fixture() -> (Vec<[f32; 3]>, Vec<[i32; 3]>) {
    let verts = vec![
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [-1.0, 0.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 0.0, -1.0],
    ];
    let tris = vec![[0, 1, 2], [0, 2, 3], [0, 3, 4], [5, 2, 1], [5, 3, 2], [5, 4, 3]];
    (verts, tris)
}

/// Methods too slow for the self-check budget.
///
/// Same set as Python's `AVAILABILITY_ONLY_METHODS`. Methods that need manual
/// input (`transplant_plus`, `wall_thicken`, `dressing`, `tamponade`, `splint`)
/// are asserted rather than run, because there is no interactive input on the
/// fixture — also matching Python, which only runs non-interactive methods.
const AVAILABILITY_ONLY: [&str; 5] = [
    "balloon",
    "scaffold",
    "mirror_complete",
    "ftetwild",
    "transplant_plus",
];

/// Availability of a method on this machine: `None` when it can run, otherwise
/// the (status, reason) pair Python's `check_availability_detail` would give.
///
/// A missing **optional** addon is `skip`, not `fail` — `health.py` is explicit
/// that fTetWild and friends are optional (`KIND_SKIPPED`), so they must not
/// turn the self-check red. Only a *required* component that is broken is a
/// failure, and that cannot happen in the Rust build, where everything except
/// the external fTetWild binary is compiled in.
fn availability(m: &MethodInfo) -> Option<(&'static str, String)> {
    if m.id == "ftetwild" {
        let ok = std::env::var_os("SUTURA_FTETWILD_BIN")
            .map(|p| !p.is_empty() && std::path::Path::new(&p).exists())
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| {
                        std::path::Path::new(&h)
                            .join(".local/share/sutura/ftetwild/bin/ftetwild")
                            .exists()
                    })
                    .unwrap_or(false)
            });
        if !ok {
            return Some(("skip", "fTetWild (pytetwild) is not installed".into()));
        }
    }
    if !m.available {
        return Some(("skip", "optional addon not installed".into()));
    }
    None
}

/// Run the self-check suite. `run_fixtures=false` only asserts availability.
pub fn run(run_fixtures: bool) -> Value {
    let (verts, tris) = fixture();
    let started = Instant::now();
    let mut methods = Map::new();

    for m in METHODS.iter() {
        if let Some((status, reason)) = availability(m) {
            // Python's `run_self_check`: a SKIP keeps `mode: "skipped"` and an
            // elapsed of 0; the fixture is never attempted.
            methods.insert(
                m.id.to_string(),
                json!({
                    "num": m.num,
                    "name": m.name,
                    "status": status,
                    "reason": reason,
                    "elapsed_s": 0.0,
                    "mode": if status == "skip" { "skipped" } else { "availability" },
                }),
            );
            continue;
        }
        // Python's `run_self_check` does NOT consult `needs_user_input` here;
        // only the AVAILABILITY_ONLY set is asserted. Methods that need manual
        // input still have well-defined behaviour on the fixture (they simply
        // decline to adopt), so running them is both safe and what Python does.
        if !run_fixtures || AVAILABILITY_ONLY.contains(&m.id) {
            methods.insert(
                m.id.to_string(),
                json!({
                    "num": m.num,
                    "name": m.name,
                    "status": "ok",
                    "reason": Value::Null,
                    "elapsed_s": 0.0,
                    "mode": "availability_only",
                }),
            );
            continue;
        }

        // Fast, non-interactive method: run it on the fixture for real.
        let t0 = Instant::now();
        let mut opts = RepairOptions::default();
        opts.forced_methods = vec![m.num];
        let token = crate::repair::CancelToken::new();
        // `repair` takes the wide buffers; the fixture is the same geometry
        // Python hands to `m.run`, just widened from float32/int32.
        let wide_v: Vec<[f64; 3]> = verts
            .iter()
            .map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])])
            .collect();
        let wide_t: Vec<[i64; 3]> = tris
            .iter()
            .map(|t| [i64::from(t[0]), i64::from(t[1]), i64::from(t[2])])
            .collect();
        let outcome = crate::repair::repair(&wide_v, &wide_t, &opts, &crate::repair::NoopSink, &token);
        let dt = t0.elapsed().as_secs_f64();
        let (status, reason) = match outcome {
            crate::repair::RepairOutcome::Ok { .. } => ("ok", Value::Null),
            crate::repair::RepairOutcome::Unavailable { tier, reason } => (
                "fail",
                Value::from(format!("fixture execution failed: {tier} unavailable: {reason}")),
            ),
            crate::repair::RepairOutcome::Error { message } => {
                ("fail", Value::from(format!("fixture execution failed: {message}")))
            }
        };
        methods.insert(
            m.id.to_string(),
            json!({
                "num": m.num,
                "name": m.name,
                "status": status,
                "reason": reason,
                "elapsed_s": dt,
                "mode": "fixture",
            }),
        );
    }

    let mut ok = 0;
    let mut failed = 0;
    let mut skipped = 0;
    for r in methods.values() {
        match r.get("status").and_then(Value::as_str) {
            Some("ok") => ok += 1,
            Some("fail") => failed += 1,
            Some("skip") => skipped += 1,
            _ => {}
        }
    }
    json!({
        "total": methods.len(),
        "ok": ok,
        "failed": failed,
        "skipped": skipped,
        "total_time_s": started.elapsed().as_secs_f64(),
        "methods": Value::Object(methods),
    })
}

/// The human report, byte-matching `run_self_check_cli`'s per-method lines.
pub fn human_text(report: &Value) -> String {
    let mut lines = vec!["Sutura Self-Check:".to_string()];
    let empty = Vec::new();
    let mut entries: Vec<(&String, &Value)> = report
        .get("methods")
        .and_then(Value::as_object)
        .map(|m| m.iter().collect())
        .unwrap_or(empty);
    entries.sort_by_key(|(_, r)| r.get("num").and_then(Value::as_u64).unwrap_or(0));

    for (id, r) in entries {
        let status = r.get("status").and_then(Value::as_str).unwrap_or("");
        let num = r.get("num").and_then(Value::as_u64).unwrap_or(0);
        let name = r
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(id);
        let mode = r.get("mode").and_then(Value::as_str).unwrap_or("");
        let elapsed = r.get("elapsed_s").and_then(Value::as_f64).unwrap_or(0.0);
        let reason = r.get("reason").and_then(Value::as_str);

        match status {
            "ok" => {
                let detail = if mode == "fixture" {
                    format!("({elapsed:.3}s)")
                } else {
                    "(available)".to_string()
                };
                lines.push(format!("  [OK]   #{num:2} {name:<18} {detail}"));
            }
            "skip" => lines.push(format!(
                "  [SKIP] #{num:2} {name:<18} ({})",
                reason.unwrap_or("optional addon not installed")
            )),
            _ => lines.push(format!(
                "  [FAIL] #{num:2} {name:<18} ({})",
                reason.unwrap_or("failed")
            )),
        }
    }
    let summary = crate::report::health::human_line(report);
    let total_time = report
        .get("total_time_s")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    lines.push(String::new());
    lines.push(format!("{summary} in {total_time:.2}s"));
    lines.join("\n")
}

/// Exit code: 1 when any method failed, matching Python.
pub fn exit_code(report: &Value) -> u8 {
    match report.get("failed").and_then(Value::as_i64).unwrap_or(0) {
        0 => 0,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_is_the_python_shell() {
        let (v, t) = fixture();
        assert_eq!(v.len(), 6);
        assert_eq!(t.len(), 6);
        assert_eq!(v[0], [0.0, 0.0, 1.0]);
        assert_eq!(v[5], [0.0, 0.0, -1.0]);
        assert_eq!(t[0], [0, 1, 2]);
        assert_eq!(t[5], [5, 4, 3]);
    }

    /// The summary fields must add up, and every method must carry the six keys
    /// Python emits — this is what the gate compares.
    #[test]
    fn report_shape_matches_python() {
        let r = run(false);
        assert_eq!(r["total"], 19);
        let methods = r["methods"].as_object().expect("methods object");
        assert_eq!(methods.len(), 19);
        let mut ok = 0;
        let mut failed = 0;
        let mut skipped = 0;
        for (_, m) in methods {
            let mut keys: Vec<&str> = m.as_object().unwrap().keys().map(String::as_str).collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                ["elapsed_s", "mode", "name", "num", "reason", "status"],
                "unexpected method keys"
            );
            match m["status"].as_str().unwrap() {
                "ok" => ok += 1,
                "fail" => failed += 1,
                "skip" => skipped += 1,
                other => panic!("unexpected status {other}"),
            }
        }
        assert_eq!(r["ok"], ok);
        assert_eq!(r["failed"], failed);
        assert_eq!(r["skipped"], skipped);
    }

    /// A missing fTetWild is a SKIP, never a FAIL — `health.py` classifies a
    /// missing optional addon as `KIND_SKIPPED`, and getting this wrong turns
    /// the self-check red on every machine without the binary.
    #[test]
    fn missing_optional_addon_is_skip_not_fail() {
        let r = run(false);
        let ftw = &r["methods"]["ftetwild"];
        // Depends on whether the binary exists here, but never "fail" merely
        // because it is absent.
        match ftw["status"].as_str().unwrap() {
            "skip" => assert_eq!(ftw["mode"], "skipped"),
            "ok" => {}
            other => panic!("fTetWild must be ok or skip, got {other}"),
        }
    }

    #[test]
    fn exit_code_follows_failures() {
        assert_eq!(exit_code(&serde_json::json!({"failed": 0})), 0);
        assert_eq!(exit_code(&serde_json::json!({"failed": 2})), 1);
    }
}

