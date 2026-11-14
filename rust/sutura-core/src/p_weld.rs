//! Reload-safe weld and P-HONEST verdict, matching `pipeline/p_weld.py`.

use std::collections::HashMap;

use serde_json::Value;
use sutura_geom::hole::close_holes_core;
use sutura_geom::topology::{
    repair_non_manifold_edges_core, repair_non_manifold_vertices_core,
    remove_unreferenced_vertices_core, weld_reload_equivalent_core, weld_vertices_core,
};

use crate::report::detect_defects;

const MAX_NUDGE_ATTEMPTS: usize = 12;
const MAX_HOLE: i64 = 200;
const MIN_FACE_FRACTION: f64 = 0.98;

pub fn weld_reload_equivalent(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
) -> Result<(Vec<[f32; 3]>, Vec<[i64; 3]>), String> {
    let v32: Vec<[f32; 3]> = verts.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
    weld_reload_equivalent_core(&v32, tris)
}

pub fn reload_topology(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> (usize, usize, usize) {
    let (wv, wt) = match weld_reload_equivalent(verts, tris) {
        Ok(x) => x,
        Err(_) => return (0, 0, 0),
    };
    let t32: Vec<[i32; 3]> = wt.iter().map(|r| [r[0] as i32, r[1] as i32, r[2] as i32]).collect();
    let d = detect_defects(&wv, &t32, false);
    (d.holes.len(), d.non_manifold.len(), pinched_count(&wv, &wt))
}

pub fn reload_strict_holes_nm(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> (usize, usize) {
    let (wv, wt) = match weld_reload_equivalent(verts, tris) {
        Ok(x) => x,
        Err(_) => return (0, 0),
    };
    let t32: Vec<[i32; 3]> = wt.iter().map(|r| [r[0] as i32, r[1] as i32, r[2] as i32]).collect();
    let d = detect_defects(&wv, &t32, false);
    (d.holes.len(), d.non_manifold.len())
}

pub fn spacing_f32(x: f32) -> f32 {
    if x == 0.0 {
        return f32::from_bits(1);
    }
    let a = x.abs();
    let next = f32::from_bits(a.to_bits() + 1);
    (next - a).abs()
}

fn index_boundary_nm(tris: &[[i64; 3]]) -> (usize, usize) {
    let mut edges: HashMap<(i64, i64), usize> = HashMap::new();
    for t in tris {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let k = if a <= b { (a, b) } else { (b, a) };
            *edges.entry(k).or_insert(0) += 1;
        }
    }
    let mut boundary = 0;
    let mut nm = 0;
    for c in edges.values() {
        if *c == 1 { boundary += 1; }
        if *c > 2 { nm += 1; }
    }
    (boundary, nm)
}

pub fn pinched_count(vrows: &[[f32; 3]], trows: &[[i64; 3]]) -> usize {
    let nv = vrows.len();
    let nf = trows.len();
    if nf == 0 || nv == 0 {
        return 0;
    }
    let mut edge_faces: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (fi, t) in trows.iter().enumerate() {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let k = if a <= b { (a, b) } else { (b, a) };
            edge_faces.entry(k).or_default().push(fi);
        }
    }
    let mut nm_edge_verts = std::collections::HashSet::new();
    for ((a, b), faces) in &edge_faces {
        if faces.len() > 2 {
            nm_edge_verts.insert(*a);
            nm_edge_verts.insert(*b);
        }
    }
    let mut v_faces: Vec<Vec<usize>> = vec![Vec::new(); nv];
    for (fi, t) in trows.iter().enumerate() {
        let mut uv = Vec::with_capacity(3);
        for &v in t {
            if v >= 0 && (v as usize) < nv && !uv.contains(&(v as usize)) {
                uv.push(v as usize);
            }
        }
        for v in uv {
            v_faces[v].push(fi);
        }
    }
    let mut count = 0;
    for v in 0..nv {
        let inc = &v_faces[v];
        if inc.len() <= 1 || nm_edge_verts.contains(&(v as i64)) {
            continue;
        }
        let n = inc.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(p: &mut [usize], mut x: usize) -> usize {
            while p[x] != x { p[x] = p[p[x]]; x = p[x]; }
            x
        }
        let mut edge_neighbors: HashMap<i64, Vec<usize>> = HashMap::new();
        for (li, &fi) in inc.iter().enumerate() {
            let tri = trows[fi];
            let mut others = Vec::new();
            for &w in &tri {
                if w != v as i64 && !others.contains(&w) { others.push(w); }
            }
            for w in others { edge_neighbors.entry(w).or_default().push(li); }
        }
        for fl in edge_neighbors.values() {
            if fl.len() > 1 {
                for k in 1..fl.len() {
                    let a = find(&mut parent, fl[0]);
                    let b = find(&mut parent, fl[k]);
                    if a != b { parent[a] = b; }
                }
            }
        }
        let mut roots: Vec<usize> = (0..n).map(|i| find(&mut parent, i)).collect();
        roots.sort_unstable(); roots.dedup();
        if roots.len() > 1 { count += 1; }
    }
    count
}

