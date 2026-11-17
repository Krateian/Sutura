// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Method ranking and suggestions for the read-only `--analyze` path.
//!
//! Port of `sutura_engine/methods/__init__.py` (`rank_methods`,
//! `suggest_methods`) restricted to what the Rust CLI needs: the analysis
//! fields, the policy knobs (`ranking.*`, `templates.*`) and the learning
//! bonus. Method availability and `needs_user_input` are static here; the
//! Python registry can be extended at runtime, so this is the built-in set.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::config::learning::{learning_bonus, LearningKnobs};
use crate::config::{LoadedPolicy, PolicyValue};

/// Built-in method metadata used by the ranking.
pub struct MethodInfo {
    pub num: u8,
    pub id: &'static str,
    pub name: &'static str,
    pub family: &'static str,
    pub needs_user_input: bool,
    /// `available`: fTetWild depends on a local binary; everything else is
    /// compiled in.
    pub available: bool,
}

pub const METHODS: [MethodInfo; 19] = [
    MethodInfo { num: 1, id: "quick_clean", name: "Quick Clean", family: "clean", needs_user_input: false, available: true },
    MethodInfo { num: 2, id: "local_mend", name: "Local Mend", family: "topology", needs_user_input: false, available: true },
    MethodInfo { num: 3, id: "full_mend", name: "Full Mend", family: "topology", needs_user_input: false, available: true },
    MethodInfo { num: 4, id: "join", name: "Join", family: "topology", needs_user_input: false, available: true },
    MethodInfo { num: 5, id: "autorefine", name: "Autorefine", family: "si", needs_user_input: false, available: true },
    MethodInfo { num: 6, id: "exact_refine", name: "Exact Refine", family: "si", needs_user_input: false, available: true },
    MethodInfo { num: 7, id: "ftetwild", name: "fTetWild", family: "envelope", needs_user_input: false, available: false },
    MethodInfo { num: 8, id: "balloon", name: "Balloon", family: "closing", needs_user_input: false, available: true },
    MethodInfo { num: 9, id: "backplate", name: "Backplate", family: "closing", needs_user_input: false, available: true },
    MethodInfo { num: 10, id: "scaffold", name: "Scaffold", family: "template", needs_user_input: false, available: true },
    MethodInfo { num: 11, id: "transplant", name: "Transplant", family: "pattern", needs_user_input: false, available: true },
    MethodInfo { num: 12, id: "transplant_plus", name: "Transplant+", family: "pattern", needs_user_input: true, available: true },
    MethodInfo { num: 13, id: "graft", name: "Graft", family: "envelope", needs_user_input: false, available: true },
    MethodInfo { num: 14, id: "mirror_complete", name: "Mirror Complete", family: "closing", needs_user_input: false, available: true },
    MethodInfo { num: 15, id: "wall_thicken", name: "Wall Thicken", family: "envelope", needs_user_input: true, available: true },
    MethodInfo { num: 16, id: "dressing", name: "Dressing", family: "envelope", needs_user_input: true, available: true },
    MethodInfo { num: 17, id: "tamponade", name: "Tamponade", family: "interior", needs_user_input: true, available: true },
    MethodInfo { num: 18, id: "splint", name: "Splint", family: "solid", needs_user_input: true, available: true },
    MethodInfo { num: 19, id: "coaptation", name: "Coaptation", family: "solid", needs_user_input: false, available: true },
];

/// Analysis fields the ranking reads (`sutura_engine.diagnosis.ObjectAnalysis`).
#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub mesh_type: Option<String>,
    pub type_confidence: f64,
    pub boundary_loops: f64,
    pub non_manifold_edges: f64,
    pub self_intersections: f64,
    pub components: f64,
    pub largest_loop_ratio: f64,
    pub open_area_ratio: f64,
    pub single_side_score: f64,
    pub relief_score: f64,
    pub repetition_score: f64,
    /// diagnosis-v2 JSON (channels/regions): needed by `_damage_class` /
    /// `_has_body_overlap` for the suggestion prior and by the avoid-prune.
    /// `None` behaves like Python's non-dict case.
    pub diagnosis_v2: Option<Value>,
}

