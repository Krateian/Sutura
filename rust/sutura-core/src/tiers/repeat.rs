// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Repeated-element transplant (#11/#12) tier gate, ported from
//! `sutura_engine/pipeline/tiers/repeat.py`.
//!
//! Detection / alignment / CSG stay in `sutura_engine.methods.repeat`; this
//! module owns the watertight-input skip, the candidate gate, the one-sided
//! Hausdorff outside guard and the report.

use serde_json::{json, Map, Value};

use super::common;

fn rec_base(params: &Value) -> Map<String, Value> {
    let mut rec = Map::new();
    rec.insert(
        "mode".into(),
        params.get("mode").cloned().unwrap_or(Value::Null),
    );
    rec.insert("ran".into(), json!(false));
    rec.insert("adopted".into(), json!(false));
    rec.insert("watertight".into(), json!(false));
    rec.insert("pattern_type".into(), Value::Null);
    rec.insert("positions_repaired".into(), Value::Null);
    rec.insert("hausdorff_outside".into(), Value::Null);
    rec.insert("notes".into(), json!([]));
    rec.insert("reason".into(), Value::Null);
    rec
}

pub fn decide(
    cur_v: &[[f64; 3]],
    cur_t: &[[i64; 3]],
    cand_v: &[[f64; 3]],
    cand_t: &[[i64; 3]],
    rep: &Value,
    params: &Value,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let mut rec = rec_base(params);
    let holes = common::boundary_loop_count(cur_t);
    let nm = common::non_two_manifold_edges(cur_t);
    let two_manifold = common::is_two_manifold(cur_v.len(), cur_t);
    if holes != 0 || nm != 0 || !two_manifold {
        rec.insert(
            "reason".into(),
            json!(format!(
                "repeated-element transplant needs a watertight input; stage 1 left holes={} non-manifold={}",
                holes, nm
            )),
        );
        return (cur_v.to_vec(), cur_t.to_vec(), Value::Object(rec));
    }
    rec.insert("ran".into(), json!(true));
    for (k, name) in [
        ("notes", "notes"),
        ("pattern_type", "pattern_type"),
        ("positions_repaired", "positions_repaired"),
        ("hausdorff_outside", "hausdorff_outside"),
    ] {
        if let Some(v) = rep.get(name) {
            rec.insert(k.into(), v.clone());
        }
    }
    if let Some(err) = rep.get("error").and_then(Value::as_str) {
        rec.insert("reason".into(), json!(format!("module error: {}", err)));
    }
    let repaired = rep.get("repaired").and_then(Value::as_bool).unwrap_or(false);
    if !repaired || cand_t.is_empty() {
        if rec.get("reason").and_then(Value::as_str).is_none() {
            let notes = rep
                .get("notes")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(Value::as_str)
                .unwrap_or("nothing to repair");
            rec.insert("reason".into(), json!(notes));
        }
        return (cur_v.to_vec(), cur_t.to_vec(), Value::Object(rec));
    }
    let v32: Vec<[f32; 3]> = cand_v
        .iter()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    let t32: Vec<[i32; 3]> = cand_t
        .iter()
        .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
        .collect();
    let det = crate::report::detect_defects(&v32, &t32, false);
    let cand_holes = det.holes.len();
    let cand_nm = det.non_manifold.len();
    rec.insert("holes".into(), json!(cand_holes));
    rec.insert("non_manifold".into(), json!(cand_nm));
    let (rh, rnm) = common::reload_strict_holes_nm(cand_v, cand_t);
    rec.insert("reload_holes".into(), json!(rh));
    rec.insert("reload_non_manifold".into(), json!(rnm));
    let hd = rep.get("hausdorff_outside").and_then(Value::as_f64);
    let max_hd = params
        .get("max_hausdorff_outside")
        .and_then(Value::as_f64)
        .unwrap_or(0.02);
    if cand_holes == 0
        && cand_nm == 0
        && cand_holes + cand_nm <= holes + nm
        && hd.is_some()
        && hd.unwrap_or(f64::INFINITY) <= max_hd
    {
        rec.insert("adopted".into(), json!(true));
        rec.insert("watertight".into(), json!(false));
        rec.insert("candidate_watertight".into(), json!(true));
        rec.insert(
            "reason".into(),
            json!("repeated element(s) transplanted (stage 2 pending)"),
        );
        return (cand_v.to_vec(), cand_t.to_vec(), Value::Object(rec));
    }
    if rec.get("reason").and_then(Value::as_str).is_none() {
        if let Some(hd) = hd {
            if hd > max_hd {
                rec.insert(
                    "reason".into(),
                    json!(format!(
                        "untouched geometry moved (Hausdorff {:.3} > {:.3})",
                        hd, max_hd
                    )),
                );
            }
        }
        if rec.get("reason").and_then(Value::as_str).is_none() {
            rec.insert(
                "reason".into(),
                json!(format!(
                    "candidate not strict-watertight (holes={} non-manifold={})",
                    cand_holes, cand_nm
                )),
            );
        }
    }
    (cur_v.to_vec(), cur_t.to_vec(), Value::Object(rec))
}

