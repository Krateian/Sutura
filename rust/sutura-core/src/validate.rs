// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Read-only mesh validation (`sutura validate`, `--dry-run`).
//!
//! Port of `sutura_engine/pipeline/validate.py`. Nothing here writes a file;
//! the report shapes follow the Python dicts byte for byte (key order aside,
//! the comparator parses JSON).

use serde_json::{json, Map, Value};

use sutura_geom::self_intersections_core;

use crate::classify::classify_mesh;
use crate::config::intensity::{triage_report_fields, IntensitySpec};
use crate::config::policy::LoadedPolicy;
use crate::mesh_io::three_mf::check_units;
use crate::report::classify::{estimate_confidence_value, Confidence};
use crate::report::defects::{detect_defects, Hole, NonManifoldRegion};
use crate::Mesh;

/// `sutura_engine.pipeline.utils.resolve_si_mode` values.
pub const SI_MODES: [&str; 3] = ["auto", "off", "on"];

/// Python's `round(x, n)`.
fn round_to(value: f64, digits: i32) -> f64 {
    crate::classify::py_round(value, digits)
}

fn holes_json(holes: &[Hole]) -> Value {
    Value::Array(
        holes
            .iter()
            .map(|h| {
                let mut o = Map::new();
                o.insert("centroid".into(), json!(h.centroid));
                o.insert("diameter".into(), json!(h.diameter));
                o.insert("vertices".into(), json!(h.vertices));
                if let Some(idx) = &h.verts_idx {
                    o.insert("verts_idx".into(), json!(idx));
                }
                Value::Object(o)
            })
            .collect(),
    )
}

fn non_manifold_json(regions: &[NonManifoldRegion]) -> Value {
    Value::Array(
        regions
            .iter()
            .map(|r| {
                let mut o = Map::new();
                o.insert("centroid".into(), json!(r.centroid));
                o.insert("faces".into(), json!(r.faces));
                if let Some(idx) = &r.verts_idx {
                    o.insert("verts_idx".into(), json!(idx));
                }
                Value::Object(o)
            })
            .collect(),
    )
}

/// `mesh_topology(...)['connected_components_number']`.
///
/// Faces are unioned through shared *edges* (a vertex-only touch does not
/// join two components), exactly like the union-find in
/// `sutura_geom::topology::mesh_topology`.
pub fn connected_components(tris: &[[i32; 3]]) -> usize {
    if tris.is_empty() {
        return 0;
    }
    let mut parent: Vec<usize> = (0..tris.len()).collect();
    fn find(parent: &mut [usize], x: usize) -> usize {
        let mut root = x;
        while parent[root] != root {
            root = parent[root];
        }
        let mut cur = x;
        while parent[cur] != root {
            let next = parent[cur];
            parent[cur] = root;
            cur = next;
        }
        root
    }
    // First face seen for each undirected edge; later faces union with it.
    let mut first_face: std::collections::HashMap<(i32, i32), usize> =
        std::collections::HashMap::new();
    for (fi, f) in tris.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (f[k], f[(k + 1) % 3]);
            let key = if a <= b { (a, b) } else { (b, a) };
            match first_face.get(&key) {
                Some(&other) => {
                    let (ra, rb) = (find(&mut parent, fi), find(&mut parent, other));
                    if ra != rb {
                        parent[ra] = rb;
                    }
                }
                None => {
                    first_face.insert(key, fi);
                }
            }
        }
    }
    let mut roots: Vec<usize> = (0..tris.len()).map(|f| find(&mut parent, f)).collect();
    roots.sort_unstable();
    roots.dedup();
    roots.len()
}

