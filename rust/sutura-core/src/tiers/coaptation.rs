//! Coaptation (#19) tier orchestration, ported from
//! `sutura_engine/pipeline/tiers/coaptation.py`.
//!
//! Tolerance seam-weld + per-body `close_holes` + exact Manifold union.  The
//! seam weld is the only new geometry step; the ULP / lexsort / union-find
//! order is kept exactly so the output matches the Python tier byte for byte.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use serde_json::{json, Map, Value};
use sutura_geom::hole::close_holes_core;
use sutura_geom::manifold::{union_pair_core, watertight_check_core};

use super::common::{self, json_i, GuardPolicy};

fn rec_base(params: &Value) -> Map<String, Value> {
    let mut rec = Map::new();
    rec.insert("tier".into(), json!("coaptation"));
    rec.insert("ran".into(), json!(false));
    rec.insert("adopted".into(), json!(false));
    rec.insert("gate".into(), Value::Null);
    rec.insert("ulp_k".into(), json!(json_i(params, "ulp_k", 1)));
    rec.insert("tol".into(), Value::Null);
    rec.insert("bodies_in".into(), Value::Null);
    rec.insert("bodies_after_weld".into(), Value::Null);
    rec.insert("merged".into(), Value::Null);
    rec.insert("baseline".into(), Value::Null);
    rec.insert("candidate".into(), Value::Null);
    rec.insert("output_faces".into(), Value::Null);
    rec.insert("guard_rejected".into(), Value::Null);
    rec.insert("guard_reason".into(), Value::Null);
    rec.insert("si_policy".into(), Value::Null);
    rec.insert("si_checked".into(), json!(false));
    rec.insert("si_flagged".into(), json!(false));
    rec.insert("seconds".into(), Value::Null);
    rec.insert("skipped_budget".into(), json!(false));
    rec.insert("aborted".into(), json!(false));
    rec.insert("work_units".into(), Value::Null);
    rec.insert("work_budget".into(), json!(json_i(params, "work_limit", 0)));
    rec.insert("budget_limited_by".into(), Value::Null);
    rec.insert("reject_reason".into(), Value::Null);
    rec.insert("reason".into(), Value::Null);
    rec.insert("warnings".into(), json!([]));
    rec.insert(
        "input_faces".into(),
        json!(json_i(params, "input_faces", 0)),
    );
    rec
}

fn boundary_vertex_ids(tris: &[[i64; 3]]) -> Vec<i64> {
    let mut counts: HashMap<(i64, i64), usize> = HashMap::new();
    for t in tris {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let k = if a <= b { (a, b) } else { (b, a) };
            *counts.entry(k).or_insert(0) += 1;
        }
    }
    let mut out = HashSet::new();
    for ((a, b), c) in counts {
        if c == 1 {
            out.insert(a);
            out.insert(b);
        }
    }
    let mut v: Vec<i64> = out.into_iter().collect();
    v.sort_unstable();
    v
}

