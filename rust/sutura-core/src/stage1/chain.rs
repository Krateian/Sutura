// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Stage-1 filter-chain builders and the pure `apply_chain` runner.
//!
//! Every filter is a pure `sutura-geom` kernel; the Python module keeps the
//! same chain definitions and delegates here when `SUTURA_STAGE1_ENGINE=rust`.

use std::collections::HashMap;

use serde_json::{json, Value};
use sutura_geom::hole::close_holes_core;
use sutura_geom::topology::{
    invert_face_orientation_core, re_orient_faces_coherently_core,
    remove_connected_components_core, remove_duplicate_faces_core,
    remove_duplicate_vertices_core, remove_null_faces_core,
    remove_selected_faces_core, remove_unreferenced_vertices_core,
    repair_non_manifold_edges_core, repair_non_manifold_vertices_core,
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
pub fn apply_chain(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    chain: &Value,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, usize, Value, Option<Value>) {
    let mut v = verts.to_vec();
    let mut t = tris.to_vec();
    let mut applied = 0usize;
    let mut skipped = serde_json::Map::new();
    let mut removed_small_parts = None;
    let mut selected = vec![false; t.len()];
    let steps = chain.as_array().cloned().unwrap_or_default();
    for entry in steps {
        let a = match entry.as_array() {
            Some(a) if !a.is_empty() => a,
            _ => continue,
        };
        let name = a[0].as_str().unwrap_or("");
        let params = a.get(1).cloned().unwrap_or_else(|| json!({}));
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
                    let (nv, nt) = remove_duplicate_vertices_core(&v32, &t);
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
                    let (nv, nt) = remove_connected_components_core(&v32, &t, min, unref);
                    v = f32_to_f64(&nv);
                    t = nt;
                }
                "meshing_invert_face_orientation" => {
                    t = invert_face_orientation_core(&t);
                }
                "meshing_repair_non_manifold_edges" => {
                    let v32 = as_f32(&v);
                    t = repair_non_manifold_edges_core(&v32, &t);
                }
                "meshing_re_orient_faces_coherently" => {
                    t = re_orient_faces_coherently_core(&t).map_err(|_| {
                        "Mesh has some not 2-manifold faces, Orientability requires manifoldness"
                            .to_string()
                    })?;
                }
                "meshing_repair_non_manifold_vertices" => {
                    let v32 = as_f32(&v);
                    let (nv, nt) = repair_non_manifold_vertices_core(&v32, &t);
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
                    let rep = close_holes_core(&v, &t, max, si)?;
                    v = rep.verts;
                    t = rep.tris;
                }
                other => return Err(format!("unhandled native chain filter: {}", other)),
            }
            Ok(())
        })();
        match res {
            Ok(()) => applied += 1,
            Err(msg) => {
                skipped.insert(name.to_string(), Value::from(msg));
            }
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
