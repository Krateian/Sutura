//! Liepa-style patch refinement: conforming edge bisection (or a
//! circumradius/centroid needle split) followed by Delaunay and valence flips.

use super::{cross, dot, norm, sub, V3};
use std::collections::{HashMap, HashSet};

const FLIP_MIN_ANGLE: f64 = 3.0;

#[inline]
fn angle_at(p: V3, a: V3, b: V3) -> f64 {
    let v1 = sub(a, p);
    let v2 = sub(b, p);
    let d = dot(v1, v2);
    let n = norm(v1) * norm(v2);
    if n < 1e-20 {
        0.0
    } else {
        (d / n).clamp(-1.0, 1.0).acos().to_degrees()
    }
}

#[inline]
fn tri_min_angle(p: &[V3], i: usize, j: usize, k: usize) -> f64 {
    angle_at(p[i], p[j], p[k])
        .min(angle_at(p[j], p[i], p[k]))
        .min(angle_at(p[k], p[i], p[j]))
}

#[inline]
fn circumradius(p: &[V3], f: &[u32; 3]) -> f64 {
    let (a, b, c) = (p[f[0] as usize], p[f[1] as usize], p[f[2] as usize]);
    let ab = norm(sub(b, a));
    let bc = norm(sub(c, b));
    let ca = norm(sub(a, c));
    let cr = norm(cross(sub(b, a), sub(c, a)));
    ab * bc * ca / (2.0 * cr).max(1e-30)
}

#[inline]
fn tri_longest(p: &[V3], f: &[u32; 3]) -> f64 {
    let (a, b, c) = (p[f[0] as usize], p[f[1] as usize], p[f[2] as usize]);
    norm(sub(b, a)).max(norm(sub(c, b))).max(norm(sub(a, c)))
}

/// Per-face `(min_angle_deg, aspect)`.
fn tri_angles_aspect(p: &[V3], faces: &[[u32; 3]]) -> Vec<(f64, f64)> {
    faces
        .iter()
        .map(|f| {
            let (a, b, c) = (p[f[0] as usize], p[f[1] as usize], p[f[2] as usize]);
            let e0 = norm(sub(b, a));
            let e1 = norm(sub(c, b));
            let e2 = norm(sub(a, c));
            let s = 0.5 * (e0 + e1 + e2);
            let area = (s * (s - e0) * (s - e1) * (s - e2)).max(0.0).sqrt();
            let inr = if s > 0.0 { area / s.max(1e-20) } else { 1e-20 };
            let longest = e0.max(e1).max(e2);
            let aspect = longest / (2.0 * inr.max(1e-20));
            let a0 = angle_at(a, b, c);
            let a1 = angle_at(b, a, c);
            let a2 = angle_at(c, a, b);
            (a0.min(a1).min(a2), aspect)
        })
        .collect()
}

