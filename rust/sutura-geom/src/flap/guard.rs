//! Commit guards: component stats (flat/zero-volume-shell detection) and the
//! coplanar positive-area overlap test (with a spatial grid hash replacing the
//! reference's `cKDTree`).

use super::{cross, dot, median, norm, sub, Dsu, T3, V3};
use std::collections::HashMap;

pub const COPLANAR_INTRA_MAX: usize = 20000;

pub struct CompStats {
    pub lab: Vec<u32>,
    pub vol_c: Vec<f64>,
    pub area_c: Vec<f64>,
    pub flat_c: Vec<bool>,
}

/// Per-face component labels (shared-edge adjacency), signed volume, area and a
/// flat flag per component.  A component is flat when the minimum angle between
/// every face normal and the component's area-weighted mean normal is below
/// `angle_deg`.
pub fn component_stats(v: &[V3], t: &[T3], angle_deg: f64) -> CompStats {
    let f = t.len();
    if f == 0 {
        return CompStats {
            lab: Vec::new(),
            vol_c: Vec::new(),
            area_c: Vec::new(),
            flat_c: Vec::new(),
        };
    }
    // face normals / areas / volumes
    let mut nrm = vec![[0.0f64; 3]; f];
    let mut a2 = vec![0.0f64; f];
    let mut vol_f = vec![0.0f64; f];
    for (fi, tri) in t.iter().enumerate() {
        let (p0, p1, p2) = (v[tri[0] as usize], v[tri[1] as usize], v[tri[2] as usize]);
        let n = cross(sub(p1, p0), sub(p2, p0));
        let ln = norm(n);
        a2[fi] = 0.5 * ln;
        let inv = 1.0 / ln.max(1e-30);
        nrm[fi] = [n[0] * inv, n[1] * inv, n[2] * inv];
        vol_f[fi] = dot(p0, cross(p1, p2)) / 6.0;
    }
    // adjacency via edges used by exactly two faces
    let mut edges: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for (fi, tri) in t.iter().enumerate() {
        for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            edges.entry(super::tkey(a, b)).or_default().push(fi as u32);
        }
    }
    let mut dsu = Dsu::new(f);
    for (_, fs) in &edges {
        if fs.len() == 2 {
            dsu.union(fs[0], fs[1]);
        }
    }
    let mut dense: HashMap<u32, u32> = HashMap::new();
    let mut lab = vec![0u32; f];
    for fi in 0..f {
        let r = dsu.find(fi as u32);
        let next = dense.len() as u32;
        lab[fi] = *dense.entry(r).or_insert(next);
    }
    let nu = dense.len();
    let mut vol_c = vec![0.0f64; nu];
    let mut area_c = vec![0.0f64; nu];
    let mut mn = vec![[0.0f64; 3]; nu];
    for fi in 0..f {
        let l = lab[fi] as usize;
        vol_c[l] += vol_f[fi];
        area_c[l] += a2[fi];
        mn[l][0] += nrm[fi][0] * a2[fi];
        mn[l][1] += nrm[fi][1] * a2[fi];
        mn[l][2] += nrm[fi][2] * a2[fi];
    }
    for m in mn.iter_mut() {
        let n = norm(*m).max(1e-30);
        m[0] /= n;
        m[1] /= n;
        m[2] /= n;
    }
    let mut comp_min = vec![1.0f64; nu];
    for fi in 0..f {
        let l = lab[fi] as usize;
        let d = dot(nrm[fi], mn[l]);
        if d < comp_min[l] {
            comp_min[l] = d;
        }
    }
    let cos_tol = angle_deg.to_radians().cos();
    let flat_c = comp_min.iter().map(|&c| c > cos_tol).collect();
    CompStats {
        lab,
        vol_c,
        area_c,
        flat_c,
    }
}

