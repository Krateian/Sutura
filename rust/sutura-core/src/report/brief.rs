// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! The compact report: one verdict line, the counts, and the plain notes under
//! it.
//!
//! Ported from `sutura/classification.py:333` (`_brief_notes`) and `:377`
//! (`brief`), which the Qt GUI formatted in `_render_report_brief` and
//! `_brief_note_lines`. The split is the same: this module decides *what* to say
//! and the GUI decides how it looks, so the CLI and the window cannot disagree.
//!
//! Nothing here changes `classify()` or the JSON category. `brief` is display
//! only, which is why it is allowed to fold several signals into one verdict.

use serde_json::Value;

use crate::report::classify::classify_value;

/// A note's code and its arguments, exactly as Python carried them.
///
/// Kept machine-readable so the GUI can localise it: a note that is a formatted
/// English string cannot be translated, and half the point of this struct is that
/// both front ends say the same thing in their own language.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub code: String,
    pub args: Value,
}

/// The compact summary for one repair result.
#[derive(Clone, Debug, PartialEq)]
pub struct Brief {
    /// `ready` (clean), `notes` (repaired with caveats), or `failed`.
    pub verdict: String,
    pub holes_closed: i64,
    pub orientation_corrected: bool,
    pub parts_before: Option<i64>,
    pub parts_after: Option<i64>,
    pub shape_change_pct: Option<f64>,
    /// `deviation` or `volume`: which measure `shape_change_pct` came from.
    pub shape_source: Option<String>,
    pub notes: Vec<Note>,
    pub error: Option<String>,
}

fn get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.get(key)
}

fn num(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.parse::<f64>().ok(),
        _ => None,
    }
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0) != 0.0,
        Some(Value::String(s)) => !s.is_empty(),
        _ => false,
    }
}

/// `_code_value`: the first matching code's value, wherever the report put it.
fn code_value(data: &Value, wanted: &str) -> Option<Value> {
    for key in ["codes", "code"] {
        if let Some(list) = get(data, key).and_then(|v| v.as_array()) {
            for item in list {
                if item.get("code").and_then(|v| v.as_str()) == Some(wanted) {
                    if let Some(v) = item.get("value") {
                        return Some(v.clone());
                    }
                }
            }
        }
    }
    None
}

/// The eight issue codes that become notes, in Python's order.
const NOTE_CODES: &[&str] = &[
    "stage2_skipped",
    "stage2_error",
    "shape_changed",
    "volume_warning",
    "graft_detail_loss",
    "dressing_detail_loss",
    "reload_non_manifold_vertices",
];

/// `attribute_stats`: how much paint/seam/support data the repair dropped.
fn attribute_loss(data: &Value) -> Option<(i64, i64, i64)> {
    let stats = get(data, "attribute_stats")?;
    let dropped = num(get(stats, "dropped"))? as i64;
    if dropped <= 0 {
        return None;
    }
    Some((
        dropped,
        num(get(stats, "kept")).unwrap_or(0.0) as i64,
        num(get(stats, "new_without")).unwrap_or(0.0) as i64,
    ))
}

