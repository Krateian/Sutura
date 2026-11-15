// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! `--analyze` payload and its human renderer.
//!
//! Port of `method_registry.analyze_report` and `format_analyze_human`. The
//! payload is `{input, analysis|object_analyses, suggestions, recommendations,
//! suggestion_basis, suggestions_are_estimates, external_engines}`, and the
//! human form is English-only -- `--lang` sets `SUTURA_LANG` for the repair
//! reports but `format_analyze_human` has no translated branch, so EN and TR
//! produce byte-identical `--analyze --human` output. That is the tested
//! contract, not an oversight.

use serde_json::{json, Map, Value};

use crate::analysis::{analyze_file, combine, Analysis};
use crate::config::external_engines::external_engines;
use crate::config::LoadedPolicy;
use crate::ranking::{recommendations_json, rank_methods, suggest_methods};

/// `suggestion_basis`: the fixed provenance string `--analyze` reports.
pub const SUGGESTION_BASIS: &str = "rank+core150-c26ed2e";

/// `method_registry.analyze_report(path, engine, extra_features)`.
pub fn analyze_report(
    path: &str,
    loaded: &LoadedPolicy,
    engine: &str,
    count_self_intersections: bool,
) -> Value {
    let mut result = Map::new();
    result.insert("input".into(), json!(path));

    let objs = match analyze_file(path, engine, count_self_intersections) {
        Ok(o) => o,
        Err(e) => {
            result.insert("error".into(), json!(format!("analysis failed: {}", e)));
            return Value::Object(result);
        }
    };
    if objs.is_empty() {
        result.insert(
            "error".into(),
            json!("analysis failed: no mesh objects found in 3MF"),
        );
        return Value::Object(result);
    }
    let combined = combine(&objs);

    // `rank_methods` applies a bounded Bayesian bonus from the local
    // (template, method) history unless learning is turned off. Python reads
    // this inside `rank_methods` itself; the Rust ranking takes it as a
    // parameter, so load it here -- one place, same gate.
    let learning_stats: Option<serde_json::Map<String, Value>> =
        if crate::config::learning::learning_enabled(None) {
            Some(crate::config::learning::triage_stats(None, None))
        } else {
            None
        };
    let learning_ref = learning_stats.as_ref();

    let analysis_to_value = |a: &Analysis, with_suggestions: bool| -> Value {
        let mut v = a.to_json();
        if let Some(map) = v.as_object_mut() {
            if with_suggestions {
                map.insert(
                    "suggestions".into(),
                    suggest_methods(loaded, &to_ranking(a), 3, None),
                );
            }
        }
        v
    };

    let recs = rank_methods(loaded, &to_ranking(&combined), None, learning_ref);

    if objs.len() == 1 {
        result.insert("analysis".into(), analysis_to_value(&objs[0], false));
        result.insert(
            "suggestions".into(),
            suggest_methods(loaded, &to_ranking(&objs[0]), 3, learning_ref),
        );
    } else {
        result.insert("objects".into(), json!(objs.len()));
        let object_analyses: Vec<Value> = objs
            .iter()
            .map(|o| analysis_to_value(o, true))
            .collect();
        result.insert("object_analyses".into(), Value::Array(object_analyses));
        result.insert(
            "suggestions".into(),
            suggest_methods(loaded, &to_ranking(&combined), 3, learning_ref),
        );
    }
    // Key ORDER matters for byte comparison: Python builds
    // {input, analysis, suggestions, recommendations, ...}, so
    // `recommendations` must be inserted after the analysis block.
    result.insert("recommendations".into(), recommendations_json(&recs));
    result.insert("suggestion_basis".into(), json!(SUGGESTION_BASIS));
    result.insert("suggestions_are_estimates".into(), json!(true));
    result.insert("external_engines".into(), external_engines());
    Value::Object(result)
}

/// Bridge `Analysis` into the ranking module's own view of it.
///
/// They are separate structs on purpose: `analysis` is the report contract and
/// `ranking::Analysis` is the scoring input. Keeping the conversion in one
/// place means a new field only has to be wired here.
fn to_ranking(a: &Analysis) -> crate::ranking::Analysis {
    crate::ranking::Analysis {
        mesh_type: Some(a.mesh_type.clone()),
        type_confidence: a.type_confidence,
        boundary_loops: a.boundary_loops as f64,
        non_manifold_edges: a.non_manifold_edges as f64,
        self_intersections: a.self_intersections as f64,
        components: a.components as f64,
        largest_loop_ratio: a.largest_loop_ratio,
        open_area_ratio: a.open_area_ratio,
        single_side_score: a.single_side_score,
        relief_score: a.relief_score,
        repetition_score: a.repetition_score,
        diagnosis_v2: a.diagnosis_v2.clone(),
    }
}