/// `validate_mesh_from_arrays`.
pub fn validate_mesh(
    mesh: &Mesh,
    loaded: &LoadedPolicy,
    engine: &str,
    declared_unit: Option<&str>,
    si_mode: &str,
) -> Result<Value, String> {
    let v = &mesh.verts;
    let t = &mesh.tris;
    if t.is_empty() || v.is_empty() {
        return Err("input mesh is empty (no triangles)".into());
    }
    if v.iter().any(|p| !p.iter().all(|c| c.is_finite())) {
        return Err("input mesh contains NaN or infinite coordinates".into());
    }

    let cls = classify_mesh(&f64_verts(v), &usize_tris(t), engine);
    let reported_type = cls
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let reported_conf = cls
        .get("confidence")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let defects = detect_defects(v, t, false);

    let self_intersections: Option<i64> = if si_mode == "off" {
        None
    } else {
        let sv: Vec<[f64; 3]> = v.iter().map(|p| [p[0] as f64, p[1] as f64, p[2] as f64]).collect();
        let st: Vec<[usize; 3]> = t
            .iter()
            .map(|f| [f[0] as usize, f[1] as usize, f[2] as usize])
            .collect();
        let (_mask, count, skipped) = self_intersections_core(&sv, &st);
        if skipped || count < 0 {
            None
        } else {
            Some(count)
        }
    };

    let vol = signed_volume(v, t);
    let area = surface_area(v, t);

    let mut validation = Map::new();
    validation.insert("vertices".into(), json!(v.len()));
    validation.insert("faces".into(), json!(t.len()));
    validation.insert("holes".into(), holes_json(&defects.holes));
    validation.insert("non_manifold".into(), non_manifold_json(&defects.non_manifold));
    validation.insert(
        "self_intersections".into(),
        match self_intersections {
            Some(n) => json!(n),
            None => Value::Null,
        },
    );
    validation.insert(
        "connected_components".into(),
        json!(connected_components(t)),
    );
    validation.insert(
        "watertight".into(),
        json!(defects.holes.is_empty()
            && defects.non_manifold.is_empty()
            && matches!(self_intersections, Some(0) | None)),
    );
    validation.insert("signed_volume".into(), json!(round_to(vol, 6)));
    validation.insert("surface_area".into(), json!(round_to(area, 3)));
    validation.insert(
        "orientation".into(),
        json!(if vol < 0.0 { "inverted" } else { "consistent" }),
    );

    if let Some(unit) = check_units(mesh, declared_unit) {
        validation.insert("unit_warning".into(), json!(true));
        validation.insert("unit_hint".into(), json!(unit.unit_hint));
    }

    let bridge = true;
    let signals = json!({
        "detected_type": reported_type,
        "detected_confidence": reported_conf,
        "stage2_bridge_available": bridge,
        "holes": defects.holes.len(),
        "non_manifold": defects.non_manifold.len(),
        "self_intersections": self_intersections,
    });
    let (mech_gate, org_gate) = tuning_gates(loaded);
    let est: Confidence = estimate_confidence_value(&signals, mech_gate, org_gate);

    Ok(json!({
        "validation": Value::Object(validation),
        "detected_type": reported_type,
        "detected_confidence": reported_conf,
        "classifier_engine": if engine == "classic" { "classic" } else { "experimental" },
        "stage2_bridge_available": bridge,
        "estimated_confidence": est.score,
        "estimated_confidence_label": est.label,
        "estimated_confidence_factors": est.factors,
    }))
}

fn f64_verts(v: &[[f32; 3]]) -> Vec<[f64; 3]> {
    v.iter()
        .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
        .collect()
}

fn usize_tris(t: &[[i32; 3]]) -> Vec<[usize; 3]> {
    t.iter()
        .map(|f| [f[0] as usize, f[1] as usize, f[2] as usize])
        .collect()
}

/// `utils.signed_volume`, from the shared numeric kernel.
pub fn signed_volume(verts: &[[f32; 3]], tris: &[[i32; 3]]) -> f64 {
    let v: Vec<[f64; 3]> = verts.iter().map(|p| p.map(f64::from)).collect();
    let t: Vec<[i64; 3]> = tris
        .iter()
        .map(|f| [f[0] as i64, f[1] as i64, f[2] as i64])
        .collect();
    sutura_geom::measures::signed_volume(&v, &t)
}

/// `utils.surface_area`: total triangle area in f32, summed with NumPy's
/// pairwise order (the per-face areas stay f32, like `np.sum` over an f32
/// array).
pub fn surface_area(verts: &[[f32; 3]], tris: &[[i32; 3]]) -> f64 {
    if tris.is_empty() || verts.is_empty() {
        return 0.0;
    }
    let mut areas: Vec<f32> = Vec::with_capacity(tris.len());
    for f in tris {
        let (i, j, k) = (f[0] as usize, f[1] as usize, f[2] as usize);
        if i >= verts.len() || j >= verts.len() || k >= verts.len() {
            areas.push(0.0);
            continue;
        }
        let a = verts[i];
        let b = verts[j];
        let c = verts[k];
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        areas.push(0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt());
    }
    f64::from(crate::reduce::sum_f32(&areas))
}