/// Centroid, unit normal and longest-edge length of each face.
pub fn face_cent_norm(p: &[V3], faces: &[[u32; 3]]) -> (Vec<V3>, Vec<V3>, Vec<f64>) {
    let mut c = Vec::with_capacity(faces.len());
    let mut n = Vec::with_capacity(faces.len());
    let mut ml = Vec::with_capacity(faces.len());
    for f in faces {
        let (a, b, cc) = (p[f[0] as usize], p[f[1] as usize], p[f[2] as usize]);
        c.push([
            (a[0] + b[0] + cc[0]) / 3.0,
            (a[1] + b[1] + cc[1]) / 3.0,
            (a[2] + b[2] + cc[2]) / 3.0,
        ]);
        let nn = cross(sub(b, a), sub(cc, a));
        let ln = norm(nn).max(1e-30);
        n.push([nn[0] / ln, nn[1] / ln, nn[2] / ln]);
        ml.push(norm(sub(a, b)).max(norm(sub(b, cc))).max(norm(sub(cc, a))));
    }
    (c, n, ml)
}

/// A committed patch reference: centroids, unit normals and world-space
/// triangles.
pub struct PatchRef {
    pub cent: Vec<V3>,
    pub norm: Vec<V3>,
    pub tri: Vec<[V3; 3]>,
}

/// Positive-area overlap of two coplanar triangles (SAT in B's plane).
fn pairs_overlap_one(a3: &[V3; 3], b3: &[V3; 3], rn: V3) -> bool {
    let rt0 = b3[0];
    let u = sub(b3[1], rt0);
    let lu = norm(u);
    if lu <= 1e-12 {
        return false;
    }
    let u = [u[0] / lu, u[1] / lu, u[2] / lu];
    let w = cross(rn, u);
    let proj = |x: V3| -> [f64; 2] {
        let d = sub(x, rt0);
        [dot(d, u), dot(d, w)]
    };
    let a2: [[f64; 2]; 3] = [proj(a3[0]), proj(a3[1]), proj(a3[2])];
    let b2: [[f64; 2]; 3] = [proj(b3[0]), proj(b3[1]), proj(b3[2])];
    // scale: max edge length among the six edges
    let edge = |t: &[[f64; 2]; 3], i: usize, j: usize| norm(sub(
        [t[i][0], t[i][1], 0.0],
        [t[j][0], t[j][1], 0.0],
    ));
    let mut scale = 0.0f64;
    for (i, j) in [(0, 1), (1, 2), (2, 0)] {
        scale = scale.max(edge(&a2, i, j)).max(edge(&b2, i, j));
    }
    let scale = scale.max(1e-12);
    let eps = 1e-9 * scale;
    let axes = |t: &[[f64; 2]; 3]| -> [[f64; 2]; 3] {
        let mut out = [[0.0; 2]; 3];
        let idx = [(0, 1), (1, 2), (2, 0)];
        for k in 0..3 {
            let (i, j) = idx[k];
            let te = [t[j][0] - t[i][0], t[j][1] - t[i][1]];
            out[k] = [-te[1], te[0]];
        }
        out
    };
    let mut separated = false;
    for axes_t in [axes(&a2), axes(&b2)] {
        for ax in axes_t {
            let dot2 = |p: [f64; 2]| p[0] * ax[0] + p[1] * ax[1];
            let pa = [dot2(a2[0]), dot2(a2[1]), dot2(a2[2])];
            let pb = [dot2(b2[0]), dot2(b2[1]), dot2(b2[2])];
            let pamax = pa.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let pamin = pa.iter().cloned().fold(f64::INFINITY, f64::min);
            let pbmax = pb.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let pbmin = pb.iter().cloned().fold(f64::INFINITY, f64::min);
            if pamax <= pbmin + eps || pbmax <= pamin + eps {
                separated = true;
            }
        }
    }
    !separated
}