fn brief_notes(data: &Value, issues: &[String], pr: &Value) -> Vec<Note> {
    let mut notes: Vec<Note> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    let empty = Value::Object(Default::default());

    let mut add = |notes: &mut Vec<Note>, seen: &mut Vec<&'static str>, code: &'static str, args: Value| {
        if seen.contains(&code) {
            return;
        }
        seen.push(code);
        notes.push(Note {
            code: code.to_string(),
            args,
        });
    };

    for code in issues {
        if let Some(known) = NOTE_CODES.iter().find(|c| *c == code) {
            // The table is static, so the &'static str can be recovered.
            let known: &'static str = known;
            add(&mut notes, &mut seen, known, Value::Object(Default::default()));
        }
    }
    if truthy(get(data, "unit_warning")) {
        add(&mut notes, &mut seen, "unit_warning", empty.clone());
    }
    if truthy(get(data, "material_discarded")) {
        add(&mut notes, &mut seen, "material_discarded", empty.clone());
    }
    if let Some((dropped, kept, new_without)) = attribute_loss(data) {
        add(
            &mut notes,
            &mut seen,
            "attribute_loss",
            serde_json::json!({ "dropped": dropped, "kept": kept, "new_without": new_without }),
        );
    }
    if let Some(b) = get(data, "budget") {
        if get(b, "within_budget") == Some(&Value::Bool(false)) {
            add(&mut notes, &mut seen, "budget", empty.clone());
        }
    }
    for advisory in get(pr, "advisories").and_then(|v| v.as_array()).into_iter().flatten() {
        if advisory.get("code").and_then(|v| v.as_str()) == Some("thin_wall") {
            add(
                &mut notes,
                &mut seen,
                "thin_wall",
                serde_json::json!({
                    "ratio": num(get(advisory, "thin_ratio")),
                    "threshold": num(get(advisory, "threshold_norm")),
                    "min_mm": num(get(advisory, "min_mm")),
                    "estimated": truthy(get(advisory, "estimated")),
                }),
            );
        }
    }
    if let Some(esc) = get(data, "si_escalation") {
        if truthy(get(esc, "triggered")) {
            add(
                &mut notes,
                &mut seen,
                "si_escalation",
                serde_json::json!({
                    "outcome": get(esc, "outcome").cloned().unwrap_or(Value::Null),
                    "reason": get(esc, "reason").cloned().unwrap_or(Value::Null),
                    "method": get(esc, "method").cloned().unwrap_or(Value::Null),
                }),
            );
        }
    }
    notes
}

/// `brief`: the compact summary, notes included.
pub fn brief(data: &Value) -> Brief {
    let (category, issues, summary) = classify_value(data);
    let empty = Value::Object(Default::default());
    let pr = get(data, "print_report").unwrap_or(&empty);
    let s1 = get(data, "stage1").unwrap_or(&empty);
    let geom = get(pr, "geometry").unwrap_or(&empty);

    let mut holes = code_value(data, "holes_closed")
        .and_then(|v| num(Some(&v)))
        .unwrap_or(0.0) as i64;
    if holes == 0 {
        holes = num(get(s1, "holes_closed")).unwrap_or(0.0) as i64;
    }
    let mut orientation = truthy(code_value(data, "orientation_corrected").as_ref());
    if !orientation {
        if let (Some(vb), Some(va)) = (num(get(s1, "volume_before")), num(get(s1, "volume_after"))) {
            orientation = vb < 0.0 && va > 0.0;
        }
    }
    let parts_before = num(get(geom, "components_before"))
        .or_else(|| num(get(s1, "components_before")))
        .map(|v| v as i64);
    let parts_after = num(get(geom, "components_after"))
        .or_else(|| num(get(s1, "components")))
        .map(|v| v as i64);
    let mut shape = num(get(geom, "deviation_max_pct"));
    let mut shape_source = shape.map(|_| "deviation".to_string());
    if shape.is_none() {
        shape = num(get(geom, "volume_change_pct"));
        if shape.is_some() {
            shape_source = Some("volume".to_string());
        }
    }

    let mut notes = brief_notes(data, &issues, pr);
    let has_output = get(data, "output").map(|v| !v.is_null()).unwrap_or(false);
    let failed = category == "error" || summary == "partial" || summary == "holes" || !has_output;
    let verdict = if failed {
        let mut holes_left = 0i64;
        for item in get(pr, "issues").and_then(|v| v.as_array()).into_iter().flatten() {
            if item.get("code").and_then(|v| v.as_str()) == Some("holes_remaining") {
                holes_left = num(get(item, "count")).unwrap_or(0.0) as i64;
                break;
            }
        }
        if holes_left == 0 {
            holes_left = num(get(s1, "holes_remaining")).unwrap_or(0.0) as i64;
        }
        if holes_left > 0 {
            notes.push(Note {
                code: "holes_remaining".into(),
                args: serde_json::json!({ "count": holes_left }),
            });
        } else if summary == "partial" || summary == "holes" || (category != "error" && !has_output) {
            notes.push(Note {
                code: "not_closed".into(),
                args: Value::Object(Default::default()),
            });
        }
        "failed"
    } else if category == "watertight" && issues.is_empty() && notes.is_empty() {
        "ready"
    } else {
        "notes"
    };

    Brief {
        verdict: verdict.to_string(),
        holes_closed: holes,
        orientation_corrected: orientation,
        parts_before,
        parts_after,
        shape_change_pct: shape,
        shape_source,
        notes,
        error: get(data, "error").and_then(|v| v.as_str()).map(str::to_string),
    }
}