/// `_seam_weld`: merge only boundary-edge vertices within `tol` per axis.
fn seam_weld(
    v32: &[[f32; 3]],
    tris: &[[i64; 3]],
    tol: f64,
) -> (Vec<[f32; 3]>, Vec<[i64; 3]>, usize) {
    let nv = v32.len();
    let seam = boundary_vertex_ids(tris);
    if seam.is_empty() || tol <= 0.0 {
        return (v32.to_vec(), tris.to_vec(), 0);
    }
    let pts: Vec<[f64; 3]> = seam
        .iter()
        .map(|&i| {
            let p = v32[i as usize];
            [p[0] as f64, p[1] as f64, p[2] as f64]
        })
        .collect();
    let cell: Vec<[i64; 3]> = pts
        .iter()
        .map(|p| {
            [
                (p[0] / tol).floor() as i64,
                (p[1] / tol).floor() as i64,
                (p[2] / tol).floor() as i64,
            ]
        })
        .collect();
    let mut parent: Vec<usize> = (0..nv).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        let mut root = x;
        while p[root] != root {
            root = p[root];
        }
        while p[x] != root {
            let next = p[x];
            p[x] = root;
            x = next;
        }
        root
    }
    let mut order: Vec<usize> = (0..seam.len()).collect();
    order.sort_by(|&a, &b| {
        (cell[a][0], cell[a][1], cell[a][2]).cmp(&(cell[b][0], cell[b][1], cell[b][2]))
    });
    let mut buckets: HashMap<(i64, i64, i64), Vec<usize>> = HashMap::new();
    for i in order {
        let ci = cell[i];
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let key = (ci[0] + dx, ci[1] + dy, ci[2] + dz);
                    if let Some(list) = buckets.get(&key) {
                        for &j in list {
                            let d = [
                                (pts[i][0] - pts[j][0]).abs(),
                                (pts[i][1] - pts[j][1]).abs(),
                                (pts[i][2] - pts[j][2]).abs(),
                            ];
                            if d[0] <= tol && d[1] <= tol && d[2] <= tol {
                                let ra = find(&mut parent, seam[i] as usize);
                                let rb = find(&mut parent, seam[j] as usize);
                                if ra != rb {
                                    parent[ra.max(rb)] = ra.min(rb);
                                }
                            }
                        }
                    }
                }
            }
        }
        buckets.entry((ci[0], ci[1], ci[2])).or_default().push(i);
    }
    let roots: Vec<usize> = (0..nv).map(|i| find(&mut parent, i)).collect();
    let mut uniq = roots.clone();
    uniq.sort_unstable();
    uniq.dedup();
    let mut rank: HashMap<usize, i64> = HashMap::with_capacity(uniq.len());
    for (i, r) in uniq.iter().enumerate() {
        rank.insert(*r, i as i64);
    }
    let remap: Vec<i64> = roots.iter().map(|r| rank[r]).collect();
    let out_v: Vec<[f32; 3]> = uniq.iter().map(|&r| v32[r]).collect();
    let mut out_t: Vec<[i64; 3]> = Vec::with_capacity(tris.len());
    for t in tris {
        let a = remap[t[0] as usize];
        let b = remap[t[1] as usize];
        let c = remap[t[2] as usize];
        if a != b && b != c && a != c {
            out_t.push([a, b, c]);
        }
    }
    let mut seen = HashSet::new();
    for &i in &seam {
        seen.insert(remap[i as usize]);
    }
    let merged = seam.len().saturating_sub(seen.len());
    (out_v, out_t, merged)
}

fn unique_remap(tris: &[[i64; 3]]) -> (Vec<i64>, Vec<[i64; 3]>) {
    let mut used: Vec<i64> = tris.iter().flat_map(|t| t.iter().copied()).collect();
    used.sort_unstable();
    used.dedup();
    let mut rank: HashMap<i64, i64> = HashMap::with_capacity(used.len());
    for (i, v) in used.iter().enumerate() {
        rank.insert(*v, i as i64);
    }
    let out: Vec<[i64; 3]> = tris
        .iter()
        .map(|t| [rank[&t[0]], rank[&t[1]], rank[&t[2]]])
        .collect();
    (used, out)
}

fn measure(v: &[[f64; 3]], t: &[[i64; 3]]) -> (usize, usize, usize, Option<i64>, bool) {
    let (holes, nm) = common::reload_strict_holes_nm(v, t);
    let (_n, labels) = common::edge_face_labels(t);
    let parts = labels
        .iter()
        .copied()
        .max()
        .map(|x| x as usize + 1)
        .unwrap_or(0);
    let si = common::exact_si_count(v, t);
    let t32: Vec<[i32; 3]> = t
        .iter()
        .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
        .collect();
    let wt = watertight_check_core(v, &t32);
    (holes, nm, parts, si, wt)
}

fn policy_from(params: &Value) -> GuardPolicy {
    common::policy_from_json(params.get("guard").unwrap_or(&Value::Null))
}

fn opt_i_str(v: Option<i64>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "None".into())
}

