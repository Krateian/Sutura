//! Topology helpers: non-manifold split, boundary extraction (figure-8 /
//! pinch cutting), iterative crack welding, lone-triangle dropping and the
//! STL reload-safety vertex nudge.

use super::*;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct SplitInfo {
    pub splits: usize,
    pub nm_edges_in: usize,
    pub faces_dropped_degenerate: usize,
}

#[derive(Debug, Clone, Default)]
pub struct NmStats {
    pub boundary_edges: usize,
    pub non_manifold_edges: usize,
    pub faces_on_nm_edges: usize,
}

#[derive(Debug, Clone)]
pub struct WeldSite {
    pub a: V3,
    pub b: V3,
    pub gap: f64,
    pub is_orig: bool,
}

#[derive(Debug, Clone, Default)]
pub struct NormInfo {
    pub iterations: usize,
    pub splits: usize,
    pub welds: usize,
    pub unbalanced: usize,
    pub nm_edges: usize,
    pub weld_sites: Vec<WeldSite>,
}

/// Count boundary and non-manifold edges (edges used by 1 or >2 faces).
pub fn non_manifold_stats(tris: &[T3], _nverts: usize) -> NmStats {
    let mut counts: HashMap<(u32, u32), (u32, u32)> = HashMap::new();
    for (fi, t) in tris.iter().enumerate() {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let e = counts.entry(tkey(a, b)).or_insert((0, fi as u32));
            e.0 += 1;
        }
    }
    let mut s = NmStats::default();
    let mut nm_faces: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for (_, (c, _)) in &counts {
        if *c == 1 {
            s.boundary_edges += 1;
        } else if *c > 2 {
            s.non_manifold_edges += 1;
        }
    }
    for (fi, t) in tris.iter().enumerate() {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            if counts[&tkey(a, b)].0 > 2 {
                nm_faces.insert(fi as u32);
            }
        }
    }
    s.faces_on_nm_edges = nm_faces.len();
    s
}

/// Vertex-fan explosion across edges used by exactly two faces.  Returns
/// `(verts2, tris2, info)`.
pub fn split_non_manifold(verts: &[V3], tris_in: &[T3]) -> (Vec<V3>, Vec<T3>, SplitInfo) {
    let nv = verts.len();
    // Drop triangles with a repeated vertex (degenerate).
    let mut dropped = 0usize;
    let mut tris: Vec<T3> = Vec::with_capacity(tris_in.len());
    for t in tris_in {
        if t[0] != t[1] && t[1] != t[2] && t[2] != t[0] {
            tris.push(*t);
        } else {
            dropped += 1;
        }
    }
    let f = tris.len();
    if f == 0 {
        return (
            verts.to_vec(),
            tris,
            SplitInfo {
                splits: 0,
                nm_edges_in: 0,
                faces_dropped_degenerate: dropped,
            },
        );
    }

    // half-edges in block-stacked order.
    let he = face_half_edges(&tris); // (face, start, end)
    let mut order: Vec<u32> = (0..he.len() as u32).collect();
    order.sort_by_key(|&i| encoded(he[i as usize].1, he[i as usize].2));
    // runs by key
    let mut nm_edges = 0usize;
    let mut dsu = Dsu::new(f * 3);
    let key_at = |i: u32| encoded(he[i as usize].1, he[i as usize].2);
    let mut i = 0usize;
    while i < order.len() {
        let k = key_at(order[i]);
        let mut j = i + 1;
        while j < order.len() && key_at(order[j]) == k {
            j += 1;
        }
        let run = j - i;
        if run == 2 {
            let h0 = order[i] as usize;
            let h1 = order[i + 1] as usize;
            let (s0, e0) = (he[h0].1, he[h0].2);
            let (a, b) = ekey(s0, e0);
            // face_half_edges is local-major: index h == local * f + face.
            let (fa, la) = (h0 % f, (h0 / f) as u32);
            let (fb, lb) = (h1 % f, (h1 / f) as u32);
            // corner (local index) at endpoint v of half-edge `local`.
            let corner_at = |fi: usize, local: u32, v: u32| -> u32 {
                let s = tris[fi][local as usize];
                let e = tris[fi][((local + 1) % 3) as usize];
                let c = if s == v { local } else if e == v {
                    (local + 1) % 3
                } else {
                    (local + 1) % 3
                };
                (fi * 3) as u32 + c
            };
            dsu.union(corner_at(fa, la, a), corner_at(fb, lb, a));
            dsu.union(corner_at(fa, la, b), corner_at(fb, lb, b));
        } else if run > 2 {
            nm_edges += 1;
        }
        i = j;
    }

    // component ids (dense).
    let ncorner = f * 3;
    let mut root_to_dense: HashMap<u32, u32> = HashMap::new();
    let mut comp = vec![0u32; ncorner];
    for c in 0..ncorner as u32 {
        let r = dsu.find(c);
        let next = root_to_dense.len() as u32;
        let id = *root_to_dense.entry(r).or_insert(next);
        comp[c as usize] = id;
    }
    let ncomp = root_to_dense.len();

    // comp_vertex[comp] = corner_vertex, last corner wins (increasing order).
    let mut comp_vertex = vec![0u32; ncomp];
    for c in 0..ncorner {
        let fi = c / 3;
        let li = c % 3;
        comp_vertex[comp[c] as usize] = tris[fi][li];
    }
    let verts2: Vec<V3> = comp_vertex.iter().map(|&v| verts[v as usize]).collect();
    let tris2: Vec<T3> = (0..f)
        .map(|fi| {
            [
                comp[fi * 3],
                comp[fi * 3 + 1],
                comp[fi * 3 + 2],
            ]
        })
        .collect();
    (
        verts2,
        tris2,
        SplitInfo {
            splits: ncomp.saturating_sub(nv),
            nm_edges_in: nm_edges,
            faces_dropped_degenerate: dropped,
        },
    )
}

