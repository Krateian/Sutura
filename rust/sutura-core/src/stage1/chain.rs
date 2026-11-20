// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Stage-1 filter-chain builders and the pure `apply_chain` runner.
//!
//! Every filter is a pure `sutura-geom` kernel; the Python module keeps the
//! same chain definitions and delegates here when `SUTURA_STAGE1_ENGINE=rust`.

use std::collections::{BTreeSet, HashMap};

use serde_json::{json, Value};
use sutura_geom::topology::{
    invert_face_orientation_core, re_orient_faces_coherently_core_with_cancel,
    remove_connected_components_core_with_cancel, remove_duplicate_faces_core,
    remove_duplicate_vertices_core_with_cancel, remove_null_faces_core,
    remove_selected_faces_core, remove_unreferenced_vertices_core,
    repair_non_manifold_edges_core_with_cancel,
    repair_non_manifold_vertices_core_with_cancel,
};

fn step(name: &str, params: Value) -> Value {
    json!([name, params])
}

pub fn cleanup_chain(safe_null: bool) -> Vec<Value> {
    vec![
        step("meshing_remove_duplicate_faces", json!({})),
        step(
            "meshing_remove_null_faces",
            if safe_null { json!({"safe": true}) } else { json!({}) },
        ),
        step("meshing_remove_duplicate_vertices", json!({})),
        step("meshing_remove_duplicate_faces", json!({})),
        step("meshing_repair_non_manifold_edges", json!({})),
        step("meshing_re_orient_faces_coherently", json!({})),
        step("meshing_repair_non_manifold_vertices", json!({})),
    ]
}

pub fn stage1_chain(maxholesize: i64, mincomponentsize: i64, join_components: bool) -> Vec<Value> {
    let mut chain = cleanup_chain(true);
    chain.push(step(
        "meshing_close_holes",
        json!({"maxholesize": maxholesize}),
    ));
    if !join_components {
        chain.push(step(
            "meshing_remove_connected_component_by_face_number",
            json!({"mincomponentsize": mincomponentsize, "removeunref": true}),
        ));
        chain.push(step("meshing_remove_unreferenced_vertices", json!({})));
        chain.push(step("meshing_re_orient_faces_coherently", json!({})));
        chain.push(step(
            "meshing_close_holes",
            json!({"maxholesize": maxholesize}),
        ));
    }
    chain
}

pub fn delete_fallback_chain(
    maxholesize: i64,
    mincomponentsize: i64,
    join_components: bool,
) -> Vec<Value> {
    let mut chain = vec![
        step("meshing_remove_duplicate_faces", json!({})),
        step("meshing_remove_null_faces", json!({})),
        step("meshing_remove_duplicate_vertices", json!({})),
        step("meshing_remove_duplicate_faces", json!({})),
        step("compute_selection_by_non_manifold_edges_per_face", json!({})),
        step("meshing_remove_selected_faces", json!({})),
        step("set_selection_none", json!({})),
        step("meshing_remove_unreferenced_vertices", json!({})),
        step("meshing_repair_non_manifold_edges", json!({})),
        step("meshing_repair_non_manifold_vertices", json!({})),
    ];
    if !join_components {
        chain.push(step(
            "meshing_remove_connected_component_by_face_number",
            json!({"mincomponentsize": mincomponentsize, "removeunref": true}),
        ));
    }
    chain.push(step("meshing_re_orient_faces_coherently", json!({})));
    chain.push(step(
        "meshing_close_holes",
        json!({"maxholesize": maxholesize}),
    ));
    chain
}

fn as_f32(v: &[[f64; 3]]) -> Vec<[f32; 3]> {
    v.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect()
}

fn f32_to_f64(v: &[[f32; 3]]) -> Vec<[f64; 3]> {
    v.iter().map(|p| [p[0] as f64, p[1] as f64, p[2] as f64]).collect()
}

fn non_manifold_edge_faces(t: &[[i64; 3]]) -> Vec<bool> {
    let mut counts: HashMap<(i64, i64), usize> = HashMap::new();
    for r in t {
        for (a, b) in [(r[0], r[1]), (r[1], r[2]), (r[2], r[0])] {
            let k = if a <= b { (a, b) } else { (b, a) };
            *counts.entry(k).or_insert(0) += 1;
        }
    }
    let mut selected = vec![false; t.len()];
    for (fi, r) in t.iter().enumerate() {
        for (a, b) in [(r[0], r[1]), (r[1], r[2]), (r[2], r[0])] {
            let k = if a <= b { (a, b) } else { (b, a) };
            if counts.get(&k).copied().unwrap_or(0) > 2 {
                selected[fi] = true;
                break;
            }
        }
    }
    selected
}

