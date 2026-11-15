//! Method ranking and suggestions for the read-only `--analyze` path.
//!
//! Port of `sutura_engine/methods/__init__.py` (`rank_methods`,
//! `suggest_methods`) restricted to what the Rust CLI needs: the analysis
//! fields, the policy knobs (`ranking.*`, `templates.*`) and the learning
//! bonus. Method availability and `needs_user_input` are static here; the
//! Python registry can be extended at runtime, so this is the built-in set.

use std::collections::BTreeMap;

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
    templates: BTreeMap<String, Value>,
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
    let mut templates = BTreeMap::new();
    let mut insert = |id: &str, value: Value| {
        templates.insert(id.to_string(), value);
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
    insert("mechanical", json!({"id": "mechanical", "preferred": preferred("mechanical"), "cfg": tpl}));
    insert("organic", json!({"id": "organic", "preferred": preferred("organic"), "cfg": tpl}));
    insert("single_side_scan", json!({"id": "single_side_scan", "preferred": preferred("single_side_scan"), "cfg": tpl}));
    insert("relief", json!({"id": "relief", "preferred": preferred("relief"), "cfg": tpl}));
    insert("repeated_pattern", json!({"id": "repeated_pattern", "preferred": preferred("repeated_pattern"), "cfg": tpl}));
    insert("dense_scan_heavy_si", json!({"id": "dense_scan_heavy_si", "preferred": preferred("dense_scan_heavy_si"), "cfg": tpl}));
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
