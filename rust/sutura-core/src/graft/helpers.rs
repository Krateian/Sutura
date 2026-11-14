//! Pure Graft helpers (`_auto_voxel`, `_gap_estimate`, `_r_ladder`,
//! `_project_envelope`, geometry measures).

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};
use sutura_geom::winding::MeshBvh;

use crate::tiers::common;

pub const MAX_GRID_DIV: f64 = 512.0;
pub const BUDGET_PAD: i64 = 8;
pub const R_START_VOXELS: f64 = 1.5;
pub const R_STEP: f64 = 1.6;
pub const R_MAX_DIAG_FRACTION: f64 = 0.02;
pub const HEALTHY_DIST_FACTOR: f64 = 1.5;
pub const HEALTHY_NORMAL_DOT: f64 = 0.5;

pub fn bbox(v: &[[f64; 3]]) -> ([f64; 3], [f64; 3]) {
    if v.is_empty() {
        return ([0.0; 3], [0.0; 3]);
    }
    let mut lo = v[0];
    let mut hi = v[0];
    for p in v {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (lo, hi)
}

pub fn diag(v: &[[f64; 3]]) -> f64 {
    let (lo, hi) = bbox(v);
    let d = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

pub fn face_normals(v: &[[f64; 3]], t: &[[i64; 3]]) -> Vec<[f64; 3]> {
    t.iter()
        .map(|r| {
            let a = v[r[0] as usize];
            let b = v[r[1] as usize];
            let c = v[r[2] as usize];
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let n = [
                u[1] * w[2] - u[2] * w[1],
                u[2] * w[0] - u[0] * w[2],
                u[0] * w[1] - u[1] * w[0],
            ];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if len == 0.0 {
                [0.0, 0.0, 0.0]
            } else {
                [n[0] / len, n[1] / len, n[2] / len]
            }
        })
        .collect()
}

pub fn vertex_normals(v: &[[f64; 3]], t: &[[i64; 3]]) -> Vec<[f64; 3]> {
    let fnorm = face_normals(v, t);
    let mut n = vec![[0.0f64; 3]; v.len()];
    for (fi, r) in t.iter().enumerate() {
        let f = fnorm[fi];
        for &vi in r {
            let i = vi as usize;
            n[i][0] += f[0];
            n[i][1] += f[1];
            n[i][2] += f[2];
        }
    }
    for p in &mut n {
        let len = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
        if len != 0.0 {
            p[0] /= len;
            p[1] /= len;
            p[2] /= len;
        }
    }
    n
}

pub fn unique_edges(t: &[[i64; 3]]) -> Vec<(i64, i64)> {
    let mut set = HashSet::new();
    for r in t {
        for (a, b) in [(r[0], r[1]), (r[1], r[2]), (r[2], r[0])] {
            set.insert(if a <= b { (a, b) } else { (b, a) });
        }
    }
    let mut out: Vec<(i64, i64)> = set.into_iter().collect();
    out.sort_unstable();
    out
}

pub fn median_edge_length(v: &[[f64; 3]], t: &[[i64; 3]]) -> f64 {
    if t.is_empty() {
        return 0.0;
    }
    let edges = unique_edges(t);
    if edges.is_empty() {
        return 0.0;
    }
    let mut lens: Vec<f64> = edges
        .iter()
        .map(|&(a, b)| {
            let pa = v[a as usize];
            let pb = v[b as usize];
            let d = [pa[0] - pb[0], pa[1] - pb[1], pa[2] - pb[2]];
            (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
        })
        .collect();
    lens.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = lens.len();
    if n % 2 == 1 {
        lens[n / 2]
    } else {
        (lens[n / 2 - 1] + lens[n / 2]) / 2.0
    }
}

pub fn auto_voxel(v: &[[f64; 3]], t: &[[i64; 3]], grid_budget: Option<i64>) -> f64 {
    if v.is_empty() {
        return 1.0;
    }
    let (lo, hi) = bbox(v);
    let ext = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let maxext = ext.iter().fold(0.0f64, |m, x| m.max(*x)).max(1e-12);
    let mut voxel = median_edge_length(v, t);
    if !(voxel > 0.0) {
        voxel = maxext / 96.0;
    }
    voxel = voxel.max(maxext / MAX_GRID_DIV);
    let budget = match grid_budget {
        Some(b) => b,
        None => return voxel,
    };
    for _ in 0..120 {
        let dims = [
            (ext[0].max(0.0) / voxel).ceil() as i64 + 1 + 2 * BUDGET_PAD,
            (ext[1].max(0.0) / voxel).ceil() as i64 + 1 + 2 * BUDGET_PAD,
            (ext[2].max(0.0) / voxel).ceil() as i64 + 1 + 2 * BUDGET_PAD,
        ];
        let dims = [dims[0].max(3), dims[1].max(3), dims[2].max(3)];
        let cells = dims[0] as i128 * dims[1] as i128 * dims[2] as i128;
        if cells <= budget as i128 {
            break;
        }
        voxel *= 1.15;
    }
    voxel
}

pub fn grid_cell_count(v: &[[f64; 3]], voxel: f64) -> i64 {
    if v.is_empty() || !(voxel > 0.0) {
        return 0;
    }
    let (lo, hi) = bbox(v);
    let ext = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let dims = [
        (ext[0].max(0.0) / voxel).ceil() as i64 + 1 + 2 * BUDGET_PAD,
        (ext[1].max(0.0) / voxel).ceil() as i64 + 1 + 2 * BUDGET_PAD,
        (ext[2].max(0.0) / voxel).ceil() as i64 + 1 + 2 * BUDGET_PAD,
    ];
    let dims = [dims[0].max(3), dims[1].max(3), dims[2].max(3)];
    (dims[0] as i128 * dims[1] as i128 * dims[2] as i128) as i64
}

fn damaged_region(t: &[[i64; 3]]) -> (Vec<bool>, usize) {
    let mut counts: HashMap<(i64, i64), usize> = HashMap::new();
    for r in t {
        for (a, b) in [(r[0], r[1]), (r[1], r[2]), (r[2], r[0])] {
            let k = if a <= b { (a, b) } else { (b, a) };
            *counts.entry(k).or_insert(0) += 1;
        }
    }
    let mut seed = vec![false; t.len()];
    for (fi, r) in t.iter().enumerate() {
        for (a, b) in [(r[0], r[1]), (r[1], r[2]), (r[2], r[0])] {
            let k = if a <= b { (a, b) } else { (b, a) };
            if counts.get(&k).copied().unwrap_or(0) != 2 {
                seed[fi] = true;
                break;
            }
        }
    }
    if !seed.iter().any(|x| *x) {
        return (seed, 0);
    }
    let nv = t.iter().flat_map(|r| r.iter().copied()).max().unwrap_or(-1) + 1;
    let mut vmark = vec![false; nv as usize];
    for (fi, r) in t.iter().enumerate() {
        if seed[fi] {
            for &v in r {
                vmark[v as usize] = true;
            }
        }
    }
    let region: Vec<bool> = t
        .iter()
        .map(|r| r.iter().any(|&v| vmark[v as usize]))
        .collect();
    let mut parent: Vec<usize> = (0..nv as usize).collect();
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
    for (fi, r) in t.iter().enumerate() {
        if !region[fi] {
            continue;
        }
        let r0 = find(&mut parent, r[0] as usize);
        for &x in &r[1..] {
            let rx = find(&mut parent, x as usize);
            if rx != r0 {
                parent[rx] = r0;
            }
        }
    }
    let mut used: HashSet<usize> = HashSet::new();
    for (fi, r) in t.iter().enumerate() {
        if region[fi] {
            for &v in r {
                used.insert(find(&mut parent, v as usize));
            }
        }
    }
    (region, used.len())
}

/// `(gap, info, region_mask)`.
pub fn gap_estimate(v: &[[f64; 3]], t: &[[i64; 3]]) -> (f64, Value, Vec<bool>) {
    let v32: Vec<[f32; 3]> = v.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
    let t32: Vec<[i32; 3]> = t.iter().map(|r| [r[0] as i32, r[1] as i32, r[2] as i32]).collect();
    let d = crate::report::detect_defects(&v32, &t32, true);
    let mut gap = 0.0f64;
    for h in &d.holes {
        gap = gap.max(h.diameter);
    }
    let (region_mask, n_regions) = damaged_region(t);
    let mut region_diag = 0.0f64;
    if region_mask.iter().any(|x| *x) {
        let mut used: HashSet<i64> = HashSet::new();
        for (fi, r) in t.iter().enumerate() {
            if region_mask[fi] {
                for &x in r {
                    used.insert(x);
                }
            }
        }
        let rv: Vec<[f64; 3]> = used.iter().map(|&i| v[i as usize]).collect();
        region_diag = diag(&rv);
    }
    if gap <= 0.0 {
        gap = region_diag;
    }
    let damaged_faces = region_mask.iter().filter(|x| **x).count();
    let info = json!({
        "holes": d.holes.len(),
        "non_manifold_regions": d.non_manifold.len(),
        "gap": gap,
        "damaged_faces": damaged_faces,
        "damaged_bbox_diag": region_diag,
        "damaged_bbox_fraction": 0.0,
    });
    (gap, info, region_mask)
}

pub fn r_ladder(gap: f64, voxel: f64, diag: f64, max_tries: i64) -> Vec<f64> {
    let r0 = R_START_VOXELS * voxel;
    let mut cap = if diag > 0.0 { R_MAX_DIAG_FRACTION * diag } else { r0 };
    if gap > 0.0 {
        cap = if cap > 0.0 { cap.min(0.5 * gap) } else { 0.5 * gap };
    }
    cap = cap.max(r0);
    let mut out = Vec::new();
    let mut cur = r0;
    for _ in 0..max_tries.max(1) {
        out.push(cur.min(cap));
        if cur >= cap {
            break;
        }
        cur *= R_STEP;
    }
    out
}

/// Move envelope vertices onto the original surface where healthy.
pub fn project_envelope(
    ev: &[[f64; 3]],
    et: &[[i64; 3]],
    ov: &[[f64; 3]],
    ot: &[[i64; 3]],
    r: f64,
) -> (Vec<[f64; 3]>, Vec<bool>) {
    if ev.is_empty() || ot.is_empty() {
        return (ev.to_vec(), vec![false; ev.len()]);
    }
    let ut: Vec<[usize; 3]> = ot.iter().map(|x| [x[0] as usize, x[1] as usize, x[2] as usize]).collect();
    let bvh = MeshBvh::from_arrays(ov, &ut);
    let vn = vertex_normals(ev, et);
    let fnorm = face_normals(ov, ot);
    let mut pv = ev.to_vec();
    let mut healthy = vec![false; ev.len()];
    for i in 0..ev.len() {
        let (q, d, fid) = bvh.closest_point(ev[i]);
        if fid == usize::MAX {
            continue;
        }
        let ndot = vn[i][0] * fnorm[fid][0] + vn[i][1] * fnorm[fid][1] + vn[i][2] * fnorm[fid][2];
        if d < HEALTHY_DIST_FACTOR * r && ndot > HEALTHY_NORMAL_DOT {
            pv[i] = q;
            healthy[i] = true;
        }
    }
    (pv, healthy)
}