fn small_parts_json(v: &[[f64; 3]], t: &[[i64; 3]], min_faces: usize) -> Option<Value> {
    let t32: Vec<[i32; 3]> = t.iter().map(|r| [r[0] as i32, r[1] as i32, r[2] as i32]).collect();
    let r = crate::report::removed_small_parts(v, &t32, min_faces)?;
    Some(json!({
        "count": r.count,
        "min_faces": r.min_faces,
        "faces_total": r.faces_total,
        "capped": r.capped,
        "parts": r.parts.iter().map(|p| json!({
            "faces": p.faces,
            "size_mm": p.size_mm,
            "size_pct": p.size_pct,
            "distance_mm": p.distance_mm,
        })).collect::<Vec<_>>(),
    }))
}

/// Run a chain. Returns `(verts, tris, applied, skipped, removed_small_parts)`.
fn stage1_fp(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut feed = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    };
    for p in verts {
        for c in p {
            for b in c.to_le_bytes() {
                feed(b);
            }
        }
    }
    for t in tris {
        for c in t {
            for b in c.to_le_bytes() {
                feed(b);
            }
        }
    }
    h
}

pub fn apply_chain(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    chain: &Value,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, usize, Value, Option<Value>) {
    apply_chain_with_cancel(verts, tris, chain, None)
}

/// `apply_chain` with a cooperative cancellation probe.  On cancellation the
/// chain stops at the current step and returns the mesh produced so far; the
/// caller checks the token and reports the cancelled outcome.
pub fn apply_chain_with_cancel(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    chain: &Value,
    cancel: Option<&dyn sutura_geom::cancel::Cancel>,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, usize, Value, Option<Value>) {
    let mut _errors = Vec::new();
    apply_chain_with_cancel_errors(verts, tris, chain, cancel, &mut _errors)
}

