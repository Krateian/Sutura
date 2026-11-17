//! Guarded SI excise + re-cap (the `si_excise_recap` port).
//!
//! The loop is array-only and uses the same pure kernels as the Python
//! implementation: exact SI mask, one/two-ring face dilation, selected-face
//! removal, topology repair, hole re-cap and the lazy visible-shape guard
//! (`local` default, `full`, `parity`).

use std::collections::HashMap;
use std::time::Instant;

use serde_json::{json, Map, Value};
use sutura_geom::hole::close_holes_core;
use sutura_geom::shell::{buried_face_mask, BuriedMaskContext};
use sutura_geom::topology::{
    remove_selected_faces_core, remove_unreferenced_vertices_core,
    repair_non_manifold_edges_core, repair_non_manifold_vertices_core,
};
use sutura_geom::winding::MeshBvh;

use crate::tiers::common;

const MAX_ROUNDS: usize = 3;
const MAX_FACE_FRACTION: f64 = 0.30;
const DEFAULT_BUDGET: f64 = 5.0;
const SHAPE_BUDGET_MM: f64 = 0.1;
const MOVE_EPS_MM: f64 = 1e-9;

fn py_round(x: f64, nd: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{:.*}", nd, x).parse::<f64>().unwrap_or(x)
}

fn as_f32(v: &[[f64; 3]]) -> Vec<[f32; 3]> {
    v.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect()
}

fn f32_to_f64(v: &[[f32; 3]]) -> Vec<[f64; 3]> {
    v.iter().map(|p| [p[0] as f64, p[1] as f64, p[2] as f64]).collect()
}

fn u32_tris(t: &[[i64; 3]]) -> Vec<[u32; 3]> {
    t.iter().map(|r| [r[0] as u32, r[1] as u32, r[2] as u32]).collect()
}

fn centroids(v: &[[f64; 3]], t: &[[i64; 3]]) -> Vec<[f64; 3]> {
    t.iter()
        .map(|r| {
            let a = v[r[0] as usize];
            let b = v[r[1] as usize];
            let c = v[r[2] as usize];
            [
                (a[0] + b[0] + c[0]) / 3.0,
                (a[1] + b[1] + c[1]) / 3.0,
                (a[2] + b[2] + c[2]) / 3.0,
            ]
        })
        .collect()
}

fn distances(cand_v: &[[f64; 3]], cand_t: &[[i64; 3]], pts: &[[f64; 3]]) -> Option<Vec<f64>> {
    if cand_v.is_empty() || cand_t.is_empty() || pts.is_empty() {
        return None;
    }
    let ut: Vec<[usize; 3]> = cand_t
        .iter()
        .map(|r| [r[0] as usize, r[1] as usize, r[2] as usize])
        .collect();
    let bvh = MeshBvh::from_arrays(cand_v, &ut);
    // Parallel closest-point queries (same values/order as the scalar loop).
    Some(
        bvh.closest_points_par(pts)
            .into_iter()
            .map(|(_, d, _)| d)
            .collect(),
    )
}

fn visible_from_distance(d: &[f64], buried: Option<&[bool]>) -> Option<(f64, f64)> {
    if d.is_empty() {
        return None;
    }
    let mut all = 0.0f64;
    let mut vis = 0.0f64;
    let mut any_vis = false;
    for (i, &x) in d.iter().enumerate() {
        if x > all {
            all = x;
        }
        let is_buried = buried.map(|b| b.get(i).copied().unwrap_or(false)).unwrap_or(false);
        if !is_buried {
            any_vis = true;
            if x > vis {
                vis = x;
            }
        }
    }
    Some((if any_vis { vis } else { 0.0 }, all))
}

