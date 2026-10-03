//! Flap: surface-based boundary-loop hole filling (Rust port of the validated
//! Python prototype `/tmp/v073/epoxy/flap.py`, reference state documented in
//! `report.md` §10 and `rust-spec.md`).
//!
//! Every output triangle is either an input triangle (kept verbatim) or a flap
//! face built from a hole's own boundary vertices plus new interior points.
//! No input face is deleted or moved.  The algorithm is Liepa 2003's
//! minimum-area dynamic programme, conforming density refinement and a discrete
//! biharmonic (thin-plate) fairing with a C1 ghost boundary.

pub mod binding;
pub mod dp;
pub mod fair;
pub mod guard;
pub mod refine;
pub mod topology;

pub use binding::flap_fill_py;

use std::collections::{HashMap, HashSet};

/// A 3-D point.
pub type V3 = [f64; 3];
/// A triangle with vertex indices.
pub type T3 = [u32; 3];

const FAIR_SMALL: f64 = 3.0;
const FAIR_LARGE: f64 = 3.0;
const FAIR_RAMP: f64 = 30.0;
const ZERO_VOL_FRAC: f64 = 1e-3;

#[inline]
pub fn ekey(a: u32, b: u32) -> (u32, u32) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Packed undirected edge key `(min << 32) | max`.
#[inline]
pub fn encoded(a: u32, b: u32) -> u64 {
    let (x, y) = ekey(a, b);
    ((x as u64) << 32) | (y as u64)
}

#[inline]
pub fn tkey(a: u32, b: u32) -> (u32, u32) {
    ekey(a, b)
}

/// Disjoint-set union over `n` elements.
pub struct Dsu {
    parent: Vec<u32>,
}

impl Dsu {
    pub fn new(n: usize) -> Self {
        Dsu {
            parent: (0..n as u32).collect(),
        }
    }
    pub fn find(&mut self, mut x: u32) -> u32 {
        while self.parent[x as usize] != x {
            self.parent[x as usize] = self.parent[self.parent[x as usize] as usize];
            x = self.parent[x as usize];
        }
        x
    }
    pub fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra as usize] = rb;
        }
    }
}

pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn norm(a: V3) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
pub fn median(mut xs: Vec<f64>) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = xs.len();
    if n % 2 == 1 {
        xs[n / 2]
    } else {
        0.5 * (xs[n / 2 - 1] + xs[n / 2])
    }
}

/// Half-edges in block-stacked order `[all (0,1), all (1,2), all (2,0)]`.
pub(crate) fn face_half_edges(tris: &[T3]) -> Vec<(u32, u32, u32)> {
    let f = tris.len();
    let mut out = Vec::with_capacity(f * 3);
    for local in 0..3u32 {
        for (fi, t) in tris.iter().enumerate() {
            out.push((fi as u32, t[local as usize], t[((local + 1) % 3) as usize]));
        }
    }
    out
}

/// `edge -> incident faces` for a triangle array.
pub fn edge_faces(tris: &[T3]) -> HashMap<(u32, u32), Vec<u32>> {
    let mut m: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for (fi, t) in tris.iter().enumerate() {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            m.entry(tkey(a, b)).or_default().push(fi as u32);
        }
    }
    m
}

fn fair_fraction(m: usize) -> f64 {
    FAIR_LARGE + (FAIR_SMALL - FAIR_LARGE) * (-((m as f64) - 3.0) / FAIR_RAMP).exp()
}

fn vertex_adjacency(t: &[T3], nv: usize) -> Vec<Vec<u32>> {
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); nv];
    for f in t {
        adj[f[0] as usize].push(f[1]);
        adj[f[1] as usize].push(f[2]);
        adj[f[2] as usize].push(f[0]);
    }
    adj
}

fn vertex_normals(v: &[V3], t: &[T3]) -> Vec<V3> {
    let mut vn = vec![[0.0f64; 3]; v.len()];
    for f in t {
        let (a, b, c) = (v[f[0] as usize], v[f[1] as usize], v[f[2] as usize]);
        let fnv = cross(sub(b, a), sub(c, a));
        for &idx in f {
            vn[idx as usize][0] += fnv[0];
            vn[idx as usize][1] += fnv[1];
            vn[idx as usize][2] += fnv[2];
        }
    }
    for x in vn.iter_mut() {
        let n = norm(*x);
        let n = if n == 0.0 { 1.0 } else { n };
        x[0] /= n;
        x[1] /= n;
        x[2] /= n;
    }
    vn
}

