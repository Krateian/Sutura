// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Mirror-completion producer, ported from `sutura_engine/mirror.py`.

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Value};
use sutura_geom::manifold::outer_shell_union_core;

use crate::closing::{boundary_loops, check_validity, jacobi_eigen, loop_stats, poisson_close};
use crate::graft::helpers::{bbox, np_pairwise_sum, referenced_only};
use crate::stage1::chain::apply_chain;

pub const PLANE_RMS_GOOD_REL: f64 = 0.01;
pub const PLANE_RMS_BAD_REL: f64 = 0.06;
pub const MIRROR_MIN_SCORE: f64 = 0.35;
pub const MIRROR_WELD_REL: f64 = 2.0e-4;

pub struct MirrorOutput {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[i64; 3]>,
    pub report: Value,
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn diag_of(verts: &[[f64; 3]]) -> f64 {
    let (lo, hi) = bbox(verts);
    let d = sub(hi, lo);
    dot(d, d).sqrt()
}

fn mean(points: &[[f64; 3]]) -> [f64; 3] {
    let n = points.len() as f64;
    [
        np_pairwise_sum(&points.iter().map(|p| p[0]).collect::<Vec<_>>()) / n,
        np_pairwise_sum(&points.iter().map(|p| p[1]).collect::<Vec<_>>()) / n,
        np_pairwise_sum(&points.iter().map(|p| p[2]).collect::<Vec<_>>()) / n,
    ]
}

fn round_nd(x: f64, digits: usize) -> f64 {
    format!("{:.*}", digits, x).parse().unwrap_or(x)
}

pub fn detect_mirror_plane(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
) -> (Option<[f64; 3]>, Option<[f64; 3]>, f64, Value) {
    let mut info = json!({ "n_loops": 0 });
    if tris.is_empty() {
        info["reason"] = json!("empty");
        return (None, None, 0.0, info);
    }

    let loops = boundary_loops(tris);
    info["n_loops"] = json!(loops.len());
    if loops.is_empty() {
        info["reason"] = json!("closed");
        return (None, None, 0.0, info);
    }

    let perims: Vec<f64> = loops.iter().map(|l| loop_stats(verts, l).0).collect();
    let total_perim: f64 = perims.iter().sum();
    if total_perim <= 1e-12 {
        info["reason"] = json!("zero_perimeter");
        return (None, None, 0.0, info);
    }
    let mut dom_idx = 0usize;
    for i in 1..perims.len() {
        if perims[i] > perims[dom_idx] {
            dom_idx = i;
        }
    }
    let dom_ratio = perims[dom_idx] / total_perim;

    let diag = diag_of(verts);
    if diag <= 1e-12 {
        info["reason"] = json!("degenerate_bbox");
        return (None, None, 0.0, info);
    }

    let loop_indices = &loops[dom_idx];
    let pts: Vec<[f64; 3]> = loop_indices
        .iter()
        .map(|&i| verts[i as usize])
        .collect();
    let c = mean(&pts);

    let mut cov = [[0.0f64; 3]; 3];
    for p in &pts {
        let d = sub(*p, c);
        for i in 0..3 {
            for j in 0..3 {
                cov[i][j] += d[i] * d[j];
            }
        }
    }
    let (_eigs, vecs) = jacobi_eigen(cov);
    let n = vecs[2];

    let rms = (np_pairwise_sum(
        &pts
            .iter()
            .map(|p| {
                let d = dot(sub(*p, c), n);
                d * d
            })
            .collect::<Vec<_>>(),
    ) / pts.len() as f64)
        .sqrt();
    let rms_rel = rms / diag;

    let s_planar = ((PLANE_RMS_BAD_REL - rms_rel) / (PLANE_RMS_BAD_REL - PLANE_RMS_GOOD_REL))
        .clamp(0.0, 1.0);
    let s_dom = ((dom_ratio - 0.40) / 0.45).clamp(0.0, 1.0);
    let score = round_nd(s_planar * s_dom, 4);
    if let Some(obj) = info.as_object_mut() {
        obj.insert("reason".into(), json!("ok"));
        obj.insert("dominant_ratio".into(), json!(round_nd(dom_ratio, 4)));
        obj.insert("rms_rel".into(), json!(round_nd(rms_rel, 5)));
        obj.insert("loop_len".into(), json!(loop_indices.len()));
        obj.insert("normal".into(), json!(n));
        obj.insert("center".into(), json!(c));
        obj.insert("score".into(), json!(score));
    }
    if score <= 0.0 {
        return (None, None, 0.0, info);
    }
    (Some(n), Some(c), score, info)
}

fn weld(verts: &[[f64; 3]], tris: &[[i64; 3]], tol: f64) -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
    if tol <= 0.0 {
        return (verts.to_vec(), tris.to_vec());
    }
    type Key = (i64, i64, i64);
    let keys: Vec<Key> = verts
        .iter()
        .map(|p| {
            (
                (p[0] / tol).round_ties_even() as i64,
                (p[1] / tol).round_ties_even() as i64,
                (p[2] / tol).round_ties_even() as i64,
            )
        })
        .collect();
    let mut first: BTreeMap<Key, usize> = BTreeMap::new();
    for (i, k) in keys.iter().enumerate() {
        first.entry(*k).or_insert(i);
    }
    let mut index: HashMap<Key, usize> = HashMap::with_capacity(first.len());
    let mut new_verts = Vec::with_capacity(first.len());
    for (idx, (k, &first_pos)) in first.iter().enumerate() {
        index.insert(*k, idx);
        new_verts.push(verts[first_pos]);
    }
    let new_tris = tris
        .iter()
        .map(|t| {
            [
                index[&keys[t[0] as usize]] as i64,
                index[&keys[t[1] as usize]] as i64,
                index[&keys[t[2] as usize]] as i64,
            ]
        })
        .collect();
    (new_verts, new_tris)
}

