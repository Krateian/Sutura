//! Shared helpers for the ported repair tiers.
//!
//! These mirror the small pure-numpy helpers the Python tier code uses
//! (`pipeline/utils.py`, `pipeline/adopt_guard.py`, `topology.py`).  The
//! numeric paths deliberately keep the Python cast order (float32 surface
//! area, float64 volume/perimeter) so a tier decision does not move.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use serde_json::Value;
use sutura_geom::antipodal::AntipodalGwn;
use sutura_geom::winding::MeshBvh;

pub fn to_usize_tris(t: &[[i64; 3]]) -> Vec<[usize; 3]> {
    t.iter()
        .map(|r| [r[0] as usize, r[1] as usize, r[2] as usize])
        .collect()
}

pub fn to_i64_tris(t: &[[u32; 3]]) -> Vec<[i64; 3]> {
    t.iter()
        .map(|r| [r[0] as i64, r[1] as i64, r[2] as i64])
        .collect()
}

pub fn to_f64_verts(v: &[[f32; 3]]) -> Vec<[f64; 3]> {
    v.iter()
        .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
        .collect()
}

fn norm3(a: [f64; 3], b: [f64; 3]) -> f64 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// Vertex-connected component count over referenced vertices.
///
/// Python's `_count_vertex_components` unions the three vertices of each
/// triangle.  The root choice does not matter for the count, only the sets.
pub fn count_vertex_components(n_verts: usize, tris: &[[i64; 3]]) -> usize {
    if tris.is_empty() || n_verts == 0 {
        return 0;
    }
    let mut parent: Vec<usize> = (0..n_verts).collect();
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
    for t in tris {
        let a = t[0] as usize;
        let ra = find(&mut parent, a);
        for &v in &t[1..] {
            let rb = find(&mut parent, v as usize);
            if rb != ra {
                parent[rb] = ra;
            }
        }
    }
    let mut used = vec![false; n_verts];
    for t in tris {
        for &v in t {
            used[v as usize] = true;
        }
    }
    let mut roots = HashSet::new();
    for i in 0..n_verts {
        if used[i] {
            roots.insert(find(&mut parent, i));
        }
    }
    roots.len()
}

/// Node labels with SciPy's convention: component 0 contains node 0, etc.
pub fn component_labels(n_nodes: usize, tris: &[[i64; 3]]) -> (usize, Vec<i64>) {
    if n_nodes == 0 {
        return (0, Vec::new());
    }
    let mut parent: Vec<usize> = (0..n_nodes).collect();
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
    for t in tris {
        let a = t[0] as usize;
        let b = t[1] as usize;
        let c = t[2] as usize;
        let ra = find(&mut parent, a);
        let rb = find(&mut parent, b);
        if ra < rb {
            parent[rb] = ra;
        } else if rb < ra {
            parent[ra] = rb;
        }
        let ra = find(&mut parent, a);
        let rc = find(&mut parent, c);
        if ra < rc {
            parent[rc] = ra;
        } else if rc < ra {
            parent[ra] = rc;
        }
    }
    let roots: Vec<i64> = (0..n_nodes).map(|i| find(&mut parent, i) as i64).collect();
    let mut uniq: Vec<i64> = roots.clone();
    uniq.sort_unstable();
    uniq.dedup();
    let mut map: HashMap<i64, i64> = HashMap::with_capacity(uniq.len());
    for (i, r) in uniq.iter().enumerate() {
        map.insert(*r, i as i64);
    }
    let labels: Vec<i64> = roots.iter().map(|r| map[r]).collect();
    (uniq.len(), labels)
}