impl Analysis {
    fn get(&self, name: &str) -> f64 {
        match name {
            "type_confidence" => self.type_confidence,
            "boundary_loops" => self.boundary_loops,
            "non_manifold_edges" => self.non_manifold_edges,
            "self_intersections" => self.self_intersections,
            "components" => {
                if self.components == 0.0 { 1.0 } else { self.components }
            }
            "largest_loop_ratio" => self.largest_loop_ratio,
            "open_area_ratio" => self.open_area_ratio,
            "single_side_score" => self.single_side_score,
            "relief_score" => self.relief_score,
            "repetition_score" => self.repetition_score,
            _ => 0.0,
        }
    }
}

fn clamp01(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Python `round(x, 3)` for the scores the report carries.
fn round3(value: f64) -> f64 {
    crate::classify::py_round(value, 3)
}

struct Knobs {
    preference_boost: f64,
    top_n: usize,
    max_bonus: f64,
    prior: f64,
    templates: Vec<(String, Value)>,
}

fn knob_f64(loaded: &LoadedPolicy, section: &str, key: &str, default: f64) -> f64 {
    loaded
        .get(section)
        .and_then(|s| s.get(key))
        .and_then(|v| match v {
            PolicyValue::Int(i) => Some(*i as f64),
            PolicyValue::Float(f) => Some(*f),
            _ => None,
        })
        .unwrap_or(default)
}

fn knobs(loaded: &LoadedPolicy) -> Knobs {
    let mut templates: Vec<(String, Value)> = Vec::new();
    let mut insert = |id: &str, value: Value| {
        templates.push((id.to_string(), value));
    };
    let numeric = |key: &str, default: f64| {
        loaded
            .get("templates")
            .and_then(|t| t.get(key))
            .and_then(|v| match v {
                PolicyValue::Int(i) => Some(*i as f64),
                PolicyValue::Float(f) => Some(*f),
                _ => None,
            })
            .unwrap_or(default)
    };
    let preferred = |id: &str| -> Vec<u8> {
        loaded
            .get("templates")
            .and_then(|t| t.get("preferred"))
            .and_then(|p| p.get(id))
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_i64())
                    .map(|v| v as u8)
                    .collect()
            })
            .unwrap_or_default()
    };
    let tpl = json!({
        "open_area_scan": numeric("open_area_scan", 0.02),
        "open_area_relief": numeric("open_area_relief", 0.08),
        "large_loop_ratio": numeric("large_loop_ratio", 0.25),
        "many_loops": numeric("many_loops", 4.0),
        "si_moderate": numeric("si_moderate", 20.0),
        "si_heavy": numeric("si_heavy", 200.0),
    });
    // The order matches `diagnosis.all_templates()`: on a combined-score tie
    // the first template seen wins (Python keeps the first `combined`).
    for id in [
        "mechanical",
        "organic",
        "single_side_scan",
        "relief",
        "repeated_pattern",
        "dense_scan_heavy_si",
    ] {
        insert(
            id,
            json!({"id": id, "preferred": preferred(id), "cfg": tpl}),
        );
    }
    Knobs {
        preference_boost: knob_f64(loaded, "ranking", "preference_boost", 1.25),
        top_n: knob_f64(loaded, "ranking", "top_n", 6.0) as usize,
        max_bonus: knob_f64(loaded, "ranking", "learning_max_bonus", 0.05),
        prior: knob_f64(loaded, "ranking", "learning_prior", 2.0),
        templates,
    }
}

fn template_conf(id: &str, a: &Analysis, cfg: &Value) -> f64 {
    let num = |key: &str| cfg.get(key).and_then(Value::as_f64).unwrap_or(0.0);
    let open_area_scan = num("open_area_scan");
    let open_area_relief = num("open_area_relief");
    let many_loops = num("many_loops");
    let large_loop_ratio = num("large_loop_ratio");
    let si_heavy = num("si_heavy");
    match id {
        "mechanical" => {
            if a.mesh_type.as_deref() == Some("mechanical") {
                clamp01(a.get("type_confidence"))
            } else {
                0.0
            }
        }
        "organic" => {
            if a.mesh_type.as_deref() == Some("organic") {
                clamp01(a.get("type_confidence"))
            } else {
                0.0
            }
        }
        "single_side_scan" => {
            let signal = clamp01(a.get("single_side_score"));
            let open_ratio = a.get("open_area_ratio");
            let loops = a.get("boundary_loops");
            let largest = a.get("largest_loop_ratio");
            let mut legacy = 0.0;
            if open_ratio >= open_area_scan && loops <= many_loops {
                legacy = (open_ratio / open_area_relief).min(1.0);
                if largest >= large_loop_ratio {
                    legacy = (legacy + 0.2).min(1.0);
                }
            }
            legacy.max(signal)
        }
        "relief" => {
            let signal = clamp01(a.get("relief_score"));
            let open_ratio = a.get("open_area_ratio");
            let loops = a.get("boundary_loops");
            let mut legacy = 0.0;
            if open_ratio >= open_area_scan && loops <= 2.0 {
                legacy = (open_ratio / open_area_relief).min(1.0);
            }
            legacy.max(signal)
        }
        "repeated_pattern" => clamp01(a.get("repetition_score")),
        "dense_scan_heavy_si" => {
            let si = a.get("self_intersections");
            if si < si_heavy {
                0.0
            } else {
                (si / (4.0 * si_heavy)).min(1.0)
            }
        }
        _ => 0.0,
    }
}