pub fn run(
    v: &[[f64; 3]],
    t: &[[i64; 3]],
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    params: &Value,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let t0 = Instant::now();
    let mut rec = rec_base(params);
    let cap = json_i(params, "cap", 0);
    if cap > 0 && input_t.len() > cap as usize {
        rec.insert("skipped_budget".into(), json!(true));
        rec.insert("reject_reason".into(), json!("too_large"));
        rec.insert(
            "reason".into(),
            json!(format!(
                "input has {} faces, above the Coaptation cap of {}; the ladder result was kept",
                input_t.len(),
                cap
            )),
        );
        rec.insert("seconds".into(), json!(t0.elapsed().as_secs_f64()));
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }
    let (wv32, wt) = match common::weld_reload_equivalent(input_v, input_t) {
        Ok(x) => x,
        Err(e) => {
            rec.insert("reject_reason".into(), json!("unavailable"));
            rec.insert("reason".into(), json!(format!("weld failed: {}", e)));
            rec.insert("seconds".into(), json!(t0.elapsed().as_secs_f64()));
            return (v.to_vec(), t.to_vec(), Value::Object(rec));
        }
    };
    let (bodies_in_n, _lab_in) = common::edge_face_labels(&wt);
    rec.insert("bodies_in".into(), json!(bodies_in_n));
    if bodies_in_n < 2 {
        rec.insert("gate".into(), json!("single_body"));
        rec.insert("reject_reason".into(), json!("single_body"));
        rec.insert(
            "reason".into(),
            json!("a single connected body; nothing to coapt"),
        );
        rec.insert("seconds".into(), json!(t0.elapsed().as_secs_f64()));
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }
    let max_abs = wv32
        .iter()
        .flat_map(|p| [p[0].abs(), p[1].abs(), p[2].abs()])
        .fold(0.0f32, f32::max);
    let ulp_k = json_i(params, "ulp_k", 1);
    let tol = if max_abs > 0.0 {
        ulp_k as f64 * crate::p_weld::spacing_f32(max_abs) as f64
    } else {
        0.0
    };
    rec.insert("tol".into(), json!(tol));
    let (sw_v, sw_t, merged) = seam_weld(&wv32, &wt, tol);
    rec.insert("merged".into(), json!(merged));
    if merged == 0 {
        rec.insert("gate".into(), json!("no_seam"));
        rec.insert("reject_reason".into(), json!("no_seam"));
        rec.insert(
            "reason".into(),
            json!(format!(
                "no boundary vertices coapt within {} ULP; the ladder result was kept",
                ulp_k
            )),
        );
        rec.insert("seconds".into(), json!(t0.elapsed().as_secs_f64()));
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }
    let (n_bodies, lab) = common::edge_face_labels(&sw_t);
    let mut bodies: Vec<(Vec<[f64; 3]>, Vec<[i64; 3]>)> = Vec::new();
    for b in 0..n_bodies {
        let fb: Vec<[i64; 3]> = sw_t
            .iter()
            .zip(lab.iter())
            .filter(|(_, l)| **l == b as i64)
            .map(|(t, _)| *t)
            .collect();
        if fb.is_empty() {
            continue;
        }
        let (u, bt) = unique_remap(&fb);
        let bv: Vec<[f64; 3]> = u
            .iter()
            .map(|&i| {
                let p = sw_v[i as usize];
                [p[0] as f64, p[1] as f64, p[2] as f64]
            })
            .collect();
        let max_hole = 1000.max(bt.len() as i64);
        let rep = match close_holes_core(&bv, &bt, max_hole, true) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let mut bt2 = rep.tris;
        if common::signed_volume(&rep.verts, &bt2) < 0.0 {
            for face in &mut bt2 {
                face.swap(0, 2);
            }
        }
        bodies.push((rep.verts, bt2));
    }
    rec.insert("bodies_after_weld".into(), json!(bodies.len()));
    let work_units: usize = bodies.iter().map(|(_, bt)| bt.len()).sum();
    rec.insert("work_units".into(), json!(work_units));
    let work_limit = json_i(params, "work_limit", 0);
    if work_limit > 0 && work_units > work_limit as usize {
        rec.insert("aborted".into(), json!(true));
        rec.insert("budget_limited_by".into(), json!("work"));
        rec.insert("reject_reason".into(), json!("work"));
        rec.insert(
            "reason".into(),
            json!(format!(
                "Coaptation used {} work units > {}; the ladder result was kept",
                work_units, work_limit
            )),
        );
        rec.insert("seconds".into(), json!(t0.elapsed().as_secs_f64()));
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }
    if bodies.len() < 2 {
        rec.insert("gate".into(), json!("single_body_after_weld"));
        rec.insert("reject_reason".into(), json!("single_body_after_weld"));
        rec.insert(
            "reason".into(),
            json!("the seam weld leaves a single body; the ladder result was kept"),
        );
        rec.insert("seconds".into(), json!(t0.elapsed().as_secs_f64()));
        return (v.to_vec(), t.to_vec(), Value::Object(rec));
    }
    let mut acc_v = bodies[0].0.clone();
    let mut acc_t = bodies[0].1.clone();
    for (bv, bt) in bodies.iter().skip(1) {
        let at: Vec<[usize; 3]> = acc_t
            .iter()
            .map(|x| [x[0] as usize, x[1] as usize, x[2] as usize])
            .collect();
        let bt_u: Vec<[usize; 3]> = bt
            .iter()
            .map(|x| [x[0] as usize, x[1] as usize, x[2] as usize])
            .collect();
        match union_pair_core(&acc_v, &at, bv, &bt_u) {
            Ok((ov, ot)) => {
                acc_v = ov;
                acc_t = ot
                    .iter()
                    .map(|x| [x[0] as i64, x[1] as i64, x[2] as i64])
                    .collect();
            }
            Err(e) => {
                rec.insert("reject_reason".into(), json!("union_failed"));
                rec.insert("reason".into(), json!(format!("union failed: {}", e)));
                rec.insert("seconds".into(), json!(t0.elapsed().as_secs_f64()));
                return (v.to_vec(), t.to_vec(), Value::Object(rec));
            }
        }
    }
    rec.insert("ran".into(), json!(true));
    rec.insert("output_faces".into(), json!(acc_t.len()));
    rec.insert(
        "seconds".into(),
        json!((t0.elapsed().as_secs_f64() * 1000.0).round() / 1000.0),
    );

    let base = measure(v, t);
    let cand = measure(&acc_v, &acc_t);
    rec.insert(
        "baseline".into(),
        json!({
            "holes": base.0,
            "non_manifold": base.1,
            "parts": base.2,
            "self_intersections": base.3,
        }),
    );
    rec.insert(
        "candidate".into(),
        json!({
            "holes": cand.0,
            "non_manifold": cand.1,
            "parts": cand.2,
            "self_intersections": cand.3,
            "watertight": cand.4,
            "faces": acc_t.len(),
        }),
    );
    let policy = policy_from(params);
    let guard = common::guard_fields(
        input_t.len(),
        input_v.len(),
        acc_t.len(),
        acc_v.len(),
        0,
        None,
        None,
        &policy,
        false,
    );
    let guard_ok = guard
        .get("guard_ok")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    for (k, val) in guard {
        if rec.contains_key(&k) && k != "guard_ok" {
            rec.insert(k, val);
        }
    }
    let strictly_better = guard_ok
        && cand.4
        && cand.0 == 0
        && cand.1 == 0
        && cand.3 == Some(0)
        && base.3.is_some()
        && cand.3 < base.3
        && cand.0 <= base.0
        && cand.1 <= base.1
        && cand.2 <= base.2;
    if strictly_better {
        rec.insert("adopted".into(), json!(true));
        rec.insert("watertight".into(), json!(false));
        rec.insert("candidate_watertight".into(), json!(true));
        rec.insert(
            "reason".into(),
            json!(format!(
                "strict-watertight union candidate (self-intersections {} -> 0, parts {})",
                opt_i_str(base.3),
                cand.2
            )),
        );
        return (acc_v, acc_t, Value::Object(rec));
    }
    rec.insert("reject_reason".into(), json!("not_strictly_better"));
    rec.insert(
        "reason".into(),
        json!(format!(
            "union result is not strictly better (candidate SI={} wt={} holes={} nm={} parts={} vs baseline SI={} holes={} nm={} parts={}); the ladder result was kept",
            opt_i_str(cand.3), cand.4, cand.0, cand.1, cand.2,
            opt_i_str(base.3), base.0, base.1, base.2
        )),
    );
    (v.to_vec(), t.to_vec(), Value::Object(rec))
}