/// Directed boundary (`start -> [ends]`, insertion-ordered) plus the ordered
/// list of starts (in boundary-key sort order, then any new extra-edge starts).
pub fn directed_boundary(
    tris: &[T3],
    extra_edges: &[(u32, u32)],
) -> (Vec<u32>, HashMap<u32, Vec<u32>>) {
    let f = tris.len();
    let he = face_half_edges(tris);
    let mut order: Vec<u32> = (0..he.len() as u32).collect();
    order.sort_by_key(|&i| encoded(he[i as usize].1, he[i as usize].2));
    let mut de: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut starts: Vec<u32> = Vec::new();
    let mut i = 0usize;
    while i < order.len() {
        let k = encoded(he[order[i] as usize].1, he[order[i] as usize].2);
        let mut j = i + 1;
        while j < order.len()
            && encoded(he[order[j] as usize].1, he[order[j] as usize].2) == k
        {
            j += 1;
        }
        if j - i == 1 {
            let h = order[i] as usize;
            let (s, e) = (he[h].1, he[h].2);
            if !de.contains_key(&s) {
                starts.push(s);
            }
            de.entry(s).or_default().push(e);
        }
        i = j;
    }
    let _ = f;
    for &(s, e) in extra_edges {
        if !de.contains_key(&s) {
            starts.push(s);
        }
        de.entry(s).or_default().push(e);
    }
    (starts, de)
}

/// Cut a raw walk (which may repeat a vertex) into simple loops at repeats.
fn cut_simple(loop_path: &[u32], loops: &mut Vec<Vec<u32>>, skipped: &mut usize, _closed: bool) {
    let mut stack: Vec<u32> = Vec::new();
    let mut seen: HashMap<u32, usize> = HashMap::new();
    for &v in loop_path {
        if let Some(&j) = seen.get(&v) {
            let sub = stack[j..].to_vec();
            let uniq: std::collections::HashSet<u32> = sub.iter().copied().collect();
            if sub.len() >= 3 && uniq.len() == sub.len() {
                loops.push(sub.clone());
            } else {
                *skipped += 1;
            }
            for x in &sub {
                seen.remove(x);
            }
            stack.truncate(j);
        }
        seen.insert(v, stack.len());
        stack.push(v);
    }
    if stack.len() >= 3 {
        let uniq: std::collections::HashSet<u32> = stack.iter().copied().collect();
        if uniq.len() == stack.len() {
            loops.push(stack);
        } else {
            *skipped += 1;
        }
    } else if !stack.is_empty() {
        *skipped += 1;
    }
}