/// The classifier's confidence gates (`stage1_tuning.mech_gate/org_gate`).
pub fn tuning_gates(loaded: &LoadedPolicy) -> (f64, f64) {
    let read = |key: &str, default: f64| {
        loaded
            .get("stage1_tuning")
            .and_then(|t| t.get(key))
            .and_then(|v| match v {
                crate::config::PolicyValue::Float(f) => Some(*f),
                crate::config::PolicyValue::Int(i) => Some(*i as f64),
                _ => None,
            })
            .unwrap_or(default)
    };
    (read("mech_gate", 0.6), read("org_gate", 0.5))
}

/// `tuning_applied_for(mesh_type, confidence)`.
pub fn tuning_applied_for(loaded: &LoadedPolicy, mesh_type: &str, confidence: f64) -> bool {
    let (mech_gate, org_gate) = tuning_gates(loaded);
    match mesh_type {
        "mechanical" => confidence >= mech_gate,
        "organic" => confidence >= org_gate,
        _ => false,
    }
}

/// `utils.boundary_loop_stats`: `(n_loops, max_loop_len)` over the
/// boundary-edge graph (edges used by exactly one face).
pub fn boundary_loop_stats(verts_len: usize, tris: &[[i32; 3]]) -> (usize, usize) {
    if tris.is_empty() {
        return (0, 0);
    }
    let mut counts: std::collections::HashMap<(i32, i32), usize> =
        std::collections::HashMap::new();
    for t in tris {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a <= b { (a, b) } else { (b, a) };
            *counts.entry(key).or_insert(0) += 1;
        }
    }
    let mut adj: std::collections::HashMap<i32, Vec<i32>> = std::collections::HashMap::new();
    let mut order: Vec<i32> = Vec::new();
    let mut boundary: Vec<(i32, i32)> = counts
        .iter()
        .filter(|(_, &count)| count == 1)
        .map(|(&key, _)| key)
        .collect();
    // `utils.boundary_loop_stats` iterates `np.unique` output, so the
    // adjacency lists are filled in ascending edge-key order and the loops are
    // then entered in first-appearance order.  Iterating `counts` (a HashMap)
    // here used to randomise both, and on a boundary vertex with degree > 2
    // the greedy walk below picks a different branch per process: the same
    // mesh produced max_loop_len 150 / 154 / 157 across runs, which moved
    // `would_apply.maxholesize`.  Sort the edges and record the insertion
    // order explicitly so the walk is deterministic and matches Python.
    boundary.sort_unstable();
    for (a, b) in boundary {
        for v in [a, b] {
            if !adj.contains_key(&v) {
                order.push(v);
            }
            adj.entry(v).or_default().push(if v == a { b } else { a });
        }
    }
    let _ = verts_len;
    let mut seen: std::collections::HashSet<i32> = std::collections::HashSet::new();
    let mut loops: Vec<usize> = Vec::new();
    for &start in &order {
        if seen.contains(&start) {
            continue;
        }
        let (mut n, mut cur, mut prev) = (0usize, start, None::<i32>);
        while !seen.contains(&cur) {
            seen.insert(cur);
            n += 1;
            let nxt = adj
                .get(&cur)
                .and_then(|list| list.iter().copied().find(|&x| Some(x) != prev));
            match nxt {
                Some(v) => {
                    prev = Some(cur);
                    cur = v;
                }
                None => break,
            }
        }
        loops.push(n);
    }
    (loops.len(), loops.iter().copied().max().unwrap_or(0))
}

/// `topology.remove_connected_components(...)` face count, used for the
/// dry-run debris count (`before_faces - kept_faces`).
pub fn faces_after_component_filter(tris: &[[i32; 3]], min_faces: i64) -> usize {
    if tris.is_empty() {
        return 0;
    }
    let mut parent: Vec<usize> = (0..tris.len()).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    let mut first: std::collections::HashMap<(i32, i32), usize> =
        std::collections::HashMap::new();
    for (fi, f) in tris.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (f[k], f[(k + 1) % 3]);
            let key = if a <= b { (a, b) } else { (b, a) };
            match first.get(&key) {
                Some(&other) => {
                    let (ra, rb) = (find(&mut parent, fi), find(&mut parent, other));
                    if ra != rb {
                        parent[ra] = rb;
                    }
                }
                None => {
                    first.insert(key, fi);
                }
            }
        }
    }
    let mut sizes: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for fi in 0..tris.len() {
        let r = find(&mut parent, fi);
        *sizes.entry(r).or_insert(0) += 1;
    }
    sizes
        .values()
        .filter(|&&n| n as i64 >= min_faces)
        .sum()
}