/// Per-method suitability `(score, reason, reason_key, reason_args)`.
fn method_score(num: u8, a: &Analysis) -> (f64, String, &'static str, Vec<Value>) {
    let holes = a.get("boundary_loops");
    let nm = a.get("non_manifold_edges");
    let si = a.get("self_intersections");
    let components = a.get("components");
    let largest = a.get("largest_loop_ratio");
    let open_ratio = a.get("open_area_ratio");
    match num {
        1 => {
            let penalty = 0.5 * (nm / 5.0).min(1.0)
                + 0.3 * (holes / 5.0).min(1.0)
                + 0.2 * (si / 50.0).min(1.0);
            let light = penalty < 0.3;
            (
                clamp01(1.0 - penalty),
                if light {
                    "light defect load".to_string()
                } else {
                    "defects may need a deeper tier".to_string()
                },
                if light {
                    "rec_reason_fast_light"
                } else {
                    "rec_reason_fast_deep"
                },
                vec![],
            )
        }
        2 => {
            let score = if nm <= 0.0 && holes <= 2.0 && largest <= 0.3 {
                0.75
            } else {
                0.6 * (1.0 - ((holes - 2.0).max(0.0) / 5.0).min(1.0))
                    * (1.0 - (nm / 3.0).min(1.0))
            };
            (
                clamp01(score),
                "local re-mesh of small damaged regions".to_string(),
                "rec_reason_deep_local",
                vec![],
            )
        }
        3 => (
            clamp01(0.55 + 0.35 * ((holes + nm) / 5.0).min(1.0)),
            "full deep-repair ladder".to_string(),
            "rec_reason_deep_full",
            vec![],
        ),
        4 => {
            if components > 1.0 {
                (
                    clamp01(((components - 1.0) / 2.0).min(1.0)),
                    format!("{} connected components", components as i64),
                    "rec_reason_components",
                    vec![json!(components as i64)],
                )
            } else {
                (
                    0.0,
                    "single connected component".to_string(),
                    "rec_reason_component_single",
                    vec![],
                )
            }
        }
        5 | 6 => {
            if si <= 0.0 {
                (
                    0.0,
                    "no self-intersections".to_string(),
                    "rec_reason_no_si",
                    vec![],
                )
            } else {
                (
                    clamp01(0.4 + 0.6 * (si / 200.0).min(1.0)),
                    format!("self-intersections present ({})", si as i64),
                    "rec_reason_si",
                    vec![json!(si as i64)],
                )
            }
        }
        7 => (
            clamp01(
                0.4 * clamp01(open_ratio / 0.05)
                    + 0.3 * clamp01((holes + nm) / 3.0)
                    + 0.3 * clamp01(si / 200.0),
            ),
            "large openings / heavy self-intersections".to_string(),
            "rec_reason_ftetwild",
            vec![],
        ),
        13 => (
            clamp01(
                0.45 * clamp01(open_ratio / 0.05)
                    + 0.30 * clamp01((holes + nm) / 3.0)
                    + 0.25 * clamp01(si / 200.0),
            ),
            "openings / holes closable by a shell wrap".to_string(),
            "rec_reason_graft",
            vec![],
        ),
        18 => (
            clamp01(
                0.45 * clamp01(open_ratio / 0.05)
                    + 0.30 * clamp01((holes + nm) / 3.0)
                    + 0.25 * clamp01(si / 200.0),
            ),
            "damage reconstructable by an exact solid rebuild".to_string(),
            "rec_reason_splint",
            vec![],
        ),
        19 => {
            let multi = clamp01((components - 1.0) / 3.0);
            (
                clamp01(
                    0.5 * multi
                        + 0.3 * clamp01((holes + nm) / 3.0)
                        + 0.2 * clamp01(si / 200.0),
                ),
                "folded/overlapping multi-body soup (seam-weld union)".to_string(),
                "rec_reason_coaptation",
                vec![],
            )
        }
        8 => {
            let single = clamp01(a.get("single_side_score"));
            let relief = clamp01(a.get("relief_score"));
            let score = single * (1.0 - relief);
            if score <= 0.0 {
                (
                    0.0,
                    "not a single-sided open scan".to_string(),
                    "rec_reason_not_scan",
                    vec![],
                )
            } else {
                (
                    clamp01(score),
                    format!("single-sided open scan ({single:.2})"),
                    "rec_reason_poisson",
                    vec![json!(round4(single))],
                )
            }
        }
        9 => {
            let relief = clamp01(a.get("relief_score"));
            if relief <= 0.0 {
                (
                    0.0,
                    "no relief-like opening".to_string(),
                    "rec_reason_not_relief",
                    vec![],
                )
            } else {
                (
                    clamp01(relief),
                    "relief / flat-back profile".to_string(),
                    "rec_reason_relief",
                    vec![],
                )
            }
        }
        10 => {
            let damage = holes + nm + (components - 1.0).max(0.0);
            if damage <= 0.0 {
                (
                    0.0,
                    "no holes/non-manifold to reconstruct".to_string(),
                    "rec_reason_no_damage",
                    vec![],
                )
            } else {
                let open_pen = (open_ratio / 0.5).min(1.0);
                let si_pen = (si / 200.0).min(1.0);
                (
                    clamp01(
                        0.45 * (damage / 4.0).min(1.0)
                            + 0.35 * (1.0 - open_pen)
                            + 0.20 * (1.0 - si_pen),
                    ),
                    "holes/non-manifold/debris with a mostly healthy surface"
                        .to_string(),
                    "rec_reason_proxy",
                    vec![],
                )
            }
        }
        11 | 12 => {
            let rep = clamp01(a.get("repetition_score"));
            if rep <= 0.0 {
                (
                    0.0,
                    "no repeated pattern detected".to_string(),
                    "rec_reason_no_repeat",
                    vec![],
                )
            } else {
                (
                    clamp01(rep),
                    format!("repeated pattern ({rep:.2})"),
                    "rec_reason_repeat",
                    vec![json!(round4(rep))],
                )
            }
        }
        14 => {
            let single = clamp01(a.get("single_side_score"));
            let relief = clamp01(a.get("relief_score"));
            let score = single * (1.0 - relief);
            if score <= 0.0 {
                (
                    0.0,
                    "not a single-sided open scan".to_string(),
                    "rec_reason_not_scan",
                    vec![],
                )
            } else {
                (
                    clamp01(score),
                    format!("single-sided scan with a symmetry plane ({single:.2})"),
                    "rec_reason_mirror",
                    vec![json!(round4(single))],
                )
            }
        }
        15 => (
            0.0,
            "opt-in thin-wall thicken".to_string(),
            "rec_reason_wall_optin",
            vec![],
        ),
        16 => (
            clamp01(
                0.45 * clamp01(open_ratio / 0.05)
                    + 0.30 * clamp01((holes + nm) / 3.0)
                    + 0.25 * clamp01(si / 2000.0),
            ),
            "complex folds / heavy self-intersections skin-coatable".to_string(),
            "rec_reason_dressing",
            vec![],
        ),
        17 => (
            clamp01(
                0.5 * clamp01((holes + nm) / 3.0)
                    + 0.3 * clamp01(open_ratio / 0.05)
                    + 0.2 * clamp01(si / 500.0),
            ),
            "moderate holes/non-manifold pluggable from the inside".to_string(),
            "rec_reason_tamponade",
            vec![],
        ),
        _ => (
            0.0,
            "not implemented yet".to_string(),
            "rec_reason_not_implemented",
            vec![],
        ),
    }
}

