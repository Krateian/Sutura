// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use sutura_core::config::policy::default_policy;
use sutura_core::ranking::{rank_methods, recommendations_json, suggest_methods, Analysis};

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "rank".to_string());
    // Fields are fed as a JSON object so the probe can compare with Python's
    // `rank_methods(analysis)` on the same analysis.
    let payload: serde_json::Value = serde_json::from_str(&args.next().expect("analysis json"))
        .expect("analysis json");
    let f = |k: &str| payload.get(k).and_then(serde_json::Value::as_f64).unwrap_or(0.0);
    let analysis = Analysis {
        mesh_type: payload
            .get("mesh_type")
            .and_then(serde_json::Value::as_str)
            .map(|s| s.to_string()),
        type_confidence: f("type_confidence"),
        boundary_loops: f("boundary_loops"),
        non_manifold_edges: f("non_manifold_edges"),
        self_intersections: f("self_intersections"),
        components: f("components"),
        largest_loop_ratio: f("largest_loop_ratio"),
        open_area_ratio: f("open_area_ratio"),
        single_side_score: f("single_side_score"),
        relief_score: f("relief_score"),
        repetition_score: f("repetition_score"),
        diagnosis_v2: payload.get("diagnosis_v2").cloned(),
    };
    let loaded = default_policy().expect("policy");
    if mode == "suggest" {
        println!("{}", suggest_methods(&loaded, &analysis, 3, None));
        return;
    }
    let recs = rank_methods(&loaded, &analysis, None, None);
    println!("{}", recommendations_json(&recs));
}