/// True when a candidate patch has a positive-area coplanar overlap with a
/// committed reference face or (for small patches) another face of itself.
pub fn coplanar_collides(
    p: &[V3],
    faces: &[[u32; 3]],
    base: &[PatchRef],
    extra: &[PatchRef],
    overlap_frac: f64,
    cos_tol: f64,
) -> bool {
    let f = faces.len();
    if f == 0 {
        return false;
    }
    let tri: Vec<[V3; 3]> = faces
        .iter()
        .map(|fc| {
            [
                p[fc[0] as usize],
                p[fc[1] as usize],
                p[fc[2] as usize],
            ]
        })
        .collect();
    let (c, n, ml) = face_cent_norm(p, faces);
    let _ = overlap_frac;

    let mut rc_l: Vec<V3> = Vec::new();
    let mut rn_l: Vec<V3> = Vec::new();
    let mut rt_l: Vec<[V3; 3]> = Vec::new();
    let mut intra_l: Vec<bool> = Vec::new();
    for r in base.iter().chain(extra.iter()) {
        for k in 0..r.cent.len() {
            rc_l.push(r.cent[k]);
            rn_l.push(r.norm[k]);
            rt_l.push(r.tri[k]);
            intra_l.push(false);
        }
    }
    let off = rc_l.len();
    let do_intra = f <= COPLANAR_INTRA_MAX;
    if !do_intra && off == 0 {
        return false;
    }
    if do_intra {
        for i in 0..f {
            rc_l.push(c[i]);
            rn_l.push(n[i]);
            rt_l.push(tri[i]);
            intra_l.push(true);
        }
    }
    let rc = rc_l;
    let rn = rn_l;
    let rt = rt_l;
    let intra = intra_l;

    let cap = if ml.is_empty() {
        0.0
    } else {
        4.0 * median(ml.clone())
    };
    let radius: Vec<f64> = if cap > 0.0 {
        ml.iter().map(|&m| 2.0 * m.min(cap)).collect()
    } else {
        vec![0.0; f]
    };

    // Spatial grid over reference centroids.
    let max_radius = radius.iter().cloned().fold(1e-12f64, f64::max);
    let cell = max_radius.max(1e-12);
    let key = |p: V3| -> (i64, i64, i64) {
        (
            (p[0] / cell).floor() as i64,
            (p[1] / cell).floor() as i64,
            (p[2] / cell).floor() as i64,
        )
    };
    let mut grid: HashMap<(i64, i64, i64), Vec<u32>> = HashMap::new();
    for (j, &rcj) in rc.iter().enumerate() {
        grid.entry(key(rcj)).or_default().push(j as u32);
    }

    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for i in 0..f {
        let r = radius[i].max(1e-12);
        let k0 = key([c[i][0] - r, c[i][1] - r, c[i][2] - r]);
        let k1 = key([c[i][0] + r, c[i][1] + r, c[i][2] + r]);
        for gx in k0.0..=k1.0 {
            for gy in k0.1..=k1.1 {
                for gz in k0.2..=k1.2 {
                    if let Some(js) = grid.get(&(gx, gy, gz)) {
                        for &j in js {
                            let j = j as usize;
                            if intra[j] && j == off + i {
                                continue;
                            }
                            let d = sub(c[i], rc[j]);
                            if norm(d) <= r {
                                pairs.push((i, j));
                            }
                        }
                    }
                }
            }
        }
    }
    if pairs.is_empty() {
        return false;
    }
    // normal alignment
    pairs.retain(|&(i, j)| dot(n[i], rn[j]).abs() >= cos_tol);
    if pairs.is_empty() {
        return false;
    }
    // coplanarity
    pairs.retain(|&(i, j)| {
        let base = rt[j][0];
        let mut m = 0.0f64;
        for v in tri[i] {
            m = m.max(dot(sub(v, base), rn[j]).abs());
        }
        m <= 1e-3 * ml[i].max(1e-9)
    });
    if pairs.is_empty() {
        return false;
    }
    pairs
        .iter()
        .any(|&(i, j)| pairs_overlap_one(&tri[i], &rt[j], rn[j]))
}