fn round4(value: f64) -> f64 {
    crate::classify::py_round(value, 4)
}

#[derive(Clone, Debug)]
pub struct Recommendation {
    pub num: u8,
    pub id: &'static str,
    pub name: &'static str,
    pub score: f64,
    pub reason: String,
    pub template: Option<String>,
    pub reason_key: &'static str,
    pub reason_args: Vec<Value>,
}

/// `rank_methods(analysis, top_n)`.
pub fn rank_methods(
    loaded: &LoadedPolicy,
    analysis: &Analysis,
    top_n: Option<usize>,
    learning_stats: Option<&serde_json::Map<String, Value>>,
) -> Vec<Recommendation> {
    let knobs = knobs(loaded);
    let limit = top_n.unwrap_or(knobs.top_n);
    let mut out = Vec::new();
    for method in METHODS.iter() {
        if !method.available || method.needs_user_input {
            continue;
        }
        let (base, method_reason, reason_key, reason_args) =
            method_score(method.num, analysis);
        let mut best_combined: Option<f64> = None;
        let mut best_template: Option<String> = None;
        let mut best_score = base;
        let mut best_bonus = 0.0f64;
        let mut reason = method_reason.clone();
        for (template_id, template) in &knobs.templates {
            let cfg = template.get("cfg").cloned().unwrap_or(Value::Null);
            let conf = template_conf(template_id, analysis, &cfg);
            if conf <= 0.0 {
                continue;
            }
            let preferred = template
                .get("preferred")
                .and_then(Value::as_array)
                .map(|items| items.iter().any(|v| v.as_u64() == Some(method.num as u64)))
                .unwrap_or(false);
            let boost = if preferred { knobs.preference_boost } else { 1.0 };
            let combined = conf * (base * boost).min(1.0);
            if best_combined.map(|b| combined > b).unwrap_or(true) {
                best_combined = Some(combined);
                best_template = Some(template_id.clone());
                best_score = base.max(combined);
            }
            if let Some(stats) = learning_stats {
                let bonus = learning_bonus(
                    Some(template_id),
                    Some(method.id),
                    Some(stats),
                    None,
                    LearningKnobs {
                        max_bonus: knobs.max_bonus,
                        prior: knobs.prior,
                    },
                );
                best_bonus = best_bonus.max(bonus);
            }
        }
        if let Some(template_id) = &best_template {
            reason = format!("{method_reason}; template {template_id}");
        }
        if best_score <= 0.0 {
            continue;
        }
        if best_bonus > 0.0 {
            best_score = (best_score + best_bonus).min(1.0);
        }
        out.push(Recommendation {
            num: method.num,
            id: method.id,
            name: method.name,
            score: round3(clamp01(best_score)),
            reason,
            template: best_template,
            reason_key,
            reason_args,
        });
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.num.cmp(&b.num))
    });
    out.truncate(limit);
    out
}