fn remove_duplicate_faces(tris: &[[i64; 3]]) -> Vec<[i64; 3]> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for t in tris {
        let mut k = *t;
        k.sort_unstable();
        if seen.insert(k) { out.push(*t); }
    }
    out
}

fn repair_welded_topology(v: &[[f32; 3]], t: &[[i64; 3]]) -> (Vec<[f32; 3]>, Vec<[i64; 3]>) {
    let mut t = repair_non_manifold_edges_core(v, t);
    t = remove_duplicate_faces(&t);
    let (v, t) = repair_non_manifold_vertices_core(v, &t);
    remove_unreferenced_vertices_core(&v, &t)
}

fn close_small_holes(v: &[[f64; 3]], t: &[[i64; 3]]) -> (Vec<[f32; 3]>, Vec<[i64; 3]>) {
    let v32: Vec<[f32; 3]> = v.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
    let (v2, t2) = repair_non_manifold_vertices_core(&v32, t);
    let v64: Vec<[f64; 3]> = v2.iter().map(|p| [p[0] as f64, p[1] as f64, p[2] as f64]).collect();
    let (ov, ot) = match close_holes_core(&v64, &t2, MAX_HOLE, false) {
        Ok(r) => (r.verts, r.tris),
        Err(_) => (v64, t2),
    };
    let ov32: Vec<[f32; 3]> = ov.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
    remove_unreferenced_vertices_core(&ov32, &ot)
}

pub fn separate_weld_collisions(v: &[[f64; 3]], t: &[[i64; 3]]) -> (Vec<[f32; 3]>, Vec<[i64; 3]>) {
    let v32: Vec<[f32; 3]> = v.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
    let (_, inv) = match weld_vertices_core(&v32) {
        Ok(x) => x,
        Err(_) => return (v32, t.to_vec()),
    };
    let mut counts: HashMap<i64, usize> = HashMap::new();
    for &i in &inv { *counts.entry(i).or_insert(0) += 1; }
    if counts.values().all(|&c| c <= 1) {
        return (v32, t.to_vec());
    }
    let mut order: Vec<usize> = (0..inv.len()).collect();
    order.sort_by_key(|&i| inv[i]);
    let mut pos = vec![0usize; inv.len()];
    for (p, &i) in order.iter().enumerate() { pos[i] = p; }
    let ngroups = counts.keys().copied().max().unwrap_or(-1) as usize + 1;
    let mut count_vec = vec![0usize; ngroups];
    for (&g, &c) in &counts { count_vec[g as usize] = c; }
    let mut group_start = vec![0usize; ngroups];
    let mut acc = 0usize;
    for g in 0..ngroups {
        group_start[g] = acc;
        acc += count_vec[g];
    }
    let rank: Vec<f64> = (0..inv.len())
        .map(|i| (pos[i] - group_start[inv[i] as usize]) as f64)
        .collect();
    let nonrep: Vec<bool> = rank.iter().map(|&r| r > 0.0).collect();
    let dir = 1.0 / 3f64.sqrt();
    for attempt in 0..MAX_NUDGE_ATTEMPTS {
        let mut d = v.to_vec();
        for i in 0..d.len() {
            if nonrep[i] {
                let mag = [
                    (v32[i][0].abs() as f64).max(1.0),
                    (v32[i][1].abs() as f64).max(1.0),
                    (v32[i][2].abs() as f64).max(1.0),
                ];
                let ulp = mag
                    .iter()
                    .map(|&m| spacing_f32(m as f32) as f64)
                    .fold(1e-7f64, f64::max);
                let step = ulp * rank[i] * 2f64.powi(attempt as i32 + 1);
                d[i][0] += step * dir;
                d[i][1] += step * dir;
                d[i][2] += step * dir;
            }
        }
        let d32: Vec<[f32; 3]> = d.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
        if let Ok((uniq, _)) = weld_vertices_core(&d32) {
            if uniq.len() == d32.len() { return (d32, t.to_vec()); }
        }
    }
    (v32, t.to_vec())
}

pub struct PWeldResult {
    pub verts: Vec<[f32; 3]>,
    pub tris: Vec<[i32; 3]>,
    pub report: Option<Value>,
}

