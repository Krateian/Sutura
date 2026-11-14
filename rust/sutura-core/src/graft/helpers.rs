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

/// NumPy `einsum("ij,ij->i", a, b)` dot order (verified against numpy 1.x on
/// this platform): `fma(a2,b2, a0*b0) + a1*b1`.
#[inline]
pub fn np_dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[2].mul_add(b[2], a[0] * b[0]) + a[1] * b[1]
}

/// NumPy `add.reduce` float64 pairwise summation (PW_BLOCKSIZE 128).
pub fn np_pairwise_sum(values: &[f64]) -> f64 {
    fn rec(a: &[f64]) -> f64 {
        let n = a.len();
        if n < 8 {
            let mut res = 0.0;
            for x in a {
                res += *x;
            }
            return res;
        }
        if n <= 128 {
            let mut r = [a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]];
            let mut i = 8;
            while i < n - (n % 8) {
                for k in 0..8 {
                    r[k] += a[i + k];
                }
                i += 8;
            }
            let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
            while i < n {
                res += a[i];
                i += 1;
            }
            return res;
        }
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        rec(&a[..n2]) + rec(&a[n2..])
    }
    rec(values)
}

/// `_referenced_only`: unique referenced vertices (sorted) and remapped faces.
pub fn referenced_only(v: &[[f64; 3]], t: &[[i64; 3]]) -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
    let mut used: Vec<i64> = t.iter().flat_map(|r| r.iter().copied()).collect();
    used.sort_unstable();
    used.dedup();
    let remap: HashMap<i64, i64> = used
        .iter()
        .enumerate()
        .map(|(i, &u)| (u, i as i64))
        .collect();
    let nv = used.iter().map(|&u| v[u as usize]).collect();
    let nt = t
        .iter()
        .map(|r| [remap[&r[0]], remap[&r[1]], remap[&r[2]]])
        .collect();
    (nv, nt)
}

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
    // Match numpy's `np.add.at(n, t[:, k], fn)` k-major accumulation order.
    for k in 0..3 {
        for (fi, r) in t.iter().enumerate() {
            let f = fnorm[fi];
            let i = r[k] as usize;
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
        let ndot = np_dot3(vn[i], fnorm[fid]);
        if d < HEALTHY_DIST_FACTOR * r && ndot > HEALTHY_NORMAL_DOT {
            pv[i] = q;
            healthy[i] = true;
        }
    }
    (pv, healthy)
}

const REMESH_MAX_PASSES: usize = 6;

fn unique_edges_with_inv(t: &[[i64; 3]]) -> (Vec<(i64, i64)>, Vec<usize>) {
    let edges = unique_edges(t);
    let map: HashMap<(i64, i64), usize> = edges
        .iter()
        .enumerate()
        .map(|(i, &e)| (e, i))
        .collect();
    let mut inv = Vec::with_capacity(3 * t.len());
    for r in t {
        for (a, b) in [(r[0], r[1]), (r[1], r[2]), (r[2], r[0])] {
            let k = if a <= b { (a, b) } else { (b, a) };
            inv.push(map[&k]);
        }
    }
    (edges, inv)
}

