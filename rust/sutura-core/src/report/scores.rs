// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! The score block every front end adds to a repair report.
//!
//! Both the CLI and the GUI must publish the same health/risk/status/fidelity/
//! confidence values for the same repair. They were diverging: the GUI passed a
//! default (empty) report, so its panel showed `—` for everything while the CLI's
//! JSON was full. Keeping the recipe here means a front end calls one function
//! and cannot forget a field.

use serde_json::{json, Map, Value};

use crate::report;

/// The scoring configuration (weights, tiers, budgets).
///
/// Identical to the values the Python scorer uses; kept here so every caller
/// reads one copy.
pub fn score_cfg() -> Value {
    json!({
        "health": {"weights": {"watertight": 39, "no_non_manifold": 22,
                               "no_self_intersections": 22, "no_holes": 17}},
        "risk": {"weights": {"face_delta": 30, "vertex_delta": 20,
                             "component_change": 25, "volume_delta": 25},
                 "delta_scale_percent": 25, "component_loss_penalty": 30},
        "fidelity": {
            "weights": {"healthy_distance": 35, "feature_recall": 25,
                        "healthy_volume": 20, "volume_bbox": 10, "invented_area": 10},
            "distance_scale_pct": 0.5, "distance_mean_weight": 0.3,
            "volume_scale_fraction": 0.01, "volume_change_scale_pct": 15.0,
            "bbox_scale_pct": 5.0, "invented_scale_fraction": 0.05,
            "invented_tol_pct": 1.0, "change_tol_factor": 3.0,
            "changed_min_healthy_distance_pct": 1.0,
            "tiers": {"high": 85, "mid": 60},
            "low_confidence": {"invented_area_fraction": 0.03,
                               "area_min_fraction": 0.0005,
                               "low_area_min_fraction": 0.03, "max_areas": 99},
            "budgets": {"full_faces": 20000, "max_faces": 150000,
                        "distance_samples": 4000, "reduced_distance_samples": 1200,
                        "sharp_samples_per_edge": 4, "reduced_sharp_samples_per_edge": 2,
                        "recall_max_faces": 50000, "max_sharp_samples": 6000,
                        "full_lattice_area": 1200, "full_lattice_layers": 6,
                        "reduced_lattice_area": 400, "reduced_lattice_layers": 4}
        },
        "status": {
            "health_tiers": {"high": 80, "mid": 60},
            "risk_tiers": {"high": 70, "mid": 40},
            "labels": {"safe": "Safe to inspect", "review": "Review recommended",
                       "caution": "Caution advised", "failed": "Failed / inspect",
                       "unavailable": "Scoring unavailable"},
            "table": {"high_low": "safe", "high_mid": "review", "high_high": "review",
                      "mid_low": "review", "mid_mid": "review", "mid_high": "caution",
                      "low_low": "failed", "low_mid": "failed", "low_high": "failed"}
        }
    })
}

/// Add the fidelity, confidence and health/risk/status blocks to `obj`.
///
/// `obj` is the report built so far; it is updated in place. Whole numbers are
/// emitted as integers so the JSON matches Python's (`87`, not `87.0`).
pub fn add_scores(
    obj: &mut Map<String, Value>,
    in_f64: &[[f64; 3]],
    in_tris: &[[i32; 3]],
    out_f64: &[[f64; 3]],
    out_i32: &[[i32; 3]],
) {
    let cfg = score_cfg();

    // The input defect counts are an input to scoring: `repair_confidence` gives
    // a bonus for "every input hole was closed", which it can only see through
    // `defects`. Without this the GUI scored 82 where the CLI scored 87.
    if !obj.contains_key("defects") {
        let in_f32: Vec<[f32; 3]> = in_f64
            .iter()
            .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
            .collect();
        let defects = report::detect_defects(&in_f32, in_tris, false);
        obj.insert(
            "defects".into(),
            json!({
                "holes": defects.holes.iter().map(|h| json!({
                    "centroid": h.centroid,
                    "diameter": h.diameter,
                    "vertices": h.vertices,
                })).collect::<Vec<_>>(),
                "non_manifold": defects.non_manifold.iter().map(|n| json!({
                    "centroid": n.centroid,
                    "faces": n.faces,
                })).collect::<Vec<_>>(),
            }),
        );
    }

    let fidelity = report::compute_fidelity_value(
        in_f64,
        in_tris,
        out_f64,
        out_i32,
        &Value::Object(obj.clone()),
        &cfg,
    );
    if let Some(fmap) = fidelity.as_object() {
        for (key, value) in fmap {
            obj.insert(key.clone(), integerise(key, value));
        }
    }

    let confidence = report::repair_confidence_value(&Value::Object(obj.clone()));
    obj.insert("repair_confidence".into(), Value::from(confidence.score));
    obj.insert(
        "repair_confidence_label".into(),
        Value::from(confidence.label.clone()),
    );
    obj.insert(
        "repair_confidence_factors".into(),
        Value::Object(confidence.factors.clone()),
    );

    let scores = report::compute_scores_value(&Value::Object(obj.clone()), &cfg);
    for key in [
        "repair_health",
        "repair_health_factors",
        "repair_risk",
        "repair_risk_factors",
        "repair_status",
        "repair_status_code",
    ] {
        let source = key.strip_prefix("repair_").unwrap_or(key);
        if let Some(value) = scores.get(source) {
            obj.insert(key.into(), integerise(key, value));
        }
    }
}

/// Whole floats become integers, matching Python's `87` vs `87.0`.
fn integerise(key: &str, value: &Value) -> Value {
    match value {
        Value::Number(n) if key != "repair_fidelity_note" => match n.as_f64() {
            Some(f) if f.fract() == 0.0 => Value::from(f as i64),
            _ => value.clone(),
        },
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> (Vec<[f64; 3]>, Vec<[i32; 3]>) {
        let v = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        let t = vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ];
        (v, t)
    }

    /// The score block must be fully populated — the GUI bug was a report with
    /// every score missing, which rendered as dashes.
    ///
    /// Driven through a real repair rather than a hand-built report: the fidelity
    /// block reads many fields, and a fixture missing them produces a legitimate
    /// "failed" status that says nothing about this function.
    #[test]
    fn add_scores_populates_every_field() {
        let (v, t) = cube();
        let v64: Vec<[f64; 3]> = v.clone();
        let t64: Vec<[i64; 3]> = t.iter().map(|x| [x[0] as i64, x[1] as i64, x[2] as i64]).collect();
        let opts = crate::repair::RepairOptions::default();
        let token = crate::repair::CancelToken::new();
        let outcome = crate::repair::repair(&v64, &t64, &opts, &crate::repair::NoopSink, &token);
        let crate::repair::RepairOutcome::Ok { report, .. } = outcome else {
            panic!("cube should repair");
        };

        let mut obj = report.as_object().expect("report object").clone();
        add_scores(&mut obj, &v64, &t, &v64, &t);

        for key in [
            "repair_fidelity",
            "repair_confidence",
            "repair_health",
            "repair_risk",
            "repair_status",
            "repair_status_code",
        ] {
            assert!(obj.contains_key(key), "missing {key}");
            assert!(!obj[key].is_null(), "{key} must not be null");
        }
        // A clean closed cube is fully healed.
        assert_eq!(obj["repair_status_code"], json!("safe"));
        assert!(obj["repair_health"].as_f64().unwrap() > 80.0);
        // Whole numbers are integers, not floats.
        assert!(obj["repair_health"].is_i64() || obj["repair_health"].is_u64());
    }
}