/// `format_suggestions`.
fn format_suggestions(suggestions: &Value) -> String {
    let arr = match suggestions.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return "    (no safe suggestions)".to_string(),
    };
    let mut lines = Vec::new();
    for (i, s) in arr.iter().enumerate() {
        let label = s
            .get("confidence_label")
            .and_then(Value::as_str)
            .unwrap_or("low");
        let mut label = label.to_string();
        if let Some(first) = label.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        let name = s
            .get("display_name")
            .and_then(Value::as_str)
            .or_else(|| s.get("name").and_then(Value::as_str))
            .unwrap_or("");
        let num = s.get("num").and_then(Value::as_i64).unwrap_or(0);
        let conf = s.get("confidence").and_then(Value::as_i64).unwrap_or(0);
        let reason = s
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("suggested");
        let id = s.get("id").and_then(Value::as_str).unwrap_or("");
        let _ = id;
        // `'    %d. #%s %s — %d%% %s — %s'` -- the num comes from `get('num')`,
        // which Python renders with `%s`, so an int has no padding.
        lines.push(format!(
            "    {}. #{} {} — {}% {} — {}",
            i + 1,
            num,
            name,
            conf,
            label,
            reason
        ));
    }
    lines.join("\n")
}

/// `format_recommendations`.
fn format_recommendations(recs: &Value) -> String {
    let arr = match recs.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return "  (none)".to_string(),
    };
    let mut lines = Vec::new();
    for r in arr {
        let num = r.get("num").and_then(Value::as_i64).unwrap_or(0);
        let name = r.get("name").and_then(Value::as_str).unwrap_or("");
        let score = r.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let reason = r.get("reason").and_then(Value::as_str).unwrap_or("");
        // `'  %-4d %-21s score=%.2f  %s'`
        lines.push(format!(
            "  {:<4} {:<21} score={:.2}  {}",
            num, name, score, reason
        ));
    }
    lines.join("\n")
}

/// `format_analyze_human` — English only, like Python.
pub fn format_analyze_human(result: &Value) -> String {
    if let Some(err) = result.get("error").and_then(Value::as_str) {
        return format!("Analyze failed: {}", err);
    }
    let input = result
        .get("input")
        .and_then(Value::as_str)
        .unwrap_or("None");
    let mut lines = vec![format!("Input : {}", input)];

    let empty = Vec::new();
    let analyses: &Vec<Value> = match result.get("object_analyses").and_then(Value::as_array) {
        Some(a) => a,
        None => {
            // `[result.get('analysis', {})]` -- a missing analysis is an empty
            // dict, so every field prints as None/0.
            let single = result.get("analysis").cloned().unwrap_or_else(|| json!({}));
            return format_single(result, &single, &mut lines, &empty);
        }
    };

    for (idx, a) in analyses.iter().enumerate() {
        let label = match a.get("model").and_then(Value::as_str) {
            Some(m) => m.to_string(),
            None => format!("object {}", idx),
        };
        lines.push(format!("Object: {}", label));
        lines.extend(analysis_detail(a));
        let sug = match a.get("suggestions") {
            Some(s) if !s.is_null() => Some(s),
            _ => {
                if idx == 0 {
                    result.get("suggestions")
                } else {
                    None
                }
            }
        };
        if let Some(s) = sug {
            if s.as_array().map(|x| !x.is_empty()).unwrap_or(false) {
                lines.push("  Suggested methods (advanced, estimate):".to_string());
                lines.push(format_suggestions(s));
            }
        }
    }
    if analyses.len() > 1 {
        if let Some(s) = result.get("suggestions") {
            if s.as_array().map(|x| !x.is_empty()).unwrap_or(false) {
                lines.push("File-level suggestion (worst case):".to_string());
                lines.push(format_suggestions(s));
            }
        }
    }
    lines.push("Recommended methods:".to_string());
    lines.push(format_recommendations(
        result.get("recommendations").unwrap_or(&Value::Null),
    ));
    lines.push("External engines (separate from the ranking):".to_string());
    let engines = result
        .get("external_engines")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if engines.is_empty() {
        lines.push("  (none configured)".to_string());
    }
    for e in &engines {
        let name = e.get("name").and_then(Value::as_str).unwrap_or("");
        let placement = e.get("placement").and_then(Value::as_str).unwrap_or("");
        let enabled = py_bool_str(e.get("enabled"));
        let available = py_bool_str(e.get("available"));
        // `'  %-16s placement=%-14s enabled=%-5s available=%s'`
        lines.push(format!(
            "  {:<16} placement={:<14} enabled={:<5} available={}",
            name, placement, enabled, available
        ));
    }
    lines.join("\n")
}