/// `CORE150_PRIOR` — the small per-damage-class prior `suggest_methods` blends
/// in at 15%.
fn core150_prior(class: &str) -> &'static [(u8, f64)] {
    match class {
        "clean" => &[(1, 0.95), (2, 0.95), (3, 0.95), (9, 0.85)],
        "holes" => &[(18, 0.80), (3, 0.80), (1, 0.70), (2, 0.70)],
        "si" => &[(10, 0.70), (7, 0.55), (13, 0.55)],
        "repeated" => &[(8, 0.80), (1, 0.80), (4, 0.80), (13, 0.70)],
        "scan" => &[(9, 0.90), (1, 0.70), (3, 0.70)],
        "duplicate" => &[(8, 0.90), (1, 0.90), (2, 0.80), (4, 0.80)],
        _ => &[],
    }
}

/// `SUGGESTION_REASONS`.
fn suggestion_reason(class: &str) -> &'static str {
    match class {
        "clean" => "safe default for a closed, clean mesh",
        "holes" => "best for holes and open edges",
        "si" => "best for overlapping surfaces",
        "repeated" => "best for repeated parts and copies",
        "scan" => "best for a single-sided scan",
        "duplicate" => "best for duplicate or degenerate faces",
        _ => "safe default for a closed, clean mesh",
    }
}

/// `SUGGESTION_METHOD_REASONS`.
fn suggestion_method_reason(num: u8) -> Option<&'static str> {
    Some(match num {
        1 => "quick cleanup of small defects",
        2 => "local repair around the damage",
        3 => "rebuilds the surface and closes holes",
        4 => "merges separate pieces into one part",
        5 => "refines self-intersections automatically",
        6 => "exact self-intersection cleanup",
        7 => "rebuilds a watertight solid",
        9 => "closes an open scan from the back",
        10 => "rebuilds a clean proxy surface",
        11 => "copies a healthy repeated pattern",
        12 => "copies a healthy pattern from picked points",
        13 => "wraps the damaged area in a thin shell",
        18 => "closes holes with an exact patch",
        19 => "welds overlapping surfaces at the seams",
        _ => return None,
    })
}