/// `presets.resolve_mode_params` — the Stage 1 threshold pair and whether the
/// classifier-driven tuning was applied.
pub fn resolve_mode_params(
    loaded: &LoadedPolicy,
    mode: &str,
    mesh_type: &str,
    confidence: f64,
    profile: Option<&str>,
) -> ((i64, i64), bool) {
    let read_pair = |table: &std::collections::BTreeMap<String, crate::config::PolicyValue>,
                     key: &str|
     -> Option<(i64, i64)> {
        let entry = table.get(key)?;
        let (mut minc, mut maxh) = (None, None);
        if let crate::config::PolicyValue::Table(map) = entry {
            for (k, v) in map {
                let num = match v {
                    crate::config::PolicyValue::Int(i) => *i,
                    _ => continue,
                };
                match k.as_str() {
                    "mincomponentsize" => minc = Some(num),
                    "maxholesize" => maxh = Some(num),
                    _ => {}
                }
            }
        }
        Some((minc?, maxh?))
    };
    if mode != "auto" {
        if let Some(t) = loaded.get("modes") {
            if let crate::config::PolicyValue::Table(map) = t {
                if let Some(pair) = read_pair(map, mode) {
                    return (pair, false);
                }
            }
        }
    }
    if let Some(name) = profile {
        if let Some(t) = loaded.get("stage1_profiles") {
            if let crate::config::PolicyValue::Table(map) = t {
                if let Some(pair) = read_pair(map, name) {
                    return (pair, false);
                }
            }
        }
    }
    let applied = tuning_applied_for(loaded, mesh_type, confidence);
    let lookup = if applied { mesh_type } else { "unknown" };
    if let Some(t) = loaded.get("stage1_types") {
        if let crate::config::PolicyValue::Table(map) = t {
            for key in [lookup, "unknown"] {
                if let Some(pair) = read_pair(map, key) {
                    return (pair, applied);
                }
            }
        }
    }
    ((8, 1000), applied)
}