fn local_buried(ref_v: &[[f64; 3]], ref_t: &[[i64; 3]], d: &[f64], ctx: Option<&BuriedMaskContext>) -> Option<Vec<bool>> {
    let moved: Vec<usize> = d
        .iter()
        .enumerate()
        .filter(|(_, x)| **x > MOVE_EPS_MM)
        .map(|(i, _)| i)
        .collect();
    if moved.is_empty() {
        return Some(vec![false; d.len()]);
    }
    if let Some(ctx) = ctx {
        let ids: Vec<u32> = moved.iter().map(|&i| i as u32).collect();
        return Some(ctx.classify(&ids).mask);
    }
    let ids: Vec<u32> = moved.iter().map(|&i| i as u32).collect();
    Some(sutura_geom::shell::buried_face_mask_subset(ref_v, &u32_tris(ref_t), &ids, 0.1).mask)
}

fn new_face_max_mm(ref_v: &[[f64; 3]], ref_t: &[[i64; 3]], cand_v: &[[f64; 3]], cand_t: &[[i64; 3]], mask: &[bool]) -> Option<f64> {
    if mask.is_empty() {
        return None;
    }
    let mut pts = Vec::new();
    for (i, &on) in mask.iter().enumerate() {
        if !on {
            continue;
        }
        let r = cand_t[i];
        let a = cand_v[r[0] as usize];
        let b = cand_v[r[1] as usize];
        let c = cand_v[r[2] as usize];
        pts.push([(a[0] + b[0] + c[0]) / 3.0, (a[1] + b[1] + c[1]) / 3.0, (a[2] + b[2] + c[2]) / 3.0]);
    }
    let d = distances(ref_v, ref_t, &pts)?;
    Some(d.iter().fold(0.0f64, |m, x| m.max(*x)))
}

struct Guard {
    on: bool,
    mode: String,
    ref_v: Vec<[f64; 3]>,
    ref_t: Vec<[i64; 3]>,
    cent: Vec<[f64; 3]>,
    ctx: Option<BuriedMaskContext>,
    buried: Option<Vec<bool>>,
    built: bool,
    initial_vis: Option<f64>,
    last_vis: Option<f64>,
    report: Value,
}

impl Guard {
    fn new(on: bool, mode: &str, ref_v: &[[f64; 3]], ref_t: &[[i64; 3]]) -> Self {
        let mut sg = Map::new();
        sg.insert("enabled".into(), json!(on));
        sg.insert("available".into(), json!(true));
        sg.insert("built".into(), json!(false));
        sg.insert("build_skipped".into(), Value::Null);
        sg.insert("guard_build_seconds".into(), json!(0.0));
        sg.insert("mode".into(), json!(mode));
        sg.insert("local_fallback_full".into(), json!(false));
        sg.insert("parity_ok".into(), json!(true));
        sg.insert("parity_checks".into(), json!(0));
        sg.insert("parity_moved_max".into(), json!(0));
        sg.insert("parity_diffs".into(), json!([]));
        sg.insert("budget_mm".into(), json!(SHAPE_BUDGET_MM));
        sg.insert("initial_vis_mm".into(), Value::Null);
        sg.insert("final_vis_mm".into(), Value::Null);
        sg.insert("rejected_round".into(), Value::Null);
        sg.insert("rounds_rejected".into(), json!(0));
        sg.insert("si_remaining_after".into(), Value::Null);
        sg.insert("reason".into(), Value::Null);
        Self {
            on,
            mode: mode.to_string(),
            ref_v: ref_v.to_vec(),
            ref_t: ref_t.to_vec(),
            cent: centroids(ref_v, ref_t),
            ctx: None,
            buried: None,
            built: false,
            initial_vis: None,
            last_vis: None,
            report: Value::Object(sg),
        }
    }

