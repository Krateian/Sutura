//! Shape-fidelity measurement, matching `sutura/fidelity.py`.
//!
//! Reporting only: this module never changes a repair decision.

use std::collections::HashMap;

use serde_json::{json, Map, Value};
use sutura_geom::pointcloud;
use sutura_geom::shell;
use sutura_geom::winding::MeshBvh;

use super::score::compute_scores_value;

const SHARP_ANGLE_DEG: f64 = 30.0;
const DIHEDRAL_MATCH_DEG: f64 = 15.0;
const DETAIL_TOL_MM: f64 = 0.1;
const DETAIL_TOL_FRACTION: f64 = 0.002;

fn g<'a>(cfg: &'a Value, key: &str) -> Option<&'a Value> {
    cfg.get("fidelity").and_then(|f| f.get(key))
}
fn num(cfg: &Value, key: &str, d: f64) -> f64 {
    g(cfg, key).and_then(Value::as_f64).unwrap_or(d)
}
fn budget(cfg: &Value, key: &str, d: i64) -> i64 {
    g(cfg, "budgets")
        .and_then(|b| b.get(key))
        .and_then(Value::as_i64)
        .unwrap_or(d)
}

fn bbox_diag(v: &[[f64; 3]]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut lo = v[0];
    let mut hi = v[0];
    for p in v {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let d = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    dot(d, d).sqrt()
}

fn detail_tol(v: &[[f64; 3]]) -> f64 {
    let diag = bbox_diag(v);
    if diag <= 0.0 {
        return DETAIL_TOL_MM;
    }
    (DETAIL_TOL_MM.min(DETAIL_TOL_FRACTION * diag)).max(1e-9)
}

fn face_areas(v: &[[f64; 3]], t: &[[i32; 3]]) -> Vec<f64> {
    t.iter()
        .map(|x| {
            let a = v[x[0] as usize];
            let b = v[x[1] as usize];
            let c = v[x[2] as usize];
            0.5 * norm(cross(sub(b, a), sub(c, a)))
        })
        .collect()
}

fn face_normals(v: &[[f64; 3]], t: &[[i32; 3]]) -> (Vec<[f64; 3]>, Vec<bool>) {
    let mut ns = Vec::with_capacity(t.len());
    let mut ok = Vec::with_capacity(t.len());
    for x in t {
        let a = v[x[0] as usize];
        let b = v[x[1] as usize];
        let c = v[x[2] as usize];
        let cr = cross(sub(b, a), sub(c, a));
        let ln = norm(cr);
        if ln > 0.0 {
            ns.push([cr[0] / ln, cr[1] / ln, cr[2] / ln]);
            ok.push(true);
        } else {
            ns.push([0.0, 0.0, 0.0]);
            ok.push(false);
        }
    }
    (ns, ok)
}

fn edge_map(t: &[[i32; 3]]) -> HashMap<(i64, i64), Vec<usize>> {
    let mut m: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (fi, x) in t.iter().enumerate() {
        for (a, b) in [(x[0], x[1]), (x[1], x[2]), (x[2], x[0])] {
            let k = if a <= b {
                (a as i64, b as i64)
            } else {
                (b as i64, a as i64)
            };
            m.entry(k).or_default().push(fi);
        }
    }
    m
}

fn defect_mask(t: &[[i32; 3]]) -> Vec<bool> {
    let mut mask = vec![false; t.len()];
    if t.is_empty() {
        return mask;
    }
    let m = edge_map(t);
    for faces in m.values() {
        if faces.len() != 2 {
            for &f in faces {
                mask[f] = true;
            }
        }
    }
    if mask.iter().any(|&x| x) {
        let maxv = t.iter().flat_map(|x| *x).max().unwrap_or(-1).max(0) as usize;
        let mut bad_v = vec![false; maxv + 1];
        for (fi, x) in t.iter().enumerate() {
            if mask[fi] {
                for &i in x {
                    bad_v[i as usize] = true;
                }
            }
        }
        for (fi, x) in t.iter().enumerate() {
            if x.iter().any(|&i| bad_v[i as usize]) {
                mask[fi] = true;
            }
        }
    }
    mask
}

fn buried_face_mask(
    vi: &[[f64; 3]],
    ti: &[[i32; 3]],
    defect: &[bool],
    delta: f64,
) -> (Vec<bool>, Value) {
    let mut info = json!({
        "available": false, "faces": 0, "area_mm2": 0.0,
        "debris_faces": 0, "degenerate_faces": 0, "note": Value::Null
    });
    let rt: Vec<[u32; 3]> = ti
        .iter()
        .map(|x| [x[0] as u32, x[1] as u32, x[2] as u32])
        .collect();
    let res = shell::buried_face_mask(vi, &rt, delta);
    let mut mask = res.mask;
    for (m, d) in mask.iter_mut().zip(defect) {
        if *d {
            *m = false;
        }
    }
    info["available"] = Value::Bool(true);
    info["debris_faces"] = Value::from(res.debris_count);
    info["degenerate_faces"] = Value::from(res.degenerate_count);
    info["faces"] = Value::from(mask.iter().filter(|&&x| x).count());
    if info["faces"].as_u64().unwrap_or(0) > 0 {
        let areas = face_areas(vi, ti);
        let area: f64 = mask
            .iter()
            .zip(&areas)
            .filter(|(m, _)| **m)
            .map(|(_, a)| *a)
            .sum();
        info["area_mm2"] = Value::from((area * 1e6).round() / 1e6);
    }
    (mask, info)
}

fn sharp_edges(
    v: &[[f64; 3]],
    t: &[[i32; 3]],
    face_mask: Option<&[bool]>,
) -> (Vec<((i64, i64), usize, usize)>, Vec<f64>) {
    if t.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let (normals, valid) = face_normals(v, t);
    let m = edge_map(t);
    let mut keys: Vec<(i64, i64)> = m.keys().copied().collect();
    keys.sort_unstable();
    let mut edges = Vec::new();
    let mut dih = Vec::new();
    for k in keys {
        let faces = &m[&k];
        if faces.len() != 2 {
            continue;
        }
        let (f0, f1) = (faces[0], faces[1]);
        if !valid[f0] || !valid[f1] {
            continue;
        }
        if let Some(fm) = face_mask {
            if fm[f0] || fm[f1] {
                continue;
            }
        }
        let dot = dot(normals[f0], normals[f1]).clamp(-1.0, 1.0);
        let d = dot.acos().to_degrees();
        if d > SHARP_ANGLE_DEG {
            edges.push((k, f0, f1));
            dih.push(d);
        }
    }
    (edges, dih)
}

fn sample_sharp(
    v: &[[f64; 3]],
    edges: &[((i64, i64), usize, usize)],
    dih: &[f64],
    step: f64,
    max_per_edge: i64,
) -> (Vec<[f64; 3]>, Vec<f64>, Vec<f64>) {
    if edges.is_empty() {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let mut pts = Vec::new();
    let mut ds = Vec::new();
    let mut ws = Vec::new();
    for (i, (&(k, _, _), &d)) in edges.iter().zip(dih).enumerate() {
        let a = v[k.0 as usize];
        let b = v[k.1 as usize];
        let len = norm(sub(b, a));
        if len <= 0.0 {
            continue;
        }
        let n = ((len / step.max(1e-12) + 0.5).floor() as i64)
            .max(1)
            .min(max_per_edge);
        for j in 0..n {
            let frac = (j as f64 + 0.5) / n as f64;
            pts.push(add(scale(a, 1.0 - frac), scale(b, frac)));
            ds.push(d);
            ws.push(len / n as f64);
        }
        let _ = i;
    }
    (pts, ds, ws)
}

fn masked_sharp_length(v: &[[f64; 3]], t: &[[i32; 3]], mask: &[bool]) -> f64 {
    let (edges, _) = sharp_edges(v, t, Some(mask));
    edges
        .iter()
        .map(|(k, _, _)| norm(sub(v[k.1 as usize], v[k.0 as usize])))
        .sum()
}

fn area_samples(
    v: &[[f64; 3]],
    t: &[[i32; 3]],
    n_total: i64,
    face_mask: Option<&[bool]>,
) -> (Vec<[f64; 3]>, Vec<[f64; 3]>, Vec<f64>, Vec<usize>) {
    let n = n_total.max(0) as usize;
    if n == 0 || t.is_empty() {
        return (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    }
    let mut keep: Vec<usize> = (0..t.len()).collect();
    let mut tt = t.to_vec();
    if let Some(mask) = face_mask {
        keep = (0..t.len()).filter(|&i| !mask[i]).collect();
        if keep.is_empty() {
            return (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        }
        tt = keep.iter().map(|&i| t[i]).collect();
    }
    let areas = face_areas(v, &tt);
    let total: f64 = areas.iter().sum();
    if total <= 0.0 {
        return (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    }
    let (normals, _) = face_normals(v, &tt);
    let mut cdf = Vec::with_capacity(areas.len());
    let mut acc = 0.0;
    for a in &areas {
        acc += *a;
        cdf.push(acc);
    }
    let mut pts = Vec::with_capacity(n);
    let mut ns = Vec::with_capacity(n);
    let mut ws = Vec::with_capacity(n);
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let u_area = (i as f64 + 0.5) / n as f64 * total;
        let idx = cdf.partition_point(|&x| x <= u_area).min(tt.len() - 1);
        let mut u = ((i as f64 + 0.5) * 0.7548776662466927).rem_euclid(1.0);
        let mut w = ((i as f64 + 0.5) * 0.5698402909980532).rem_euclid(1.0);
        if u + w > 1.0 {
            u = 1.0 - u;
            w = 1.0 - w;
        }
        let a = v[tt[idx][0] as usize];
        let b = v[tt[idx][1] as usize];
        let c = v[tt[idx][2] as usize];
        pts.push(add(add(scale(a, 1.0 - u - w), scale(b, u)), scale(c, w)));
        ns.push(normals[idx]);
        ws.push(total / n as f64);
        ids.push(if face_mask.is_some() { keep[idx] } else { idx });
    }
    (pts, ns, ws, ids)
}

fn signed_volume(v: &[[f64; 3]], t: &[[i32; 3]]) -> f64 {
    let mut sum = 0.0;
    for x in t {
        let a = v[x[0] as usize];
        let b = v[x[1] as usize];
        let c = v[x[2] as usize];
        sum += dot(a, cross(b, c)) / 6.0;
    }
    sum.abs()
}

fn healthy_fill(
    vi: &[[f64; 3]],
    ti: &[[i32; 3]],
    vo: &[[f64; 3]],
    to: &[[i32; 3]],
    mask: &[bool],
    tol: f64,
    area_n: i64,
    layers: i64,
) -> Option<Value> {
    let (base, normals, weight, _) = area_samples(vi, ti, area_n, Some(mask));
    if base.is_empty() {
        return None;
    }
    let diag = bbox_diag(vi);
    let band = (8.0 * tol).max(0.01 * diag);
    let mut layers = layers;
    if layers % 2 != 0 {
        layers += 1;
    }
    let layers = layers.max(2) as usize;
    let delta = 2.0 * band / layers as f64;
    let mut pts = Vec::with_capacity(base.len() * layers);
    let mut vol_w = Vec::with_capacity(base.len() * layers);
    for i in 0..base.len() {
        for l in 0..layers {
            let off = (l as f64 + 0.5 - layers as f64 / 2.0) * delta;
            pts.push(add(base[i], scale(normals[i], off)));
            vol_w.push(weight[i] * delta);
        }
    }
    let rt_i: Vec<[usize; 3]> = ti
        .iter()
        .map(|x| [x[0] as usize, x[1] as usize, x[2] as usize])
        .collect();
    let rt_o: Vec<[usize; 3]> = to
        .iter()
        .map(|x| [x[0] as usize, x[1] as usize, x[2] as usize])
        .collect();
    let bvh_in = MeshBvh::from_arrays(vi, &rt_i);
    let bvh_out = MeshBvh::from_arrays(vo, &rt_o);
    let wi = bvh_in.winding_points_par(&pts);
    let wo = bvh_out.winding_points_par(&pts);
    let mut fill = 0.0;
    let mut rem = 0.0;
    for i in 0..pts.len() {
        if wo[i] > 0.5 && wi[i] < 0.5 {
            fill += vol_w[i];
        }
        if wi[i] > 0.5 && wo[i] < 0.5 {
            rem += vol_w[i];
        }
    }
    let mut basis = signed_volume(vo, to);
    if !(basis > 0.0) {
        basis = signed_volume(vi, ti);
    }
    if !(basis > 0.0) {
        let mut lo = vi[0];
        let mut hi = vi[0];
        for p in vi {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        basis =
            (hi[0] - lo[0]).max(1e-12) * (hi[1] - lo[1]).max(1e-12) * (hi[2] - lo[2]).max(1e-12);
    }
    let mut out = json!({
        "fill_mm3": (fill * 1e6).round() / 1e6,
        "removed_mm3": (rem * 1e6).round() / 1e6,
        "fill_fraction": Value::Null,
        "removed_fraction": Value::Null,
        "samples": pts.len(),
    });
    if basis > 0.0 {
        out["fill_fraction"] = Value::from(fill / basis);
        out["removed_fraction"] = Value::from(rem / basis);
    }
    Some(out)
}

fn feature_recall(
    vi: &[[f64; 3]],
    ti: &[[i32; 3]],
    vo: &[[f64; 3]],
    to: &[[i32; 3]],
    mask: &[bool],
    tol: f64,
    sharp_per_edge: i64,
) -> (Option<f64>, Value) {
    let (in_e, in_d) = sharp_edges(vi, ti, Some(mask));
    if in_e.is_empty() {
        return (None, json!({"note": "no healthy sharp input edges"}));
    }
    let (out_e, out_d) = sharp_edges(vo, to, None);
    let (q, qd, qw) = sample_sharp(vi, &in_e, &in_d, tol, sharp_per_edge);
    if q.is_empty() {
        return (None, json!({"note": "no sharp-edge samples"}));
    }
    let (r, rd, _) = sample_sharp(vo, &out_e, &out_d, tol, sharp_per_edge);
    if r.is_empty() {
        return (
            Some(0.0),
            json!({"note": "no sharp output edges",
                                  "input_sharp_edges": in_e.len()}),
        );
    }
    let (dist, idx, _take) = pointcloud::nearest_points(&r, &q, 1);
    let mut delta = vec![f64::INFINITY; q.len()];
    for i in 0..q.len() {
        let j = idx[i];
        if j >= 0 && dist[i] <= tol {
            delta[i] = (qd[i] - rd[j as usize]).abs();
        }
    }
    let mut keep_num = 0.0;
    let total: f64 = qw.iter().sum();
    for i in 0..q.len() {
        if idx[i] >= 0 && dist[i] <= tol && delta[i] <= DIHEDRAL_MATCH_DEG {
            keep_num += qw[i];
        }
    }
    let recall = if total > 0.0 {
        Some(keep_num / total)
    } else {
        None
    };
    let detail = json!({
        "input_sharp_edges": in_e.len(),
        "output_sharp_edges": out_e.len(),
        "samples": q.len(),
        "tol_mm": (tol * 1e6).round() / 1e6,
        "recall": recall.map(|x| (x * 1e6).round() / 1e6),
    });
    (recall, detail)
}

fn union_areas(t: &[[i32; 3]], flagged: &[bool], area: &[f64], min_area: f64) -> Option<usize> {
    let n = t.len();
    let m = edge_map(t);
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    for faces in m.values() {
        if faces.len() == 2 && flagged[faces[0]] && flagged[faces[1]] {
            let a = find(&mut parent, faces[0]);
            let b = find(&mut parent, faces[1]);
            if a != b {
                parent[b] = a;
            }
        }
    }
    let mut comp: HashMap<usize, f64> = HashMap::new();
    for i in 0..n {
        if flagged[i] {
            let r = find(&mut parent, i);
            *comp.entry(r).or_insert(0.0) += area[i];
        }
    }
    let total: f64 = area.iter().sum();
    if total <= 0.0 {
        return None;
    }
    Some(comp.values().filter(|&&a| a >= min_area * total).count())
}

fn bbox_change_pct(vi: &[[f64; 3]], vo: &[[f64; 3]]) -> Option<f64> {
    if vi.is_empty() || vo.is_empty() {
        return None;
    }
    let ext = |v: &[[f64; 3]]| {
        let mut lo = v[0];
        let mut hi = v[0];
        for p in v {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]]
    };
    let da = norm(ext(vi));
    if da <= 0.0 {
        return None;
    }
    Some((norm(ext(vo)) - da).abs() / da * 100.0)
}

fn health_tier(health: i64, cfg: &Value) -> bool {
    let mid = cfg
        .get("status")
        .and_then(|s| s.get("health_tiers"))
        .and_then(|t| t.get("mid"))
        .and_then(Value::as_f64)
        .unwrap_or(60.0);
    health as f64 >= mid
}

#[allow(clippy::too_many_arguments)]
fn verdict(
    score: Option<i64>,
    report: &Value,
    low_confidence: bool,
    areas: Option<usize>,
    cfg: &Value,
    max_pct: Option<f64>,
) -> (String, usize) {
    let mut low_areas = areas.unwrap_or(0);
    let health = compute_scores_value(report, cfg)
        .get("health")
        .and_then(Value::as_i64);
    if let Some(h) = health {
        if !health_tier(h, cfg) {
            return ("not_repaired".into(), low_areas);
        }
    }
    if low_confidence {
        return ("low_confidence".into(), low_areas);
    }
    let Some(score) = score else {
        return ("unavailable".into(), 0);
    };
    let mut code = if areas.unwrap_or(0) > 0 {
        "changed"
    } else {
        let high = g(cfg, "tiers")
            .and_then(|t| t.get("high"))
            .and_then(Value::as_f64)
            .unwrap_or(85.0);
        if score as f64 >= high {
            "preserved"
        } else {
            "changed"
        }
    };
    let gate = num(cfg, "changed_min_healthy_distance_pct", 1.0);
    if code == "changed" {
        if let Some(m) = max_pct {
            if m <= gate {
                code = "preserved";
            }
        }
    }
    if code == "unavailable" {
        low_areas = 0;
    }
    (code.into(), low_areas)
}

pub fn compute_fidelity_value(
    vi: &[[f64; 3]],
    ti: &[[i32; 3]],
    vo: &[[f64; 3]],
    to: &[[i32; 3]],
    report: &Value,
    cfg: &Value,
) -> Value {
    let mut empty = json!({
        "repair_fidelity": Value::Null,
        "repair_fidelity_factors": {},
        "repair_fidelity_note": "not measured",
        "repair_verdict_code": "unavailable",
        "repair_verdict_areas": 0,
        "repair_verdict_low_confidence": false,
        "repair_verdict_reasons": [],
        "interior_walls_note": Value::Null,
    });
    if vi.is_empty() || ti.is_empty() || vo.is_empty() || to.is_empty() {
        empty["repair_fidelity_note"] = Value::from("empty mesh");
        return empty;
    }
    let tol = detail_tol(vi);
    let diag = bbox_diag(vi);
    let full_faces = budget(cfg, "full_faces", 20000);
    let max_faces = budget(cfg, "max_faces", 150000);
    let reduced = to.len() as i64 > full_faces;
    let gated = to.len() as i64 > max_faces || ti.len() as i64 > max_faces;
    let mut note: Vec<String> = Vec::new();
    if gated {
        note.push("large mesh: reduced budget".into());
    } else if reduced {
        note.push("reduced budget".into());
    }
    let recall_max = budget(cfg, "recall_max_faces", 50000);
    let mask_available = !gated;
    let mask = if mask_available {
        defect_mask(ti)
    } else {
        vec![false; ti.len()]
    };
    if !mask_available {
        note.push("healthy mask skipped (large mesh)".into());
    }
    let (buried_mask, buried_info) = if mask_available {
        buried_face_mask(vi, ti, &mask, num(cfg, "interior_wall_delta", 0.1))
    } else {
        (
            vec![false; ti.len()],
            json!({"available": false, "faces": 0, "area_mm2": 0.0,
            "debris_faces": 0, "degenerate_faces": 0,
            "note": "interior walls unavailable (large mesh)"}),
        )
    };

    let (pts, w, _face_idx, area) = if !reduced {
        let a = face_areas(vo, to);
        let p: Vec<[f64; 3]> = to
            .iter()
            .map(|x| {
                let (a0, b0, c0) = (vo[x[0] as usize], vo[x[1] as usize], vo[x[2] as usize]);
                [
                    (a0[0] + b0[0] + c0[0]) / 3.0,
                    (a0[1] + b0[1] + c0[1]) / 3.0,
                    (a0[2] + b0[2] + c0[2]) / 3.0,
                ]
            })
            .collect();
        (p, a.clone(), (0..to.len()).collect::<Vec<_>>(), a)
    } else {
        let (p, _n, ww, ids) =
            area_samples(vo, to, budget(cfg, "reduced_distance_samples", 1200), None);
        (p, ww.clone(), ids, ww)
    };
    if pts.is_empty() {
        empty["repair_fidelity_note"] = Value::from("no samples");
        return empty;
    }
    let rt_i: Vec<[usize; 3]> = ti
        .iter()
        .map(|x| [x[0] as usize, x[1] as usize, x[2] as usize])
        .collect();
    let bvh = MeshBvh::from_arrays(vi, &rt_i);
    let mut d = Vec::with_capacity(pts.len());
    let mut nf = Vec::with_capacity(pts.len());
    for p in &pts {
        let (_, dd, f) = bvh.closest_point(*p);
        d.push(dd);
        nf.push(if f == usize::MAX { -1 } else { f as i64 });
    }
    let mut healthy = vec![false; d.len()];
    let mut sample_ok = vec![false; d.len()];
    if mask_available {
        if !mask.is_empty() {
            for i in 0..d.len() {
                let f = nf[i];
                if f >= 0 && (f as usize) < mask.len() {
                    sample_ok[i] = true;
                    healthy[i] = !mask[f as usize];
                }
            }
        } else {
            sample_ok.iter_mut().for_each(|x| *x = true);
            healthy.iter_mut().for_each(|x| *x = true);
        }
    }
    let mut max_pct = None;
    let mut mean_pct = None;
    let mut sum_w = 0.0;
    let mut sum_dw = 0.0;
    let mut max_d: f64 = 0.0;
    for i in 0..d.len() {
        if healthy[i] {
            sum_w += w[i];
            sum_dw += w[i] * d[i];
            max_d = max_d.max(d[i]);
        }
    }
    if sum_w > 0.0 && diag > 0.0 {
        max_pct = Some(max_d / diag * 100.0);
        mean_pct = Some((sum_dw / sum_w) / diag * 100.0);
    }
    let inv_tol = tol.max(num(cfg, "invented_tol_pct", 1.0) / 100.0 * diag);
    let mut inv_den = 0.0;
    let mut inv_num = 0.0;
    for i in 0..d.len() {
        if sample_ok[i] {
            inv_den += w[i];
            if d[i] > inv_tol {
                inv_num += w[i];
            }
        }
    }
    let invented_frac = if inv_den > 0.0 {
        inv_num / inv_den
    } else {
        0.0
    };

    let mut areas_n = None;
    let mut low_areas = 0usize;
    if !reduced {
        let flagged: Vec<bool> = d
            .iter()
            .map(|&x| x > tol * num(cfg, "change_tol_factor", 3.0))
            .collect();
        let min_frac = cfg
            .get("fidelity")
            .and_then(|f| f.get("low_confidence"))
            .and_then(|l| l.get("area_min_fraction"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0005);
        areas_n = union_areas(to, &flagged, &area, min_frac);
        let max_areas = cfg
            .get("fidelity")
            .and_then(|f| f.get("low_confidence"))
            .and_then(|l| l.get("max_areas"))
            .and_then(Value::as_i64)
            .unwrap_or(99) as usize;
        if let Some(n) = areas_n {
            areas_n = Some(n.min(max_areas));
        }
        let flagged2: Vec<bool> = flagged
            .iter()
            .zip(&d)
            .map(|(f, &dd)| *f && dd > tol)
            .collect();
        let low_frac = cfg
            .get("fidelity")
            .and_then(|f| f.get("low_confidence"))
            .and_then(|l| l.get("low_area_min_fraction"))
            .and_then(Value::as_f64)
            .unwrap_or(0.03);
        low_areas = union_areas(to, &flagged2, &area, low_frac)
            .unwrap_or(0)
            .min(max_areas);
    }

    let mut recall = None;
    let mut recall_all = None;
    let mut rec_detail = json!({"note": "feature recall skipped (large mesh)"});
    let mut buried_length = 0.0;
    let mut buried_fraction = 0.0;
    if !(gated || ti.len().max(to.len()) as i64 > recall_max) {
        let mut per_edge = budget(cfg, "sharp_samples_per_edge", 4);
        if reduced {
            per_edge = budget(cfg, "reduced_sharp_samples_per_edge", 2);
        }
        let (ra, da) = feature_recall(vi, ti, vo, to, &mask, tol, per_edge);
        recall_all = ra;
        rec_detail = da.clone();
        if buried_mask.iter().any(|&x| x) {
            let mut combined = mask.clone();
            for (a, b) in combined.iter_mut().zip(&buried_mask) {
                *a |= *b;
            }
            let (r, rd) = feature_recall(vi, ti, vo, to, &combined, tol, per_edge);
            recall = r;
            rec_detail = rd;
        } else {
            recall = recall_all;
        }
        let all_len = masked_sharp_length(vi, ti, &mask);
        let vis_len = if buried_mask.iter().any(|&x| x) {
            let mut combined = mask.clone();
            for (a, b) in combined.iter_mut().zip(&buried_mask) {
                *a |= *b;
            }
            masked_sharp_length(vi, ti, &combined)
        } else {
            all_len
        };
        buried_length = (all_len - vis_len).max(0.0);
        if all_len > 0.0 {
            buried_fraction = buried_length / all_len;
        }
        rec_detail["recall_all"] = recall_all.map(Value::from).unwrap_or(Value::Null);
        rec_detail["buried_length_mm"] = Value::from((buried_length * 1e6).round() / 1e6);
        rec_detail["buried_fraction"] = Value::from((buried_fraction * 1e6).round() / 1e6);
    }
    let mut fill: Option<Value> = None;
    if !gated {
        let (mut area_n, mut lay) = (
            budget(cfg, "full_lattice_area", 1200),
            budget(cfg, "full_lattice_layers", 6),
        );
        if reduced {
            area_n = budget(cfg, "reduced_lattice_area", 400);
            lay = budget(cfg, "reduced_lattice_layers", 4);
        }
        fill = healthy_fill(vi, ti, vo, to, &mask, tol, area_n, lay);
    }
    let s1 = report.get("stage1").unwrap_or(&Value::Null);
    let vol_pct = s1.get("volume_change_percent").and_then(Value::as_f64);
    let bbox_pct = bbox_change_pct(vi, vo);

    let weights = g(cfg, "weights").cloned().unwrap_or_else(|| json!({}));
    let mut factors = Map::new();
    let mut used = 0.0;
    let mut numv = 0.0;
    let mut add = |name: &str, penalty: f64, detail: Value, factors: &mut Map<String, Value>| {
        let w = weights
            .get(name)
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .max(0.0);
        if w <= 0.0 {
            return;
        }
        let pen = penalty.clamp(0.0, 1.0);
        used += w;
        numv += w * pen;
        factors.insert(
            name.into(),
            json!({"penalty": (pen * 1e4).round() / 1e4,
                                            "weight": (w * 100.0).round() / 100.0,
                                            "detail": detail}),
        );
    };
    if let Some(mp) = max_pct {
        let scale = num(cfg, "distance_scale_pct", 0.5).max(1e-12);
        let mw = num(cfg, "distance_mean_weight", 0.3);
        let pen =
            (1.0 - mw) * (mp / scale).min(1.0) + mw * ((mean_pct.unwrap_or(0.0)) / scale).min(1.0);
        add(
            "healthy_distance",
            pen,
            json!({
                "max_pct": (mp * 1e4).round() / 1e4,
                "mean_pct": mean_pct.map(|x| (x * 1e4).round() / 1e4),
                "samples": healthy.iter().filter(|&&x| x).count(),
            }),
            &mut factors,
        );
    }
    if let Some(r) = recall {
        add("feature_recall", 1.0 - r, rec_detail.clone(), &mut factors);
    }
    if let Some(f) = &fill {
        if let Some(fr) = f.get("fill_fraction").and_then(Value::as_f64) {
            let rr = f
                .get("removed_fraction")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let vs = num(cfg, "volume_scale_fraction", 0.01).max(1e-12);
            add(
                "healthy_volume",
                ((fr + rr) / vs).min(1.0),
                json!({
                    "fill_fraction": (fr * 1e6).round() / 1e6,
                    "removed_fraction": (rr * 1e6).round() / 1e6,
                    "samples": f.get("samples").cloned().unwrap_or(Value::Null),
                }),
                &mut factors,
            );
        }
    }
    if vol_pct.is_some() || bbox_pct.is_some() {
        let vs = num(cfg, "volume_change_scale_pct", 15.0).max(1e-12);
        let bs = num(cfg, "bbox_scale_pct", 5.0).max(1e-12);
        let vp = (vol_pct.unwrap_or(0.0).abs() / vs).min(1.0);
        let bp = (bbox_pct.unwrap_or(0.0).abs() / bs).min(1.0);
        add(
            "volume_bbox",
            vp.max(bp),
            json!({
                "volume_change_pct": vol_pct.map(|x| (x * 1e4).round() / 1e4),
                "bbox_change_pct": bbox_pct.map(|x| (x * 1e4).round() / 1e4),
            }),
            &mut factors,
        );
    }
    let is = num(cfg, "invented_scale_fraction", 0.05).max(1e-12);
    add(
        "invented_area",
        (invented_frac / is).min(1.0),
        json!({"fraction": (invented_frac * 1e6).round() / 1e6}),
        &mut factors,
    );

    let in_area = face_areas(vi, ti);
    let buried_faces = if buried_info["available"].as_bool().unwrap_or(false) {
        Some(buried_mask.iter().filter(|&&x| x).count())
    } else {
        None
    };
    let buried_area = buried_faces.map(|_| {
        (buried_mask
            .iter()
            .zip(&in_area)
            .filter(|(m, _)| **m)
            .map(|(_, a)| *a)
            .sum::<f64>()
            * 1e6)
            .round()
            / 1e6
    });
    let mut removed_faces = None;
    let mut removed_area = None;
    if buried_info["available"].as_bool().unwrap_or(false) {
        removed_faces = Some(0usize);
        removed_area = Some(0.0);
        if buried_mask.iter().any(|&x| x) {
            let rt_o: Vec<[usize; 3]> = to
                .iter()
                .map(|x| [x[0] as usize, x[1] as usize, x[2] as usize])
                .collect();
            let bv = MeshBvh::from_arrays(vo, &rt_o);
            let mut d_i2o = Vec::with_capacity(vi.len());
            for p in vi {
                d_i2o.push(bv.closest_point(*p).1);
            }
            let mut removed = 0usize;
            let mut rarea = 0.0;
            for (fi, x) in ti.iter().enumerate() {
                if buried_mask[fi] {
                    let mx = d_i2o[x[0] as usize]
                        .max(d_i2o[x[1] as usize])
                        .max(d_i2o[x[2] as usize]);
                    if mx > tol {
                        removed += 1;
                        rarea += in_area[fi];
                    }
                }
            }
            removed_faces = Some(removed);
            removed_area = Some((rarea * 1e6).round() / 1e6);
        }
    }
    let cavity_faces = report
        .get("tamponade_shell_prepass")
        .and_then(|p| p.get("cavity_count"))
        .and_then(Value::as_i64);
    let mut wall_bits: Vec<String> = Vec::new();
    if let Some(rf) = removed_faces {
        if rf > 0 {
            wall_bits.push(format!(
                "{} interior wall face(s) removed ({:.3} mm²)",
                rf,
                removed_area.unwrap_or(0.0)
            ));
        }
    }
    if let (Some(bf), Some(rf)) = (buried_faces, removed_faces) {
        let kept = bf as i64 - rf as i64;
        if kept > 0 {
            wall_bits.push(format!("{} buried face(s) kept", kept));
        }
    }
    if let Some(df) = buried_info["debris_faces"].as_i64() {
        if df > 0 {
            wall_bits.push(format!("{} debris face(s) ignored", df));
        }
    }
    if let Some(cf) = cavity_faces {
        if cf > 0 {
            wall_bits.push(format!("{} cavity face(s) kept", cf));
        }
    }
    if !buried_info["available"].as_bool().unwrap_or(false) {
        wall_bits = vec![format!(
            "interior walls unavailable ({})",
            buried_info["note"].as_str().unwrap_or("large mesh")
        )];
    }
    let interior_walls_note = if wall_bits.is_empty() {
        Value::Null
    } else {
        Value::from(wall_bits.join("; "))
    };

    let mut score = None;
    if max_pct.is_none() {
        note.push("no healthy distance samples".into());
    }
    if recall.is_none() {
        note.push("feature recall unavailable".into());
    }
    if fill.is_none() {
        note.push("healthy volume unavailable".into());
    }
    if max_pct.is_some() && used > 0.0 {
        score = Some(((1.0 - (numv / used)).clamp(0.0, 1.0) * 100.0).round_ties_even() as i64);
    }
    let mut reasons: Vec<String> = Vec::new();
    for (name, flag) in [("ftetwild", "experimental_ftetwild"), ("splint", "splint")] {
        let tier = report.get(name).or_else(|| report.get(flag));
        if tier
            .and_then(|t| t.get("adopted"))
            .map(|v| v.as_bool().unwrap_or(false))
            .unwrap_or(false)
        {
            reasons.push(name.into());
        }
    }
    let lc = |k: &str, d: f64| {
        cfg.get("fidelity")
            .and_then(|f| f.get("low_confidence"))
            .and_then(|l| l.get(k))
            .and_then(Value::as_f64)
            .unwrap_or(d)
    };
    if invented_frac >= lc("invented_area_fraction", 0.03) {
        reasons.push("invented_area".into());
    }
    if low_areas > 0 {
        reasons.push("changed_areas".into());
    }
    let low_confidence = !reasons.is_empty();
    let (code, verdict_areas) = verdict(
        score,
        report,
        low_confidence,
        areas_n.or(Some(low_areas)),
        cfg,
        max_pct,
    );
    empty["repair_fidelity_note"] = if note.is_empty() {
        Value::Null
    } else {
        Value::from(note.join("; "))
    };
    empty["repair_fidelity"] = score.map(Value::from).unwrap_or(Value::Null);
    empty["repair_fidelity_factors"] = Value::Object(factors);
    empty["repair_fidelity_metrics"] = json!({
        "healthy_distance_max_pct": max_pct.map(|x| (x * 1e4).round() / 1e4),
        "healthy_distance_mean_pct": mean_pct.map(|x| (x * 1e4).round() / 1e4),
        "feature_recall": recall,
        "feature_recall_all": recall_all,
        "feature_buried_length_mm": (buried_length * 1e6).round() / 1e6,
        "feature_buried_fraction": (buried_fraction * 1e6).round() / 1e6,
        "interior_walls_removed_faces": removed_faces,
        "interior_walls_removed_area_mm2": removed_area,
        "interior_walls_buried_faces": buried_faces,
        "interior_walls_buried_area_mm2": buried_area,
        "interior_walls_debris_faces": buried_info["debris_faces"],
        "interior_walls_cavity_faces": cavity_faces,
        "healthy_fill_fraction": fill.as_ref().and_then(|f| f.get("fill_fraction")).cloned().unwrap_or(Value::Null),
        "healthy_removed_fraction": fill.as_ref().and_then(|f| f.get("removed_fraction")).cloned().unwrap_or(Value::Null),
        "invented_area_fraction": (invented_frac * 1e6).round() / 1e6,
        "changed_areas": areas_n,
        "low_confidence_areas": low_areas,
        "budget": if gated { "gated" } else if reduced { "reduced" } else { "full" },
    });
    empty["repair_verdict_code"] = Value::from(code);
    empty["repair_verdict_areas"] = Value::from(verdict_areas);
    empty["repair_verdict_low_confidence"] = Value::Bool(low_confidence);
    empty["repair_verdict_reasons"] = json!(reasons);
    empty["interior_walls_note"] = interior_walls_note;
    empty
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

pub fn compute_fidelity_json(
    vi: &[[f64; 3]],
    ti: &[[i32; 3]],
    vo: &[[f64; 3]],
    to: &[[i32; 3]],
    report: &str,
    cfg: &str,
) -> Result<String, String> {
    let report: Value = serde_json::from_str(report).map_err(|e| e.to_string())?;
    let cfg: Value = serde_json::from_str(cfg).map_err(|e| e.to_string())?;
    Ok(compute_fidelity_value(vi, ti, vo, to, &report, &cfg).to_string())
}