fn edge_len(v: &[[f64; 3]], e: (i64, i64)) -> f64 {
    let a = v[e.0 as usize];
    let b = v[e.1 as usize];
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

pub fn split_long_edges(v: &[[f64; 3]], t: &[[i64; 3]], target: f64) -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
    let thr = (4.0 / 3.0) * target;
    let mut v = v.to_vec();
    let mut t = t.to_vec();
    for _ in 0..REMESH_MAX_PASSES {
        let (edges, inv) = unique_edges_with_inv(&t);
        let long: Vec<bool> = edges.iter().map(|&e| edge_len(&v, e) > thr).collect();
        if !long.iter().any(|x| *x) {
            break;
        }
        let long_ids: Vec<usize> = long
            .iter()
            .enumerate()
            .filter(|(_, x)| **x)
            .map(|(i, _)| i)
            .collect();
        let mut mid_id = vec![-1i64; edges.len()];
        let base = v.len();
        for (k, &ei) in long_ids.iter().enumerate() {
            let e = edges[ei];
            let a = v[e.0 as usize];
            let b = v[e.1 as usize];
            v.push([(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5, (a[2] + b[2]) * 0.5]);
            mid_id[ei] = (base + k) as i64;
        }
        let f = t.len();
        let mut s0: Vec<[i64; 3]> = Vec::new();
        let mut s1_01a: Vec<[i64; 3]> = Vec::new();
        let mut s1_01b: Vec<[i64; 3]> = Vec::new();
        let mut s1_12a: Vec<[i64; 3]> = Vec::new();
        let mut s1_12b: Vec<[i64; 3]> = Vec::new();
        let mut s1_20a: Vec<[i64; 3]> = Vec::new();
        let mut s1_20b: Vec<[i64; 3]> = Vec::new();
        let mut s2_20a: Vec<[i64; 3]> = Vec::new();
        let mut s2_20b: Vec<[i64; 3]> = Vec::new();
        let mut s2_20c: Vec<[i64; 3]> = Vec::new();
        let mut s2_01a: Vec<[i64; 3]> = Vec::new();
        let mut s2_01b: Vec<[i64; 3]> = Vec::new();
        let mut s2_01c: Vec<[i64; 3]> = Vec::new();
        let mut s2_12a: Vec<[i64; 3]> = Vec::new();
        let mut s2_12b: Vec<[i64; 3]> = Vec::new();
        let mut s2_12c: Vec<[i64; 3]> = Vec::new();
        let mut s3a: Vec<[i64; 3]> = Vec::new();
        let mut s3b: Vec<[i64; 3]> = Vec::new();
        let mut s3c: Vec<[i64; 3]> = Vec::new();
        let mut s3d: Vec<[i64; 3]> = Vec::new();
        for fi in 0..f {
            let m01 = mid_id[inv[3 * fi]];
            let m12 = mid_id[inv[3 * fi + 1]];
            let m20 = mid_id[inv[3 * fi + 2]];
            let [a, b, c] = t[fi];
            let s = (m01 >= 0) as i32 + (m12 >= 0) as i32 + (m20 >= 0) as i32;
            match s {
                0 => s0.push([a, b, c]),
                1 => {
                    if m01 >= 0 {
                        s1_01a.push([a, m01, c]);
                        s1_01b.push([m01, b, c]);
                    }
                    if m12 >= 0 {
                        s1_12a.push([b, m12, a]);
                        s1_12b.push([m12, c, a]);
                    }
                    if m20 >= 0 {
                        s1_20a.push([c, m20, b]);
                        s1_20b.push([m20, a, b]);
                    }
                }
                2 => {
                    if m20 < 0 {
                        s2_20a.push([a, m01, c]);
                        s2_20b.push([m01, b, m12]);
                        s2_20c.push([m12, c, a]);
                    } else if m01 < 0 {
                        s2_01a.push([b, m12, a]);
                        s2_01b.push([m12, c, m20]);
                        s2_01c.push([m20, a, b]);
                    } else {
                        s2_12a.push([c, m20, b]);
                        s2_12b.push([m20, a, m01]);
                        s2_12c.push([m01, b, c]);
                    }
                }
                _ => {
                    s3a.push([a, m01, m20]);
                    s3b.push([m01, b, m12]);
                    s3c.push([m20, m12, c]);
                    s3d.push([m01, m12, m20]);
                }
            }
        }
        let mut pieces: Vec<[i64; 3]> = Vec::new();
        pieces.extend(s0);
        pieces.extend(s1_01a);
        pieces.extend(s1_01b);
        pieces.extend(s1_12a);
        pieces.extend(s1_12b);
        pieces.extend(s1_20a);
        pieces.extend(s1_20b);
        pieces.extend(s2_20a);
        pieces.extend(s2_20b);
        pieces.extend(s2_20c);
        pieces.extend(s2_01a);
        pieces.extend(s2_01b);
        pieces.extend(s2_01c);
        pieces.extend(s2_12a);
        pieces.extend(s2_12b);
        pieces.extend(s2_12c);
        pieces.extend(s3a);
        pieces.extend(s3b);
        pieces.extend(s3c);
        pieces.extend(s3d);
        t = pieces;
    }
    (v, t)
}

pub fn collapse_short_edges(v: &[[f64; 3]], t: &[[i64; 3]], target: f64) -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
    let thr = (4.0 / 5.0) * target;
    let mut v = v.to_vec();
    let mut t = t.to_vec();
    for _ in 0..REMESH_MAX_PASSES {
        let (edges, _inv) = unique_edges_with_inv(&t);
        if edges.is_empty() {
            break;
        }
        let cand: Vec<(i64, i64)> = edges
            .iter()
            .copied()
            .filter(|&e| edge_len(&v, e) < thr)
            .collect();
        if cand.is_empty() {
            break;
        }
        let nv = v.len();
        let mut adj: Vec<HashSet<usize>> = vec![HashSet::new(); nv];
        for &(a, b) in &edges {
            adj[a as usize].insert(b as usize);
            adj[b as usize].insert(a as usize);
        }
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
        let mut merged = vec![false; nv];
        let mut any_merge = false;
        for (a, b) in cand {
            let (a, b) = (a as usize, b as usize);
            if merged[a] || merged[b] {
                continue;
            }
            let common = adj[a].intersection(&adj[b]).count();
            if common != 2 {
                continue;
            }
            merged[a] = true;
            merged[b] = true;
            let ra = find(&mut parent, a);
            let rb = find(&mut parent, b);
            if ra != rb {
                parent[rb] = ra;
            }
            any_merge = true;
        }
        if !any_merge {
            break;
        }
        let root: Vec<usize> = (0..nv).map(|i| find(&mut parent, i)).collect();
        let mut root_ids: HashMap<usize, usize> = HashMap::new();
        let mut acc: Vec<[f64; 3]> = Vec::new();
        let mut cnt: Vec<usize> = Vec::new();
        for i in 0..nv {
            let r = root[i];
            let id = *root_ids.entry(r).or_insert_with(|| {
                acc.push([0.0; 3]);
                cnt.push(0);
                acc.len() - 1
            });
            acc[id][0] += v[i][0];
            acc[id][1] += v[i][1];
            acc[id][2] += v[i][2];
            cnt[id] += 1;
        }
        let new_v: Vec<[f64; 3]> = acc
            .iter()
            .zip(cnt.iter())
            .map(|(a, c)| [a[0] / *c as f64, a[1] / *c as f64, a[2] / *c as f64])
            .collect();
        let remap: Vec<i64> = root.iter().map(|r| root_ids[r] as i64).collect();
        let mut nt: Vec<[i64; 3]> = t
            .iter()
            .map(|r| [remap[r[0] as usize], remap[r[1] as usize], remap[r[2] as usize]])
            .filter(|r| r[0] != r[1] && r[1] != r[2] && r[0] != r[2])
            .collect();
        if nt.is_empty() {
            break;
        }
        let mut seen: HashSet<[i64; 3]> = HashSet::new();
        nt.retain(|r| {
            let mut k = *r;
            k.sort_unstable();
            seen.insert(k)
        });
        let mut used: Vec<i64> = nt.iter().flat_map(|r| r.iter().copied()).collect();
        used.sort_unstable();
        used.dedup();
        let mut new_id: HashMap<i64, i64> = HashMap::new();
        for (i, &u) in used.iter().enumerate() {
            new_id.insert(u, i as i64);
        }
        v = used.iter().map(|&u| new_v[u as usize]).collect();
        t = nt
            .iter()
            .map(|r| [new_id[&r[0]], new_id[&r[1]], new_id[&r[2]]])
            .collect();
    }
    (v, t)
}

