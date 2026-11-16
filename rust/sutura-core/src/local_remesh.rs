// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Local re-mesh deep-repair tier, ported from
//! `sutura_engine/pipeline/tiers/local_remesh.py`.
//!
//! The tier deletes the damaged region, repairs pinch vertices, closes the
//! resulting holes and moves only the vertices the closer appended.  It is
//! adopted only when every face outside the region is preserved verbatim and
//! holes / non-manifold edges / exact self-intersections do not get worse.
//! The work budget is a pure function of the input (P10 phase 4), so the same
//! mesh always takes the same path.

use std::collections::HashMap;
use std::time::Instant;

use serde_json::{json, Map, Value};
use sutura_geom::hole::close_holes_core;
use sutura_geom::topology::repair_non_manifold_vertices_core;

use crate::graft::helpers::{damaged_region, referenced_only};
use crate::proxy::{surface_samples, HAUSDORFF_SEED};
use crate::tiers::common::{self, TopologySummary};

pub const LOCAL_MAX_NM_EDGES: usize = 0;
pub const LOCAL_MAX_LOOP_LEN: usize = 16;
pub const LOCAL_MAX_REGIONS: usize = 8;
pub const LOCAL_MAX_REMOVED_FACES: usize = 2000;
pub const LOCAL_REMESH_MAX_HOLE: i64 = 5000;
pub const LOCAL_REMESH_FAIR_STEPS: usize = 3;
pub const LOCAL_MAX_WORK: i64 = 10_000_000;

/// `_hausdorff_rel`'s default sample count (`HAUSDORFF_SAMPLES` from the
/// Balanced intensity spec) and fixed seed.
const LOCAL_HAUSDORFF_SAMPLES: usize = 200_000;

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn round6(x: f64) -> f64 {
    (x * 1_000_000.0).round() / 1_000_000.0
}

fn loop_stats(t: &[[i64; 3]]) -> (usize, usize) {
    crate::repair::boundary_loop_stats(t)
}

/// Umbrella-operator smoothing: every vertex with index `>= first_free` moves
/// to the mean of its 1-ring neighbours.  The accumulation follows Python's
/// `np.add.at(acc, src, out[dst])` order exactly (column-major concatenation
/// `[t0, t1, t2, t1, t2, t0]`), so a multi-step run is bit-reproducible.
fn umbrella_fair(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    first_free: usize,
    steps: usize,
) -> Vec<[f64; 3]> {
    let n = verts.len();
    if first_free >= n || steps == 0 {
        return verts.to_vec();
    }
    let mut deg = vec![0usize; n];
    for j in 0..3 {
        for r in tris.iter() {
            deg[r[j] as usize] += 1;
            deg[r[(j + 1) % 3] as usize] += 1;
        }
    }
    let mut free = vec![false; n];
    for i in first_free..n {
        free[i] = deg[i] > 0;
    }
    let mut out = verts.to_vec();
    for _ in 0..steps {
        let mut acc = vec![[0.0f64; 3]; n];
        for j in 0..3 {
            for r in tris.iter() {
                let src = r[j] as usize;
                let dst = r[(j + 1) % 3] as usize;
                let p = out[dst];
                acc[src][0] += p[0];
                acc[src][1] += p[1];
                acc[src][2] += p[2];
            }
        }
        for j in 0..3 {
            for r in tris.iter() {
                let src = r[(j + 1) % 3] as usize;
                let dst = r[j] as usize;
                let p = out[dst];
                acc[src][0] += p[0];
                acc[src][1] += p[1];
                acc[src][2] += p[2];
            }
        }
        for i in 0..n {
            if free[i] {
                let d = deg[i] as f64;
                out[i] = [acc[i][0] / d, acc[i][1] / d, acc[i][2] / d];
            }
        }
    }
    out
}

fn face_point(p: [f64; 3]) -> [u32; 3] {
    [
        (p[0] as f32).to_bits(),
        (p[1] as f32).to_bits(),
        (p[2] as f32).to_bits(),
    ]
}

fn canonical_rotation(points: [[u32; 3]; 3]) -> [[u32; 3]; 3] {
    let mut best = points;
    for k in 1..3 {
        let r = [points[k % 3], points[(k + 1) % 3], points[(k + 2) % 3]];
        if r < best {
            best = r;
        }
    }
    best
}