/// `headline_verdict`: one folded line for the report.
///
/// Returns `(code, areas)`. A failed repair always reads `not_repaired`; when
/// Fidelity was not measured the headline falls back to the honest brief verdict
/// rather than claiming a shape check that never ran.
pub fn headline_verdict(data: &Value) -> (String, i64) {
    let b = brief(data);
    let code = get(data, "repair_verdict_code").and_then(|v| v.as_str());
    let areas = num(get(data, "repair_verdict_areas")).unwrap_or(0.0) as i64;
    if b.verdict == "failed" || code == Some("not_repaired") {
        return ("not_repaired".to_string(), areas);
    }
    match code {
        None | Some("unavailable") => (b.verdict.clone(), 0),
        Some(c) => (fold_with_topology(c, &b.verdict), areas),
    }
}

fn fold_with_topology(code: &str, brief_verdict: &str) -> String {
    if brief_verdict == "notes" {
        match code {
            "preserved" | "ready" => "preserved_notes".to_string(),
            other => other.to_string(),
        }
    } else {
        code.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_watertight_report_reads_ready() {
        let data = serde_json::json!({
            "status": "ok",
            "output": "/tmp/x_fixed.stl",
            "stage1": { "two_manifold": true, "holes_remaining": 0, "holes_closed": 4, "components": 1 },
            "stage2": { "ok": true },
        });
        let b = brief(&data);
        assert_eq!(b.verdict, "ready");
        assert_eq!(b.holes_closed, 4);
        assert!(b.notes.is_empty());
    }

    #[test]
    fn a_report_without_an_output_reads_failed() {
        let data = serde_json::json!({
            "status": "ok",
            "stage1": { "two_manifold": false },
            "stage2": { "ok": true },
        });
        let b = brief(&data);
        assert_eq!(b.verdict, "failed");
        assert!(b.notes.iter().any(|n| n.code == "not_closed"));
    }

    #[test]
    fn remaining_holes_are_named_with_their_count() {
        let data = serde_json::json!({
            "status": "ok",
            "stage1": { "two_manifold": true, "holes_remaining": 7 },
            "stage2": { "ok": true },
        });
        let b = brief(&data);
        assert_eq!(b.verdict, "failed");
        let note = b.notes.iter().find(|n| n.code == "holes_remaining").unwrap();
        assert_eq!(note.args["count"], 7);
    }

    #[test]
    fn an_out_of_budget_run_says_so() {
        let data = serde_json::json!({
            "status": "ok",
            "output": "/tmp/x_fixed.stl",
            "budget": { "within_budget": false },
            "stage1": { "two_manifold": true, "holes_remaining": 0 },
            "stage2": { "ok": true },
        });
        let b = brief(&data);
        assert_eq!(b.verdict, "notes");
        assert!(b.notes.iter().any(|n| n.code == "budget"));
    }

    #[test]
    fn the_headline_falls_back_to_the_brief_when_fidelity_is_absent() {
        let data = serde_json::json!({
            "status": "ok",
            "output": "/tmp/x_fixed.stl",
            "stage1": { "two_manifold": true, "holes_remaining": 0 },
            "stage2": { "ok": true },
        });
        assert_eq!(headline_verdict(&data).0, "ready");
    }

    #[test]
    fn a_caveat_demotes_a_preserved_shape_to_preserved_notes() {
        let data = serde_json::json!({
            "status": "ok",
            "output": "/tmp/x_fixed.stl",
            "repair_verdict_code": "preserved",
            "repair_verdict_areas": 2,
            "budget": { "within_budget": false },
            "stage1": { "two_manifold": true, "holes_remaining": 0 },
            "stage2": { "ok": true },
        });
        let (code, areas) = headline_verdict(&data);
        assert_eq!(code, "preserved_notes");
        assert_eq!(areas, 2);
    }

    #[test]
    fn a_failed_repair_always_reads_not_repaired() {
        let data = serde_json::json!({
            "status": "ok",
            "repair_verdict_code": "preserved",
            "stage1": { "two_manifold": true, "holes_remaining": 0 },
            "stage2": { "ok": true },
        });
        assert_eq!(headline_verdict(&data).0, "not_repaired");
    }
}