    fn ensure(&mut self, v0: &[[f64; 3]], t0: &[[i64; 3]]) {
        if self.built || !self.on {
            return;
        }
        let t0u = u32_tris(&self.ref_t);
        if self.mode == "local" || self.mode == "parity" {
            self.ctx = Some(BuriedMaskContext::new(&self.ref_v, &t0u, 0.1));
        }
        if self.mode == "full" || self.mode == "parity" {
            self.buried = Some(buried_face_mask(&self.ref_v, &t0u, 0.1).mask);
        }
        if let Some(d0) = distances(v0, t0, &self.cent) {
            let full = visible_from_distance(&d0, self.buried.as_deref());
            let local = if self.mode == "local" || self.mode == "parity" {
                local_buried(&self.ref_v, &self.ref_t, &d0, self.ctx.as_ref())
                    .and_then(|b| visible_from_distance(&d0, Some(&b)))
            } else {
                None
            };
            let m = if self.mode == "local" { local } else { full };
            if self.mode == "parity" {
                self.record_parity("initial", full, local, 0);
            }
            if let Some((vis, _all)) = m {
                self.initial_vis = Some(vis);
                self.last_vis = Some(vis);
                if let Value::Object(ref mut sg) = self.report {
                    sg.insert("initial_vis_mm".into(), json!(py_round(vis, 4)));
                }
            }
        }
        self.built = true;
        if let Value::Object(ref mut sg) = self.report {
            sg.insert("built".into(), json!(true));
        }
    }

    fn record_parity(
        &mut self,
        stage: &str,
        full: Option<(f64, f64)>,
        local: Option<(f64, f64)>,
        moved: usize,
    ) {
        if let Value::Object(ref mut sg) = self.report {
            let checks = sg.get("parity_checks").and_then(Value::as_i64).unwrap_or(0) + 1;
            sg.insert("parity_checks".into(), json!(checks));
            let mm = sg.get("parity_moved_max").and_then(Value::as_i64).unwrap_or(0);
            sg.insert("parity_moved_max".into(), json!(mm.max(moved as i64)));
            let same = match (full, local) {
                (Some(f), Some(l)) => {
                    py_round(f.0, 4) == py_round(l.0, 4)
                        && py_round(f.1, 4) == py_round(l.1, 4)
                }
                (None, None) => true,
                _ => false,
            };
            if !same {
                sg.insert("parity_ok".into(), json!(false));
                if let Some(arr) = sg.get_mut("parity_diffs").and_then(Value::as_array_mut) {
                    if arr.len() < 20 {
                        arr.push(json!({
                            "stage": stage,
                            "full": full.map(|x| [py_round(x.0, 9), py_round(x.1, 9)]),
                            "local": local.map(|x| [py_round(x.0, 9), py_round(x.1, 9)]),
                        }));
                    }
                }
            }
        }
    }

    fn eval(&mut self, cand_v: &[[f64; 3]], cand_t: &[[i64; 3]]) -> Option<(f64, f64, usize, bool)> {
        let d = distances(cand_v, cand_t, &self.cent)?;
        let moved = d.iter().filter(|x| **x > MOVE_EPS_MM).count();
        if self.mode == "local" {
            let lb = local_buried(&self.ref_v, &self.ref_t, &d, self.ctx.as_ref())?;
            let m = visible_from_distance(&d, Some(&lb))?;
            return Some((m.0, m.1, moved, false));
        }
        let full = visible_from_distance(&d, self.buried.as_deref())?;
        if self.mode == "parity" {
            let lb = local_buried(&self.ref_v, &self.ref_t, &d, self.ctx.as_ref());
            let local = lb.as_deref().and_then(|b| visible_from_distance(&d, Some(b)));
            if local.is_none() {
                if let Value::Object(ref mut sg) = self.report {
                    sg.insert("parity_ok".into(), json!(false));
                    if let Some(arr) = sg.get_mut("parity_diffs").and_then(Value::as_array_mut) {
                        if arr.len() < 20 {
                            arr.push(json!({"stage": "candidate", "local_unavailable": true}));
                        }
                    }
                }
            } else {
                self.record_parity("candidate", Some(full), local, moved);
            }
            return Some((full.0, full.1, moved, true));
        }
        Some((full.0, full.1, 0, false))
    }
}