/// Exposed for the Python wrapper's `_measure` parity on the Rust path.
pub fn measure_json(v: &[[f64; 3]], t: &[[i64; 3]]) -> Value {
    let m = measure(v, t);
    json!({
        "holes": m.0,
        "non_manifold": m.1,
        "parts": m.2,
        "self_intersections": m.3,
        "watertight": m.4,
    })
}

#[cfg(test)]
mod tests {
    use super::seam_weld;

    #[test]
    fn seam_weld_merges_one_ulp_boundary_vertices() {
        let a = 1.0f32;
        let a_next = f32::from_bits(a.to_bits() + 1);
        let b = 1.0f32;
        let b_next = f32::from_bits(b.to_bits() + 1);
        // Two triangles sharing the A-B edge with a one-ULP duplicate copy.
        let v = [
            [0.0, 0.0, 0.0],    // C
            [1.0, 0.0, 0.0],    // A
            [0.0, 1.0, 0.0],    // B
            [a_next, 0.0, 0.0], // A'
            [1.0, 1.0, 0.0],    // D
            [0.0, b_next, 0.0], // B'
        ];
        let t = [[0, 1, 2], [3, 4, 5]];
        let tol = crate::p_weld::spacing_f32(1.0) as f64;
        let (ov, ot, merged) = seam_weld(&v, &t, tol);
        assert_eq!(merged, 2, "A/A' and B/B' must merge");
        assert_eq!(ov.len(), 4);
        assert_eq!(ot.len(), 2);
        let (_, _, none) = seam_weld(&v, &t, 0.0);
        assert_eq!(none, 0);
    }
}