fn face_keys(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> Vec<[[u32; 3]; 3]> {
    tris.iter()
        .map(|r| {
            canonical_rotation([
                face_point(verts[r[0] as usize]),
                face_point(verts[r[1] as usize]),
                face_point(verts[r[2] as usize]),
            ])
        })
        .collect()
}

/// Python `_faces_preserved`: every base face occurs in the candidate with
/// bit-identical float32 coordinates and the same orientation, counted with
/// multiplicity and independent of the starting corner.
fn faces_preserved(
    base_v: &[[f64; 3]],
    base_t: &[[i64; 3]],
    cand_v: &[[f64; 3]],
    cand_t: &[[i64; 3]],
) -> bool {
    if base_t.is_empty() {
        return true;
    }
    let mut available: HashMap<[[u32; 3]; 3], usize> = HashMap::new();
    for key in face_keys(cand_v, cand_t) {
        *available.entry(key).or_insert(0) += 1;
    }
    let mut needed: HashMap<[[u32; 3]; 3], usize> = HashMap::new();
    for key in face_keys(base_v, base_t) {
        *needed.entry(key).or_insert(0) += 1;
    }
    needed
        .iter()
        .all(|(key, count)| available.get(key).copied().unwrap_or(0) >= *count)
}

/// One-sided Hausdorff distance from the candidate to the input, relative to
/// the input bbox diagonal.  Only the maximum is recorded (the Python tier
/// takes `[0]`).
pub(crate) fn hausdorff_rel(
    in_v: &[[f64; 3]],
    in_t: &[[i64; 3]],
    out_v: &[[f64; 3]],
    out_t: &[[i64; 3]],
) -> Option<f64> {
    if in_t.is_empty() || out_t.is_empty() {
        return None;
    }
    let (in_v, in_t) = referenced_only(in_v, in_t);
    let (out_v, out_t) = referenced_only(out_v, out_t);
    if in_v.is_empty() || out_v.is_empty() {
        return None;
    }
    let diag = common::bbox_diagonal(&in_v);
    if diag == 0.0 {
        return None;
    }
    let pts = surface_samples(&out_v, &out_t, LOCAL_HAUSDORFF_SAMPLES, HAUSDORFF_SEED);
    if pts.is_empty() {
        return None;
    }
    let closest = common::closest_points(&in_v, &in_t, &pts);
    if closest.is_empty() {
        return None;
    }
    let max = closest.iter().fold(f64::NEG_INFINITY, |m, x| m.max(x.1));
    Some(round6(max / diag))
}

/// Run the local re-mesh tier.
///
/// Returns `(verts, tris, after, report)`.  A skip or reject returns the input
/// arrays and topology unchanged, exactly like `_local_remesh_tier`.
pub fn local_remesh(
    v: &[[f64; 3]],
    t: &[[i64; 3]],
    after: &TopologySummary,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, TopologySummary, Value) {
    local_remesh_with_budget(v, t, after, LOCAL_MAX_WORK)
}

fn local_remesh_with_budget(
    v: &[[f64; 3]],
    t: &[[i64; 3]],
    after: &TopologySummary,
    work_budget: i64,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, TopologySummary, Value) {
    let t0 = Instant::now();
    let base_nm = after.non_two_manifold_edges;
    let (base_holes, base_loop) = loop_stats(t);
    let mut rep = Map::new();
    rep.insert("ran".into(), json!(true));
    rep.insert("adopted".into(), json!(false));
    rep.insert("outside_unchanged".into(), json!(false));
    rep.insert("holes_before".into(), json!(base_holes));
    rep.insert("nm_before".into(), json!(base_nm));
    rep.insert("max_loop_len".into(), json!(base_loop));

    macro_rules! skip {
        ($reason:expr) => {{
            rep.insert("reject_reason".into(), Value::from($reason));
            rep.insert("time".into(), json!(round3(t0.elapsed().as_secs_f64())));
            return (v.to_vec(), t.to_vec(), *after, Value::Object(rep));
        }};
    }

    if base_nm > LOCAL_MAX_NM_EDGES {
        skip!("scope: non-manifold edges");
    }
    if base_loop > LOCAL_MAX_LOOP_LEN {
        skip!("scope: boundary loop too long");
    }

    let (region, n_regions_full) = damaged_region(t);
    let faces_removed = region.iter().filter(|x| **x).count();
    rep.insert("faces_removed".into(), json!(faces_removed));
    if faces_removed > LOCAL_MAX_REMOVED_FACES {
        skip!("scope: region too large");
    }
    let n_regions = n_regions_full;
    rep.insert("regions".into(), json!(n_regions));
    if n_regions == 0 {
        skip!("no damaged region");
    }
    if n_regions > LOCAL_MAX_REGIONS {
        skip!("scope: too many regions");
    }

    let kept: Vec<[i64; 3]> = t
        .iter()
        .enumerate()
        .filter(|(i, _)| !region[*i])
        .map(|(_, r)| *r)
        .collect();
    if kept.is_empty() {
        skip!("region covers the whole mesh");
    }

    let (kv, kt) = referenced_only(v, &kept);

    let kv32: Vec<[f32; 3]> = kv
        .iter()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    let (nv32, mut nt) = repair_non_manifold_vertices_core(&kv32, &kt);
    let nv0 = nv32.len();
    let mut nv: Vec<[f64; 3]> = nv32
        .iter()
        .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
        .collect();
    let n0 = nt.len();

    match close_holes_core(&nv, &nt, LOCAL_REMESH_MAX_HOLE, true) {
        Ok(report) => {
            nv = report.verts;
            nt = report.tris;
        }
        Err(message) => {
            rep.insert("error".into(), Value::from(message));
            rep.insert("time".into(), json!(round3(t0.elapsed().as_secs_f64())));
            return (v.to_vec(), t.to_vec(), *after, Value::Object(rep));
        }
    }

    rep.insert("faces_added".into(), json!((nt.len() as i64) - (n0 as i64)));
    let work_units = faces_removed as i64 + nt.len() as i64 * (LOCAL_REMESH_FAIR_STEPS as i64 + 1);
    rep.insert("work_units".into(), json!(work_units));
    rep.insert("work_budget".into(), json!(work_budget));
    if work_units > work_budget {
        skip!("work budget");
    }

    nv = umbrella_fair(&nv, &nt, nv0, LOCAL_REMESH_FAIR_STEPS);
    rep.insert("fairing_iters".into(), json!(LOCAL_REMESH_FAIR_STEPS));

    let t_after = common::topology_summary(&nv, &nt);
    let (holes, _) = loop_stats(&nt);
    let nm = t_after.non_two_manifold_edges;
    rep.insert("holes_after".into(), json!(holes));
    rep.insert("nm_after".into(), json!(nm));

    let outside_ok = faces_preserved(&kv, &kt, &nv, &nt);
    rep.insert("outside_unchanged".into(), json!(outside_ok));
    let better = holes <= base_holes && nm <= base_nm && (holes, nm) != (base_holes, base_nm);

    let mut si_ok = true;
    if outside_ok && better {
        let si_work = work_units + nt.len() as i64 + t.len() as i64;
        if si_work > work_budget {
            rep.insert("work_units".into(), json!(si_work));
            skip!("work budget");
        }
        rep.insert("work_units".into(), json!(si_work));
        let ut: Vec<[usize; 3]> = common::to_usize_tris(t);
        let un: Vec<[usize; 3]> = common::to_usize_tris(&nt);
        let (_, base_si, _) = sutura_geom::self_intersections_core(v, &ut);
        let (_, new_si, _) = sutura_geom::self_intersections_core(&nv, &un);
        rep.insert("si_before".into(), json!(base_si));
        rep.insert("si_after".into(), json!(new_si));
        si_ok = new_si <= base_si;
    }

    if !outside_ok {
        rep.insert(
            "reject_reason".into(),
            Value::from("faces outside the region changed"),
        );
    } else if !better {
        rep.insert(
            "reject_reason".into(),
            Value::from("no improvement on holes/non-manifold edges"),
        );
    } else if !si_ok {
        rep.insert(
            "reject_reason".into(),
            Value::from("more self-intersections"),
        );
    } else {
        rep.insert(
            "hausdorff_rel".into(),
            match hausdorff_rel(v, t, &nv, &nt) {
                Some(x) => json!(x),
                None => Value::Null,
            },
        );
        rep.insert("adopted".into(), json!(true));
        rep.insert("time".into(), json!(round3(t0.elapsed().as_secs_f64())));
        return (nv, nt, t_after, Value::Object(rep));
    }

    rep.insert("time".into(), json!(round3(t0.elapsed().as_secs_f64())));
    (v.to_vec(), t.to_vec(), *after, Value::Object(rep))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Closed cube surface with every face split into `n` x `n` quads.
    /// Vertices are deduplicated on the integer lattice, so the surface is a
    /// single closed 2-manifold.
    fn subdivided_cube(n: i64) -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
        let nf = n as f64;
        let mut verts: Vec<[f64; 3]> = Vec::new();
        let mut index: HashMap<[i64; 3], i64> = HashMap::new();
        let mut tris: Vec<[i64; 3]> = Vec::new();
        let faces = [
            ((0, 0, 0), (0, 0, 1), (0, 1, 0)),
            ((n, 0, 0), (0, 1, 0), (0, 0, 1)),
            ((0, 0, 0), (1, 0, 0), (0, 0, 1)),
            ((0, n, 0), (0, 0, 1), (1, 0, 0)),
            ((0, 0, 0), (0, 1, 0), (1, 0, 0)),
            ((0, 0, n), (1, 0, 0), (0, 1, 0)),
        ];
        for (origin, u, v) in faces {
            let mut grid: Vec<Vec<i64>> = Vec::new();
            for i in 0..=n {
                let mut row = Vec::new();
                for j in 0..=n {
                    let key = [
                        origin.0 + u.0 * i + v.0 * j,
                        origin.1 + u.1 * i + v.1 * j,
                        origin.2 + u.2 * i + v.2 * j,
                    ];
                    let id = *index.entry(key).or_insert_with(|| {
                        let id = verts.len() as i64;
                        verts.push([key[0] as f64 / nf, key[1] as f64 / nf, key[2] as f64 / nf]);
                        id
                    });
                    row.push(id);
                }
                grid.push(row);
            }
            for i in 0..n as usize {
                for j in 0..n as usize {
                    let a = grid[i][j];
                    let b = grid[i + 1][j];
                    let c = grid[i + 1][j + 1];
                    let d = grid[i][j + 1];
                    tris.push([a, b, c]);
                    tris.push([a, c, d]);
                }
            }
        }
        (verts, tris)
    }

    fn cube_with_patch_hole() -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
        let (verts, tris) = subdivided_cube(6);
        // Remove one triangle in the middle of the z = 1 face, well away from
        // the face border, so the one-ring damage stays local.
        let hole = tris
            .iter()
            .position(|r| {
                let c = [
                    (verts[r[0] as usize][0] + verts[r[1] as usize][0] + verts[r[2] as usize][0])
                        / 3.0,
                    (verts[r[0] as usize][1] + verts[r[1] as usize][1] + verts[r[2] as usize][1])
                        / 3.0,
                    (verts[r[0] as usize][2] + verts[r[1] as usize][2] + verts[r[2] as usize][2])
                        / 3.0,
                ];
                c[2] > 0.999 && (c[0] - 0.5).abs() < 0.1 && (c[1] - 0.5).abs() < 0.1
            })
            .expect("central cap triangle");
        let mut open = tris;
        open.remove(hole);
        (verts, open)
    }

    fn watertight(t: &[[i64; 3]]) -> bool {
        loop_stats(t) == (0, 0) && common::non_two_manifold_edges(t) == 0
    }

    #[test]
    fn damaged_patch_on_an_open_cube_closes() {
        let (v, t) = cube_with_patch_hole();
        let after = common::topology_summary(&v, &t);
        let (_ov, ot, after2, rep) = local_remesh(&v, &t, &after);
        assert_eq!(rep.get("ran").and_then(Value::as_bool), Some(true));
        assert_eq!(
            rep.get("adopted").and_then(Value::as_bool),
            Some(true),
            "{rep:?}"
        );
        assert!(
            rep.get("outside_unchanged").and_then(Value::as_bool) == Some(true),
            "{rep:?}"
        );
        assert_eq!(rep.get("holes_after").and_then(Value::as_i64), Some(0));
        assert_eq!(rep.get("nm_after").and_then(Value::as_i64), Some(0));
        assert!(watertight(&ot), "candidate is not watertight");
        assert_eq!(after2.non_two_manifold_edges, 0);
        assert_eq!(loop_stats(&ot).0, 0);
    }

    #[test]
    fn work_budget_below_cost_is_a_noop() {
        let (v, t) = cube_with_patch_hole();
        let after = common::topology_summary(&v, &t);
        let (ov, ot, after2, rep) = local_remesh_with_budget(&v, &t, &after, 1);
        assert_eq!(ov, v);
        assert_eq!(ot, t);
        assert_eq!(after2.boundary_edges, after.boundary_edges);
        assert_eq!(rep.get("adopted").and_then(Value::as_bool), Some(false));
        assert_eq!(
            rep.get("reject_reason").and_then(Value::as_str),
            Some("work budget")
        );
        let units = rep.get("work_units").and_then(Value::as_i64).unwrap();
        assert!(units > 1);
    }

    #[test]
    fn two_runs_are_array_equal() {
        let (v, t) = cube_with_patch_hole();
        let after = common::topology_summary(&v, &t);
        let (av, at, _, arep) = local_remesh(&v, &t, &after);
        let (bv, bt, _, brep) = local_remesh(&v, &t, &after);
        assert_eq!(av, bv);
        assert_eq!(at, bt);
        for key in [
            "adopted",
            "outside_unchanged",
            "holes_before",
            "nm_before",
            "max_loop_len",
            "faces_removed",
            "regions",
            "faces_added",
            "work_units",
            "work_budget",
            "fairing_iters",
            "holes_after",
            "nm_after",
            "si_before",
            "si_after",
            "hausdorff_rel",
            "reject_reason",
        ] {
            assert_eq!(
                arep.get(key).cloned().unwrap_or(Value::Null),
                brep.get(key).cloned().unwrap_or(Value::Null),
                "{key}"
            );
        }
    }
}