/// Run the guarded SI excise + re-cap. Returns `(verts, tris, report)`.
pub fn run(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    ref_verts: &[[f64; 3]],
    ref_tris: &[[i64; 3]],
    params: &Value,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let t0 = Instant::now();
    let mut v = verts.to_vec();
    let mut t = tris.to_vec();
    let maxholesize = params.get("maxholesize").and_then(Value::as_i64).unwrap_or(1000);
    let max_rounds = params.get("max_rounds").and_then(Value::as_i64).unwrap_or(MAX_ROUNDS as i64).max(0) as usize;
    let time_budget = params.get("time_budget").and_then(Value::as_f64);
    let face_fraction = params.get("face_fraction").and_then(Value::as_f64).unwrap_or(MAX_FACE_FRACTION);
    let guard_on = params.get("shape_guard").and_then(Value::as_bool).unwrap_or(true);
    let guard_mode = params.get("guard_mode").and_then(Value::as_str).unwrap_or("local");
    let mut report = Map::new();
    report.insert("ran".into(), json!(false));
    report.insert("applied".into(), json!(false));
    report.insert("rounds".into(), json!(0));
    report.insert("initial_si".into(), json!(0));
    report.insert("si_after".into(), json!(0));
    report.insert("removed_si".into(), json!(0));
    report.insert("reason".into(), Value::Null);
    report.insert("dilation_rings".into(), json!(1));
    report.insert("budget_s".into(), time_budget.map(Value::from).unwrap_or(Value::Null));
    report.insert("seconds".into(), json!(0.0));
    report.insert("history".into(), json!([]));

    let ut: Vec<[usize; 3]> = t.iter().map(|r| [r[0] as usize, r[1] as usize, r[2] as usize]).collect();
    let (init_mask, si, _skipped) = sutura_geom::self_intersections_core(&v, &ut);
    let initial_si = si.max(0);
    report.insert("initial_si".into(), json!(initial_si));
    if si < 0 {
        report.insert("reason".into(), json!("error: self-intersection check failed"));
        report.insert("seconds".into(), json!(t0.elapsed().as_secs_f64()));
        return (v, t, Value::Object(report));
    }
    if initial_si == 0 {
        report.insert("reason".into(), json!("no_si"));
        report.insert("seconds".into(), json!(t0.elapsed().as_secs_f64()));
        return (v, t, Value::Object(report));
    }
    if let Some(b) = time_budget {
        if b <= 0.0 {
            report.insert("reason".into(), json!("budget"));
            report.insert("seconds".into(), json!(t0.elapsed().as_secs_f64()));
            return (v, t, Value::Object(report));
        }
    }
    let pass_v0 = v.clone();
    let pass_t0 = t.clone();
    let mut guard = if guard_on {
        Some(Guard::new(true, guard_mode, ref_verts, ref_tris))
    } else {
        None
    };
    report.insert("ran".into(), json!(true));
    let mut current_si = initial_si;
    let total_faces = t.len();
    let mut accepted_any = false;

    for round_idx in 0..max_rounds {
        let before = common::topology_summary(&v, &t);
        let saved_v = v.clone();
        let saved_t = t.clone();
        let sel_cur: Vec<bool> = if round_idx == 0 && !init_mask.is_empty() {
            init_mask.clone()
        } else {
            let ut: Vec<[usize; 3]> = t.iter().map(|r| [r[0] as usize, r[1] as usize, r[2] as usize]).collect();
            let (m, _s, _k) = sutura_geom::self_intersections_core(&v, &ut);
            m
        };
        let mut accepted = false;
        for attempt in 0..2usize {
            let rings = attempt + 1;
            if attempt > 0 {
                v = saved_v.clone();
                t = saved_t.clone();
            }
            report.insert("dilation_rings".into(), json!(rings));
            let sel_dil = common::dilate_face_selection(&saved_t, &sel_cur, rings);
            let selected = sel_dil.iter().filter(|x| **x).count();
            if selected == 0 {
                report.insert("reason".into(), json!("no_selection"));
                break;
            }
            if selected as f64 > face_fraction * total_faces.max(1) as f64 {
                report.insert("reason".into(), json!("scope"));
                break;
            }
            let mut t_work = remove_selected_faces_core(&saved_t, &sel_dil);
            let sv32 = as_f32(&saved_v);
            let (mut v_work, nt) = remove_unreferenced_vertices_core(&sv32, &t_work);
            t_work = nt;
            let vw32 = as_f32(&f32_to_f64(&v_work));
            t_work = repair_non_manifold_edges_core(&vw32, &t_work);
            let (v_work2, t_work2) = repair_non_manifold_vertices_core(&vw32, &t_work);
            v_work = v_work2;
            t_work = t_work2;
            let vw64 = f32_to_f64(&v_work);
            let rep = match close_holes_core(&vw64, &t_work, maxholesize, true) {
                Ok(r) => r,
                Err(_) => {
                    v = saved_v.clone();
                    t = saved_t.clone();
                    report.insert("reason".into(), json!("rollback"));
                    break;
                }
            };
            let mut new_face_mask = vec![false; rep.tris.len()];
            if rep.new_faces > 0 {
                let start = rep.tris.len().saturating_sub(rep.new_faces);
                for m in &mut new_face_mask[start..] {
                    *m = true;
                }
            }
            let vw32b = as_f32(&rep.verts);
            let (v_final, t_final) = remove_unreferenced_vertices_core(&vw32b, &rep.tris);
            let v_final64 = f32_to_f64(&v_final);
            let after = common::topology_summary(&v_final64, &t_final);
            let ut2: Vec<[usize; 3]> = t_final.iter().map(|r| [r[0] as usize, r[1] as usize, r[2] as usize]).collect();
            let (_m2, si2, _k2) = sutura_geom::self_intersections_core(&v_final64, &ut2);
            let si_after = if si2 >= 0 { Some(si2) } else { None };
            let nm_before = before.non_two_manifold_edges + before.non_two_manifold_vertices;
            let nm_after = after.non_two_manifold_edges + after.non_two_manifold_vertices;
            let holes_ok = after.boundary_edges == 0;
            let comps_ok = after.components <= before.components;
            let si_ok = si_after.map(|s| s < current_si).unwrap_or(false);
            let topo_ok = si_ok && holes_ok && nm_after <= nm_before && comps_ok;

            let mut guard_ok = true;
            let mut post_vis = None;
            let mut post_all = None;
            let mut new_face_mm = None;
            if topo_ok && guard_on {
                if let Some(g) = guard.as_mut() {
                    g.ensure(&pass_v0, &pass_t0);
                    if let Some((vis, all, _moved, _parity)) = g.eval(&v_final64, &t_final) {
                        post_vis = Some(vis);
                        post_all = Some(all);
                        new_face_mm = new_face_max_mm(ref_verts, ref_tris, &v_final64, &t_final, &new_face_mask);
                        let per_round_ok = g.last_vis.map(|lv| vis <= lv + SHAPE_BUDGET_MM).unwrap_or(true);
                        let cumulative_ok = g.initial_vis.map(|iv| vis <= iv + SHAPE_BUDGET_MM).unwrap_or(true);
                        let new_face_ok = new_face_mm.map(|x| x <= SHAPE_BUDGET_MM).unwrap_or(true);
                        guard_ok = per_round_ok && cumulative_ok && new_face_ok;
                    }
                }
            }
            if topo_ok && guard_ok {
                accepted = true;
                v = v_final64;
                t = t_final;
                let mut entry = Map::new();
                entry.insert("round".into(), json!(round_idx + 1));
                entry.insert("si_before".into(), json!(current_si));
                entry.insert("si_after".into(), json!(si_after));
                entry.insert("faces_after".into(), json!(after.faces));
                entry.insert("dilation_rings".into(), json!(rings));
                if let Some(pv) = post_vis {
                    entry.insert(
                        "pre_vis_mm".into(),
                        guard
                            .as_ref()
                            .and_then(|g| g.last_vis)
                            .map(|x| json!(py_round(x, 4)))
                            .unwrap_or(Value::Null),
                    );
                    entry.insert("post_vis_mm".into(), json!(py_round(pv, 4)));
                    entry.insert(
                        "post_all_mm".into(),
                        post_all
                            .map(|x| json!(py_round(x, 4)))
                            .unwrap_or(Value::Null),
                    );
                    entry.insert(
                        "new_face_mm".into(),
                        new_face_mm
                            .map(|x| json!(py_round(x, 4)))
                            .unwrap_or(Value::Null),
                    );
                }
                if let Some(arr) = report.get_mut("history").and_then(Value::as_array_mut) {
                    arr.push(Value::Object(entry));
                }
                if let Some(pv) = post_vis {
                    if let Some(g) = guard.as_mut() {
                        g.last_vis = Some(pv);
                    }
                }
                current_si = si_after.unwrap();
                report.insert("rounds".into(), json!(report.get("rounds").and_then(Value::as_i64).unwrap_or(0) + 1));
                accepted_any = true;
                if current_si == 0 {
                    report.insert("reason".into(), json!("clean"));
                } else if round_idx + 1 >= max_rounds {
                    report.insert("reason".into(), json!("max_rounds"));
                }
                break;
            }
            if !guard_ok {
                v = saved_v.clone();
                t = saved_t.clone();
                let pre_vis = guard.as_ref().and_then(|g| g.last_vis);
                if let Some(g) = guard.as_mut() {
                    if let Value::Object(ref mut sg) = g.report {
                        let rr = sg.get("rounds_rejected").and_then(Value::as_i64).unwrap_or(0) + 1;
                        sg.insert("rounds_rejected".into(), json!(rr));
                        sg.insert("rejected_round".into(), json!(round_idx + 1));
                        sg.insert("pre_vis_mm".into(), pre_vis.map(|x| json!(py_round(x, 4))).unwrap_or(Value::Null));
                        sg.insert("post_vis_mm".into(), post_vis.map(|x| json!(py_round(x, 4))).unwrap_or(Value::Null));
                        sg.insert("post_all_mm".into(), post_all.map(|x| json!(py_round(x, 4))).unwrap_or(Value::Null));
                        sg.insert("new_face_mm".into(), new_face_mm.map(|x| json!(py_round(x, 4))).unwrap_or(Value::Null));
                        sg.insert("reason".into(), json!("shape_guard"));
                    }
                }
                if let Some(arr) = report.get_mut("history").and_then(Value::as_array_mut) {
                    arr.push(json!({
                        "round": round_idx + 1,
                        "si_before": current_si,
                        "si_after": si_after,
                        "faces_after": after.faces,
                        "dilation_rings": rings,
                        "reason": "shape_guard",
                        "pre_vis_mm": pre_vis.map(|x| json!(py_round(x, 4))).unwrap_or(Value::Null),
                        "post_vis_mm": post_vis.map(|x| json!(py_round(x, 4))).unwrap_or(Value::Null),
                        "new_face_mm": new_face_mm.map(|x| json!(py_round(x, 4))).unwrap_or(Value::Null),
                    }));
                }
                report.insert("reason".into(), json!("shape_guard"));
                break;
            }
            if rings == 1 && after.boundary_edges > 0 && after.boundary_edges < before.boundary_edges && si_ok && nm_after <= nm_before {
                continue;
            }
            v = saved_v.clone();
            t = saved_t.clone();
            break;
        }
        if !accepted {
            if !matches!(report.get("reason").and_then(Value::as_str), Some("no_selection") | Some("scope") | Some("shape_guard")) {
                report.insert("reason".into(), json!("rollback"));
            }
            break;
        }
        if matches!(report.get("reason").and_then(Value::as_str), Some("clean") | Some("max_rounds")) {
            break;
        }
    }
    report.insert("si_after".into(), json!(current_si));
    report.insert("removed_si".into(), json!(initial_si - current_si));
    report.insert("applied".into(), json!(accepted_any));
    report.insert("seconds".into(), json!(py_round(t0.elapsed().as_secs_f64(), 3)));
    if let Some(g) = guard.as_ref() {
        let mut sg = g.report.as_object().cloned().unwrap_or_default();
        sg.insert("final_vis_mm".into(), g.last_vis.map(|x| json!(py_round(x, 4))).unwrap_or(Value::Null));
        sg.insert("si_remaining_after".into(), json!(current_si));
        if !g.built && sg.get("reason").and_then(Value::as_str).is_none() {
            sg.insert("build_skipped".into(), json!("no_candidate"));
        }
        report.insert("shape_guard".into(), Value::Object(sg));
    }
    (v, t, Value::Object(report))
}