/// The single-object path builds the same block, then the shared tail.
fn format_single(
    result: &Value,
    a: &Value,
    lines: &mut Vec<String>,
    _empty: &Vec<Value>,
) -> String {
    let label = match a.get("model").and_then(Value::as_str) {
        Some(m) => m.to_string(),
        None => "object 0".to_string(),
    };
    lines.push(format!("Object: {}", label));
    lines.extend(analysis_detail(a));
    if let Some(s) = result.get("suggestions") {
        if s.as_array().map(|x| !x.is_empty()).unwrap_or(false) {
            lines.push("  Suggested methods (advanced, estimate):".to_string());
            lines.push(format_suggestions(s));
        }
    }
    lines.push("Recommended methods:".to_string());
    lines.push(format_recommendations(
        result.get("recommendations").unwrap_or(&Value::Null),
    ));
    lines.push("External engines (separate from the ranking):".to_string());
    let engines = result
        .get("external_engines")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if engines.is_empty() {
        lines.push("  (none configured)".to_string());
    }
    for e in &engines {
        let name = e.get("name").and_then(Value::as_str).unwrap_or("");
        let placement = e.get("placement").and_then(Value::as_str).unwrap_or("");
        lines.push(format!(
            "  {:<16} placement={:<14} enabled={:<5} available={}",
            name,
            placement,
            py_bool_str(e.get("enabled")),
            py_bool_str(e.get("available"))
        ));
    }
    lines.join("\n")
}

/// `'  faces=%s vertices=%s ... type=%s (%.2f)'` plus the open-area line.
fn analysis_detail(a: &Value) -> Vec<String> {
    let g = |k: &str| a.get(k);
    let s = |k: &str| -> String {
        match g(k) {
            Some(Value::String(v)) => v.clone(),
            Some(Value::Null) | None => "None".to_string(),
            Some(v) => v.to_string(),
        }
    };
    let conf = g("type_confidence").and_then(Value::as_f64).unwrap_or(0.0);
    let est = g("self_intersections_estimated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    vec![
        format!(
            "  faces={} vertices={} components={} loops={} nm_edges={} si={}{} type={} ({:.2})",
            s("faces"),
            s("vertices"),
            s("components"),
            s("boundary_loops"),
            s("non_manifold_edges"),
            s("self_intersections"),
            if est { "~" } else { "" },
            s("mesh_type"),
            conf
        ),
        format!(
            "  open_area_ratio={} largest_loop_ratio={} repetition_score={}",
            s("open_area_ratio"),
            s("largest_loop_ratio"),
            s("repetition_score")
        ),
    ]
}

/// Python's `str(True)`/`str(False)`.
fn py_bool_str(v: Option<&Value>) -> String {
    match v.and_then(Value::as_bool) {
        Some(true) => "True".to_string(),
        Some(false) => "False".to_string(),
        None => "None".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_payload_renders_the_failure_line() {
        let v = json!({"input": "x.stl", "error": "analysis failed: boom"});
        assert_eq!(format_analyze_human(&v), "Analyze failed: analysis failed: boom");
    }

    #[test]
    fn no_engines_prints_the_placeholder() {
        let v = json!({
            "input": "x.stl",
            "analysis": {"faces": 1, "vertices": 2, "components": 1, "boundary_loops": 0,
                          "non_manifold_edges": 0, "self_intersections": 0,
                          "self_intersections_estimated": false, "mesh_type": "clean",
                          "type_confidence": 0.5, "open_area_ratio": 0.0,
                          "largest_loop_ratio": 0.0, "repetition_score": 0.0},
            "recommendations": [],
            "external_engines": []
        });
        let text = format_analyze_human(&v);
        assert!(text.contains("  (none configured)"), "{text}");
        assert!(text.contains("  (none)"), "recommendations placeholder: {text}");
        assert!(text.contains("type=clean (0.50)"), "{text}");
    }

    #[test]
    fn suggested_estimate_marker_is_rendered() {
        let v = json!({
            "input": "x.stl",
            "analysis": {"faces": 1, "vertices": 2, "components": 1, "boundary_loops": 0,
                          "non_manifold_edges": 0, "self_intersections": 3,
                          "self_intersections_estimated": true, "mesh_type": "clean",
                          "type_confidence": 1.0},
            "recommendations": [],
            "external_engines": []
        });
        assert!(format_analyze_human(&v).contains("si=3~ type="), "estimate tilde");
    }
}