/// `apply_chain_with_cancel` that also appends Python-style Stage-1 filter
/// exceptions to `errors`.  The Rust filters succeed where trimesh raised, so
/// these records are health-only and do not skip the filter.
pub fn apply_chain_with_cancel_errors(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    chain: &Value,
    cancel: Option<&dyn sutura_geom::cancel::Cancel>,
    errors: &mut Vec<Value>,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, usize, Value, Option<Value>) {
    let mut v = verts.to_vec();
    let mut t = tris.to_vec();
    let mut applied = 0usize;
    let mut skipped = serde_json::Map::new();
    let mut removed_small_parts = None;
    let mut selected = vec![false; t.len()];
    let steps = chain.as_array().cloned().unwrap_or_default();
    let fp_on = std::env::var_os("SUTURA_STAGE1_FP").is_some();
    if fp_on {
        eprintln!(
            "[stage1-fp] input faces={} verts={} {:016x}",
            t.len(),
            v.len(),
            stage1_fp(&v, &t)
        );
    }
    for entry in steps {
        if sutura_geom::cancel::cancelled(cancel) {
            break;
        }
        let a = match entry.as_array() {
            Some(a) if !a.is_empty() => a,
            _ => continue,
        };
        let name = a[0].as_str().unwrap_or("");
        let params = a.get(1).cloned().unwrap_or_else(|| json!({}));
        if name == "meshing_re_orient_faces_coherently"
            || name == "meshing_close_holes"
        {
            let summary = crate::tiers::common::topology_summary(&v, &t);
            if name == "meshing_re_orient_faces_coherently"
                && summary.non_two_manifold_edges > 0
            {
                errors.push(json!({
                    "method": "stage1:meshing_re_orient_faces_coherently",
                    "stage": "filter",
                    "kind": "exception",
                    "message": "ValueError: Mesh has some not 2-manifold faces, Orientability requires manifoldness",
                }));
            }
            if name == "meshing_close_holes" && summary.non_two_manifold_edges > 0 {
                errors.push(json!({
                    "method": "stage1:meshing_close_holes",
                    "stage": "filter",
                    "kind": "exception",
                    "message": "ValueError: mesh has non-2-manifold edges",
                }));
            }
        }
        let _t_step = std::time::Instant::now();
        let _f_before = t.len();
        let _t_step = std::time::Instant::now();
        let _f_before = t.len();
        let res: Result<(), String> = (|| {
            match name {
                "meshing_remove_duplicate_faces" => {
                    t = remove_duplicate_faces_core(&t);
                }
                "meshing_remove_null_faces" => {
                    let v32 = as_f32(&v);
                    let safe = params.get("safe").and_then(Value::as_bool).unwrap_or(false);
                    t = remove_null_faces_core(&v32, &t, safe);
                }
                "meshing_remove_duplicate_vertices" => {
                    let v32 = as_f32(&v);
                    let (nv, nt) =
                        remove_duplicate_vertices_core_with_cancel(&v32, &t, cancel);
                    v = f32_to_f64(&nv);
                    t = nt;
                }
                "meshing_remove_unreferenced_vertices" => {
                    let v32 = as_f32(&v);
                    let (nv, nt) = remove_unreferenced_vertices_core(&v32, &t);
                    v = f32_to_f64(&nv);
                    t = nt;
                }
                "meshing_remove_connected_component_by_face_number" => {
                    let min = params.get("mincomponentsize").and_then(Value::as_i64).unwrap_or(8);
                    let unref = params.get("removeunref").and_then(Value::as_bool).unwrap_or(true);
                    removed_small_parts = small_parts_json(&v, &t, min as usize);
                    let v32 = as_f32(&v);
                    let (nv, nt) = remove_connected_components_core_with_cancel(
                        &v32, &t, min, unref, cancel);
                    v = f32_to_f64(&nv);
                    t = nt;
                }
                "meshing_invert_face_orientation" => {
                    t = invert_face_orientation_core(&t);
                }
                "meshing_repair_non_manifold_edges" => {
                    let v32 = as_f32(&v);
                    t = repair_non_manifold_edges_core_with_cancel(&v32, &t, cancel);
                }
                "meshing_re_orient_faces_coherently" => {
                    t = re_orient_faces_coherently_core_with_cancel(&t, cancel).map_err(|_| {
                        "Mesh has some not 2-manifold faces, Orientability requires manifoldness"
                            .to_string()
                    })?;
                }
                "meshing_repair_non_manifold_vertices" => {
                    let v32 = as_f32(&v);
                    let (nv, nt) =
                        repair_non_manifold_vertices_core_with_cancel(&v32, &t, cancel);
                    v = f32_to_f64(&nv);
                    t = nt;
                }
                "compute_selection_by_non_manifold_edges_per_face" => {
                    selected = non_manifold_edge_faces(&t);
                }
                "set_selection_none" => {
                    selected = vec![false; t.len()];
                }
                "meshing_remove_selected_faces" => {
                    let mask = if selected.len() == t.len() {
                        selected.clone()
                    } else {
                        vec![false; t.len()]
                    };
                    t = remove_selected_faces_core(&t, &mask);
                }
                "meshing_close_holes" => {
                    let max = params.get("maxholesize").and_then(Value::as_i64).unwrap_or(1000);
                    let si = params
                        .get("selfintersection")
                        .and_then(Value::as_bool)
                        .unwrap_or(true);
                    let rep = sutura_geom::hole::close_holes_core_with_cancel(
                        &v, &t, max, si, cancel)?;
                    v = rep.verts;
                    t = rep.tris;
                }
                other => return Err(format!("unhandled native chain filter: {}", other)),
            }
            Ok(())
        })();
        if std::env::var_os("SUTURA_STAGE_TIMES").is_some() {
            eprintln!(
                "[stage1-time] {} {:.3} ms faces {}->{}",
                name,
                _t_step.elapsed().as_secs_f64() * 1000.0,
                _f_before,
                t.len()
            );
        }
        let failed = res.is_err();
        match res {
            Ok(()) => applied += 1,
            Err(msg) => {
                skipped.insert(name.to_string(), Value::from(msg));
            }
        }
        if fp_on {
            eprintln!(
                "[stage1-fp] {}{} faces={} verts={} {:016x}",
                name,
                if failed { " SKIP" } else { "" },
                t.len(),
                v.len(),
                stage1_fp(&v, &t)
            );
        }
        if sutura_geom::cancel::cancelled(cancel) {
            break;
        }
    }
    (v, t, applied, Value::Object(skipped), removed_small_parts)
}