/// `SUGGESTION_METHOD_REASONS` reason keys, as `&'static str` so nothing is
/// allocated per call (`sugg_method_<num>`).
fn suggestion_method_key(num: u8) -> &'static str {
    match num {
        1 => "sugg_method_1",
        2 => "sugg_method_2",
        3 => "sugg_method_3",
        4 => "sugg_method_4",
        5 => "sugg_method_5",
        6 => "sugg_method_6",
        7 => "sugg_method_7",
        9 => "sugg_method_9",
        10 => "sugg_method_10",
        11 => "sugg_method_11",
        12 => "sugg_method_12",
        13 => "sugg_method_13",
        18 => "sugg_method_18",
        19 => "sugg_method_19",
        _ => "",
    }
}

/// `_damage_class`: coarse class used only to pick the suggestion prior.
fn damage_class(analysis: &Analysis) -> &'static str {
    let ch = |name: &str| -> i64 {
        analysis
            .diagnosis_v2
            .as_ref()
            .and_then(|d| d.get("channels"))
            .and_then(|c| c.get(name))
            .and_then(|c| c.get("count"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    if analysis.self_intersections > 0.0 || ch("self_intersection") > 0 {
        return "si";
    }
    if analysis.non_manifold_edges > 0.0 || analysis.boundary_loops > 0.0 || ch("loops") > 0 {
        return "holes";
    }
    if analysis.repetition_score >= 0.5 {
        return "repeated";
    }
    if analysis.single_side_score >= 0.5 || analysis.relief_score >= 0.5 {
        return "scan";
    }
    if ch("duplicate") > 0 || ch("degenerate") > 0 {
        return "duplicate";
    }
    "clean"
}

/// `_has_body_overlap`: true only with diagnosis evidence of touching bodies.
fn has_body_overlap(analysis: &Analysis) -> bool {
    let d = match analysis.diagnosis_v2.as_ref() {
        Some(d) => d,
        None => return false,
    };
    if d.get("self_intersections")
        .and_then(|s| s.get("inter_body"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
        > 0
    {
        return true;
    }
    match d.get("parts").and_then(|p| p.get("overlap_edges")) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) > 0,
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Bool(b)) => *b,
        _ => false,
    }
}

/// `int(round(conf * 100))` — Python rounds half-to-even on the binary value.
fn py_round_int(value: f64) -> i64 {
    if !value.is_finite() {
        return 0;
    }
    format!("{:.0}", value).parse::<f64>().unwrap_or(0.0) as i64
}

/// `suggest_methods(analysis, top_n)` — presentation only.
///
/// `rank_methods` stays the source ranking; this blends the core150 prior at
/// 15% and adds the High/Medium/Low label. Kept in the same module as the
/// ranking so there is one ordering implementation, not two.
pub fn suggest_methods(
    loaded: &LoadedPolicy,
    analysis: &Analysis,
    top_n: usize,
    learning_stats: Option<&serde_json::Map<String, Value>>,
) -> Value {
    let class = damage_class(analysis);
    let priors = core150_prior(class);
    let mut recs = rank_methods(loaded, analysis, Some(top_n.max(6)), learning_stats);
    if !has_body_overlap(analysis) {
        recs.retain(|r| r.num != 4);
    }

    let mut items: Vec<(u8, i64, f64, &'static str, &'static str, &'static str)> = Vec::new();
    for rec in &recs {
        let prior = priors
            .iter()
            .find(|(num, _)| *num == rec.num)
            .map(|(_, p)| *p);
        let blended = match prior {
            None => rec.score,
            Some(p) => 0.85 * rec.score + 0.15 * p,
        };
        let conf = min95(clamp01(blended));
        let (reason, reason_key) = match suggestion_method_reason(rec.num) {
            Some(r) => (r, suggestion_method_key(rec.num)),
            None => (suggestion_reason(class), class_reason_key(class)),
        };
        let display_name = rec.name;
        items.push((
            rec.num,
            py_round_int(conf * 100.0),
            round3(conf),
            display_name,
            reason,
            reason_key,
        ));
    }
    // sort by (-confidence_value, num)
    items.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    items.truncate(top_n);

    let mut out = Vec::new();
    for (i, (num, pct, conf_value, name, reason, reason_key)) in items.iter().enumerate() {
        let nxt = items.get(i + 1).map(|x| x.1);
        let prev = if i > 0 { Some(items[i - 1].1) } else { None };
        let next_gap = match nxt {
            Some(n) => pct - n,
            None => match prev {
                Some(p) => pct - p,
                None => 0,
            },
        };
        let prev_gap = match prev {
            Some(p) => pct - p,
            None => 100,
        };
        let label = if *pct >= 85 && next_gap >= 10 && prev_gap >= 10 {
            "high"
        } else if *pct >= 50 {
            "medium"
        } else {
            "low"
        };
        let method = METHODS.iter().find(|m| m.num == *num);
        out.push(json!({
            "num": num,
            "id": method.map(|m| m.id).unwrap_or(""),
            "name": method.map(|m| m.name).unwrap_or(name),
            "display_name": method.map(|m| m.name).unwrap_or(name),
            "confidence": pct,
            "confidence_value": conf_value,
            "reason": reason,
            "reason_key": reason_key,
            "reason_args": [],
            "basis": "rank+core150-c26ed2e",
            "estimate": true,
            "confidence_label": label,
        }));
    }
    Value::Array(out)
}

fn class_reason_key(class: &str) -> &'static str {
    match class {
        "clean" => "sugg_reason_clean",
        "holes" => "sugg_reason_holes",
        "si" => "sugg_reason_si",
        "repeated" => "sugg_reason_repeated",
        "scan" => "sugg_reason_scan",
        "duplicate" => "sugg_reason_duplicate",
        _ => "sugg_reason_clean",
    }
}

fn min95(value: f64) -> f64 {
    value.min(0.95)
}

/// `recommendations` JSON block used by `--analyze`.
pub fn recommendations_json(recs: &[Recommendation]) -> Value {
    Value::Array(
        recs.iter()
            .map(|r| {
                json!({
                    "num": r.num,
                    "id": r.id,
                    "name": r.name,
                    "score": r.score,
                    "reason": r.reason,
                    "template": r.template,
                    "reason_key": r.reason_key,
                    "reason_args": r.reason_args,
                })
            })
            .collect(),
    )
}


/// Registry numbers used by the Graft/fTetWild ordering and tie-break.
pub const GRAFT_METHOD_NUM: u8 = 13;
pub const FTETWILD_METHOD_NUM: u8 = 7;

/// Python `pipeline.order.order_graft_before_ftetwild`: keep the set, move
/// Graft in front of fTetWild when both are present.
pub fn order_graft_before_ftetwild(recs: Vec<Recommendation>) -> Vec<Recommendation> {
    let graft = recs.iter().position(|r| r.num == GRAFT_METHOD_NUM);
    let ft = recs.iter().position(|r| r.num == FTETWILD_METHOD_NUM);
    match (graft, ft) {
        (Some(g), Some(f)) if g > f => {
            let mut out = recs;
            let item = out.remove(g);
            out.insert(f, item);
            out
        }
        _ => recs,
    }
}

/// Python `pipeline.order.ftetwild_supersedes_graft`.
pub fn ftetwild_supersedes_graft(
    ft_dev: Option<f64>,
    graft_dev: Option<f64>,
    coverage_failed: bool,
) -> bool {
    if coverage_failed {
        return true;
    }
    match (ft_dev, graft_dev) {
        (Some(ft), Some(g)) => ft < g,
        _ => false,
    }
}

/// Python `triage._prune_avoid_nums`: duplicate damage makes Join futile and
/// interior damage makes Transplant/Wall futile (the latter are already
/// filtered out by `needs_user_input`).
pub fn prune_avoid_nums(analysis: &Analysis) -> HashSet<u8> {
    let mut avoid = HashSet::new();
    let Some(d2) = analysis.diagnosis_v2.as_ref() else {
        return avoid;
    };
    let count = |name: &str| -> f64 {
        d2.get("channels")
            .and_then(|c| c.get(name))
            .and_then(|e| e.get("count"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    };
    if count("duplicate") > 0.0 {
        avoid.insert(4);
    }
    let interior_region = d2
        .get("regions")
        .and_then(Value::as_array)
        .map(|regions| {
            regions.iter().any(|r| {
                r.get("channel").and_then(Value::as_str) == Some("interior")
            })
        })
        .unwrap_or(false);
    if count("interior") > 0.0 || interior_region {
        avoid.insert(11);
        avoid.insert(15);
    }
    avoid
}

#[cfg(test)]
mod order_tests {
    use super::*;

    fn rec(num: u8) -> Recommendation {
        Recommendation {
            num,
            id: "",
            name: "",
            score: 0.0,
            reason: String::new(),
            template: None,
            reason_key: "",
            reason_args: Vec::new(),
        }
    }

    #[test]
    fn graft_moves_before_ftetwild() {
        let out = order_graft_before_ftetwild(vec![rec(7), rec(3), rec(13)]);
        let nums: Vec<u8> = out.iter().map(|r| r.num).collect();
        assert_eq!(nums, vec![13, 7, 3]);
    }

    #[test]
    fn tie_break_matches_python() {
        assert!(ftetwild_supersedes_graft(Some(0.1), Some(0.2), false));
        assert!(!ftetwild_supersedes_graft(Some(0.2), Some(0.1), false));
        assert!(!ftetwild_supersedes_graft(None, Some(0.1), false));
        assert!(ftetwild_supersedes_graft(None, None, true));
    }

    #[test]
    fn avoid_prune_reads_channels() {
        let mut a = Analysis::default();
        a.diagnosis_v2 = Some(json!({"channels": {"duplicate": {"count": 2}}}));
        assert!(prune_avoid_nums(&a).contains(&4));
        a.diagnosis_v2 = Some(json!({"channels": {"interior": {"count": 1}}}));
        let avoid = prune_avoid_nums(&a);
        assert!(avoid.contains(&11) && avoid.contains(&15));
    }
}


// --- Suggestions / auto-reorder -------------------------------------------

const CORE150_PRIOR_CLEAN: &[(u8, f64)] = &[(1, 0.95), (2, 0.95), (3, 0.95), (9, 0.85)];
const CORE150_PRIOR_HOLES: &[(u8, f64)] = &[(18, 0.80), (3, 0.80), (1, 0.70), (2, 0.70)];
const CORE150_PRIOR_SI: &[(u8, f64)] = &[(10, 0.70), (7, 0.55), (13, 0.55)];
const CORE150_PRIOR_REPEATED: &[(u8, f64)] = &[(8, 0.80), (1, 0.80), (4, 0.80), (13, 0.70)];
const CORE150_PRIOR_SCAN: &[(u8, f64)] = &[(9, 0.90), (1, 0.70), (3, 0.70)];
const CORE150_PRIOR_DUPLICATE: &[(u8, f64)] = &[(8, 0.90), (1, 0.90), (2, 0.80), (4, 0.80)];

fn prior_for(class: &str) -> &'static [(u8, f64)] {
    match class {
        "holes" => CORE150_PRIOR_HOLES,
        "si" => CORE150_PRIOR_SI,
        "repeated" => CORE150_PRIOR_REPEATED,
        "scan" => CORE150_PRIOR_SCAN,
        "duplicate" => CORE150_PRIOR_DUPLICATE,
        _ => CORE150_PRIOR_CLEAN,
    }
}

/// Python `methods.suggest_methods` ordering only (numbers, confidence-ranked).
pub fn suggest_order(loaded: &LoadedPolicy, analysis: &Analysis, top_n: usize) -> Vec<u8> {
    let mut recs = rank_methods(loaded, analysis, Some(top_n.max(6)), None);
    if !has_body_overlap(analysis) {
        recs.retain(|r| r.num != 4);
    }
    let priors = prior_for(damage_class(analysis));
    let mut scored: Vec<(u8, f64)> = recs
        .iter()
        .map(|r| {
            let prior = priors.iter().find(|(n, _)| *n == r.num).map(|(_, p)| *p);
            let conf = match prior {
                Some(p) => 0.85 * r.score + 0.15 * p,
                None => r.score,
            };
            (r.num, crate::classify::py_round(conf.min(0.95).clamp(0.0, 1.0), 3))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    scored.into_iter().take(top_n).map(|(n, _)| n).collect()
}

/// Python `triage._apply_auto_reorder`: suggestion order, then the Graft-before-
/// fTetWild invariant.  Opt-in (`--auto-reorder`).
pub fn apply_auto_reorder(
    recs: Vec<Recommendation>,
    loaded: &LoadedPolicy,
    analysis: &Analysis,
) -> Vec<Recommendation> {
    let order = suggest_order(loaded, analysis, recs.len().max(3));
    let rank: std::collections::HashMap<u8, usize> = order
        .iter()
        .enumerate()
        .map(|(i, n)| (*n, i))
        .collect();
    let tail = rank.len();
    let mut out = recs;
    out.sort_by_key(|r| rank.get(&r.num).copied().unwrap_or(tail));
    order_graft_before_ftetwild(out)
}