pub fn smooth_tangential(v: &[[f64; 3]], t: &[[i64; 3]], iterations: usize) -> Vec<[f64; 3]> {
    if v.is_empty() || t.is_empty() {
        return v.to_vec();
    }
    // Python builds a = concat(t[:,0], t[:,1], t[:,2]) and
    // b = concat(t[:,1], t[:,2], t[:,0]); src = [a, b], dst = [b, a].
    // The accumulation order is part of the float result.
    let mut a = Vec::with_capacity(3 * t.len());
    let mut b = Vec::with_capacity(3 * t.len());
    // numpy concatenates all t[:,0], then all t[:,1], then all t[:,2].
    for k in 0..3 {
        for r in t {
            a.push(r[k] as usize);
        }
    }
    for k in [1usize, 2, 0] {
        for r in t {
            b.push(r[k] as usize);
        }
    }
    let mut src = Vec::with_capacity(6 * t.len());
    let mut dst = Vec::with_capacity(6 * t.len());
    src.extend_from_slice(&a);
    src.extend_from_slice(&b);
    dst.extend_from_slice(&b);
    dst.extend_from_slice(&a);
    let mut deg = vec![0.0f64; v.len()];
    for &s in &src {
        deg[s] += 1.0;
    }
    let mut boundary = vec![false; v.len()];
    let mut counts: HashMap<(i64, i64), usize> = HashMap::new();
    for r in t {
        for (a, b) in [(r[0], r[1]), (r[1], r[2]), (r[2], r[0])] {
            let k = if a <= b { (a, b) } else { (b, a) };
            *counts.entry(k).or_insert(0) += 1;
        }
    }
    for ((a, b), c) in counts {
        if c == 1 {
            boundary[a as usize] = true;
            boundary[b as usize] = true;
        }
    }
    let free: Vec<bool> = (0..v.len()).map(|i| !boundary[i] && deg[i] > 0.0).collect();
    if !free.iter().any(|x| *x) {
        return v.to_vec();
    }
    let mut out = v.to_vec();
    for _ in 0..iterations {
        let vn = vertex_normals(&out, t);
        let mut acc = vec![[0.0f64; 3]; out.len()];
        for k in 0..src.len() {
            let s = src[k];
            let d = dst[k];
            acc[s][0] += out[d][0];
            acc[s][1] += out[d][1];
            acc[s][2] += out[d][2];
        }
        for i in 0..out.len() {
            if !free[i] {
                continue;
            }
            let cen = [
                acc[i][0] / deg[i],
                acc[i][1] / deg[i],
                acc[i][2] / deg[i],
            ];
            let mut disp = [cen[0] - out[i][0], cen[1] - out[i][1], cen[2] - out[i][2]];
            let dot = np_dot3(disp, vn[i]);
            disp[0] -= dot * vn[i][0];
            disp[1] -= dot * vn[i][1];
            disp[2] -= dot * vn[i][2];
            out[i][0] += 0.5 * disp[0];
            out[i][1] += 0.5 * disp[1];
            out[i][2] += 0.5 * disp[2];
        }
    }
    out
}

pub fn isotropic_remesh(v: &[[f64; 3]], t: &[[i64; 3]], target: f64) -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
    if target <= 0.0 || t.is_empty() {
        return (v.to_vec(), t.to_vec());
    }
    let (v1, t1) = split_long_edges(v, t, target);
    let (v2, t2) = collapse_short_edges(&v1, &t1, target);
    let v3 = smooth_tangential(&v2, &t2, 2);
    (v3, t2)
}