/// Extract simple boundary loops plus the raw open chains.
pub fn boundary_loops_ex(
    _verts: &[V3],
    tris: &[T3],
    extra_edges: &[(u32, u32)],
) -> (Vec<Vec<u32>>, usize, Vec<Vec<u32>>) {
    if tris.is_empty() {
        return (Vec::new(), 0, Vec::new());
    }
    let (starts, mut de) = directed_boundary(tris, extra_edges);
    let mut loops: Vec<Vec<u32>> = Vec::new();
    let mut chains: Vec<Vec<u32>> = Vec::new();
    let mut skipped = 0usize;
    let guard_cap = 5 * tris.len() + 10;
    for s0 in starts {
        loop {
            let has = de.get(&s0).map_or(false, |v| !v.is_empty());
            if !has {
                break;
            }
            let mut cur = s0;
            let mut path = vec![s0];
            let mut closed = false;
            let mut guard = 0usize;
            loop {
                let nxt = match de.get_mut(&cur) {
                    Some(v) if !v.is_empty() => v.pop().unwrap(),
                    _ => break,
                };
                if nxt == s0 {
                    closed = true;
                    break;
                }
                path.push(nxt);
                cur = nxt;
                guard += 1;
                if guard > guard_cap {
                    break;
                }
            }
            if !closed {
                chains.push(path.clone());
            }
            cut_simple(&path, &mut loops, &mut skipped, closed);
        }
    }
    (loops, skipped, chains)
}

/// Boundary loops only (see `boundary_loops_ex`).
pub fn boundary_loops(
    verts: &[V3],
    tris: &[T3],
    extra_edges: &[(u32, u32)],
) -> (Vec<Vec<u32>>, usize) {
    let (loops, skipped, _) = boundary_loops_ex(verts, tris, extra_edges);
    (loops, skipped)
}

/// Bridge each open chain sink->source; iterative.
pub fn close_open_chains(verts: &[V3], tris: &[T3], max_bridges: usize, max_rounds: usize) -> Vec<(u32, u32)> {
    let mut bridges: Vec<(u32, u32)> = Vec::new();
    for _ in 0..max_rounds {
        let (_, _, chains) = boundary_loops_ex(verts, tris, &bridges);
        let mut new: Vec<(u32, u32)> = Vec::new();
        for ch in &chains {
            if ch.len() >= 2 && ch[0] != ch[ch.len() - 1] {
                new.push((ch[ch.len() - 1], ch[0]));
            }
        }
        if new.is_empty() {
            break;
        }
        bridges.extend(new);
        if bridges.len() >= max_bridges {
            break;
        }
    }
    bridges
}

/// Drop isolated single-triangle components; returns `(v2, t2, dropped)`.
pub fn drop_lone_triangles(verts: &[V3], tris: &[T3]) -> (Vec<V3>, Vec<T3>, usize) {
    if tris.len() <= 1 {
        return (verts.to_vec(), tris.to_vec(), 0);
    }
    let f = tris.len();
    let mut dsu = Dsu::new(f);
    let mut counts: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for (fi, t) in tris.iter().enumerate() {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            counts.entry(tkey(a, b)).or_default().push(fi as u32);
        }
    }
    for (_, fs) in &counts {
        if fs.len() == 2 {
            dsu.union(fs[0], fs[1]);
        }
    }
    let mut comp = vec![0u32; f];
    let mut size: HashMap<u32, usize> = HashMap::new();
    for fi in 0..f {
        let r = dsu.find(fi as u32);
        comp[fi] = r;
        *size.entry(r).or_insert(0) += 1;
    }
    let keep: Vec<bool> = (0..f).map(|fi| size[&comp[fi]] >= 2).collect();
    let nkeep = keep.iter().filter(|&&b| b).count();
    if nkeep == 0 || nkeep == f {
        return (verts.to_vec(), tris.to_vec(), f - nkeep);
    }
    let t2: Vec<T3> = (0..f).filter(|&fi| keep[fi]).map(|fi| tris[fi]).collect();
    // compact used vertices (ascending).
    let mut used_set: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    for t in &t2 {
        for &x in t {
            used_set.insert(x);
        }
    }
    let used: Vec<u32> = used_set.into_iter().collect();
    let mut remap: HashMap<u32, u32> = HashMap::new();
    for (new, &old) in used.iter().enumerate() {
        remap.insert(old, new as u32);
    }
    let v2: Vec<V3> = used.iter().map(|&i| verts[i as usize]).collect();
    let t2m: Vec<T3> = t2
        .iter()
        .map(|t| [remap[&t[0]], remap[&t[1]], remap[&t[2]]])
        .collect();
    (v2, t2m, f - nkeep)
}