/// Repair `cur_v`/`cur_t` through the shared kernel, with no Python involved.
///
/// This is the entry point the Rust CLI uses. `decide` above takes the repair
/// report as JSON because the pyo3 bridge still supplies it; here the kernel
/// produces it, so the whole Repeat path -- plan, Manifold transplant and the
/// Hausdorff guard -- is Rust.
///
/// Returns the repaired mesh and the report `decide` consumes, in the shape the
/// Python module emits (`repaired`, `pattern_type`, `positions_repaired`,
/// `hausdorff_outside`, `notes`, ...).
pub fn repair_auto_report(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    margin: f64,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let v: Vec<[f64; 3]> = verts.to_vec();
    let t: Vec<[usize; 3]> = tris
        .iter()
        .map(|f| [f[0] as usize, f[1] as usize, f[2] as usize])
        .collect();
    let (ov, ot, rep) = sutura_geom::repeat_repair::repair_repeat_auto_core(&v, &t, margin);
    let ot_i: Vec<[i64; 3]> = ot
        .iter()
        .map(|f| [f[0] as i64, f[1] as i64, f[2] as i64])
        .collect();
    let mut out = Map::new();
    out.insert("watertight".into(), json!(rep.watertight));
    out.insert("repaired".into(), json!(rep.repaired));
    out.insert("pattern_type".into(), json!(rep.pattern_type));
    out.insert("group_size".into(), json!(rep.group_size));
    out.insert("positions_checked".into(), json!(rep.positions_checked));
    out.insert("positions_repaired".into(), json!(rep.positions_repaired));
    out.insert("volume_change".into(), json!(rep.volume_change));
    out.insert("hausdorff_outside".into(), json!(rep.hausdorff_outside));
    out.insert("notes".into(), json!(rep.notes));
    if let Some(e) = rep.error {
        out.insert("error".into(), json!(e));
    }
    let devs: Vec<Value> = rep
        .per_position_deviation
        .iter()
        .map(|p| json!({"index": p.index, "deviation": p.deviation}))
        .collect();
    out.insert("per_position_deviation".into(), json!(devs));
    (ov, ot_i, Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repeat tier must reach the Manifold transplant with no pyo3 in the
    /// graph.
    ///
    /// A tetrahedron is not a neutral fixture: it legitimately carries 3-fold
    /// rotational symmetry, so the repair transplants one position and the
    /// output has 8 vertices. That is exactly what makes it a useful census --
    /// the whole plan + CSG + Hausdorff path runs, and the mesh changes.
    #[test]
    fn repair_auto_report_runs_without_python() {
        let v = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
        let t = vec![[0i64, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3]];
        let (ov, ot, rep) = repair_auto_report(&v, &t, 0.10);
        assert_eq!(
            rep.get("pattern_type").and_then(Value::as_str),
            Some("rotational")
        );
        assert_eq!(rep.get("positions_repaired").and_then(Value::as_u64), Some(1));
        // The transplant ran: a repaired mesh is not the input.
        assert_ne!(ov.len(), v.len());
        assert!(!ot.is_empty());
        assert!(rep.get("hausdorff_outside").is_some(), "{rep}");

        // Deterministic: same input, same report and same mesh.
        let (ov2, ot2, rep2) = repair_auto_report(&v, &t, 0.10);
        assert_eq!(rep, rep2);
        assert_eq!(ov, ov2);
        assert_eq!(ot, ot2);
    }
}