/// Extreme-mode extra passes: drop SI faces, then re-run the main chain once.
pub fn extreme_extra_passes(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    params: &Value,
) -> (bool, i64, usize, usize, Value, Vec<[f64; 3]>, Vec<[i64; 3]>) {
    let ut: Vec<[usize; 3]> = tris
        .iter()
        .map(|r| [r[0] as usize, r[1] as usize, r[2] as usize])
        .collect();
    let (mask, found, _skipped) = sutura_geom::self_intersections_core(verts, &ut);
    if found <= 0 {
        return (false, found.max(0), 0, 0, json!({}), verts.to_vec(), tris.to_vec());
    }
    let before = tris.len();
    let t2 = remove_selected_faces_core(tris, &mask);
    let v32 = as_f32(verts);
    let (nv, nt) = remove_unreferenced_vertices_core(&v32, &t2);
    let mut v = f32_to_f64(&nv);
    let mut t = nt;
    let removed = before.saturating_sub(t2.len());
    let maxholesize = params.get("maxholesize").and_then(Value::as_i64).unwrap_or(1000);
    let mincomp = params
        .get("mincomponentsize")
        .and_then(Value::as_i64)
        .unwrap_or(8);
    let join = params
        .get("join_components")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let chain = Value::Array(stage1_chain(maxholesize, mincomp, join));
    let (v2, t2, applied2, skipped2, _) = apply_chain(&v, &t, &chain);
    v = v2;
    t = t2;
    (true, found, removed, applied2, skipped2, v, t)
}

/// Python `stage1.join_small_components`: move every connected component
/// smaller than `mincomponentsize` onto the nearest vertex of the nearest
/// larger component, fuse the coincident vertices and re-close the holes.
/// Returns `(verts, tris, moved, remaining_small)`; opt-in only
/// (`--experimental-join-components`).
pub fn join_small_components(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    mincomponentsize: i64,
    maxholesize: i64,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, usize, usize) {
    let n = tris.len();
    if n == 0 {
        return (verts.to_vec(), tris.to_vec(), 0, 0);
    }
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    fn union(parent: &mut [usize], a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent[ra] = rb;
        }
    }
    let mut parent: Vec<usize> = (0..n).collect();
    let mut vertex_faces: HashMap<i64, Vec<usize>> = HashMap::new();
    for (fi, t) in tris.iter().enumerate() {
        for &vi in t {
            vertex_faces.entry(vi).or_default().push(fi);
        }
    }
    for faces in vertex_faces.values() {
        let first = faces[0];
        for &fi in &faces[1..] {
            union(&mut parent, first, fi);
        }
    }
    let comps: Vec<usize> = (0..n).map(|i| find(&mut parent, i)).collect();
    let mut counts: HashMap<usize, usize> = HashMap::new();
    for &c in &comps {
        *counts.entry(c).or_insert(0) += 1;
    }
    let mut large: Vec<usize> = Vec::new();
    let mut small: Vec<usize> = Vec::new();
    for (&c, &count) in &counts {
        if count as i64 >= mincomponentsize {
            large.push(c);
        } else {
            small.push(c);
        }
    }
    if large.is_empty() || small.is_empty() {
        return (verts.to_vec(), tris.to_vec(), 0, small.len());
    }
    let mut comp_verts: HashMap<usize, BTreeSet<i64>> = HashMap::new();
    for (fi, &c) in comps.iter().enumerate() {
        let set = comp_verts.entry(c).or_default();
        for &x in &tris[fi] {
            set.insert(x);
        }
    }
    let comp_verts: HashMap<usize, Vec<usize>> = comp_verts
        .into_iter()
        .map(|(c, set)| (c, set.into_iter().map(|x| x as usize).collect()))
        .collect();
    let mut v_new = verts.to_vec();
    let mut moved = 0usize;
    for &sc in &small {
        let sv = &comp_verts[&sc];
        let sv3: Vec<[f64; 3]> = sv.iter().map(|&i| verts[i]).collect();
        let mut best = f64::INFINITY;
        let mut best_translate: Option<[f64; 3]> = None;
        for &lc in &large {
            let lv = &comp_verts[&lc];
            let lv3: Vec<[f64; 3]> = lv.iter().map(|&i| verts[i]).collect();
            for a in &sv3 {
                for b in &lv3 {
                    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                    let d2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                    if d2 < best {
                        best = d2;
                        best_translate = Some(d);
                    }
                }
            }
        }
        if let Some(tr) = best_translate {
            for &i in sv {
                v_new[i] = [v_new[i][0] + tr[0], v_new[i][1] + tr[1], v_new[i][2] + tr[2]];
            }
            moved += 1;
        }
    }
    let chain = Value::Array(vec![
        step("meshing_remove_duplicate_vertices", json!({})),
        step("meshing_repair_non_manifold_vertices", json!({})),
        step("meshing_remove_duplicate_faces", json!({})),
        step("meshing_close_holes", json!({"maxholesize": maxholesize})),
        step("meshing_remove_unreferenced_vertices", json!({})),
    ]);
    let (v2, t2, _applied, _skipped, _removed) = apply_chain(&v_new, tris, &chain);
    let remaining = small.len().saturating_sub(moved);
    (v2, t2, moved, remaining)
}