/// Nudge all-but-one vertex of each float32-coincident group by `diag*1e-5`
/// so an STL reload keeps the in-memory topology.
pub fn separate_coincident_stl(
    verts: &[V3],
    shift: Option<f64>,
    protect_below: Option<usize>,
) -> (Vec<V3>, usize) {
    let mut v = verts.to_vec();
    let n = v.len();
    let f32v: Vec<[f32; 3]> = v
        .iter()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    let mut order: Vec<usize> = (0..n).collect();
    // numpy.lexsort((z, y, x)) -> primary x, then y, then z; stable.
    order.sort_by(|&ia, &ib| {
        let (a, b) = (f32v[ia], f32v[ib]);
        a[0].partial_cmp(&b[0])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a[1].partial_cmp(&b[1]).unwrap_or(std::cmp::Ordering::Equal))
            .then(a[2].partial_cmp(&b[2]).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut same = vec![false; n];
    for i in 1..n {
        same[i] = f32v[order[i]] == f32v[order[i - 1]];
    }
    // group starts
    let mut starts: Vec<usize> = Vec::new();
    for i in 0..n {
        if !same[i] {
            starts.push(i);
        }
    }
    let shift = shift.unwrap_or_else(|| {
        let mut mn = [f64::INFINITY; 3];
        let mut mx = [f64::NEG_INFINITY; 3];
        for p in &v {
            for k in 0..3 {
                mn[k] = mn[k].min(p[k]);
                mx[k] = mx[k].max(p[k]);
            }
        }
        norm([mx[0] - mn[0], mx[1] - mn[1], mx[2] - mn[2]]) * 1e-5
    });
    let mut moved = 0usize;
    for (gi, &st) in starts.iter().enumerate() {
        let end = if gi + 1 < starts.len() { starts[gi + 1] } else { n };
        let kept_idx = order[st];
        let allow = protect_below.map_or(true, |pb| kept_idx >= pb);
        if !allow {
            continue;
        }
        for step in 1..(end - st) {
            let i = order[st + step];
            let s = step;
            let ang = s as f64 * 2.39996323;
            let zc = 1.0 - 2.0 * ((s % 997) as f64 / 997.0);
            let r = (1.0 - zc * zc).max(0.0).sqrt();
            let d = [r * ang.cos(), r * ang.sin(), zc];
            v[i][0] += shift * d[0];
            v[i][1] += shift * d[1];
            v[i][2] += shift * d[2];
            moved += 1;
        }
    }
    (v, moved)
}

/// `(unbalanced_vertices, nm_edges)` for a triangle array.
pub fn boundary_balance(tris: &[T3]) -> (usize, usize) {
    if tris.is_empty() {
        return (0, 0);
    }
    let mut counts: HashMap<u64, u32> = HashMap::new();
    let mut half: Vec<(u32, u32, u64)> = Vec::new();
    for t in tris {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let k = encoded(a, b);
            *counts.entry(k).or_insert(0) += 1;
            half.push((a, b, k));
        }
    }
    let nm_edges = counts.values().filter(|&&c| c > 2).count();
    let boundary: std::collections::HashSet<u64> = counts
        .iter()
        .filter(|(_, &c)| c == 1)
        .map(|(&k, _)| k)
        .collect();
    let mut outd: HashMap<u32, i64> = HashMap::new();
    let mut ind: HashMap<u32, i64> = HashMap::new();
    for (s, e, k) in &half {
        if boundary.contains(k) {
            *outd.entry(*s).or_insert(0) += 1;
            *ind.entry(*e).or_insert(0) += 1;
        }
    }
    let mut allv: std::collections::HashSet<u32> = std::collections::HashSet::new();
    allv.extend(outd.keys().copied());
    allv.extend(ind.keys().copied());
    let unbalanced = allv
        .iter()
        .filter(|v| outd.get(v).copied().unwrap_or(0) != ind.get(v).copied().unwrap_or(0))
        .count();
    (unbalanced, nm_edges)
}

/// Merge each open chain's source onto its sink (bounded), returning the new
/// mesh, the merge count and the recorded weld sites.
pub fn weld_open_chains(
    verts: &[V3],
    tris: &[T3],
    max_rounds: usize,
    max_merge_frac: Option<f64>,
    move_tol: Option<f64>,
    protect_below: Option<usize>,
) -> (Vec<V3>, Vec<T3>, usize, Vec<WeldSite>) {
    let mut v = verts.to_vec();
    let mut t = tris.to_vec();
    let mut merges = 0usize;
    let mut sites: Vec<WeldSite> = Vec::new();
    let mut max_merge_frac = max_merge_frac;
    let mut diag = 0.0f64;
    if let Some(_mf) = max_merge_frac {
        let mut mn = [f64::INFINITY; 3];
        let mut mx = [f64::NEG_INFINITY; 3];
        for p in verts {
            for k in 0..3 {
                mn[k] = mn[k].min(p[k]);
                mx[k] = mx[k].max(p[k]);
            }
        }
        diag = norm([mx[0] - mn[0], mx[1] - mn[1], mx[2] - mn[2]]);
        if diag <= 0.0 {
            max_merge_frac = None;
        }
    }
    for _ in 0..max_rounds {
        let (_, _, chains) = boundary_loops_ex(&v, &t, &[]);
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        for ch in &chains {
            if ch.len() >= 2 && ch[0] != ch[ch.len() - 1] {
                let a = ch[0];
                let b = ch[ch.len() - 1];
                let gap = norm(sub(v[a as usize], v[b as usize]));
                if let Some(mt) = move_tol {
                    if gap > mt {
                        continue;
                    }
                }
                if let Some(mf) = max_merge_frac {
                    if gap > mf * diag {
                        continue;
                    }
                }
                pairs.push((a, b));
            }
        }
        if pairs.is_empty() {
            break;
        }
        let n = v.len();
        let mut dsu = Dsu::new(n);
        for &(a, b) in &pairs {
            let ra = dsu.find(a);
            let rb = dsu.find(b);
            if ra != rb {
                dsu.union(a, b);
                if protect_below.is_some() {
                    let is_orig = a < protect_below.unwrap() as u32
                        || b < protect_below.unwrap() as u32;
                    sites.push(WeldSite {
                        a: v[a as usize],
                        b: v[b as usize],
                        gap: norm(sub(v[a as usize], v[b as usize])),
                        is_orig,
                    });
                }
            }
        }
        let roots: Vec<u32> = (0..n).map(|x| dsu.find(x as u32)).collect();
        let mut used: Vec<u32> = t.iter().flatten().copied().collect();
        used.sort_unstable();
        used.dedup();
        let mut remap: HashMap<u32, u32> = HashMap::new();
        let mut vlist: Vec<V3> = Vec::new();
        for &x in &used {
            let r = roots[x as usize];
            if !remap.contains_key(&r) {
                remap.insert(r, vlist.len() as u32);
                vlist.push(v[x as usize]);
            }
        }
        let t_new: Vec<T3> = t
            .iter()
            .map(|f| {
                [
                    remap[&roots[f[0] as usize]],
                    remap[&roots[f[1] as usize]],
                    remap[&roots[f[2] as usize]],
                ]
            })
            .collect();
        let changed = vlist.len() < n;
        if !vlist.is_empty() {
            v = vlist;
        }
        t = t_new;
        if changed {
            merges += 1;
        } else {
            break;
        }
    }
    (v, t, merges, sites)
}

/// Alternate the fan split and the crack weld until the boundary is simple,
/// closed and manifold; keeps the best `(unbalanced, nm)` seen.
pub fn normalize_boundary(
    verts: &[V3],
    tris: &[T3],
    max_iter: usize,
    max_merge_frac: Option<f64>,
    move_tol: Option<f64>,
    protect_below: Option<usize>,
) -> (Vec<V3>, Vec<T3>, NormInfo) {
    let mut v = verts.to_vec();
    let mut t = tris.to_vec();
    let mut info = NormInfo::default();
    let mut best: (Vec<V3>, Vec<T3>, (usize, usize)) = (
        v.clone(),
        t.clone(),
        (usize::MAX, usize::MAX),
    );
    for it in 1..=max_iter {
        let (v2, t2, si) = split_non_manifold(&v, &t);
        v = v2;
        t = t2;
        let (v3, t3, wm, sites) = weld_open_chains(
            &v,
            &t,
            16,
            max_merge_frac,
            move_tol,
            protect_below,
        );
        v = v3;
        t = t3;
        info.weld_sites.extend(sites);
        let (ub, nm) = boundary_balance(&t);
        info.iterations = it;
        info.splits = si.splits;
        info.welds = wm;
        info.unbalanced = ub;
        info.nm_edges = nm;
        let score = (ub, nm);
        if score < best.2 {
            best = (v.clone(), t.clone(), score);
        }
        if ub == 0 && nm == 0 {
            return (v, t, info);
        }
    }
    (best.0, best.1, info)
}
