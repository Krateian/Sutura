// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Mesh type classification: the classic engine and the v2 experimental head.
//!
//! Bit-parity port of `sutura/mesh_classifier.py` and
//! `sutura/mesh_classifier_v2.py`. The v2 head uses RANSAC plane features
//! drawn through [`crate::rng::Pcg64`], which mirrors NumPy's `default_rng`
//! exactly (see `rng.rs`).
//!
//! The classic engine remains the fallback: with no baked head, v2 returns the
//! classic decision verbatim (metrics still carry the RANSAC/curvature
//! features).

use serde_json::{json, Value};

use sutura_geom::classify::{
    curvature_developable_fraction, dihedral_stats, ransac_plane_features,
};

const MECH_NEAR90: f64 = 35.0;
const W_NEAR90: f64 = 8.0;
const MECH_FLAT: f64 = 50.0;
const W_FLAT: f64 = 12.0;
const ORG_FLAT: f64 = 45.0;
const W_ORG_FLAT: f64 = 6.0;
const ORG_GENTLE: f64 = 15.0;
const W_GENTLE: f64 = 5.0;
const MECH_FLOOR: f64 = 0.7;
const ORG_FLOOR: f64 = 0.5;


const HEAD_MECH_FLOOR: f64 = 0.50;
const HEAD_MARGIN: f64 = 0.15;
const CLASSIC_STRONG: f64 = 0.70;
const V2_WEIGHTS: [f64; 7] = [2.3823, 2.9866, -0.9694, -0.5230, 0.5083, 2.2289, 1.0432];
const V2_MEAN: [f64; 6] = [20.9532, 30.5532, 35.5085, 4.9296, 0.2917, 0.5122];
const V2_STD: [f64; 6] = [24.1622, 17.6785, 30.5004, 6.4313, 0.3129, 0.2819];

/// Opt-in edge-tiebreak head (`--experimental-edge-tiebreak`): the base six
/// features plus the five FAZ10 signals.
const V2_EXTRA_WEIGHTS: [f64; 12] = [
    2.3710, 1.2358, -0.7830, 0.0135, 0.5777, 2.1649, 1.1438, 0.2955, 1.9530, 0.0802, 0.0848,
    0.2433,
];
const V2_EXTRA_MEAN: [f64; 11] = [
    20.9532, 30.5532, 35.5085, 4.9296, 0.2917, 0.5122, 8880301.1344, 0.2947, 2.6936, 286513.4431,
    0.5724,
];
const V2_EXTRA_STD: [f64; 11] = [
    24.1622, 17.6785, 30.5004, 6.4313, 0.3129, 0.2819, 44317875.2737, 0.2680, 0.3578,
    1277023.6886, 0.8934,
];

pub use sutura_geom::classify::py_round;

fn sigmoid(x: f64) -> f64 {
    let x = x.clamp(-50.0, 50.0);
    1.0 / (1.0 + (-x).exp())
}

fn class_scores(near90: f64, flat: f64, gentle: f64) -> (f64, f64) {
    let mech_near = sigmoid((near90 - MECH_NEAR90) / W_NEAR90);
    let mech_flat = sigmoid((flat - MECH_FLAT) / W_FLAT);
    let mechanical = mech_near.max(mech_flat);
    let org_near = 1.0 - mech_near;
    let org_flat = 1.0 - sigmoid((flat - ORG_FLAT) / W_ORG_FLAT);
    let org_gentle = sigmoid((gentle - ORG_GENTLE) / W_GENTLE);
    (mechanical, organic_min(org_near, org_flat, org_gentle))
}

fn organic_min(a: f64, b: f64, c: f64) -> f64 {
    a.min(b).min(c)
}

