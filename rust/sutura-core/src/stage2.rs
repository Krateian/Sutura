//! Stage 2 gate and pinch splitting, matching `pipeline/stage2.py`.
//! The Manifold rebuild itself is the existing Rust `stage2_rebuild` kernel.

use serde_json::{json, Value};
use sutura_geom::topology::repair_non_manifold_vertices_core;

use crate::p_weld::pinched_count;
use crate::report::detect_defects;

pub const PINCH_SPLIT_MAX_PASSES: usize = 5;

pub fn should_run_stage2(report: &Value) -> bool {
    report
        .get("stage1")
        .and_then(|s| s.get("two_manifold"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && report
            .get("stage1")
            .and_then(|s| s.get("holes_remaining"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            == 0
}

fn nm_edges(v: &[[f32; 3]], t: &[[i64; 3]]) -> usize {
    let t32: Vec<[i32; 3]> = t.iter().map(|x| [x[0] as i32, x[1] as i32, x[2] as i32]).collect();
    let d = detect_defects(v, &t32, false);
    // Any non-manifold region means at least one non-manifold edge.
    if d.non_manifold.is_empty() { 0 } else { 1 }
}

pub struct PinchResult {
    pub verts: Vec<[f32; 3]>,
    pub tris: Vec<[i32; 3]>,
    pub report: Option<Value>,
}

pub fn split_pinched_vertices(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> PinchResult {
    let v0: Vec<[f32; 3]> = verts.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
    let pinched = pinched_count(&v0, tris);
    let holes0 = {
        let t32: Vec<[i32; 3]> = tris.iter().map(|x| [x[0] as i32, x[1] as i32, x[2] as i32]).collect();
        detect_defects(&v0, &t32, false).holes.len()
    };
    let nm0 = nm_edges(&v0, tris);
    if pinched == 0 || (nm0 == 0 && pinched == 0) || nm0 != 0 || holes0 != 0 {
        return PinchResult {
            verts: v0,
            tris: tris.iter().map(|x| [x[0] as i32, x[1] as i32, x[2] as i32]).collect(),
            report: None,
        };
    }
    let mut v = v0.clone();
    let mut t = tris.to_vec();
    let mut after = pinched;
    let mut passes = 0usize;
    while passes < PINCH_SPLIT_MAX_PASSES {
        passes += 1;
        let (nv, nt) = repair_non_manifold_vertices_core(&v, &t);
        v = nv;
        t = nt;
        after = pinched_count(&v, &t);
        if after == 0 && nm_edges(&v, &t) == 0 {
            break;
        }
    }
    let holes = {
        let t32: Vec<[i32; 3]> = t.iter().map(|x| [x[0] as i32, x[1] as i32, x[2] as i32]).collect();
        detect_defects(&v, &t32, false).holes.len()
    };
    let nm = nm_edges(&v, &t);
    let adopted = holes == 0 && nm == 0 && after < pinched;
    let report = json!({
        "before": pinched, "after": after, "passes": passes, "adopted": adopted
    });
    if adopted {
        PinchResult { verts: v, tris: t.iter().map(|x| [x[0] as i32, x[1] as i32, x[2] as i32]).collect(), report: Some(report) }
    } else {
        PinchResult {
            verts: v0,
            tris: tris.iter().map(|x| [x[0] as i32, x[1] as i32, x[2] as i32]).collect(),
            report: Some(report),
        }
    }
}
