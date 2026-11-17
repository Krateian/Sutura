// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Wall-thicken (P-WALL, method #15) tier: producer plus adoption gate.
//!
//! Split across two crates, the same way dressing is:
//!
//! * `sutura_geom::wall` owns the geometry — the sampled SDF, the thickness
//!   estimate, the dilation and the marching-tetrahedra extraction. All of it is
//!   byte-identical to `sutura_engine/wall.py` (verified on `broken_37402`:
//!   verts `dd40320a…`, faces `d2687e32…`).
//! * this module owns the tier: run the producer on the **original input**,
//!   clean the candidate with the Stage-1 chain, then hand it to
//!   [`crate::tiers::wall::decide`] for the strict-watertight adoption gate.
//!
//! The gate already existed and is tested; this wiring is what was missing.
//! `tiers/wall.rs` said so in its own header ("the builder stays in Python") and
//! `repair.rs` recorded the tier as an error — so `--wall-min-thickness` was
//! refused and method 15 could not thicken anything.

use serde_json::{json, Value};

use crate::stage1::chain::apply_chain;
use crate::tiers::wall as gate;

/// `wall.DEFAULT_GRID_RESOLUTION`, as used by the tier.
pub const DEFAULT_RESOLUTION: usize = sutura_geom::wall::DEFAULT_GRID_RESOLUTION;
/// `wall.DEFAULT_SAMPLES`.
pub const DEFAULT_SAMPLES: usize = sutura_geom::sample::DEFAULT_SAMPLES;

/// Summary the tier records. Mirrors the Python dict in
/// `pipeline/tiers/wall.py::_wall_thicken_tier_python` so the report block
/// serialises to the same shape.
pub struct Outcome {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[i64; 3]>,
    pub record: Value,
    pub adopted: bool,
}

fn rec_base() -> serde_json::Map<String, Value> {
    let mut rec = serde_json::Map::new();
    rec.insert("tier".into(), json!("wall_thicken"));
    rec.insert("ran".into(), json!(true));
    rec.insert("adopted".into(), json!(false));
    rec.insert("watertight".into(), json!(false));
    rec.insert("min_before".into(), Value::Null);
    rec.insert("min_after".into(), Value::Null);
    rec.insert("min_thickness".into(), Value::Null);
    rec.insert("delta".into(), Value::Null);
    rec.insert("thin_before".into(), Value::Null);
    rec.insert("faces".into(), Value::Null);
    rec.insert("notes".into(), json!([]));
    rec.insert("reason".into(), Value::Null);
    rec
}

/// Python's `_clean_and_orient` for a generated surface: dedup, unreferenced
/// removal and coherent re-orientation, through the same Stage-1 chain the rest
/// of the pipeline uses.
///
/// Chain entries are `[filter_name, params]` arrays (`stage1/chain.rs:180-186`).
fn clean_and_orient(v: &[[f64; 3]], t: &[[i64; 3]]) -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
    let chain = json!([
        ["meshing_remove_duplicate_vertices", {}],
        ["meshing_remove_duplicate_faces", {}],
        ["meshing_remove_unreferenced_vertices", {}],
        ["meshing_repair_non_manifold_edges", {}],
        ["meshing_re_orient_faces_coherently", {}],
    ]);
    let (nv, nt, _applied, _skipped, _err) = apply_chain(v, t, &chain);
    // Python swallows every failure here and returns the input unchanged; a
    // chain that produced nothing is the same case.
    if nt.is_empty() {
        return (v.to_vec(), t.to_vec());
    }
    (nv, nt)
}