pub fn p_weld_final(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> PWeldResult {
    let (bh, bnm, bn2m) = reload_topology(verts, tris);
    if bh == 0 && bnm == 0 && bn2m == 0 {
        let v32: Vec<[f32; 3]> = verts.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
        let t32: Vec<[i32; 3]> = tris.iter().map(|t| [t[0] as i32, t[1] as i32, t[2] as i32]).collect();
        return PWeldResult { verts: v32, tris: t32, report: None };
    }
    let mut rec = serde_json::json!({
        "applied": false, "method": Value::Null,
        "holes_before": bh, "non_manifold_before": bnm, "non_two_manifold_before": bn2m,
        "holes_after": bh, "non_manifold_after": bnm, "non_two_manifold_after": bn2m,
        "vertices_nudged": 0, "faces_before": tris.len(), "faces_after": tris.len()
    });
    let t32: Vec<[i32; 3]> = tris.iter().map(|t| [t[0] as i32, t[1] as i32, t[2] as i32]).collect();
    let v32: Vec<[f32; 3]> = verts.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
    let (orig_boundary, orig_nm) = index_boundary_nm(tris);
    if orig_boundary == 0 && orig_nm == 0 {
        let (cv, ct) = separate_weld_collisions(verts, tris);
        let cv64: Vec<[f64; 3]> = cv.iter().map(|p| [p[0] as f64, p[1] as f64, p[2] as f64]).collect();
        let (ch, cnm, cn2m) = reload_topology(&cv64, &ct);
        if ch == 0 && cnm == 0 && cn2m == 0 {
            let nudged = cv.iter().zip(&v32).filter(|(a, b)| a != b).count();
            rec["applied"] = Value::Bool(true);
            rec["method"] = Value::from("split-collisions");
            rec["holes_after"] = Value::from(0);
            rec["non_manifold_after"] = Value::from(0);
            rec["non_two_manifold_after"] = Value::from(0);
            rec["vertices_nudged"] = Value::from(nudged);
            rec["faces_after"] = Value::from(ct.len());
            return PWeldResult { verts: cv, tris: ct.iter().map(|t| [t[0] as i32, t[1] as i32, t[2] as i32]).collect(), report: Some(rec) };
        }
    }
    let (wv, wt) = match weld_reload_equivalent(verts, tris) { Ok(x) => x, Err(_) => return PWeldResult { verts: v32, tris: t32, report: Some(rec) } };
    let welded_faces = wt.len().max(1);
    let (rv, rt) = repair_welded_topology(&wv, &wt);
    let candidates = [("weld-repair", (rv.clone(), rt.clone())), ("weld-repair+close", close_small_holes(&wv.iter().map(|p| [p[0] as f64, p[1] as f64, p[2] as f64]).collect::<Vec<_>>(), &wt))];
    for (name, (cv, ct)) in candidates {
        if (ct.len() as f64) < MIN_FACE_FRACTION * welded_faces as f64 { continue; }
        let cv64: Vec<[f64; 3]> = cv.iter().map(|p| [p[0] as f64, p[1] as f64, p[2] as f64]).collect();
        let (ch, cnm, cn2m) = reload_topology(&cv64, &ct);
        if ch == 0 && cnm == 0 && cn2m == 0 {
            rec["applied"] = Value::Bool(true); rec["method"] = Value::from(name);
            rec["holes_after"] = Value::from(0); rec["non_manifold_after"] = Value::from(0);
            rec["non_two_manifold_after"] = Value::from(0); rec["faces_after"] = Value::from(ct.len());
            return PWeldResult { verts: cv, tris: ct.iter().map(|t| [t[0] as i32, t[1] as i32, t[2] as i32]).collect(), report: Some(rec) };
        }
    }
    PWeldResult { verts: v32, tris: t32, report: Some(rec) }
}

pub fn enforce_reload_verdict(report: &mut Value, verts: &[[f64; 3]], tris: &[[i64; 3]], include_n2m: bool) -> bool {
    let Some(s1) = report.get("stage1").and_then(Value::as_object) else { return false; };
    let claims = s1.get("two_manifold").and_then(Value::as_bool).unwrap_or(false)
        && s1.get("holes_remaining").and_then(Value::as_i64).unwrap_or(0) == 0;
    if !claims { return false; }
    let (holes, nm, n2m) = if include_n2m { reload_topology(verts, tris) } else { let (h, n) = reload_strict_holes_nm(verts, tris); (h, n, 0) };
    if holes == 0 && nm == 0 && n2m == 0 { return false; }
    if let Some(s1) = report.get_mut("stage1").and_then(Value::as_object_mut) {
        s1.insert("reload_holes".into(), Value::from(holes));
        s1.insert("reload_non_manifold".into(), Value::from(nm));
        s1.insert("reload_non_manifold_vertices".into(), Value::from(n2m));
        s1.insert("two_manifold".into(), Value::Bool(false));
        s1.insert("holes_remaining".into(), Value::from(holes));
    }
    report["reload_watertight"] = Value::Bool(false);
    if let Some(s2) = report.get_mut("stage2").and_then(Value::as_object_mut) {
        s2.insert("watertight_after_reload".into(), Value::Bool(false));
    }
    true
}