/// `validate.dry_run_mesh_from_arrays`: what a repair WOULD do, no output.
pub fn dry_run_mesh(
    mesh: &Mesh,
    loaded: &LoadedPolicy,
    mode: &str,
    profile: Option<&str>,
    engine: &str,
    declared_unit: Option<&str>,
    triage_spec: Option<&IntensitySpec>,
    si_mode: &str,
    flap: bool,
) -> Result<Value, String> {
    let v = &mesh.verts;
    let t = &mesh.tris;
    if t.is_empty() || v.is_empty() {
        return Err("input mesh is empty (no triangles)".into());
    }
    if v.iter().any(|p| !p.iter().all(|c| c.is_finite())) {
        return Err("input mesh contains NaN or infinite coordinates".into());
    }

    let cls = classify_mesh(&f64_verts(v), &usize_tris(t), engine);
    let reported_type = cls
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let reported_conf = cls
        .get("confidence")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let defects = detect_defects(v, t, false);
    let ((mut mincomp, mut maxholesize), tuning) =
        resolve_mode_params(loaded, mode, &reported_type, reported_conf, profile);
    let (_, max_loop_len) = boundary_loop_stats(v.len(), t);
    maxholesize = maxholesize.max(2 * max_loop_len as i64);

    let self_intersections: Option<i64> = if si_mode == "off" {
        None
    } else {
        let sv: Vec<[f64; 3]> = v
            .iter()
            .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
            .collect();
        let st: Vec<[usize; 3]> = t
            .iter()
            .map(|f| [f[0] as usize, f[1] as usize, f[2] as usize])
            .collect();
        let (_mask, count, skipped) = self_intersections_core(&sv, &st);
        if skipped || count < 0 {
            None
        } else {
            Some(count)
        }
    };

    let before_faces = t.len();
    let kept = faces_after_component_filter(t, mincomp);
    let debris_faces = before_faces.saturating_sub(kept);

    let bridge = true;
    let signals = json!({
        "detected_type": reported_type,
        "detected_confidence": reported_conf,
        "tuning_applied": tuning,
        "mode": mode,
        "holes": defects.holes.len(),
        "non_manifold": defects.non_manifold.len(),
        "self_intersections": self_intersections,
        "stage2_bridge_available": bridge,
    });
    let (mech_gate, org_gate) = tuning_gates(loaded);
    let est: Confidence = estimate_confidence_value(&signals, mech_gate, org_gate);
    let largest_hole = defects
        .holes
        .iter()
        .map(|h| h.diameter)
        .fold(0.0f64, f64::max);

    let mut result = serde_json::Map::new();
    result.insert("repair_mode".into(), json!(mode));
    result.insert(
        "repair_profile".into(),
        match profile {
            Some(p) => json!(p),
            None => Value::Null,
        },
    );
    result.insert("detected_type".into(), json!(reported_type));
    result.insert("detected_confidence".into(), json!(reported_conf));
    result.insert(
        "classifier_engine".into(),
        json!(if engine == "classic" { "classic" } else { "experimental" }),
    );
    result.insert("tuning_applied".into(), json!(tuning));
    result.insert(
        "would_apply".into(),
        json!({"mincomponentsize": mincomp, "maxholesize": maxholesize}),
    );
    result.insert("holes_found".into(), json!(defects.holes.len()));
    result.insert(
        "largest_hole_diameter".into(),
        json!(round_to(largest_hole, 4)),
    );
    result.insert(
        "non_manifold_regions".into(),
        json!(defects.non_manifold.len()),
    );
    result.insert("debris_faces_removable".into(), json!(debris_faces));
    result.insert(
        "self_intersections".into(),
        match self_intersections {
            Some(n) => json!(n),
            None => Value::Null,
        },
    );
    result.insert("connected_components".into(), json!(connected_components(t)));
    result.insert("stage2_bridge_available".into(), json!(bridge));
    result.insert("estimated_confidence".into(), json!(est.score));
    result.insert("estimated_confidence_label".into(), json!(est.label));
    result.insert(
        "estimated_confidence_factors".into(),
        Value::Object(est.factors),
    );
    result.insert("flap_step".into(), json!(flap));
    if let crate::config::PolicyValue::Table(fields) = triage_report_fields(triage_spec) {
        for (k, val) in fields {
            let text = crate::config::policy::json_python(&val);
            if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
                result.insert(k, parsed);
            }
        }
    }
    if let Some(unit) = check_units(mesh, declared_unit) {
        result.insert("unit_warning".into(), json!(true));
        result.insert("unit_hint".into(), json!(unit.unit_hint));
    }
    let _ = &mut mincomp;
    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A boundary vertex shared by four boundary edges makes the greedy loop
    /// walk pick a branch.  When the adjacency lists were filled by iterating a
    /// `HashMap` the branch changed per process, so the same mesh reported
    /// max_loop_len 150 / 154 / 157 and `would_apply.maxholesize` moved with it.
    /// The walk must follow `utils.boundary_loop_stats`: edges visited in
    /// ascending key order, loops entered in that same order.
    #[test]
    fn boundary_loop_stats_is_deterministic_across_figures() {
        // Two triangles sharing only vertex 1 leave that vertex with four
        // boundary edges (the branch point), plus a second separate strip.
        let tris: [[i32; 3]; 4] = [[0, 1, 2], [1, 3, 4], [5, 6, 7], [7, 8, 9]];
        let first = boundary_loop_stats(10, &tris);
        for _ in 0..64 {
            assert_eq!(boundary_loop_stats(10, &tris), first);
        }
        // Golden values from utils.boundary_loop_stats on the same triangles.
        assert_eq!(first, (6, 3));
    }

    /// `maxholesize` is `max(policy, 2 * max_loop_len)`; a loop walk that
    /// returns too short a loop silently lands the plan below the policy floor.
    #[test]
    fn maxholesize_uses_the_doubled_longest_loop() {
        let tris: [[i32; 3]; 2] = [[0, 1, 2], [2, 1, 3]];
        let (loops, max_loop) = boundary_loop_stats(4, &tris);
        assert_eq!((loops, max_loop), (1, 4));
        assert_eq!((2 * max_loop) as i64, 8);
    }
}