/// Run the wall-thicken tier.
///
/// `cur_v`/`cur_t` is the mesh the ladder has produced so far, and it is what
/// the producer measures. That matches Python, whose `pipeline_repair` passes
/// `v`/`t` — names the pipeline rebinds whenever an earlier tier is adopted
/// (`pipeline.py:504-506` for Flap), not the pristine input. Measuring the
/// original instead gave `thin_before` 2507 where Python reported 2537.
pub fn run(
    cur_v: &[[f64; 3]],
    cur_t: &[[i64; 3]],
    min_thickness: Option<f64>,
    resolution: usize,
    samples: usize,
    seed: u64,
    base_holes: usize,
    base_nm: usize,
) -> Outcome {
    let mut rec = rec_base();

    let in_v: Vec<[f64; 3]> = cur_v.to_vec();
    let in_t: Vec<[usize; 3]> = cur_t
        .iter()
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    let (cand_v, cand_t, rep) = sutura_geom::wall::thicken_to_min(
        &in_v,
        &in_t,
        min_thickness,
        resolution,
        samples,
        seed,
    );

    rec.insert("min_thickness".into(), json!(rep.min_thickness));
    rec.insert("min_before".into(), json!(rep.min_before));
    rec.insert("thin_before".into(), json!(rep.thin_before));
    rec.insert("delta".into(), json!(rep.delta));
    rec.insert("notes".into(), json!(rep.notes));
    if let Some(err) = &rep.error {
        rec.insert(
            "reason".into(),
            json!(format!("module error: {err}")),
        );
    }

    if cand_t.is_empty() {
        return Outcome {
            verts: cur_v.to_vec(),
            tris: cur_t.to_vec(),
            record: Value::Object(rec),
            adopted: false,
        };
    }

    // Clean the candidate, then measure it — Python reports the *cleaned*
    // candidate's holes and non-manifold count, not the raw extractor output.
    let cand_i64: Vec<[i64; 3]> = cand_t
        .iter()
        .map(|t| [t[0] as i64, t[1] as i64, t[2] as i64])
        .collect();
    let (cv, ct) = clean_and_orient(&cand_v, &cand_i64);
    let cv32: Vec<[f32; 3]> = cv
        .iter()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    let ct32: Vec<[i32; 3]> = ct
        .iter()
        .map(|t| [t[0] as i32, t[1] as i32, t[2] as i32])
        .collect();
    let defects = crate::report::defects::detect_defects(&cv32, &ct32, false);
    let cand_holes = defects.holes.len();
    let cand_nm = defects.non_manifold.len();

    rec.insert("holes".into(), json!(cand_holes));
    rec.insert("non_manifold".into(), json!(cand_nm));
    rec.insert("faces".into(), json!(ct.len()));
    rec.insert("min_after".into(), json!(rep.min_after));
    rec.insert("thin_after".into(), json!(rep.thin_after));

    // The adoption rule is Python's, unchanged: strict-watertight and no worse
    // than the stage-1 baseline on holes + non-manifold edges.
    let adoptable =
        cand_holes == 0 && cand_nm == 0 && cand_holes + cand_nm <= base_holes + base_nm;

    if !adoptable {
        rec.insert(
            "reason".into(),
            json!("candidate not adopted: not watertight or worse than the baseline"),
        );
        return Outcome {
            verts: cur_v.to_vec(),
            tris: cur_t.to_vec(),
            record: Value::Object(rec),
            adopted: false,
        };
    }

    // Feed the gate the *raw* candidate plus the report, exactly as the Python
    // tier does (`tier_wall_decide(..., json.dumps(rep), json.dumps({}))`).
    let (gv, gt, gate_rec) = gate::decide(
        cur_v,
        cur_t,
        // The gate's "input" pair is the same current mesh: Python calls
        // `tier_wall_decide(cur_v, cur_t, v, t, ...)` where `v`/`t` are the
        // ladder mesh too, not the pristine input.
        cur_v,
        cur_t,
        &cv,
        &ct,
        &Value::Object(rec.clone()),
        &Value::Object(serde_json::Map::new()),
    );
    let adopted = gate_rec
        .get("adopted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Outcome {
        adopted,
        verts: if adopted { gv } else { cur_v.to_vec() },
        tris: if adopted { gt } else { cur_t.to_vec() },
        record: gate_rec,
    }
}