fn percentile(mut v: Vec<f64>, q: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    let pos = (q / 100.0) * (n as f64 - 1.0);
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        v[lo]
    } else {
        v[lo] + (v[hi] - v[lo]) * (pos - lo as f64)
    }
}

#[derive(Clone)]
pub struct FlapParams {
    pub refine: bool,
    pub fair: bool,
    pub max_loop: usize,
    pub max_faces: usize,
    pub collect_quality: bool,
    pub drop_lone_tris: bool,
    pub weld_cracks: bool,
    pub split_nm: bool,
    pub separate_stl: bool,
    pub bridge_open_chains: bool,
    pub weld_max_frac: f64,
    pub orient: String,
    pub avoid_coplanar: bool,
    pub coplanar_frac: f64,
    pub coplanar_cos: f64,
    pub sliver_cleanup: bool,
}

impl Default for FlapParams {
    fn default() -> Self {
        FlapParams {
            refine: true,
            fair: true,
            max_loop: 1200,
            max_faces: 400000,
            collect_quality: false,
            drop_lone_tris: true,
            weld_cracks: true,
            split_nm: true,
            separate_stl: true,
            bridge_open_chains: false,
            weld_max_frac: 0.02,
            orient: "reverse".to_string(),
            avoid_coplanar: true,
            coplanar_frac: 0.05,
            coplanar_cos: 0.99,
            sliver_cleanup: false,
        }
    }
}

#[derive(Clone, Default)]
pub struct LoopLog {
    pub m: usize,
    pub fan: bool,
    pub large: bool,
    pub action: Option<String>,
}

#[derive(Clone, Default)]
pub struct NmCollision {
    pub m: usize,
    pub fan: bool,
    pub edges: Vec<(u32, u32)>,
}

#[derive(Clone, Default)]
pub struct QualityAgg {
    pub n: usize,
    pub min_angle_p5: f64,
    pub min_angle_p50: f64,
    pub aspect_p50: f64,
    pub aspect_p95: f64,
}

#[derive(Clone, Default)]
pub struct Report {
    pub loops_found: usize,
    pub loops_filled: usize,
    pub loops_skipped: usize,
    pub loops_skipped_nm: usize,
    pub loops_skipped_sheet: usize,
    pub loops_fan_fallback: usize,
    pub loops_seeded: usize,
    pub loops_refined: usize,
    pub patch_faces: usize,
    pub new_vertices: usize,
    pub large_loops_fan: usize,
    pub refine: bool,
    pub fair: bool,
    pub split_splits: usize,
    pub split_nm_edges_in: usize,
    pub loops_skipped_coplanar: usize,
    pub loops_skipped_flat: usize,
    pub loops_skipped_sliver: usize,
    pub non_simple_reported: usize,
    pub orient_reversed: usize,
    pub welded_cracks: usize,
    pub dropped_lone_triangles: Option<usize>,
    pub chains_bridged: Option<usize>,
    pub separated_stl: Option<usize>,
    pub boundary_normalize: Option<topology::NormInfo>,
    pub nm_collisions: Vec<NmCollision>,
    pub loop_log: Vec<LoopLog>,
    pub errors: Vec<String>,
    pub quality_before: Option<QualityAgg>,
    pub quality_after: Option<QualityAgg>,
}