/// Total surface area, cast and accumulated like the Python helper (float32
/// per-face area, `float(np.sum(...))`).
pub fn surface_area(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> f64 {
    if tris.is_empty() || verts.is_empty() {
        return 0.0;
    }
    let mut total = 0.0f64;
    for t in tris {
        let a = verts[t[0] as usize];
        let b = verts[t[1] as usize];
        let c = verts[t[2] as usize];
        let (ax, ay, az) = (a[0] as f32, a[1] as f32, a[2] as f32);
        let (bx, by, bz) = (b[0] as f32, b[1] as f32, b[2] as f32);
        let (cx, cy, cz) = (c[0] as f32, c[1] as f32, c[2] as f32);
        let ux = bx - ax;
        let uy = by - ay;
        let uz = bz - az;
        let vx = cx - ax;
        let vy = cy - ay;
        let vz = cz - az;
        let nx = uy * vz - uz * vy;
        let ny = uz * vx - ux * vz;
        let nz = ux * vy - uy * vx;
        let len = (nx * nx + ny * ny + nz * nz).sqrt();
        total += (0.5f32 * len) as f64;
    }
    total
}

/// Signed volume (origin tetrahedra), float64.
pub fn signed_volume(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> f64 {
    if tris.is_empty() || verts.is_empty() {
        return 0.0;
    }
    let mut acc = 0.0f64;
    for t in tris {
        let a = verts[t[0] as usize];
        let b = verts[t[1] as usize];
        let c = verts[t[2] as usize];
        let cx = b[1] * c[2] - b[2] * c[1];
        let cy = b[2] * c[0] - b[0] * c[2];
        let cz = b[0] * c[1] - b[1] * c[0];
        acc += a[0] * cx + a[1] * cy + a[2] * cz;
    }
    acc / 6.0
}

pub fn bbox_diagonal(verts: &[[f64; 3]]) -> f64 {
    if verts.is_empty() {
        return 0.0;
    }
    let mut lo = verts[0];
    let mut hi = verts[0];
    for p in verts {
        for k in 0..3 {
            if p[k] < lo[k] {
                lo[k] = p[k];
            }
            if p[k] > hi[k] {
                hi[k] = p[k];
            }
        }
    }
    let dx = hi[0] - lo[0];
    let dy = hi[1] - lo[1];
    let dz = hi[2] - lo[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// Total length of boundary edges (edges used by exactly one face).
pub fn boundary_perimeter(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> f64 {
    if tris.is_empty() || verts.is_empty() {
        return 0.0;
    }
    let mut counts: HashMap<(i64, i64), usize> = HashMap::new();
    for t in tris {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let k = if a <= b { (a, b) } else { (b, a) };
            *counts.entry(k).or_insert(0) += 1;
        }
    }
    let mut total = 0.0f64;
    for ((a, b), c) in counts {
        if c == 1 {
            total += norm3(verts[a as usize], verts[b as usize]);
        }
    }
    total
}

/// Area spanned by an ordered boundary loop (`_loop_spanned_area`).
pub fn loop_spanned_area(verts: &[[f64; 3]], loop_idx: &[i64]) -> f64 {
    if loop_idx.len() < 3 {
        return 0.0;
    }
    let n = loop_idx.len();
    let mut nx = 0.0f64;
    let mut ny = 0.0f64;
    let mut nz = 0.0f64;
    for i in 0..n {
        let p = verts[loop_idx[i] as usize];
        let q = verts[loop_idx[(i + 1) % n] as usize];
        nx += (p[1] - q[1]) * (p[2] + q[2]);
        ny += (p[2] - q[2]) * (p[0] + q[0]);
        nz += (p[0] - q[0]) * (p[1] + q[1]);
    }
    0.5 * (nx * nx + ny * ny + nz * nz).sqrt()
}

pub fn exact_si_count(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> Option<i64> {
    if tris.is_empty() {
        return Some(0);
    }
    let ut = to_usize_tris(tris);
    let (_mask, count, _skipped) = sutura_geom::self_intersections_core(verts, &ut);
    if count < 0 {
        None
    } else {
        Some(count)
    }
}

pub fn si_subset_count(verts: &[[f64; 3]], tris: &[[i64; 3]], subset: &[bool]) -> i64 {
    let ut = to_usize_tris(tris);
    sutura_geom::self_intersections_subset_core(verts, &ut, subset)
}

/// Python `_canonical_face_keys`: float32 coordinates, vertices sorted by
/// (x, y, z), serialized little-endian into 36 bytes.
pub fn canonical_face_keys(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> Vec<[u8; 36]> {
    let mut out = Vec::with_capacity(tris.len());
    for t in tris {
        let mut vs: Vec<[f32; 3]> = t
            .iter()
            .map(|&i| {
                let p = verts[i as usize];
                [p[0] as f32, p[1] as f32, p[2] as f32]
            })
            .collect();
        vs.sort_by(|a, b| cmp_v3(a, b));
        let mut key = [0u8; 36];
        for (i, p) in vs.iter().enumerate() {
            key[i * 12..i * 12 + 4].copy_from_slice(&p[0].to_le_bytes());
            key[i * 12 + 4..i * 12 + 8].copy_from_slice(&p[1].to_le_bytes());
            key[i * 12 + 8..i * 12 + 12].copy_from_slice(&p[2].to_le_bytes());
        }
        out.push(key);
    }
    out
}

fn cmp_v3(a: &[f32; 3], b: &[f32; 3]) -> Ordering {
    for k in 0..3 {
        let o = a[k].partial_cmp(&b[k]).unwrap_or(Ordering::Equal);
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

/// Median triangle edge length over all three edges of every face.
pub fn median_edge_length(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> f64 {
    if tris.is_empty() || verts.is_empty() {
        return 0.0;
    }
    let mut lengths = Vec::with_capacity(tris.len() * 3);
    for t in tris {
        let a = verts[t[0] as usize];
        let b = verts[t[1] as usize];
        let c = verts[t[2] as usize];
        lengths.push(norm3(a, b));
        lengths.push(norm3(b, c));
        lengths.push(norm3(c, a));
    }
    if lengths.is_empty() {
        return 0.0;
    }
    lengths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let n = lengths.len();
    if n % 2 == 1 {
        lengths[n / 2]
    } else {
        0.5 * (lengths[n / 2 - 1] + lengths[n / 2])
    }
}

pub fn weld_reload_equivalent(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
) -> Result<(Vec<[f32; 3]>, Vec<[i64; 3]>), String> {
    crate::p_weld::weld_reload_equivalent(verts, tris)
}

pub fn reload_strict_holes_nm(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> (usize, usize) {
    crate::p_weld::reload_strict_holes_nm(verts, tris)
}

pub fn closest_points(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    points: &[[f64; 3]],
) -> Vec<([f64; 3], f64, i64)> {
    if verts.is_empty() || tris.is_empty() || points.is_empty() {
        return Vec::new();
    }
    let ut = to_usize_tris(tris);
    let bvh = MeshBvh::from_arrays(verts, &ut);
    points
        .iter()
        .map(|p| {
            let (q, d, f) = bvh.closest_point(*p);
            (q, d, if f == usize::MAX { -1 } else { f as i64 })
        })
        .collect()
}

pub fn winding_points(verts: &[[f64; 3]], tris: &[[i64; 3]], points: &[[f64; 3]]) -> Vec<f64> {
    if verts.is_empty() || tris.is_empty() || points.is_empty() {
        return Vec::new();
    }
    let ut = to_usize_tris(tris);
    let bvh = MeshBvh::from_arrays(verts, &ut);
    bvh.winding_points_par(points)
}

pub fn antipodal_winding(verts: &[[f64; 3]], tris: &[[i64; 3]], points: &[[f64; 3]]) -> Vec<f64> {
    if verts.is_empty() || tris.is_empty() || points.is_empty() {
        return Vec::new();
    }
    let ut = to_usize_tris(tris);
    let gwn = AntipodalGwn::build(verts, &ut);
    gwn.winding_points_par(points)
}

#[derive(Clone, Debug, Default)]
pub struct GuardPolicy {
    pub face_ratio: f64,
    pub vertex_ratio: f64,
    pub min_slack: i64,
    pub loop_face_slack: i64,
    pub loop_vertex_slack: i64,
    pub si_policy: String,
    pub si_rel_tol: f64,
    pub si_abs_tol: i64,
    pub face_abs_floor: i64,
    pub vertex_abs_floor: i64,
    pub allow_growth_if_improved: bool,
}

pub fn blowup_reason(
    input_faces: usize,
    input_vertices: usize,
    candidate_faces: usize,
    candidate_vertices: usize,
    slack_units: i64,
    policy: &GuardPolicy,
) -> Option<String> {
    let units = slack_units.max(0);
    let face_slack = policy.min_slack + policy.loop_face_slack * units;
    let allowed_faces = ((policy.face_ratio * input_faces as f64) as i64)
        .max(input_faces as i64 + face_slack)
        .max(policy.face_abs_floor);
    if candidate_faces as i64 > allowed_faces {
        return Some(format!(
            "candidate face blow-up (faces {} -> {}; allowed {} = max({} * input, input + {}))",
            input_faces, candidate_faces, allowed_faces, policy.face_ratio, face_slack
        ));
    }
    let vertex_slack = policy.min_slack + policy.loop_vertex_slack * units;
    let allowed_vertices = ((policy.vertex_ratio * input_vertices as f64) as i64)
        .max(input_vertices as i64 + vertex_slack)
        .max(policy.vertex_abs_floor);
    if candidate_vertices as i64 > allowed_vertices {
        return Some(format!(
            "candidate vertex blow-up (vertices {} -> {}; allowed {} = max({} * input, input + {}))",
            input_vertices, candidate_vertices, allowed_vertices, policy.vertex_ratio,
            vertex_slack
        ));
    }
    None
}

pub fn si_reason(
    base_si: Option<i64>,
    candidate_si: Option<i64>,
    policy: &GuardPolicy,
    improved_topology: bool,
) -> Option<String> {
    if policy.si_policy == "off" || policy.si_policy == "flag" {
        return None;
    }
    let (b, c) = match (base_si, candidate_si) {
        (Some(b), Some(c)) => (b, c),
        _ => return None,
    };
    if policy.si_policy == "strict_zero" {
        if c > 0 {
            return Some(format!("candidate has self-intersections ({})", c));
        }
        return None;
    }
    if policy.si_policy == "improved_flag" && improved_topology {
        return None;
    }
    let allowed = b + (policy.si_abs_tol.max((policy.si_rel_tol * b as f64) as i64));
    if c > allowed {
        return Some(format!(
            "candidate increases self-intersections ({} -> {}; allowed {})",
            b, c, allowed
        ));
    }
    None
}

/// The `check_candidate` result fields (Python copies them into the tier
/// record only when the key already exists).
pub fn guard_fields(
    input_faces: usize,
    input_vertices: usize,
    candidate_faces: usize,
    candidate_vertices: usize,
    slack_units: i64,
    base_si: Option<i64>,
    candidate_si: Option<i64>,
    policy: &GuardPolicy,
    improved_topology: bool,
) -> serde_json::Map<String, Value> {
    let mut fields = serde_json::Map::new();
    fields.insert("input_faces".into(), Value::from(input_faces as i64));
    fields.insert("input_vertices".into(), Value::from(input_vertices as i64));
    fields.insert(
        "candidate_faces".into(),
        Value::from(candidate_faces as i64),
    );
    fields.insert(
        "candidate_vertices".into(),
        Value::from(candidate_vertices as i64),
    );
    fields.insert("slack_units".into(), Value::from(slack_units));
    fields.insert("guard_rejected".into(), Value::Null);
    fields.insert("guard_reason".into(), Value::Null);
    fields.insert("si_policy".into(), Value::from(policy.si_policy.clone()));
    fields.insert("si_checked".into(), Value::Bool(false));
    fields.insert("si_flagged".into(), Value::Bool(false));
    fields.insert(
        "baseline_self_intersections".into(),
        base_si.map(Value::from).unwrap_or(Value::Null),
    );
    fields.insert(
        "candidate_self_intersections".into(),
        candidate_si.map(Value::from).unwrap_or(Value::Null),
    );
    fields.insert(
        "si_improved_topology".into(),
        Value::Bool(improved_topology),
    );
    fields.insert("blowup_flagged".into(), Value::Bool(false));

    if let Some(reason) = blowup_reason(
        input_faces,
        input_vertices,
        candidate_faces,
        candidate_vertices,
        slack_units,
        policy,
    ) {
        if policy.allow_growth_if_improved && improved_topology {
            fields.insert("blowup_flagged".into(), Value::Bool(true));
            fields.insert(
                "guard_reason".into(),
                Value::from(format!("{} (flagged: holes/non-manifold improved)", reason)),
            );
        } else {
            fields.insert("guard_rejected".into(), Value::from(reason.clone()));
            fields.insert("guard_reason".into(), Value::from(reason));
            fields.insert("guard_ok".into(), Value::Bool(false));
            return fields;
        }
    }

    fields.insert(
        "si_checked".into(),
        Value::Bool(base_si.is_some() && candidate_si.is_some()),
    );
    if let Some(reason) = si_reason(base_si, candidate_si, policy, improved_topology) {
        fields.insert("guard_rejected".into(), Value::from(reason.clone()));
        fields.insert("guard_reason".into(), Value::from(reason));
        fields.insert("guard_ok".into(), Value::Bool(false));
        return fields;
    }
    if let (Some(b), Some(c)) = (base_si, candidate_si) {
        if c > b && (policy.si_policy == "flag" || policy.si_policy == "improved_flag") {
            fields.insert("si_flagged".into(), Value::Bool(true));
            fields.insert(
                "guard_reason".into(),
                Value::from(format!("self-intersections flagged ({} -> {})", b, c)),
            );
        }
    }
    fields.insert("guard_ok".into(), Value::Bool(true));
    fields
}

pub fn policy_from_json(v: &Value) -> GuardPolicy {
    let get_f = |k: &str, d: f64| v.get(k).and_then(Value::as_f64).unwrap_or(d);
    let get_i = |k: &str, d: i64| v.get(k).and_then(Value::as_i64).unwrap_or(d);
    let get_b = |k: &str, d: bool| v.get(k).and_then(Value::as_bool).unwrap_or(d);
    GuardPolicy {
        face_ratio: get_f("face_ratio", 4.0),
        vertex_ratio: get_f("vertex_ratio", 4.0),
        min_slack: get_i("min_slack", 256),
        loop_face_slack: get_i("loop_face_slack", 8),
        loop_vertex_slack: get_i("loop_vertex_slack", 1),
        si_policy: v
            .get("si_policy")
            .and_then(Value::as_str)
            .unwrap_or("flag")
            .to_string(),
        si_rel_tol: get_f("si_rel_tol", 0.0),
        si_abs_tol: get_i("si_abs_tol", 0),
        face_abs_floor: get_i("face_abs_floor", 0),
        vertex_abs_floor: get_i("vertex_abs_floor", 0),
        allow_growth_if_improved: get_b("allow_growth_if_improved", false),
    }
}

pub fn json_f(v: &Value, k: &str, d: f64) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(d)
}

pub fn json_i(v: &Value, k: &str, d: i64) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(d)
}

pub fn json_u(v: &Value, k: &str, d: usize) -> usize {
    v.get(k)
        .and_then(Value::as_u64)
        .map(|x| x as usize)
        .unwrap_or(d)
}

pub fn json_b(v: &Value, k: &str, d: bool) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(d)
}

pub fn json_s(v: &Value, k: &str, d: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or(d).to_string()
}

pub fn json_opt_i(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(Value::as_i64)
}