#[inline]
fn ekey2(a: u32, b: u32) -> (u32, u32) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Split patch edges longer than `threshold` (rim edges excluded), keeping the
/// patch conforming.  Mutates `p`/`faces`; returns the number of vertices added.
pub fn subdivide_patch(
    p: &mut Vec<V3>,
    faces: &mut Vec<[u32; 3]>,
    boundary_edges: &HashSet<(u32, u32)>,
    target: f64,
    max_faces: usize,
    threshold: Option<f64>,
) -> usize {
    let n_start = p.len();
    let target = target.max(1e-12);
    let threshold = threshold.unwrap_or(1.4 * target);
    for _ in 0..24 {
        let budget = if max_faces > faces.len() {
            (max_faces - faces.len()) / 2
        } else {
            0
        };
        if budget == 0 {
            break;
        }
        let mut mid: HashMap<(u32, u32), u32> = HashMap::new();
        let mut any_marked = false;
        let mut capped = false;
        for f in faces.iter() {
            for e in [(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
                let k = ekey2(e.0, e.1);
                if boundary_edges.contains(&k) || mid.contains_key(&k) {
                    continue;
                }
                if norm(sub(p[e.0 as usize], p[e.1 as usize])) > threshold {
                    if mid.len() >= budget {
                        capped = true;
                        break;
                    }
                    mid.insert(k, p.len() as u32);
                    let a = p[e.0 as usize];
                    let b = p[e.1 as usize];
                    p.push([0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1]), 0.5 * (a[2] + b[2])]);
                    any_marked = true;
                }
            }
            if capped {
                break;
            }
        }
        if !any_marked {
            break;
        }
        let mut new_faces: Vec<[u32; 3]> = Vec::with_capacity(faces.len() + mid.len() * 2);
        for f in faces.iter() {
            let (a, b, c) = (f[0], f[1], f[2]);
            let k0 = ekey2(a, b);
            let k1 = ekey2(b, c);
            let k2 = ekey2(c, a);
            let e0 = mid.contains_key(&k0);
            let e1 = mid.contains_key(&k1);
            let e2 = mid.contains_key(&k2);
            if !(e0 || e1 || e2) {
                new_faces.push(*f);
                continue;
            }
            let m0 = *mid.get(&k0).unwrap_or(&0);
            let m1 = *mid.get(&k1).unwrap_or(&0);
            let m2 = *mid.get(&k2).unwrap_or(&0);
            match (e0, e1, e2) {
                (true, false, false) => {
                    new_faces.push([a, m0, c]);
                    new_faces.push([m0, b, c]);
                }
                (false, true, false) => {
                    new_faces.push([b, m1, a]);
                    new_faces.push([m1, c, a]);
                }
                (false, false, true) => {
                    new_faces.push([c, m2, b]);
                    new_faces.push([m2, a, b]);
                }
                (true, true, false) => {
                    new_faces.push([a, m0, m1]);
                    new_faces.push([m0, b, m1]);
                    new_faces.push([a, m1, c]);
                }
                (false, true, true) => {
                    new_faces.push([b, m1, m2]);
                    new_faces.push([m1, c, m2]);
                    new_faces.push([b, m2, a]);
                }
                (true, false, true) => {
                    new_faces.push([c, m2, m0]);
                    new_faces.push([m2, a, m0]);
                    new_faces.push([c, m0, b]);
                }
                _ => {
                    new_faces.push([a, m0, m2]);
                    new_faces.push([m0, b, m1]);
                    new_faces.push([m2, m1, c]);
                    new_faces.push([m0, m1, m2]);
                }
            }
        }
        *faces = new_faces;
    }
    p.len() - n_start
}