fn try_shell_wrap(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
    let ut: Vec<[usize; 3]> = tris
        .iter()
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    match outer_shell_union_core(verts, &ut, None) {
        Some((v, t)) if !t.is_empty() => (
            v,
            t.iter()
                .map(|r| [r[0] as i64, r[1] as i64, r[2] as i64])
                .collect(),
        ),
        _ => (verts.to_vec(), tris.to_vec()),
    }
}

fn repair_topology(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
    let chain = json!([
        ["meshing_repair_non_manifold_edges", {}],
        ["meshing_repair_non_manifold_vertices", {}],
        ["meshing_close_holes", { "maxholesize": 10000 }]
    ]);
    let (v, t, _applied, _skipped, _removed) = apply_chain(verts, tris, &chain);
    (v, t)
}

fn poisson_fallback(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    mut base: Value,
    reason: &str,
) -> MirrorOutput {
    let out = poisson_close(verts, tris, None);
    base["watertight"] = json!(out
        .report
        .get("watertight")
        .and_then(Value::as_bool)
        .unwrap_or(false));
    base["faces"] = json!(out.tris.len());
    base["mode"] = json!("poisson_fallback");
    base["error"] = out
        .report
        .get("error")
        .cloned()
        .unwrap_or(Value::Null);
    let mut notes = vec![json!(reason)];
    if let Some(Value::Array(extra)) = out.report.get("notes") {
        notes.extend(extra.iter().cloned());
    }
    base["notes"] = Value::Array(notes);
    MirrorOutput {
        verts: out.verts,
        tris: out.tris,
        report: base,
    }
}

pub fn mirror_close(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    min_score: f64,
) -> MirrorOutput {
    let mut base = json!({
        "watertight": false,
        "faces": tris.len(),
        "mode": "none",
        "mirror_score": 0.0,
        "notes": [],
        "error": null,
    });

    let (n, c, score, info) = detect_mirror_plane(verts, tris);
    base["mirror_score"] = json!(score);
    base["plane"] = info.clone();
    let (Some(n), Some(c)) = (n, c) else {
        return poisson_fallback(
            verts,
            tris,
            base,
            &format!(
                "low mirror confidence ({:.2} < {:.2})",
                score, min_score
            ),
        );
    };
    if score < min_score {
        return poisson_fallback(
            verts,
            tris,
            base,
            &format!(
                "low mirror confidence ({:.2} < {:.2})",
                score, min_score
            ),
        );
    }

    let diag = diag_of(verts);
    let (in_v, in_t) = referenced_only(verts, tris);

    let refl_v: Vec<[f64; 3]> = in_v
        .iter()
        .map(|p| {
            let t = 2.0 * dot(sub(*p, c), n);
            [p[0] - t * n[0], p[1] - t * n[1], p[2] - t * n[2]]
        })
        .collect();
    let offset = in_v.len() as i64;
    let refl_t: Vec<[i64; 3]> = in_t
        .iter()
        .map(|t| [t[2] + offset, t[1] + offset, t[0] + offset])
        .collect();

    let mut merged_v = in_v.clone();
    merged_v.extend_from_slice(&refl_v);
    let mut merged_t = in_t.clone();
    merged_t.extend_from_slice(&refl_t);
    let (mut mv, mut mt) = weld(&merged_v, &merged_t, MIRROR_WELD_REL * diag);

    let (mut ok, mut reason) = check_validity(&mv, &mt);
    if !ok {
        let (wrapped_v, wrapped_t) = try_shell_wrap(&mv, &mt);
        if wrapped_v != mv || wrapped_t != mt {
            mv = wrapped_v;
            mt = wrapped_t;
        }
        let (rv, rt) = repair_topology(&mv, &mt);
        mv = rv;
        mt = rt;
        let verdict = check_validity(&mv, &mt);
        ok = verdict.0;
        reason = verdict.1;
    }

    if !ok {
        return poisson_fallback(
            verts,
            tris,
            base,
            &format!("mirrored stitch not watertight: {}", reason),
        );
    }

    let faces = mt.len();
    MirrorOutput {
        verts: mv,
        tris: mt,
        report: json!({
            "watertight": true,
            "faces": faces,
            "mode": "mirror",
            "mirror_score": score,
            "plane": info,
            "notes": ["back surface completed by mirrored geometry"],
            "error": null,
        }),
    }
}