/// Run Flap on `verts`/`tris`, returning `(verts_out, tris_out, report)`.
pub fn flap_fill(verts: &[V3], tris: &[T3], prm: &FlapParams) -> (Vec<V3>, Vec<T3>, Report) {
    let mut report = Report {
        refine: prm.refine,
        fair: prm.fair,
        ..Default::default()
    };

    // ---- boundary normalisation / non-manifold split --------------------
    let (mut v2, mut t2): (Vec<V3>, Vec<T3>);
    if prm.weld_cracks {
        let (a, b, info) = topology::normalize_boundary(
            verts,
            tris,
            8,
            Some(prm.weld_max_frac),
            Some(0.1),
            Some(verts.len()),
        );
        v2 = a;
        t2 = b;
        report.welded_cracks = info.welds;
        report.split_splits = info.splits;
        report.split_nm_edges_in = 0;
        report.boundary_normalize = Some(info);
    } else if prm.split_nm {
        let (a, b, si) = topology::split_non_manifold(verts, tris);
        v2 = a;
        t2 = b;
        report.split_splits = si.splits;
        report.split_nm_edges_in = si.nm_edges_in;
    } else {
        v2 = verts.to_vec();
        t2 = tris.to_vec();
    }
    if prm.drop_lone_tris {
        let (a, b, d) = topology::drop_lone_triangles(&v2, &t2);
        v2 = a;
        t2 = b;
        report.dropped_lone_triangles = Some(d);
    }

    // median edge length of the normalised mesh
    let med_edge = {
        let mut lens: Vec<f64> = Vec::with_capacity(t2.len() * 3);
        for f in &t2 {
            lens.push(norm(sub(v2[f[0] as usize], v2[f[1] as usize])));
            lens.push(norm(sub(v2[f[1] as usize], v2[f[2] as usize])));
            lens.push(norm(sub(v2[f[2] as usize], v2[f[0] as usize])));
        }
        if lens.is_empty() {
            1.0
        } else {
            median(lens)
        }
    };

    let mut bridges: Vec<(u32, u32)> = Vec::new();
    if prm.bridge_open_chains {
        bridges = topology::close_open_chains(&v2, &t2, 10000, 8);
        report.chains_bridged = Some(bridges.len());
    }
    let (loops, skipped) = topology::boundary_loops(&v2, &t2, &bridges);
    report.loops_found = loops.len() + skipped;
    report.loops_skipped = skipped;
    report.non_simple_reported = skipped;

    let mut out_v: Vec<V3> = v2.clone();
    let mut out_t: Vec<T3> = t2.clone();

    // ---- edge maps (block-ordered, stable) ------------------------------
    let he = face_half_edges(&t2);
    let mut order: Vec<u32> = (0..he.len() as u32).collect();
    order.sort_by_key(|&i| encoded(he[i as usize].1, he[i as usize].2));
    let mut gcount: HashMap<(u32, u32), u32> = HashMap::new();
    let mut orig_first: HashMap<(u32, u32), u32> = HashMap::new();
    let mut run_start: HashMap<(u32, u32), (u32, u32)> = HashMap::new();
    let mut face_run: Vec<u32> = Vec::with_capacity(he.len());
    {
        let mut i = 0usize;
        while i < order.len() {
            let k = encoded(he[order[i] as usize].1, he[order[i] as usize].2);
            let mut j = i + 1;
            while j < order.len()
                && encoded(he[order[j] as usize].1, he[order[j] as usize].2) == k
            {
                j += 1;
            }
            let h0 = order[i] as usize;
            let key = tkey(he[h0].1, he[h0].2);
            let count = (j - i) as u32;
            let start = face_run.len() as u32;
            for &h in &order[i..j] {
                face_run.push(he[h as usize].0);
            }
            gcount.insert(key, count);
            orig_first.insert(key, face_run[start as usize]);
            run_start.insert(key, (start, count));
            i = j;
        }
    }
    let _ = &orig_first;
    let orig_edge_face = |k: (u32, u32)| -> Option<u32> {
        run_start.get(&k).map(|&(s, _)| face_run[s as usize])
    };
    let edge_faces_of = |k: (u32, u32)| -> Option<&[u32]> {
        run_start
            .get(&k)
            .map(|&(s, c)| &face_run[s as usize..(s + c) as usize])
    };
    let _ = &edge_faces_of;

    let stats = guard::component_stats(&v2, &t2, 2.0);
    let mut nloops_c = vec![0i64; stats.vol_c.len()];
    for lp in &loops {
        let k = tkey(lp[0], lp[1]);
        if let Some(fi) = orig_edge_face(k) {
            nloops_c[stats.lab[fi as usize] as usize] += 1;
        }
    }

    let adj = vertex_adjacency(&t2, v2.len());
    let vnorm = vertex_normals(&v2, &t2);

    let mut extra_ref: Vec<guard::PatchRef> = Vec::new();
    let mut q_before: Vec<(f64, f64)> = Vec::new();
    let mut q_after: Vec<(f64, f64)> = Vec::new();

    for loop_v in &loops {
        let loop_arr = loop_v;
        let m = loop_arr.len();
        if m > prm.max_loop {
            report.large_loops_fan += 1;
        }
        let pts: Vec<V3> = loop_arr.iter().map(|&i| v2[i as usize]).collect();

        // forbidden local chords: an edge that already has >= 2 faces
        let mut forb: HashSet<(u32, u32)> = HashSet::new();
        for a in 0..m {
            let ga = loop_arr[a];
            for b in (a + 1)..m {
                let gb = loop_arr[b];
                let k = tkey(ga, gb);
                if gcount.get(&k).copied().unwrap_or(0) >= 2 {
                    forb.insert((a as u32, b as u32));
                }
            }
        }

        let tri_local = dp::min_area_triangulation(&pts, 1200, &forb);
        let mut is_fan = false;
        let mut p_pts: Vec<V3>;
        let mut faces: Vec<[u32; 3]>;
        if !tri_local.is_empty() {
            p_pts = pts.clone();
            faces = tri_local;
        } else {
            let cm = [
                pts.iter().map(|q| q[0]).sum::<f64>() / m as f64,
                pts.iter().map(|q| q[1]).sum::<f64>() / m as f64,
                pts.iter().map(|q| q[2]).sum::<f64>() / m as f64,
            ];
            p_pts = pts.clone();
            p_pts.push(cm);
            faces = (0..m).map(|i| [i as u32, ((i + 1) % m) as u32, m as u32]).collect();
            is_fan = true;
            report.loops_fan_fallback += 1;
        }

        // Liepa target = median rim edge length
        let mut rim_len: Vec<f64> = Vec::with_capacity(m);
        for i in 0..m {
            rim_len.push(norm(sub(pts[(i + 1) % m], pts[i])));
        }
        let target = if !rim_len.is_empty() {
            median(rim_len.clone())
        } else {
            med_edge
        };
        let large = m > prm.max_loop;
        let mut log = LoopLog {
            m,
            fan: is_fan,
            large,
            action: None,
        };

        // flat-sheet guard
        let k01 = tkey(loop_arr[0], loop_arr[1]);
        let fi_opt = orig_edge_face(k01);
        if let Some(fi) = fi_opt {
            if stats.flat_c[stats.lab[fi as usize] as usize] {
                report.loops_skipped_flat += 1;
                log.action = Some("flat".to_string());
                report.loop_log.push(log);
                continue;
            }
        }

        if prm.collect_quality && !faces.is_empty() {
            for f in &faces {
                let (a, b, c) = (p_pts[f[0] as usize], p_pts[f[1] as usize], p_pts[f[2] as usize]);
                let e0 = norm(sub(b, a));
                let e1 = norm(sub(c, b));
                let e2 = norm(sub(a, c));
                let s = 0.5 * (e0 + e1 + e2);
                let area = (s * (s - e0) * (s - e1) * (s - e2)).max(0.0).sqrt();
                let inr = if s > 0.0 { area / s.max(1e-20) } else { 1e-20 };
                let longest = e0.max(e1).max(e2);
                q_before.push((min_angle(a, b, c), longest / (2.0 * inr.max(1e-20))));
            }
        }

        // local surrounding density (2-ring, rim edges excluded)
        let rim_set: HashSet<u32> = loop_arr.iter().copied().collect();
        let mut ring1: HashSet<u32> = HashSet::new();
        for &gv in loop_arr {
            for &nb in &adj[gv as usize] {
                if !rim_set.contains(&nb) {
                    ring1.insert(nb);
                }
            }
        }
        let mut seen_e: HashSet<(u32, u32)> = HashSet::new();
        let mut surr_lens: Vec<f64> = Vec::new();
        let mut add_edges = |gv: u32, surr_lens: &mut Vec<f64>| {
            for &nb in &adj[gv as usize] {
                if rim_set.contains(&nb) || nb == gv {
                    continue;
                }
                let k = tkey(gv, nb);
                if seen_e.contains(&k) {
                    continue;
                }
                seen_e.insert(k);
                surr_lens.push(norm(sub(v2[gv as usize], v2[nb as usize])));
            }
        };
        for &gv in loop_arr {
            add_edges(gv, &mut surr_lens);
        }
        for &gv in &ring1 {
            add_edges(gv, &mut surr_lens);
        }
        let local_surr = if !surr_lens.is_empty() {
            median(surr_lens)
        } else {
            med_edge
        };

        let n_before_refine = p_pts.len();
        if prm.refine || prm.fair {
            refine::refine_patch(
                &mut p_pts,
                &mut faces,
                m,
                target,
                prm.max_faces,
                6,
                true,
                &forb,
                Some(med_edge),
                Some(local_surr),
            );
        }
        let was_seeded = p_pts.len() > n_before_refine;

        // orientation
        if prm.orient == "reverse" {
            for f in faces.iter_mut() {
                f.swap(1, 2);
            }
            report.orient_reversed += 1;
        } else if m >= 2 {
            if let Some(ofi) = orig_edge_face(k01) {
                let fo = t2[ofi as usize];
                let n_orig = cross(
                    sub(v2[fo[1] as usize], v2[fo[0] as usize]),
                    sub(v2[fo[2] as usize], v2[fo[0] as usize]),
                );
                for f in faces.iter_mut() {
                    let set: HashSet<u32> = f.iter().copied().collect();
                    if set.contains(&0) && set.contains(&1) {
                        let n_flap = cross(
                            sub(p_pts[f[1] as usize], p_pts[f[0] as usize]),
                            sub(p_pts[f[2] as usize], p_pts[f[0] as usize]),
                        );
                        if dot(n_orig, n_flap) < 0.0 {
                            for g in faces.iter_mut() {
                                g.swap(1, 2);
                            }
                        }
                        break;
                    }
                }
            }
        }

        if prm.fair && p_pts.len() > m {
            let centroid = [
                pts.iter().map(|q| q[0]).sum::<f64>() / m as f64,
                pts.iter().map(|q| q[1]).sum::<f64>() / m as f64,
                pts.iter().map(|q| q[2]).sum::<f64>() / m as f64,
            ];
            let mut ghost = vec![[0.0f64; 3]; m];
            let ell = target.max(1e-12) * fair_fraction(m);
            for i in 0..m {
                let gv = loop_arr[i];
                let pv = v2[gv as usize];
                let nrm = vnorm[gv as usize];
                let mut d = sub(centroid, pv);
                let dn = norm(d);
                if dn < 1e-12 {
                    ghost[i] = pv;
                    continue;
                }
                d = [d[0] / dn, d[1] / dn, d[2] / dn];
                let tng = sub(d, [nrm[0] * dot(d, nrm), nrm[1] * dot(d, nrm), nrm[2] * dot(d, nrm)]);
                let tn = norm(tng);
                ghost[i] = if tn > 1e-9 {
                    [pv[0] - ell * tng[0] / tn, pv[1] - ell * tng[1] / tn, pv[2] - ell * tng[2] / tn]
                } else {
                    pv
                };
            }
            fair::fair_patch(&mut p_pts, &faces, m, &ghost);
        }

        if prm.sliver_cleanup && !faces.is_empty() {
            let (np, nf) = refine::cleanup_slivers(&p_pts, &faces, m, 15.0, 500);
            p_pts = np;
            faces = nf;
        }
        if faces.is_empty() {
            report.loops_skipped_sliver += 1;
            log.action = Some("sliver".to_string());
            report.loop_log.push(log);
            continue;
        }

        // zero-volume-shell guard
        if let Some(fi) = fi_opt {
            let comp = stats.lab[fi as usize] as usize;
            if nloops_c[comp] == 1 {
                let mut vp = 0.0f64;
                let mut ap = 0.0f64;
                for f in &faces {
                    let (a, b, c) = (p_pts[f[0] as usize], p_pts[f[1] as usize], p_pts[f[2] as usize]);
                    vp += dot(a, cross(b, c)) / 6.0;
                    ap += 0.5 * norm(cross(sub(b, a), sub(c, a)));
                }
                let tot = (stats.vol_c[comp] + vp).abs();
                let area = stats.area_c[comp] + ap;
                if area > 0.0 && tot <= ZERO_VOL_FRAC * area.powf(1.5) {
                    report.loops_skipped_flat += 1;
                    log.action = Some("zero_vol".to_string());
                    report.loop_log.push(log);
                    continue;
                }
            }
        }

        let base = out_v.len();
        let gmap = |i: usize| -> u32 {
            if i < m {
                loop_arr[i]
            } else {
                (base + (i - m)) as u32
            }
        };
        let gfaces: Vec<T3> = faces
            .iter()
            .map(|f| [gmap(f[0] as usize), gmap(f[1] as usize), gmap(f[2] as usize)])
            .collect();

        // lone single-sided triangle guard
        if m == 3 {
            let mut adjs: HashSet<u32> = HashSet::new();
            for i in 0..3 {
                let k = tkey(loop_arr[i], loop_arr[(i + 1) % 3]);
                if let Some(fs) = edge_faces_of(k) {
                    for &x in fs {
                        adjs.insert(x);
                    }
                }
            }
            if adjs.len() <= 1 {
                report.loops_skipped_sheet += 1;
                log.action = Some("sheet".to_string());
                report.loop_log.push(log);
                continue;
            }
        }

        // non-manifold commit guard
        let mut ce: HashMap<(u32, u32), u32> = HashMap::new();
        for f in &gfaces {
            for (u, w) in [(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
                *ce.entry(tkey(u, w)).or_insert(0) += 1;
            }
        }
        let mut coll: Vec<(u32, u32)> = Vec::new();
        for (&k, &c) in &ce {
            if gcount.get(&k).copied().unwrap_or(0) + c > 2 {
                coll.push(k);
            }
        }
        if !coll.is_empty() {
            report.loops_skipped_nm += 1;
            report.nm_collisions.push(NmCollision {
                m,
                fan: is_fan,
                edges: coll.iter().take(4).copied().collect(),
            });
            log.action = Some("nm".to_string());
            report.loop_log.push(log);
            continue;
        }

        if prm.avoid_coplanar
            && guard::coplanar_collides(
                &p_pts,
                &faces,
                &[],
                &extra_ref,
                prm.coplanar_frac,
                prm.coplanar_cos,
            )
        {
            report.loops_skipped_coplanar += 1;
            log.action = Some("coplanar".to_string());
            report.loop_log.push(log);
            continue;
        }

        // commit
        for j in m..p_pts.len() {
            out_v.push(p_pts[j]);
        }
        out_t.extend_from_slice(&gfaces);
        if prm.avoid_coplanar {
            let (c, n, _ml) = guard::face_cent_norm(&p_pts, &faces);
            let tris: Vec<[V3; 3]> = faces
                .iter()
                .map(|f| {
                    [
                        p_pts[f[0] as usize],
                        p_pts[f[1] as usize],
                        p_pts[f[2] as usize],
                    ]
                })
                .collect();
            extra_ref.push(guard::PatchRef {
                cent: c,
                norm: n,
                tri: tris,
            });
        }
        for (&k, &c) in &ce {
            *gcount.entry(k).or_insert(0) += c;
        }
        report.loops_filled += 1;
        report.patch_faces += faces.len();
        log.action = Some("filled".to_string());
        if was_seeded {
            report.loops_seeded += 1;
            report.loops_refined += 1;
        }
        if prm.collect_quality {
            for f in &faces {
                let (a, b, c) = (p_pts[f[0] as usize], p_pts[f[1] as usize], p_pts[f[2] as usize]);
                let e0 = norm(sub(b, a));
                let e1 = norm(sub(c, b));
                let e2 = norm(sub(a, c));
                let s = 0.5 * (e0 + e1 + e2);
                let area = (s * (s - e0) * (s - e1) * (s - e2)).max(0.0).sqrt();
                let inr = if s > 0.0 { area / s.max(1e-20) } else { 1e-20 };
                let longest = e0.max(e1).max(e2);
                q_after.push((min_angle(a, b, c), longest / (2.0 * inr.max(1e-20))));
            }
        }
        report.loop_log.push(log);
    }

    report.new_vertices = out_v.len() - v2.len();

    if prm.separate_stl && !out_t.is_empty() {
        let (nv, moved) = topology::separate_coincident_stl(&out_v, None, Some(v2.len()));
        out_v = nv;
        report.separated_stl = Some(moved);
    }

    if prm.collect_quality {
        let n_before = q_before.len();
        let mins: Vec<f64> = q_before.iter().map(|x| x.0).collect();
        let asps: Vec<f64> = q_before.iter().map(|x| x.1).collect();
        report.quality_before = Some(QualityAgg {
            n: n_before,
            min_angle_p5: percentile(mins.clone(), 5.0),
            min_angle_p50: percentile(mins, 50.0),
            aspect_p50: percentile(asps.clone(), 50.0),
            aspect_p95: percentile(asps, 95.0),
        });
        let mins: Vec<f64> = q_after.iter().map(|x| x.0).collect();
        let asps: Vec<f64> = q_after.iter().map(|x| x.1).collect();
        report.quality_after = Some(QualityAgg {
            n: q_after.len(),
            min_angle_p5: percentile(mins.clone(), 5.0),
            min_angle_p50: percentile(mins, 50.0),
            aspect_p50: percentile(asps.clone(), 50.0),
            aspect_p95: percentile(asps, 95.0),
        });
    }

    (out_v, out_t, report)
}

#[inline]
fn min_angle(a: V3, b: V3, c: V3) -> f64 {
    let ang = |p: V3, x: V3, y: V3| -> f64 {
        let v1 = sub(x, p);
        let v2 = sub(y, p);
        let d = dot(v1, v2);
        let n = norm(v1) * norm(v2);
        if n < 1e-20 {
            0.0
        } else {
            (d / n).clamp(-1.0, 1.0).acos().to_degrees()
        }
    };
    ang(a, b, c).min(ang(b, a, c)).min(ang(c, a, b))
}