/// `{'type', 'confidence', 'metrics'}` from the classic engine.
pub fn classify_classic(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> Value {
    let (near90, flat, gentle) = dihedral_stats(verts, tris);
    let (mechanical, organic) = class_scores(near90, flat, gentle);
    let mut metrics = serde_json::Map::new();
    metrics.insert("near90".into(), json!(py_round(near90, 2)));
    metrics.insert("flat".into(), json!(py_round(flat, 2)));
    metrics.insert("gentle".into(), json!(py_round(gentle, 2)));
    metrics.insert(
        "mechanical_score".into(),
        json!(py_round(mechanical, 3)),
    );
    metrics.insert("organic_score".into(), json!(py_round(organic, 3)));
    if mechanical >= MECH_FLOOR {
        return json!({"type": "mechanical", "confidence": py_round(mechanical, 3),
                      "metrics": Value::Object(metrics),
                      "feature_engine": "rust"});
    }
    if organic >= ORG_FLOOR {
        return json!({"type": "organic", "confidence": py_round(organic, 3),
                      "metrics": Value::Object(metrics),
                      "feature_engine": "rust"});
    }
    metrics.insert(
        "leaning".into(),
        json!(if mechanical >= organic {
            "mechanical"
        } else {
            "organic"
        }),
    );
    json!({"type": "unknown", "confidence": py_round(mechanical.max(organic), 3),
           "metrics": Value::Object(metrics),
           "feature_engine": "rust"})
}

fn head_p_mechanical(features: [f64; 6]) -> f64 {
    let xs: Vec<f64> = features
        .iter()
        .enumerate()
        .map(|(i, x)| (x - V2_MEAN[i]) / V2_STD[i])
        .collect();
    let z = V2_WEIGHTS[0]
        + xs.iter()
            .enumerate()
            .map(|(i, x)| V2_WEIGHTS[i + 1] * x)
            .sum::<f64>();
    sigmoid(z)
}

fn head_p_mechanical_extra(base: [f64; 6], extra: [f64; 5]) -> f64 {
    let mut x = [0.0f64; 11];
    x[..6].copy_from_slice(&base);
    x[6..].copy_from_slice(&extra);
    let z = V2_EXTRA_WEIGHTS[0]
        + x.iter()
            .enumerate()
            .map(|(i, value)| {
                let xs = (value - V2_EXTRA_MEAN[i]) / V2_EXTRA_STD[i];
                V2_EXTRA_WEIGHTS[i + 1] * xs
            })
            .sum::<f64>();
    sigmoid(z)
}

fn classic_verdict(mechanical: f64, organic: f64) -> (&'static str, f64) {
    if mechanical >= MECH_FLOOR {
        ("mechanical", mechanical)
    } else if organic >= ORG_FLOOR {
        ("organic", organic)
    } else {
        ("unknown", mechanical.max(organic))
    }
}

/// `classify_mesh(..., engine='experimental')` with the baked head.
pub fn classify_v2(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> Value {
    classify_v2_extra(verts, tris, false)
}

/// The v2 head, optionally with the opt-in edge-tiebreak 11-feature model.
pub fn classify_v2_extra(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    extra_features: bool,
) -> Value {
    let (near90, flat, gentle) = dihedral_stats(verts, tris);
    let (plane_count, plane_area) = ransac_plane_features(verts, tris);
    let developable = curvature_developable_fraction(verts, tris);
    let (mechanical, organic) = class_scores(near90, flat, gentle);

    let mut metrics = serde_json::Map::new();
    metrics.insert("near90".into(), json!(py_round(near90, 2)));
    metrics.insert("flat".into(), json!(py_round(flat, 2)));
    metrics.insert("gentle".into(), json!(py_round(gentle, 2)));
    metrics.insert(
        "mechanical_score".into(),
        json!(py_round(mechanical, 3)),
    );
    metrics.insert("organic_score".into(), json!(py_round(organic, 3)));
    metrics.insert("plane_count".into(), json!(plane_count));
    metrics.insert("plane_area".into(), json!(plane_area));
    metrics.insert("developable_fraction".into(), json!(developable));

    let base = [
        near90,
        flat,
        gentle,
        plane_count as f64,
        plane_area,
        developable,
    ];
    let p_mech = if extra_features {
        let extra = sutura_geom::classify::edge_extra_features(verts, tris);
        metrics.insert("edge_oppmax_std".into(), json!(py_round(extra[0], 4)));
        metrics.insert("edge_don_mean".into(), json!(py_round(extra[1], 4)));
        metrics.insert("edge_dihed_mean".into(), json!(py_round(extra[2], 4)));
        metrics.insert("edge_rmin_mean".into(), json!(py_round(extra[3], 4)));
        metrics.insert("edge_gc_mean".into(), json!(py_round(extra[4], 4)));
        head_p_mechanical_extra(base, extra)
    } else {
        head_p_mechanical(base)
    };
    if !(0.0..=1.0).contains(&p_mech) {
        return classify_classic(verts, tris);
    }
    let mut head_type = if p_mech >= HEAD_MECH_FLOOR {
        "mechanical"
    } else {
        "organic"
    };
    let mut head_conf = ((p_mech - 0.5).abs() * 2.0).clamp(0.0, 1.0);
    if head_conf < HEAD_MARGIN {
        let (classic_type, classic_score) = classic_verdict(mechanical, organic);
        if classic_type == head_type && classic_score >= CLASSIC_STRONG {
            head_conf = classic_score;
        }
    }
    if head_type.is_empty() {
        head_type = "unknown";
    }
    json!({"type": head_type, "confidence": py_round(head_conf, 3),
           "metrics": Value::Object(metrics),
           "feature_engine": "rust"})
}

/// Public switch: `engine` is `classic` or anything else (experimental).
pub fn classify_mesh(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    engine: &str,
) -> Value {
    classify_mesh_with(verts, tris, engine, false)
}

/// `classify_mesh` with the opt-in edge-tiebreak head (`extra_features`).
pub fn classify_mesh_with(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    engine: &str,
    extra_features: bool,
) -> Value {
    if engine == "classic" {
        classify_classic(verts, tris)
    } else {
        classify_v2_extra(verts, tris, extra_features)
    }
}