/// Greedy Delaunay edge-flip relaxation.  Rim (boundary) edges are never
/// flipped.  Returns the relaxed face list.
pub fn flip_relax(
    p: &[V3],
    faces_in: &[[u32; 3]],
    n_rim: usize,
    forbidden: &HashSet<(u32, u32)>,
    max_passes: usize,
    tol_deg: f64,
) -> Vec<[u32; 3]> {
    let mut faces: Vec<[u32; 3]> = faces_in.to_vec();
    for _ in 0..max_passes {
        let mut order: Vec<(u32, u32)> = Vec::new();
        let mut emap: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
        for (fi, f) in faces.iter().enumerate() {
            for (u, w) in [(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
                let k = ekey2(u, w);
                if !emap.contains_key(&k) {
                    order.push(k);
                }
                emap.entry(k).or_default().push(fi as u32);
            }
        }
        let cands: Vec<((u32, u32), u32, u32)> = order
            .iter()
            .filter_map(|k| {
                let fl = &emap[k];
                if fl.len() == 2 {
                    Some((*k, fl[0], fl[1]))
                } else {
                    None
                }
            })
            .collect();
        let mut consumed: HashSet<u32> = HashSet::new();
        let mut created: HashSet<(u32, u32)> = HashSet::new();
        let mut n_flip = 0usize;
        for (key, i1, i2) in cands {
            let (mut a, mut b) = key;
            if consumed.contains(&i1) || consumed.contains(&i2) {
                continue;
            }
            let f1 = faces[i1 as usize];
            let f2 = faces[i2 as usize];
            let has = |f: &[u32; 3], x: u32| f[0] == x || f[1] == x || f[2] == x;
            if !has(&f1, a) || !has(&f1, b) || !has(&f2, a) || !has(&f2, b) {
                continue;
            }
            let der = |f: &[u32; 3]| -> Option<bool> {
                for i in 0..3 {
                    if f[i] == a && f[(i + 1) % 3] == b {
                        return Some(true);
                    }
                    if f[i] == b && f[(i + 1) % 3] == a {
                        return Some(false);
                    }
                }
                None
            };
            let d1 = der(&f1);
            let d2 = der(&f2);
            if d1.is_none() || d1 == d2 {
                continue;
            }
            if d1 == Some(false) {
                std::mem::swap(&mut a, &mut b);
            }
            let c = *f1.iter().find(|&&x| x != a && x != b).unwrap();
            let d = *f2.iter().find(|&&x| x != a && x != b).unwrap();
            if [a, b, c, d].iter().collect::<HashSet<_>>().len() < 4 {
                continue;
            }
            let nk = ekey2(c, d);
            if emap.contains_key(&nk) || created.contains(&nk) {
                continue;
            }
            if c < n_rim as u32 && d < n_rim as u32 && forbidden.contains(&nk) {
                continue;
            }
            if angle_at(p[c as usize], p[a as usize], p[b as usize])
                + angle_at(p[d as usize], p[a as usize], p[b as usize])
                <= 180.0 + tol_deg
            {
                continue;
            }
            let no1 = cross(sub(p[b as usize], p[a as usize]), sub(p[c as usize], p[a as usize]));
            let no2 = cross(sub(p[a as usize], p[b as usize]), sub(p[d as usize], p[b as usize]));
            let nn1 = cross(sub(p[d as usize], p[a as usize]), sub(p[c as usize], p[a as usize]));
            let nn2 = cross(sub(p[b as usize], p[d as usize]), sub(p[c as usize], p[d as usize]));
            if dot(nn1, no1) <= 0.0
                || dot(nn2, no2) <= 0.0
                || norm(nn1) < 1e-24
                || norm(nn2) < 1e-24
            {
                continue;
            }
            let ci = c as usize;
            let di = d as usize;
            let ai = a as usize;
            let bi = b as usize;
            if tri_min_angle(p, ai, di, ci) < FLIP_MIN_ANGLE
                || tri_min_angle(p, di, bi, ci) < FLIP_MIN_ANGLE
            {
                continue;
            }
            faces[i1 as usize] = [a, d, c];
            faces[i2 as usize] = [d, b, c];
            consumed.insert(i1);
            consumed.insert(i2);
            created.insert(nk);
            n_flip += 1;
        }
        if n_flip == 0 {
            break;
        }
    }
    faces
}

/// Valence regularisation flip.  Returns the relaxed face list.
pub fn valence_relax(
    p: &[V3],
    faces_in: &[[u32; 3]],
    n_rim: usize,
    forbidden: &HashSet<(u32, u32)>,
    max_passes: usize,
) -> Vec<[u32; 3]> {
    let mut faces: Vec<[u32; 3]> = faces_in.to_vec();
    for _ in 0..max_passes {
        let mut val: HashMap<u32, i64> = HashMap::new();
        for f in &faces {
            for &x in f {
                *val.entry(x).or_insert(0) += 1;
            }
        }
        let mut order: Vec<(u32, u32)> = Vec::new();
        let mut emap: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
        for (fi, f) in faces.iter().enumerate() {
            for (u, w) in [(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
                let k = ekey2(u, w);
                if !emap.contains_key(&k) {
                    order.push(k);
                }
                emap.entry(k).or_default().push(fi as u32);
            }
        }
        let cands: Vec<(u32, u32)> = order
            .iter()
            .filter(|k| emap[k].len() == 2)
            .copied()
            .collect();
        let mut consumed: HashSet<u32> = HashSet::new();
        let mut created: HashSet<(u32, u32)> = HashSet::new();
        let mut n_flip = 0usize;
        for key in cands {
            let (mut a, mut b) = key;
            let fl = &emap[&key];
            let (i1, i2) = (fl[0], fl[1]);
            if consumed.contains(&i1) || consumed.contains(&i2) {
                continue;
            }
            let f1 = faces[i1 as usize];
            let f2 = faces[i2 as usize];
            let has = |f: &[u32; 3], x: u32| f[0] == x || f[1] == x || f[2] == x;
            if !has(&f1, a) || !has(&f1, b) || !has(&f2, a) || !has(&f2, b) {
                continue;
            }
            let der = |f: &[u32; 3]| -> Option<bool> {
                for i in 0..3 {
                    if f[i] == a && f[(i + 1) % 3] == b {
                        return Some(true);
                    }
                    if f[i] == b && f[(i + 1) % 3] == a {
                        return Some(false);
                    }
                }
                None
            };
            let d1 = der(&f1);
            let d2 = der(&f2);
            if d1.is_none() || d1 == d2 {
                continue;
            }
            if d1 == Some(false) {
                std::mem::swap(&mut a, &mut b);
            }
            let c = *f1.iter().find(|&&x| x != a && x != b).unwrap();
            let d = *f2.iter().find(|&&x| x != a && x != b).unwrap();
            if [a, b, c, d].iter().collect::<HashSet<_>>().len() < 4 {
                continue;
            }
            let nk = ekey2(c, d);
            if emap.contains_key(&nk) || created.contains(&nk) {
                continue;
            }
            if c < n_rim as u32 && d < n_rim as u32 && forbidden.contains(&nk) {
                continue;
            }
            let tgt = |x: u32| if (x as usize) < n_rim { 4i64 } else { 6i64 };
            let va = *val.get(&a).unwrap_or(&0);
            let vb = *val.get(&b).unwrap_or(&0);
            let vc = *val.get(&c).unwrap_or(&0);
            let vd = *val.get(&d).unwrap_or(&0);
            let before = (va - tgt(a)).pow(2)
                + (vb - tgt(b)).pow(2)
                + (vc - tgt(c)).pow(2)
                + (vd - tgt(d)).pow(2);
            let after = (va - 1 - tgt(a)).pow(2)
                + (vb - 1 - tgt(b)).pow(2)
                + (vc + 1 - tgt(c)).pow(2)
                + (vd + 1 - tgt(d)).pow(2);
            if after >= before {
                continue;
            }
            let no1 = cross(sub(p[b as usize], p[a as usize]), sub(p[c as usize], p[a as usize]));
            let no2 = cross(sub(p[a as usize], p[b as usize]), sub(p[d as usize], p[b as usize]));
            let nn1 = cross(sub(p[d as usize], p[a as usize]), sub(p[c as usize], p[a as usize]));
            let nn2 = cross(sub(p[b as usize], p[d as usize]), sub(p[c as usize], p[d as usize]));
            if dot(nn1, no1) <= 0.0
                || dot(nn2, no2) <= 0.0
                || norm(nn1) < 1e-24
                || norm(nn2) < 1e-24
            {
                continue;
            }
            if tri_min_angle(p, a as usize, d as usize, c as usize) < FLIP_MIN_ANGLE
                || tri_min_angle(p, d as usize, b as usize, c as usize) < FLIP_MIN_ANGLE
            {
                continue;
            }
            faces[i1 as usize] = [a, d, c];
            faces[i2 as usize] = [d, b, c];
            *val.get_mut(&a).unwrap() -= 1;
            *val.get_mut(&b).unwrap() -= 1;
            *val.get_mut(&c).unwrap() += 1;
            *val.get_mut(&d).unwrap() += 1;
            consumed.insert(i1);
            consumed.insert(i2);
            created.insert(nk);
            n_flip += 1;
        }
        if n_flip == 0 {
            break;
        }
    }
    faces
}

/// Sliver cleanup by patch-only edge collapse.  Returns `(p2, faces2)`.
pub fn cleanup_slivers(
    p_in: &[V3],
    faces_in: &[[u32; 3]],
    n_rim: usize,
    min_angle_deg: f64,
    max_iter: usize,
) -> (Vec<V3>, Vec<[u32; 3]>) {
    let p: Vec<V3> = p_in.to_vec();
    let mut faces: Vec<[u32; 3]> = faces_in.to_vec();
    for _ in 0..max_iter {
        if faces.is_empty() {
            break;
        }
        let qa = tri_angles_aspect(&p, &faces);
        let mut worst = 0usize;
        for (i, (mn, _)) in qa.iter().enumerate() {
            if *mn < qa[worst].0 {
                worst = i;
            }
        }
        if qa[worst].0 >= min_angle_deg {
            break;
        }
        let (a, b, c) = (faces[worst][0], faces[worst][1], faces[worst][2]);
        let edges = [(a, b), (b, c), (c, a)];
        let lens = [
            norm(sub(p[a as usize], p[b as usize])),
            norm(sub(p[b as usize], p[c as usize])),
            norm(sub(p[c as usize], p[a as usize])),
        ];
        let mut ei = 0usize;
        for i in 1..3 {
            if lens[i] < lens[ei] {
                ei = i;
            }
        }
        let (u, v) = edges[ei];
        let u_rim = (u as usize) < n_rim;
        let v_rim = (v as usize) < n_rim;
        if u_rim && v_rim && norm(sub(p[u as usize], p[v as usize])) > 1e-12 {
            faces.remove(worst);
            continue;
        }
        let (keep, drop) = if u_rim && !v_rim {
            (u, v)
        } else if v_rim && !u_rim {
            (v, u)
        } else {
            (u, v)
        };
        for f in faces.iter_mut() {
            for k in 0..3 {
                if f[k] == drop {
                    f[k] = keep;
                }
            }
        }
        faces.retain(|f| f[0] != f[1] && f[1] != f[2] && f[2] != f[0]);
    }
    let mut used_set: std::collections::BTreeSet<u32> = (0..n_rim as u32).collect();
    for f in &faces {
        for &x in f {
            used_set.insert(x);
        }
    }
    let used: Vec<u32> = used_set.into_iter().collect();
    let mut remap: HashMap<u32, u32> = HashMap::new();
    for (new, &old) in used.iter().enumerate() {
        remap.insert(old, new as u32);
    }
    let p2: Vec<V3> = used.iter().map(|&i| p[i as usize]).collect();
    let faces2: Vec<[u32; 3]> = faces
        .iter()
        .map(|f| [remap[&f[0]], remap[&f[1]], remap[&f[2]]])
        .collect();
    (p2, faces2)
}

/// Liepa-style patch refinement.  Mutates `p`/`faces` in place.
#[allow(clippy::too_many_arguments)]
pub fn refine_patch(
    p: &mut Vec<V3>,
    faces: &mut Vec<[u32; 3]>,
    n_rim: usize,
    target: f64,
    max_faces: usize,
    rounds: usize,
    flips: bool,
    forbidden: &HashSet<(u32, u32)>,
    med_edge: Option<f64>,
    surround: Option<f64>,
) {
    if faces.len() < 3 {
        return;
    }
    let target = target.max(1e-12);
    let thresh = 2.0f64.sqrt() * target;
    let mut boundary_edges: HashSet<(u32, u32)> = HashSet::new();
    for i in 0..n_rim {
        let a = i as u32;
        let b = ((i + 1) % n_rim) as u32;
        boundary_edges.insert(ekey2(a, b));
    }
    let med = med_edge.unwrap_or(0.0);
    let density = match surround {
        Some(s) => target.max(s),
        None => target,
    };
    let surr = surround.unwrap_or(target);
    let mut rim_len = Vec::with_capacity(n_rim);
    let mut perim = 0.0;
    let mut rim_max = 0.0f64;
    for i in 0..n_rim {
        let a = p[i];
        let b = p[(i + 1) % n_rim];
        let l = norm(sub(b, a));
        rim_len.push(l);
        perim += l;
        if l > rim_max {
            rim_max = l;
        }
    }
    let _ = rim_len;
    let needs_bisect =
        (med > 0.0 && perim > 1.5 * med) || rim_max > thresh || target > 0.05 * surr;
    if needs_bisect {
        let bisect_thresh = 2.0f64.sqrt() * density;
        for _ in 0..rounds {
            let n0 = faces.len();
            let added = subdivide_patch(
                p,
                faces,
                &boundary_edges,
                density,
                max_faces,
                Some(bisect_thresh),
            );
            if added == 0 && faces.len() == n0 {
                break;
            }
        }
        return;
    }

    for _ in 0..40 {
        if faces.len() < 3 {
            break;
        }
        let mut cand: Vec<usize> = Vec::new();
        for (fi, f) in faces.iter().enumerate() {
            let r = circumradius(p, f);
            let longest = tri_longest(p, f);
            if r > thresh || longest > thresh {
                cand.push(fi);
            }
        }
        if !cand.is_empty() {
            let room = max_faces.saturating_sub(faces.len());
            if room == 0 {
                break;
            }
            if cand.len() * 2 > room {
                // descending by circumradius, ties by ascending index.
                cand.sort_by(|&ia, &ib| {
                    circumradius(p, &faces[ib])
                        .partial_cmp(&circumradius(p, &faces[ia]))
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(ia.cmp(&ib))
                });
                let take = (room / 2).max(1);
                cand.truncate(take);
            }
            let split_set: HashSet<usize> = cand.iter().copied().collect();
            let mut new_faces: Vec<[u32; 3]> = Vec::with_capacity(faces.len() + cand.len() * 2);
            for (fi, f) in faces.iter().enumerate() {
                if split_set.contains(&fi) {
                    let (a, b, c) = (f[0], f[1], f[2]);
                    let g = p.len() as u32;
                    let pa = p[a as usize];
                    let pb = p[b as usize];
                    let pc = p[c as usize];
                    p.push([
                        (pa[0] + pb[0] + pc[0]) / 3.0,
                        (pa[1] + pb[1] + pc[1]) / 3.0,
                        (pa[2] + pb[2] + pc[2]) / 3.0,
                    ]);
                    new_faces.push([a, b, g]);
                    new_faces.push([b, c, g]);
                    new_faces.push([c, a, g]);
                } else {
                    new_faces.push(*f);
                }
            }
            *faces = new_faces;
        }
        if flips {
            *faces = flip_relax(p, faces, n_rim, forbidden, 12, 0.5);
            *faces = valence_relax(p, faces, n_rim, forbidden, 8);
        }
        if cand.is_empty() {
            break;
        }
    }
}

/// Aggregate min-angle / aspect percentiles for a patch (report helper).
pub fn quality_stats(p: &[V3], faces: &[[u32; 3]]) -> (usize, f64, f64, f64, f64) {
    let qa = tri_angles_aspect(p, faces);
    if qa.is_empty() {
        return (0, 0.0, 0.0, 0.0, 0.0);
    }
    let mut mins: Vec<f64> = qa.iter().map(|x| x.0).collect();
    let mut asps: Vec<f64> = qa.iter().map(|x| x.1).collect();
    mins.sort_by(|a, b| a.partial_cmp(b).unwrap());
    asps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |v: &[f64], q: f64| -> f64 {
        let n = v.len();
        if n == 0 {
            return 0.0;
        }
        let pos = (q / 100.0) * (n as f64 - 1.0);
        let lo = pos.floor() as usize;
        let hi = pos.ceil() as usize;
        if lo == hi {
            v[lo]
        } else {
            v[lo] + (v[hi] - v[lo]) * (pos - lo as f64)
        }
    };
    (
        qa.len(),
        pct(&mins, 5.0),
        pct(&mins, 50.0),
        pct(&asps, 50.0),
        pct(&asps, 95.0),
    )
}
