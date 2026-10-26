// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! CDT3D Phase C: cell contraction, winding unary, constrained min-cut and
//! extraction.
//!
//! Cells are the unions of tets across non-constraint faces.  A PLC constraint
//! face is a cut edge between the two cells on its sides.  The output surface
//! is the subset of constraint faces whose incident cells have different
//! labels; open boundary loops are capped and reported separately.

use crate::cdt3d::dt::sorted_face;
use crate::cdt3d::face::FaceMesh;
use crate::cdt3d::plc::Plc;
use crate::dressing::orient_consistently;
use crate::dual_contour::mesh_is_manifold;
use crate::hole::{boundary_topology, close_holes_core};
use crate::mincut::BkGraph;
use crate::point::Point3;
use crate::splint::emit::snap_and_dedup_f32;
use crate::splint::label::{label_tets, label_tets_winding};
use crate::splint::normalize::Normalizer;
use crate::shell::compute_shell_components;
use crate::splint::proxy;
use crate::topology::re_orient_faces_coherently_core;
use crate::winding::MeshBvh;
use std::collections::{BTreeMap, BTreeSet};

const INF: f64 = 1e300;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelMode {
    GwnThreshold,
    MinCut,
}

impl Default for LabelMode {
    fn default() -> Self {
        Self::GwnThreshold
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SolidOptions {
    pub label_mode: LabelMode,
    pub lambda: f64,
    pub winding_cap: f64,
    pub proxy_res: usize,
    pub constraint_fraction: f64,
    pub max_tets: usize,
    pub require_certificate: bool,
    pub cap_max_loop: i64,
    pub min_component_volume_frac: f64,
}

impl Default for SolidOptions {
    fn default() -> Self {
        Self {
            label_mode: LabelMode::default(),
            lambda: 0.5,
            winding_cap: 0.5,
            proxy_res: 128,
            constraint_fraction: 0.05,
            max_tets: 250_000,
            require_certificate: true,
            cap_max_loop: 200,
            min_component_volume_frac: 0.01,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ComponentOrientation {
    pub faces: usize,
    pub signed_volume: f64,
    pub closed: bool,
    pub flipped: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SolidReport {
    pub cells: usize,
    pub tet_count: usize,
    pub constraint_faces: usize,
    pub open_input: bool,
    pub gwn_threshold: bool,
    pub orientation_used: bool,
    pub orientation_components: usize,
    pub orientation_flipped: usize,
    pub orientation_fallback: bool,
    pub orientation: Vec<ComponentOrientation>,
    pub w_hist: [usize; 10],
    pub proxy_shell_vertices: usize,
    pub proxy_shell_faces: usize,
    pub constraint_cut_faces: usize,
    pub constraint_cut_area: f64,
    pub free_cut_faces: usize,
    pub free_cut_area: f64,
    pub self_loop_faces: usize,
    pub boundary_constraint_faces: usize,
    pub inside_cells: usize,
    pub contracted: bool,
    pub output_faces: usize,
    pub cap_faces: usize,
    pub cap_area: f64,
    pub parts: usize,
    pub holes: usize,
    pub nonmanifold: usize,
    pub watertight: bool,
    pub two_faces_per_edge: bool,
    pub signed_volume: f64,
    pub positive_volume: bool,
    pub bbox_ok: bool,
    pub bbox_error: [f64; 3],
    pub hausdorff_output_to_input: f64,
    pub hausdorff_input_to_output: f64,
    pub hausdorff: f64,
    pub hausdorff_output_to_input_full: f64,
    pub hausdorff_input_to_output_full: f64,
    pub hausdorff_full: f64,
    pub valid: bool,
    pub hash: u64,
}

#[derive(Clone, Debug)]
pub struct SolidMesh {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[usize; 3]>,
    pub plc_face_count: usize,
    pub report: SolidReport,
}

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let ra = find(parent, a);
    let rb = find(parent, b);
    if ra == rb {
        return;
    }
    if ra < rb {
        parent[rb] = ra;
    } else {
        parent[ra] = rb;
    }
}

fn tet_volume6(points: &[Point3], t: &[usize; 4]) -> f64 {
    let a = points[t[0]].to_f64().unwrap_or([0.0; 3]);
    let b = points[t[1]].to_f64().unwrap_or([0.0; 3]);
    let c = points[t[2]].to_f64().unwrap_or([0.0; 3]);
    let d = points[t[3]].to_f64().unwrap_or([0.0; 3]);
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let ad = [d[0] - a[0], d[1] - a[1], d[2] - a[2]];
    let cross = [
        ac[1] * ad[2] - ac[2] * ad[1],
        ac[2] * ad[0] - ac[0] * ad[2],
        ac[0] * ad[1] - ac[1] * ad[0],
    ];
    (ab[0] * cross[0] + ab[1] * cross[1] + ab[2] * cross[2]).abs()
}

fn tri_area(points: &[Point3], f: [usize; 3]) -> f64 {
    let a = points[f[0]].to_f64().unwrap_or([0.0; 3]);
    let b = points[f[1]].to_f64().unwrap_or([0.0; 3]);
    let c = points[f[2]].to_f64().unwrap_or([0.0; 3]);
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()
}

fn output_parts(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> usize {
    if tris.is_empty() {
        return 0;
    }
    let mut parent: Vec<usize> = (0..verts.len()).collect();
    for t in tris {
        union(&mut parent, t[0], t[1]);
        union(&mut parent, t[0], t[2]);
    }
    let mut roots = BTreeSet::new();
    for t in tris {
        roots.insert(find(&mut parent, t[0]));
    }
    roots.len()
}

fn aabb(verts: &[[f64; 3]]) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in verts {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    (lo, hi)
}

fn hash_output(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> u64 {
    let mut h = FNV_OFFSET;
    let mut step = |x: u64| {
        h ^= x;
        h = h.wrapping_mul(FNV_PRIME);
    };
    step(verts.len() as u64);
    for p in verts {
        for a in 0..3 {
            step(p[a].to_bits());
        }
    }
    step(tris.len() as u64);
    for t in tris {
        for &v in t {
            step(v as u64);
        }
    }
    h
}

fn surface_samples(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> Vec<[f64; 3]> {
    let mut out = Vec::with_capacity(verts.len() + tris.len() * 8);
    out.extend_from_slice(verts);
    for t in tris {
        let a = verts[t[0]];
        let b = verts[t[1]];
        let c = verts[t[2]];
        let mid = |p: [f64; 3], q: [f64; 3]| {
            [(p[0] + q[0]) * 0.5, (p[1] + q[1]) * 0.5, (p[2] + q[2]) * 0.5]
        };
        out.push(mid(a, b));
        out.push(mid(b, c));
        out.push(mid(c, a));
        out.push([
            (a[0] + b[0] + c[0]) / 3.0,
            (a[1] + b[1] + c[1]) / 3.0,
            (a[2] + b[2] + c[2]) / 3.0,
        ]);
        out.push([
            (2.0 * a[0] + b[0] + c[0]) / 4.0,
            (2.0 * a[1] + b[1] + c[1]) / 4.0,
            (2.0 * a[2] + b[2] + c[2]) / 4.0,
        ]);
        out.push([
            (a[0] + 2.0 * b[0] + c[0]) / 4.0,
            (a[1] + 2.0 * b[1] + c[1]) / 4.0,
            (a[2] + 2.0 * b[2] + c[2]) / 4.0,
        ]);
        out.push([
            (a[0] + b[0] + 2.0 * c[0]) / 4.0,
            (a[1] + b[1] + 2.0 * c[1]) / 4.0,
            (a[2] + b[2] + 2.0 * c[2]) / 4.0,
        ]);
    }
    out
}

fn hausdorff(
    input_bvh: &MeshBvh,
    output_bvh: &MeshBvh,
    output_verts: &[[f64; 3]],
    output_tris: &[[usize; 3]],
    input_verts: &[[f64; 3]],
    input_tris: &[[usize; 3]],
) -> (f64, f64) {
    let mut o2i = 0.0f64;
    for p in surface_samples(output_verts, output_tris) {
        o2i = o2i.max(input_bvh.unsigned_distance_at(p));
    }
    let mut i2o = 0.0f64;
    for p in surface_samples(input_verts, input_tris) {
        i2o = i2o.max(output_bvh.unsigned_distance_at(p));
    }
    (o2i, i2o)
}

fn h7_proxy_phi(
    unary_verts: &[[f64; 3]],
    unary_tris: &[[usize; 3]],
    centroids: &[[f64; 3]],
    out: &[bool],
    opts: &SolidOptions,
) -> Result<(Vec<f64>, usize, usize, f64), String> {
    let normalizer = Normalizer::new(unary_verts, opts.proxy_res);
    let nverts: Vec<[f64; 3]> = unary_verts.iter().map(|p| normalizer.norm(*p)).collect();
    let bvh = MeshBvh::from_arrays(&nverts, unary_tris);
    let proxy = proxy::build(&bvh, opts.proxy_res, normalizer.eps(), None, false)?;
    if proxy.shell_len == 0 || proxy.shell_tris.is_empty() {
        return Err("H7 proxy shell is empty".into());
    }
    let shell_bvh = MeshBvh::from_arrays(&proxy.verts[..proxy.shell_len], &proxy.shell_tris);
    let ncentroids: Vec<[f64; 3]> = centroids.iter().map(|p| normalizer.norm(*p)).collect();
    let phi = label_tets(&shell_bvh, &ncentroids, out, false).phi;
    let h5_tol = 4.0 * proxy.voxel / normalizer.scale();
    Ok((
        phi,
        proxy.shell_len,
        proxy.shell_tris.len(),
        h5_tol,
    ))
}

/// Build the cut surface from the recovered CDT.
pub fn build_solid(
    face: &FaceMesh,
    plc: &Plc,
    unary_verts: &[[f64; 3]],
    unary_tris: &[[usize; 3]],
    opts: &SolidOptions,
) -> Result<SolidMesh, String> {
    if !face.report.valid
        || (opts.require_certificate && !face.report.coverage_certified)
    {
        return Err("P5 needs a valid face recovery with the B5 certificate".into());
    }
    let points = &face.points;
    let tets = &face.tets;
    let neighbours = &face.neighbours;
    let n_tets = tets.len();
    if n_tets > opts.max_tets {
        return Err(format!(
            "P5 tet budget exceeded: {} > {}",
            n_tets, opts.max_tets
        ));
    }
    if opts.label_mode == LabelMode::GwnThreshold {
        return build_solid_gwn(face, plc, unary_verts, unary_tris, opts);
    }
    if plc.report.boundary_edges > 0 {
        return build_solid_tets(face, plc, unary_verts, unary_tris, opts);
    }
    let constraints: BTreeSet<[usize; 3]> = face.constraint_faces.iter().copied().collect();

    let mut parent: Vec<usize> = (0..n_tets).collect();
    for ti in 0..n_tets {
        for lv in 0..4 {
            let n = neighbours[ti][lv];
            if n < 0 {
                continue;
            }
            let key = sorted_face(&tets[ti], lv);
            if !constraints.contains(&key) {
                union(&mut parent, ti, n as usize);
            }
        }
    }

    let mut root_cell: BTreeMap<usize, usize> = BTreeMap::new();
    let mut cell_of_tet = vec![0usize; n_tets];
    for ti in 0..n_tets {
        let r = find(&mut parent, ti);
        let next = root_cell.len();
        let c = *root_cell.entry(r).or_insert(next);
        cell_of_tet[ti] = c;
    }
    let n_cells = root_cell.len();
    if std::env::var_os("CDT3D_DEBUG_SOLID").is_some() {
        eprintln!(
            "solid: tets={} constraints={} cells={}",
            n_tets,
            constraints.len(),
            n_cells
        );
    }

    let mut cell_vol = vec![0.0f64; n_cells];
    let mut cell_cen = vec![[0.0f64; 3]; n_cells];
    let mut cell_first_tet = vec![usize::MAX; n_cells];
    let mut sink_cell = vec![false; n_cells];
    for ti in 0..n_tets {
        let c = cell_of_tet[ti];
        if cell_first_tet[c] == usize::MAX {
            cell_first_tet[c] = ti;
        }
        let v6 = tet_volume6(points, &tets[ti]);
        let vol = v6 / 6.0;
        let mut cen = [0.0f64; 3];
        for &p in &tets[ti] {
            let q = points[p].to_f64().unwrap_or([0.0; 3]);
            for a in 0..3 {
                cen[a] += q[a] * 0.25;
            }
        }
        cell_vol[c] += vol;
        for a in 0..3 {
            cell_cen[c][a] += cen[a] * vol;
        }
        for lv in 0..4 {
            if neighbours[ti][lv] < 0 {
                sink_cell[c] = true;
            }
        }
    }
    for c in 0..n_cells {
        if cell_vol[c] > 1e-18 {
            for a in 0..3 {
                cell_cen[c][a] /= cell_vol[c];
            }
        } else if cell_first_tet[c] != usize::MAX {
            let t = tets[cell_first_tet[c]];
            for a in 0..3 {
                let mut s = 0.0;
                for &p in &t {
                    s += points[p].to_f64().unwrap_or([0.0; 3])[a];
                }
                cell_cen[c][a] = s * 0.25;
            }
        }
    }

    let mut occ: BTreeMap<[usize; 3], Vec<(usize, usize)>> = BTreeMap::new();
    for ti in 0..n_tets {
        for lv in 0..4 {
            let key = sorted_face(&tets[ti], lv);
            if constraints.contains(&key) {
                occ.entry(key).or_default().push((ti, lv));
            }
        }
    }

    struct CutEdge {
        key: [usize; 3],
        ca: usize,
        cb: usize,
        tia: usize,
        lva: usize,
        tib: usize,
        lvb: usize,
        area: f64,
    }

    let mut edges: Vec<CutEdge> = Vec::new();
    let mut boundary_edges: Vec<([usize; 3], usize, usize, usize, f64)> = Vec::new();
    let mut self_loops = 0usize;
    for (key, list) in &occ {
        if list.len() == 1 {
            let (ti, lv) = list[0];
            boundary_edges.push((*key, cell_of_tet[ti], ti, lv, tri_area(points, *key)));
            continue;
        }
        if list.len() != 2 {
            return Err(format!(
                "constraint face {key:?} has {} incident tets",
                list.len()
            ));
        }
        let (tia, lva) = list[0];
        let (tib, lvb) = list[1];
        let ca = cell_of_tet[tia];
        let cb = cell_of_tet[tib];
        if ca == cb {
            self_loops += 1;
            continue;
        }
        edges.push(CutEdge {
            key: *key,
            ca,
            cb,
            tia,
            lva,
            tib,
            lvb,
            area: tri_area(points, *key),
        });
    }

    if edges.is_empty() {
        return build_solid_tets(face, plc, unary_verts, unary_tris, opts);
    }

    let (phi, proxy_shell_vertices, proxy_shell_faces, h5_tol) =
        h7_proxy_phi(unary_verts, unary_tris, &cell_cen, &sink_cell, opts)?;
    if std::env::var_os("CDT3D_DEBUG_SOLID").is_some() {
        let pmin = phi.iter().copied().fold(f64::INFINITY, f64::min);
        let pmax = phi.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        eprintln!(
            "solid: edges={} selfloops={} boundary={} phi=[{pmin:.4},{pmax:.4}] sink={}",
            edges.len(),
            self_loops,
            boundary_edges.len(),
            sink_cell.iter().filter(|&&s| s).count()
        );
    }
    let mut g = BkGraph::new(n_cells);
    for c in 0..n_cells {
        if sink_cell[c] {
            g.add_tweights(c, 0.0, INF);
        } else if phi[c] < 0.0 {
            g.add_tweights(c, INF, 0.0);
        }
    }
    for e in &edges {
        let cap = e.area * opts.lambda;
        g.add_edge(e.ca, e.cb, cap, cap);
    }
    for (_, c, _, _, area) in &boundary_edges {
        g.add_tweights(*c, 0.0, area * opts.lambda);
    }
    let _flow = g.maxflow();
    let mut inside: Vec<bool> = (0..n_cells)
        .map(|c| g.is_in_source(c) && !sink_cell[c])
        .collect();

    let mut cell_adj: Vec<Vec<usize>> = vec![Vec::new(); n_cells];
    for e in &edges {
        cell_adj[e.ca].push(e.cb);
        cell_adj[e.cb].push(e.ca);
    }
    let total_vol: f64 = cell_vol.iter().sum();
    let mut visited = vec![false; n_cells];
    for start in 0..n_cells {
        if !inside[start] || visited[start] {
            continue;
        }
        let mut comp = Vec::new();
        let mut q = vec![start];
        visited[start] = true;
        while let Some(c) = q.pop() {
            comp.push(c);
            for &n in &cell_adj[c] {
                if inside[n] && !visited[n] {
                    visited[n] = true;
                    q.push(n);
                }
            }
        }
        comp.sort_unstable();
        let vol: f64 = comp.iter().map(|&c| cell_vol[c]).sum();
        let confident = comp.iter().any(|&c| phi[c] < 0.0);
        if !confident || vol < opts.min_component_volume_frac * total_vol {
            for &c in &comp {
                inside[c] = false;
            }
        }
    }
    let inside_cells = inside.iter().filter(|&&x| x).count();
    if std::env::var_os("CDT3D_DEBUG_SOLID").is_some() {
        eprintln!("solid: inside_cells={inside_cells} total_vol={total_vol:.6}");
    }
    if inside_cells == 0 {
        return Err("constrained cut found no inside cell".into());
    }

    let mut faces: Vec<[usize; 3]> = Vec::new();
    for e in &edges {
        let la = inside[e.ca];
        let lb = inside[e.cb];
        if la == lb {
            continue;
        }
        let (ti, lv) = if la { (e.tia, e.lva) } else { (e.tib, e.lvb) };
        let (a, b, c, _) = crate::cdt3d::segment::oriented_face_apex(points, &tets[ti], lv);
        faces.push([a, b, c]);
        let _ = e.key;
    }
    for (key, c, ti, lv, _) in &boundary_edges {
        if inside[*c] {
            let (a, b, cc, _) =
                crate::cdt3d::segment::oriented_face_apex(points, &tets[*ti], *lv);
            faces.push([a, b, cc]);
        }
        let _ = key;
    }

    let meta = SolidMeta {
        cells: n_cells,
        tet_count: n_tets,
        constraint_faces: constraints.len(),
        open_input: false,
        gwn_threshold: false,
        proxy_shell_vertices,
        proxy_shell_faces,
        free_cut_faces: 0,
        free_cut_area: 0.0,
        self_loop_faces: self_loops,
        boundary_constraint_faces: boundary_edges.len(),
        inside_cells,
        contracted: true,
    };
    finish_solid(
        points,
        plc,
        faces,
        Vec::new(),
        unary_verts,
        unary_tris,
        h5_tol,
        opts,
        meta,
    )
}

struct SolidMeta {
    cells: usize,
    tet_count: usize,
    constraint_faces: usize,
    open_input: bool,
    gwn_threshold: bool,
    proxy_shell_vertices: usize,
    proxy_shell_faces: usize,
    free_cut_faces: usize,
    free_cut_area: f64,
    self_loop_faces: usize,
    boundary_constraint_faces: usize,
    inside_cells: usize,
    contracted: bool,
}

fn snap_compact(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
    let (raw, snapped) = snap_and_dedup_f32(verts, tris);
    let mut remap = vec![usize::MAX; raw.len()];
    let mut out_v: Vec<[f64; 3]> = Vec::new();
    let mut out_t: Vec<[usize; 3]> = Vec::with_capacity(snapped.len());
    for t in &snapped {
        let mut nt = [0usize; 3];
        for k in 0..3 {
            let v = t[k];
            if remap[v] == usize::MAX {
                remap[v] = out_v.len();
                out_v.push(raw[v]);
            }
            nt[k] = remap[v];
        }
        out_t.push(nt);
    }
    (out_v, out_t)
}

fn finish_solid(
    points: &[Point3],
    plc: &Plc,
    plc_faces: Vec<[usize; 3]>,
    pre_caps: Vec<[usize; 3]>,
    unary_verts: &[[f64; 3]],
    unary_tris: &[[usize; 3]],
    h5_tol: f64,
    opts: &SolidOptions,
    meta: SolidMeta,
) -> Result<SolidMesh, String> {
    let all_verts: Vec<[f64; 3]> = points
        .iter()
        .map(|p| p.to_f64().unwrap_or([0.0; 3]))
        .collect();
    let plc_face_count = plc_faces.len();
    let constraint_cut_area: f64 = plc_faces
        .iter()
        .map(|&f| tri_area(points, f))
        .sum();
    let tris_i64: Vec<[i64; 3]> = plc_faces
        .iter()
        .map(|f| [f[0] as i64, f[1] as i64, f[2] as i64])
        .collect();
    let mut out_tris: Vec<[usize; 3]> = plc_faces.clone();
    let mut cap_faces = 0usize;
    let mut cap_area = 0.0f64;
    if !pre_caps.is_empty() {
        cap_faces = pre_caps.len();
        for &f in &pre_caps {
            cap_area += tri_area(points, f);
            out_tris.push(f);
        }
    } else if !plc_faces.is_empty() {
        if let Ok(closed) = close_holes_core(&all_verts, &tris_i64, opts.cap_max_loop, false) {
            if closed.tris.len() > tris_i64.len() {
                cap_faces = closed.tris.len() - tris_i64.len();
                for t in &closed.tris[tris_i64.len()..] {
                    let f = [t[0] as usize, t[1] as usize, t[2] as usize];
                    cap_area += tri_area(points, f);
                    out_tris.push(f);
                }
            }
        }
    }

    let em = crate::splint::emit::Emitted {
        verts: all_verts.clone(),
        tris: out_tris.clone(),
        out_manifold: false,
        output_parts: 0,
    };
    let (em, _h5_moved) =
        crate::splint::emit::project_emitted(em, unary_verts, unary_tris, h5_tol);
    let (out_verts, tris) = snap_compact(&em.verts, &em.tris);
    let snapped_u32: Vec<[u32; 3]> = tris
        .iter()
        .map(|t| [t[0] as u32, t[1] as u32, t[2] as u32])
        .collect();
    let manifold = mesh_is_manifold(&out_verts, &snapped_u32);
    let parts = output_parts(&out_verts, &tris);
    let holes = if tris.is_empty() {
        0
    } else {
        let si: Vec<[i64; 3]> = tris
            .iter()
            .map(|t| [t[0] as i64, t[1] as i64, t[2] as i64])
            .collect();
        boundary_topology(&si, false)
            .map(|b| b.loops.len())
            .unwrap_or(0)
    };
    let (ilo, ihi) = aabb(unary_verts);
    let (olo, ohi) = aabb(&out_verts);
    let mut bbox_error = [0.0f64; 3];
    let mut bbox_ok = true;
    for a in 0..3 {
        let ie = ihi[a] - ilo[a];
        let oe = ohi[a] - olo[a];
        bbox_error[a] = (oe - ie).abs();
        let tol = 0.01 * ie.abs().max(1e-12) + 1e-9;
        if bbox_error[a] > tol {
            bbox_ok = false;
        }
    }

    let input_bvh = MeshBvh::from_arrays(unary_verts, unary_tris);
    let (shape_v, shape_t) = snap_compact(&all_verts, &plc_faces);
    let shape_bvh = MeshBvh::from_arrays(&shape_v, &shape_t);
    let (o2i, i2o) = hausdorff(
        &input_bvh,
        &shape_bvh,
        &shape_v,
        &shape_t,
        unary_verts,
        unary_tris,
    );
    let haus = o2i.max(i2o);
    let full_bvh = MeshBvh::from_arrays(&out_verts, &tris);
    let (fo2i, fi2o) = hausdorff(
        &input_bvh,
        &full_bvh,
        &out_verts,
        &tris,
        unary_verts,
        unary_tris,
    );

    let mut edge_counts: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for t in &tris {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a < b { (a, b) } else { (b, a) };
            *edge_counts.entry(key).or_insert(0) += 1;
        }
    }
    let two_faces_per_edge = !tris.is_empty()
        && edge_counts.values().all(|&n| n == 2);
    let watertight = holes == 0 && two_faces_per_edge;
    let mut signed_volume = 0.0f64;
    for t in &tris {
        let a = out_verts[t[0]];
        let b = out_verts[t[1]];
        let c = out_verts[t[2]];
        signed_volume += (a[0] * (b[1] * c[2] - b[2] * c[1])
            + a[1] * (b[2] * c[0] - b[0] * c[2])
            + a[2] * (b[0] * c[1] - b[1] * c[0]))
            / 6.0;
    }
    let positive_volume = signed_volume > 0.0;

    let mut report = SolidReport {
        cells: meta.cells,
        tet_count: meta.tet_count,
        constraint_faces: meta.constraint_faces,
        open_input: meta.open_input,
        gwn_threshold: meta.gwn_threshold,
        proxy_shell_vertices: meta.proxy_shell_vertices,
        proxy_shell_faces: meta.proxy_shell_faces,
        constraint_cut_faces: plc_face_count,
        constraint_cut_area,
        free_cut_faces: meta.free_cut_faces,
        free_cut_area: meta.free_cut_area,
        self_loop_faces: meta.self_loop_faces,
        boundary_constraint_faces: meta.boundary_constraint_faces,
        inside_cells: meta.inside_cells,
        contracted: meta.contracted,
        output_faces: tris.len(),
        cap_faces,
        cap_area,
        parts,
        holes,
        nonmanifold: if manifold { 0 } else { 1 },
        watertight,
        two_faces_per_edge,
        signed_volume,
        positive_volume,
        bbox_ok,
        bbox_error,
        hausdorff_output_to_input: o2i,
        hausdorff_input_to_output: i2o,
        hausdorff: haus,
        hausdorff_output_to_input_full: fo2i,
        hausdorff_input_to_output_full: fi2o,
        hausdorff_full: fo2i.max(fi2o),
        valid: false,
        hash: hash_output(&out_verts, &tris),
        ..SolidReport::default()
    };
    report.valid = report.parts == 1
        && report.holes == 0
        && report.nonmanifold == 0
        && report.watertight
        && report.two_faces_per_edge
        && report.positive_volume
        && report.bbox_ok
        && report.hausdorff <= 0.0125;
    Ok(SolidMesh {
        verts: out_verts,
        tris,
        plc_face_count,
        report,
    })
}


struct OrientationStats {
    components: usize,
    flipped: usize,
    fallback: bool,
    details: Vec<ComponentOrientation>,
}

fn orient_for_winding(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
) -> (Vec<[usize; 3]>, OrientationStats) {
    let i64tris: Vec<[i64; 3]> = tris
        .iter()
        .map(|t| [t[0] as i64, t[1] as i64, t[2] as i64])
        .collect();
    let (mut out, fallback) = match re_orient_faces_coherently_core(&i64tris) {
        Ok(v) => (
            v.iter()
                .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
                .collect::<Vec<_>>(),
            false,
        ),
        Err(_) => {
            let mut u: Vec<[u32; 3]> = tris
                .iter()
                .map(|t| [t[0] as u32, t[1] as u32, t[2] as u32])
                .collect();
            orient_consistently(&mut u);
            (
                u.into_iter()
                    .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
                    .collect(),
                true,
            )
        }
    };
    let u: Vec<[u32; 3]> = out
        .iter()
        .map(|t| [t[0] as u32, t[1] as u32, t[2] as u32])
        .collect();
    let (map, components) = compute_shell_components(&u);
    let mut vols = vec![0.0f64; components];
    let mut counts = vec![0usize; components];
    for (i, t) in out.iter().enumerate() {
        let a = verts[t[0]];
        let b = verts[t[1]];
        let c = verts[t[2]];
        vols[map[i]] += (a[0] * (b[1] * c[2] - b[2] * c[1])
            + a[1] * (b[2] * c[0] - b[0] * c[2])
            + a[2] * (b[0] * c[1] - b[1] * c[0]))
            / 6.0;
        counts[map[i]] += 1;
    }
    let mut edge_counts: Vec<BTreeMap<(usize, usize), usize>> =
        vec![BTreeMap::new(); components];
    for (i, t) in out.iter().enumerate() {
        let c = map[i];
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a < b { (a, b) } else { (b, a) };
            *edge_counts[c].entry(key).or_insert(0) += 1;
        }
    }
    let closed: Vec<bool> = edge_counts
        .iter()
        .map(|m| !m.is_empty() && m.values().all(|&n| n == 2))
        .collect();
    let mut flipped = 0usize;
    let mut details = Vec::with_capacity(components);
    for c in 0..components {
        let flip = vols[c] < 0.0;
        if flip {
            flipped += 1;
            for (i, t) in out.iter_mut().enumerate() {
                if map[i] == c {
                    t.swap(1, 2);
                }
            }
        }
        details.push(ComponentOrientation {
            faces: counts[c],
            signed_volume: vols[c],
            closed: closed[c],
            flipped: flip,
        });
    }
    (
        out,
        OrientationStats {
            components,
            flipped,
            fallback,
            details,
        },
    )
}

fn gwn_labels(
    bvh: &MeshBvh,
    cen: &[[f64; 3]],
    out: &[bool],
) -> (Vec<bool>, [usize; 10]) {
    let labels = label_tets_winding(bvh, cen, out, false, 1.0);
    let inside: Vec<bool> = labels.init_label.iter().map(|&l| l == 0).collect();
    let mut w_hist = [0usize; 10];
    for ti in 0..cen.len() {
        let w = bvh.winding_at(cen[ti]);
        let bin = ((w * 10.0).floor() as i64).clamp(0, 9) as usize;
        w_hist[bin] += 1;
    }
    (inside, w_hist)
}

fn face_on_surface(
    points: &[Point3],
    face: [usize; 3],
    bvh: &MeshBvh,
    tol: f64,
) -> bool {
    let mut c = [0.0f64; 3];
    for &v in &face {
        let p = points[v].to_f64().unwrap_or([0.0; 3]);
        if bvh.unsigned_distance_at(p) > tol {
            return false;
        }
        for a in 0..3 {
            c[a] += p[a] / 3.0;
        }
    }
    bvh.unsigned_distance_at(c) <= tol
}

fn gwn_plc_count(
    points: &[Point3],
    tets: &[[usize; 4]],
    neighbours: &[[i32; 4]],
    constraints: &BTreeSet<[usize; 3]>,
    inside: &[bool],
    surface: Option<(&MeshBvh, f64)>,
) -> usize {
    let mut seen: BTreeSet<[usize; 3]> = BTreeSet::new();
    let mut count = 0usize;
    for ti in 0..tets.len() {
        for lv in 0..4 {
            let n = neighbours[ti][lv];
            if n < 0 {
                continue;
            }
            let key = sorted_face(&tets[ti], lv);
            let is_plc = match surface {
                Some((bvh, tol)) => face_on_surface(points, key, bvh, tol),
                None => constraints.contains(&key),
            };
            if is_plc && inside[ti] != inside[n as usize] && seen.insert(key) {
                count += 1;
            }
        }
    }
    count
}

fn extract_gwn_faces(
    points: &[Point3],
    tets: &[[usize; 4]],
    neighbours: &[[i32; 4]],
    constraints: &BTreeSet<[usize; 3]>,
    inside: &[bool],
    surface: Option<(&MeshBvh, f64)>,
) -> (Vec<[usize; 3]>, Vec<[usize; 3]>, f64) {
    let mut seen: BTreeSet<[usize; 3]> = BTreeSet::new();
    let mut plc_faces: Vec<[usize; 3]> = Vec::new();
    let mut free_faces: Vec<[usize; 3]> = Vec::new();
    let mut free_area = 0.0f64;
    for ti in 0..tets.len() {
        for lv in 0..4 {
            let n = neighbours[ti][lv];
            if n < 0 {
                continue;
            }
            let key = sorted_face(&tets[ti], lv);
            if !seen.insert(key) || inside[ti] == inside[n as usize] {
                continue;
            }
            let (t_in, lv_in) = if inside[ti] {
                (ti, lv)
            } else {
                let nj = n as usize;
                let lv2 = (0..4)
                    .find(|&k| sorted_face(&tets[nj], k) == key)
                    .ok_or_else(|| "neighbour face mismatch".to_string());
                match lv2 {
                    Ok(lv2) => (nj, lv2),
                    Err(_) => continue,
                }
            };
            let (a, b, c, _) =
                crate::cdt3d::segment::oriented_face_apex(points, &tets[t_in], lv_in);
            let is_plc = match surface {
                Some((bvh, tol)) => face_on_surface(points, key, bvh, tol),
                None => constraints.contains(&key),
            };
            if is_plc {
                plc_faces.push([a, b, c]);
            } else {
                free_area += tri_area(points, key);
                free_faces.push([a, b, c]);
            }
        }
    }
    (plc_faces, free_faces, free_area)
}

fn build_solid_gwn(
    face: &FaceMesh,
    plc: &Plc,
    unary_verts: &[[f64; 3]],
    unary_tris: &[[usize; 3]],
    _opts: &SolidOptions,
) -> Result<SolidMesh, String> {
    let points = &face.points;
    let tets = &face.tets;
    let neighbours = &face.neighbours;
    let n_tets = tets.len();
    let constraints: BTreeSet<[usize; 3]> = face.constraint_faces.iter().copied().collect();
    let surface_bvh = if constraints.is_empty() {
        let (lo, hi) = aabb(&plc.verts);
        let diag = ((hi[0] - lo[0]).powi(2)
            + (hi[1] - lo[1]).powi(2)
            + (hi[2] - lo[2]).powi(2))
        .sqrt();
        Some((MeshBvh::from_arrays(&plc.verts, &plc.tris), 1e-6 * diag.max(1e-12)))
    } else {
        None
    };
    let surface = surface_bvh.as_ref().map(|(b, t)| (b, *t));
    let mut cen = vec![[0.0f64; 3]; n_tets];
    let mut out = vec![false; n_tets];
    for ti in 0..n_tets {
        for &p in &tets[ti] {
            let q = points[p].to_f64().unwrap_or([0.0; 3]);
            for a in 0..3 {
                cen[ti][a] += q[a] * 0.25;
            }
        }
        for lv in 0..4 {
            if neighbours[ti][lv] < 0 {
                out[ti] = true;
            }
        }
    }

    let raw_bvh = MeshBvh::from_arrays(unary_verts, unary_tris);
    let (raw_inside, raw_hist) = gwn_labels(&raw_bvh, &cen, &out);
    let raw_plc = gwn_plc_count(points, tets, neighbours, &constraints, &raw_inside, surface);
    let raw_ok = raw_inside.iter().any(|&x| x) && raw_inside.iter().any(|&x| !x);

    let (oriented_tris, orientation) = orient_for_winding(unary_verts, unary_tris);
    let oriented_bvh = MeshBvh::from_arrays(unary_verts, &oriented_tris);
    let (oriented_inside, oriented_hist) = gwn_labels(&oriented_bvh, &cen, &out);
    let oriented_plc = gwn_plc_count(points, tets, neighbours, &constraints, &oriented_inside, surface);
    let oriented_ok = oriented_inside.iter().any(|&x| x) && oriented_inside.iter().any(|&x| !x);

    let use_oriented = match (raw_ok, oriented_ok) {
        (false, false) => {
            return Err("gwn_threshold produced a trivial labelling on both raw and oriented input".into())
        }
        (true, false) => false,
        (false, true) => true,
        (true, true) => oriented_plc > raw_plc,
    };
    let (inside, w_hist) = if use_oriented {
        (oriented_inside, oriented_hist)
    } else {
        (raw_inside, raw_hist)
    };
    let inside_count = inside.iter().filter(|&&x| x).count();
    let (plc_faces, free_faces, free_area) =
        extract_gwn_faces(points, tets, neighbours, &constraints, &inside, surface);
    let meta = SolidMeta {
        cells: n_tets,
        tet_count: n_tets,
        constraint_faces: constraints.len(),
        open_input: plc.report.boundary_edges > 0,
        gwn_threshold: true,
        proxy_shell_vertices: 0,
        proxy_shell_faces: 0,
        free_cut_faces: free_faces.len(),
        free_cut_area: free_area,
        self_loop_faces: 0,
        boundary_constraint_faces: 0,
        inside_cells: inside_count,
        contracted: false,
    };
    let mut solid = finish_solid(
        points,
        plc,
        plc_faces,
        free_faces,
        unary_verts,
        unary_tris,
        0.0,
        _opts,
        meta,
    )?;
    solid.report.orientation_used = use_oriented;
    solid.report.orientation_components = orientation.components;
    solid.report.orientation_flipped = orientation.flipped;
    solid.report.orientation_fallback = orientation.fallback;
    solid.report.orientation = orientation.details;
    solid.report.w_hist = w_hist;
    Ok(solid)
}

fn build_solid_tets(
    face: &FaceMesh,
    plc: &Plc,
    unary_verts: &[[f64; 3]],
    unary_tris: &[[usize; 3]],
    opts: &SolidOptions,
) -> Result<SolidMesh, String> {
    let points = &face.points;
    let tets = &face.tets;
    let neighbours = &face.neighbours;
    let n_tets = tets.len();
    let constraints: BTreeSet<[usize; 3]> = face.constraint_faces.iter().copied().collect();

    let mut cen = vec![[0.0f64; 3]; n_tets];
    let mut sink = vec![false; n_tets];
    for ti in 0..n_tets {
        for &p in &tets[ti] {
            let q = points[p].to_f64().unwrap_or([0.0; 3]);
            for a in 0..3 {
                cen[ti][a] += q[a] * 0.25;
            }
        }
        for lv in 0..4 {
            if neighbours[ti][lv] < 0 {
                sink[ti] = true;
            }
        }
    }
    let (phi, proxy_shell_vertices, proxy_shell_faces, h5_tol) =
        h7_proxy_phi(unary_verts, unary_tris, &cen, &sink, opts)?;

    let mut g = BkGraph::new(n_tets);
    for ti in 0..n_tets {
        if sink[ti] {
            g.add_tweights(ti, 0.0, INF);
        } else if phi[ti] < 0.0 {
            g.add_tweights(ti, INF, 0.0);
        }
    }
    for ti in 0..n_tets {
        for lv in 0..4 {
            let n = neighbours[ti][lv];
            if n < 0 {
                continue;
            }
            let key = sorted_face(&tets[ti], lv);
            let cap = if constraints.contains(&key) {
                tri_area(points, key) * opts.lambda * opts.constraint_fraction
            } else {
                tri_area(points, key) * opts.lambda
            };
            g.add_edge(ti, n as usize, cap, cap);
        }
    }
    let _flow = g.maxflow();
    let inside: Vec<bool> = (0..n_tets)
        .map(|ti| g.is_in_source(ti) && !sink[ti])
        .collect();
    let inside_count = inside.iter().filter(|&&x| x).count();
    if std::env::var_os("CDT3D_DEBUG_SOLID").is_some() {
        let pmin = phi.iter().copied().fold(f64::INFINITY, f64::min);
        let pmax = phi.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let spos = phi.iter().filter(|&&p| p > 0.0).count();
        eprintln!(
            "solid-tets: n={} phi=[{pmin:.4},{pmax:.4}] pos={spos} sink={} inside={inside_count}",
            n_tets,
            sink.iter().filter(|&&s| s).count()
        );
    }
    if inside_count == 0 || inside_count == n_tets {
        return Err("tet-level constrained cut produced a trivial labelling".into());
    }

    let mut occ: BTreeMap<[usize; 3], Vec<(usize, usize)>> = BTreeMap::new();
    for ti in 0..n_tets {
        for lv in 0..4 {
            let key = sorted_face(&tets[ti], lv);
            if constraints.contains(&key) {
                occ.entry(key).or_default().push((ti, lv));
            }
        }
    }
    let mut plc_faces: Vec<[usize; 3]> = Vec::new();
    let mut boundary_constraints = 0usize;
    for (key, list) in &occ {
        if list.len() == 1 {
            boundary_constraints += 1;
            let (ti, lv) = list[0];
            if inside[ti] {
                let (a, b, c, _) =
                    crate::cdt3d::segment::oriented_face_apex(points, &tets[ti], lv);
                plc_faces.push([a, b, c]);
            }
            continue;
        }
        if list.len() != 2 {
            return Err(format!(
                "constraint face {key:?} has {} incident tets",
                list.len()
            ));
        }
        let (tia, lva) = list[0];
        let (tib, lvb) = list[1];
        if inside[tia] == inside[tib] {
            continue;
        }
        let (ti, lv) = if inside[tia] { (tia, lva) } else { (tib, lvb) };
        let (a, b, c, _) =
            crate::cdt3d::segment::oriented_face_apex(points, &tets[ti], lv);
        plc_faces.push([a, b, c]);
    }

    let mut free_seen: BTreeSet<[usize; 3]> = BTreeSet::new();
    let mut free_faces: Vec<[usize; 3]> = Vec::new();
    let mut free_cut_area = 0.0f64;
    for ti in 0..n_tets {
        for lv in 0..4 {
            let n = neighbours[ti][lv];
            if n < 0 {
                continue;
            }
            let key = sorted_face(&tets[ti], lv);
            if constraints.contains(&key) || !free_seen.insert(key) {
                continue;
            }
            let nj = n as usize;
            if inside[ti] == inside[nj] {
                continue;
            }
            let (t_in, lv_in) = if inside[ti] {
                (ti, lv)
            } else {
                let lv2 = (0..4)
                    .find(|&k| sorted_face(&tets[nj], k) == key)
                    .ok_or_else(|| "neighbour face mismatch".to_string())?;
                (nj, lv2)
            };
            let (a, b, c, _) =
                crate::cdt3d::segment::oriented_face_apex(points, &tets[t_in], lv_in);
            free_faces.push([a, b, c]);
            free_cut_area += tri_area(points, key);
        }
    }
    let free_cut_faces = free_faces.len();

    let meta = SolidMeta {
        cells: n_tets,
        tet_count: n_tets,
        constraint_faces: constraints.len(),
        open_input: true,
        gwn_threshold: false,
        proxy_shell_vertices,
        proxy_shell_faces,
        free_cut_faces,
        free_cut_area,
        self_loop_faces: 0,
        boundary_constraint_faces: boundary_constraints,
        inside_cells: inside_count,
        contracted: false,
    };
    finish_solid(
        points,
        plc,
        plc_faces,
        free_faces,
        unary_verts,
        unary_tris,
        h5_tol,
        opts,
        meta,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdt3d::face::{recover_faces, FaceOptions};
    use crate::cdt3d::segment::{recover_segments, SegmentOptions};
    use crate::cdt3d::plc::{build_plc, PlcOptions};
    use crate::cdt3d::testutil::{fixture, plc_for, real_sample};
    use std::time::Instant;

    #[test]
    fn thingi10k_100827_gwn_gate() {
        let plc = plc_for("real", "thingi10k_100827.stl");
        let (uv, ut) = real_sample("thingi10k_100827.stl");
        let seg = recover_segments(&plc, &SegmentOptions::default()).unwrap();
        let face = recover_faces(&seg, &plc, &FaceOptions::default()).unwrap();
        let solid = build_solid(&face, &plc, &uv, &ut, &SolidOptions::default()).unwrap();
        assert!(solid.report.gwn_threshold);
        assert!(solid.report.orientation_used);
        assert_eq!(solid.report.constraint_cut_faces, 198);
        assert_eq!(solid.report.parts, 1);
        assert_eq!(solid.report.holes, 0);
        assert_eq!(solid.report.nonmanifold, 0);
        assert!(solid.report.bbox_ok);
        assert!(solid.report.hausdorff <= 0.0125);
        assert!(solid.report.valid);
    }

    #[test]
    fn thingi10k_100827_solid_diagnostic() {
        let plc = plc_for("real", "thingi10k_100827.stl");
        let (uv, ut) = real_sample("thingi10k_100827.stl");
        let seg = recover_segments(&plc, &SegmentOptions::default()).unwrap();
        let face = recover_faces(&seg, &plc, &FaceOptions::default()).unwrap();
        let opts = SolidOptions::default();
        let start = Instant::now();
        let solid = build_solid(&face, &plc, &uv, &ut, &opts).unwrap();
        let secs = start.elapsed().as_secs_f64();
        println!(
            "100827 solid: cells={} cfaces={} open={} gwn={} ouse={} oc={} ofl={} ofb={} wh={:?} pv={} pf={} selfloop={} bc={} inside={} outF={} capF={} capA={:.6} ccut={} ccutA={:.3} fcut={} fcutA={:.3} parts={} holes={} nm={} bbox_ok={} bbox={:?} h_o2i={:.6} h_i2o={:.6} h={:.6} valid={} hash={:016x} secs={:.3}",
            solid.report.cells,
            solid.report.constraint_faces,
            solid.report.open_input,
            solid.report.gwn_threshold,
            solid.report.orientation_used,
            solid.report.orientation_components,
            solid.report.orientation_flipped,
            solid.report.orientation_fallback,
            solid.report.w_hist,
            solid.report.proxy_shell_vertices,
            solid.report.proxy_shell_faces,
            solid.report.self_loop_faces,
            solid.report.boundary_constraint_faces,
            solid.report.inside_cells,
            solid.report.output_faces,
            solid.report.cap_faces,
            solid.report.cap_area,
            solid.report.constraint_cut_faces,
            solid.report.constraint_cut_area,
            solid.report.free_cut_faces,
            solid.report.free_cut_area,
            solid.report.parts,
            solid.report.holes,
            solid.report.nonmanifold,
            solid.report.bbox_ok,
            solid.report.bbox_error,
            solid.report.hausdorff_output_to_input,
            solid.report.hausdorff_input_to_output,
            solid.report.hausdorff,
            solid.report.valid,
            solid.report.hash,
            secs
        );
    }

    #[test]
    fn thingi10k_100827_solid_is_deterministic() {
        let plc = plc_for("real", "thingi10k_100827.stl");
        let (uv, ut) = real_sample("thingi10k_100827.stl");
        let seg = recover_segments(&plc, &SegmentOptions::default()).unwrap();
        let face = recover_faces(&seg, &plc, &FaceOptions::default()).unwrap();
        let first = build_solid(&face, &plc, &uv, &ut, &SolidOptions::default()).unwrap();
        for _ in 0..4 {
            let again = build_solid(&face, &plc, &uv, &ut, &SolidOptions::default()).unwrap();
            assert_eq!(again.report.hash, first.report.hash);
            assert_eq!(again.tris, first.tris);
        }
    }

    fn read_obj(path: &str) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let data = std::fs::read_to_string(path).expect("read OBJ");
        let mut verts = Vec::new();
        let mut tris = Vec::new();
        for line in data.lines() {
            let mut it = line.split_whitespace();
            match it.next() {
                Some("v") => {
                    let x: f64 = it.next().unwrap().parse().unwrap();
                    let y: f64 = it.next().unwrap().parse().unwrap();
                    let z: f64 = it.next().unwrap().parse().unwrap();
                    verts.push([x, y, z]);
                }
                Some("f") => {
                    let mut idx = [0usize; 3];
                    for k in 0..3 {
                        let tok = it.next().unwrap();
                        idx[k] = tok.split('/').next().unwrap().parse::<usize>().unwrap() - 1;
                    }
                    tris.push(idx);
                }
                _ => {}
            }
        }
        (verts, tris)
    }

    fn ftw_metrics(
        name: &str,
        input_v: &[[f64; 3]],
        input_t: &[[usize; 3]],
        out_v: &[[f64; 3]],
        out_t: &[[usize; 3]],
    ) -> (usize, usize, usize, [f64; 3], bool, f64, f64, f64) {
        let (v, t) = snap_compact(out_v, out_t);
        let u32t: Vec<[u32; 3]> = t
            .iter()
            .map(|x| [x[0] as u32, x[1] as u32, x[2] as u32])
            .collect();
        let nm = if mesh_is_manifold(&v, &u32t) { 0 } else { 1 };
        let parts = output_parts(&v, &t);
        let holes = if t.is_empty() {
            0
        } else {
            let si: Vec<[i64; 3]> = t
                .iter()
                .map(|x| [x[0] as i64, x[1] as i64, x[2] as i64])
                .collect();
            boundary_topology(&si, false)
                .map(|b| b.loops.len())
                .unwrap_or(0)
        };
        let (ilo, ihi) = aabb(input_v);
        let (olo, ohi) = aabb(&v);
        let mut berr = [0.0f64; 3];
        let mut bok = true;
        for a in 0..3 {
            let ie = ihi[a] - ilo[a];
            let oe = ohi[a] - olo[a];
            berr[a] = (oe - ie).abs();
            if berr[a] > 0.01 * ie.abs().max(1e-12) + 1e-9 {
                bok = false;
            }
        }
        let ib = MeshBvh::from_arrays(input_v, input_t);
        let ob = MeshBvh::from_arrays(&v, &t);
        let (o2i, i2o) = hausdorff(&ib, &ob, &v, &t, input_v, input_t);
        let h = o2i.max(i2o);
        println!(
            "FTW {name}: parts={parts} holes={holes} nm={nm} bbox_ok={bok} bbox={berr:?} o2i={o2i:.6} i2o={i2o:.6} h={h:.6}"
        );
        (parts, holes, nm, berr, bok, o2i, i2o, h)
    }

    #[test]
    fn cdt3d_fixtures_gwn_gate() {
        // fTetWild baseline (pytetwild 0.4.2, optimize=False) measured with the
        // same denser Hausdorff sampling.  Bound: no worse by
        // max(0.0125 mm, 1% of the fTetWild value).
        let samples = [
            ("case1_open_cup.stl", 1.492777f64),
            ("case2_pipe.stl", 11.942217),
            ("case3_broken_cup.stl", 2.892546),
            ("case4_cracked_sphere.stl", 4.665224),
            ("case5_box_missing_face.stl", 10.148891),
            ("case6_thin_cup.stl", 15.152173),
        ];
        for (name, ftw_h) in samples {
            let plc = plc_for("fixture", name);
            let (uv, ut) = fixture(name);
            let seg = recover_segments(&plc, &SegmentOptions::default()).unwrap();
            let face = recover_faces(&seg, &plc, &FaceOptions::default()).unwrap();
            let solid = build_solid(&face, &plc, &uv, &ut, &SolidOptions::default()).unwrap();
            assert!(solid.report.gwn_threshold, "{name}: not in GWN mode");
            assert_eq!(solid.report.parts, 1, "{name}: parts");
            assert_eq!(solid.report.holes, 0, "{name}: holes");
            assert_eq!(solid.report.nonmanifold, 0, "{name}: NM");
            assert!(solid.report.watertight, "{name}: watertight");
            assert!(solid.report.two_faces_per_edge, "{name}: 2 faces/edge");
            assert!(solid.report.positive_volume, "{name}: winding");
            assert!(solid.report.bbox_ok, "{name}: bbox");
            let bound = ftw_h + (0.0125f64).max(0.01 * ftw_h);
            assert!(
                solid.report.hausdorff <= bound,
                "{name}: h={} bound={} (fTetWild {})",
                solid.report.hausdorff,
                bound,
                ftw_h
            );
        }
    }

    #[test]
    #[ignore = "plate asserted gate; run with --release -- --ignored"]
    fn plate_a_dent_shape_gate() {
        // fTetWild baseline with the same denser metric: 1.274534 mm
        // (pytetwild 0.4.2, optimize=False).  Amended gate:
        // CDT_h <= FTW_h + max(0.0125, 0.01 * FTW_h).
        const FTW_H: f64 = 1.274534;
        let name = "plate_a_dent.stl";
        let plc = plc_for("fixture", name);
        let (uv, ut) = fixture(name);
        let seg = recover_segments(&plc, &SegmentOptions::default()).unwrap();
        let face = recover_faces(&seg, &plc, &FaceOptions::default()).unwrap();
        let solid = build_solid(&face, &plc, &uv, &ut, &SolidOptions::default()).unwrap();
        assert_eq!(solid.report.parts, 1);
        assert_eq!(solid.report.holes, 0);
        assert_eq!(solid.report.nonmanifold, 0);
        assert!(solid.report.watertight);
        assert!(solid.report.two_faces_per_edge);
        assert!(solid.report.positive_volume);
        assert!(solid.report.bbox_ok);
        let bound = FTW_H + (0.0125f64).max(0.01 * FTW_H);
        assert!(
            solid.report.hausdorff <= bound,
            "plate h={} bound={} (fTetWild {})",
            solid.report.hausdorff,
            bound,
            FTW_H
        );
    }

    #[test]
    #[ignore = "fTetWild baseline; requires /tmp/opencode/ftw-baseline or CDT3D_FTETWILD_DIR"]
    fn ftetwild_baseline_diagnostic() {
        let dir = std::env::var("CDT3D_FTETWILD_DIR")
            .unwrap_or_else(|_| "/tmp/opencode/ftw-baseline".into());
        let samples = [
            ("real", "thingi10k_100827.stl", "thingi10k_100827.stl.obj"),
            ("fixture", "case1_open_cup.stl", "case1_open_cup.stl.obj"),
            ("fixture", "case2_pipe.stl", "case2_pipe.stl.obj"),
            ("fixture", "case3_broken_cup.stl", "case3_broken_cup.stl.obj"),
            ("fixture", "case4_cracked_sphere.stl", "case4_cracked_sphere.stl.obj"),
            ("fixture", "case5_box_missing_face.stl", "case5_box_missing_face.stl.obj"),
            ("fixture", "case6_thin_cup.stl", "case6_thin_cup.stl.obj"),
            ("fixture", "plate_a_dent.stl", "plate_a_dent.stl.obj"),
        ];
        for (kind, name, obj) in samples {
            let (iv, it) = if kind == "real" {
                real_sample(name)
            } else {
                fixture(name)
            };
            let path = format!("{dir}/{obj}");
            if !std::path::Path::new(&path).exists() {
                println!("FTW {name}: missing {path}");
                continue;
            }
            let (ov, ot) = read_obj(&path);
            let _ = ftw_metrics(name, &iv, &it, &ov, &ot);
        }
    }

    #[test]
    #[ignore = "P5 public-corpus gate; run with --release -- --ignored"]
    fn solid_public_corpus_diagnostic() {
        let samples = [
            ("real", "thingi10k_100827.stl"),
            ("fixture", "case1_open_cup.stl"),
            ("fixture", "case2_pipe.stl"),
            ("fixture", "case3_broken_cup.stl"),
            ("fixture", "case4_cracked_sphere.stl"),
            ("fixture", "case5_box_missing_face.stl"),
            ("fixture", "case6_thin_cup.stl"),
            ("fixture", "plate_a_dent.stl"),
        ];
        for (kind, name) in samples {
            let plc = plc_for(kind, name);
            let (uv, ut) = if kind == "real" {
                real_sample(name)
            } else {
                fixture(name)
            };
            let seg = recover_segments(&plc, &SegmentOptions::default()).unwrap();
            let face = recover_faces(&seg, &plc, &FaceOptions::default()).unwrap();
            let solid = match build_solid(&face, &plc, &uv, &ut, &SolidOptions::default()) {
                Ok(s) => s,
                Err(e) => {
                    println!("{name}: P5 ERR {e}");
                    continue;
                }
            };
            println!(
                "{name}: cells={} cfaces={} gwn={} ouse={} oc={} ofl={} ccut={} fcut={} selfloop={} inside={} outF={} capF={} parts={} holes={} nm={} bbox_ok={} h={:.6} valid={} hash={:016x}",
                solid.report.cells,
                solid.report.constraint_faces,
                solid.report.gwn_threshold,
                solid.report.orientation_used,
                solid.report.orientation_components,
                solid.report.orientation_flipped,
                solid.report.constraint_cut_faces,
                solid.report.free_cut_faces,
                solid.report.self_loop_faces,
                solid.report.inside_cells,
                solid.report.output_faces,
                solid.report.cap_faces,
                solid.report.parts,
                solid.report.holes,
                solid.report.nonmanifold,
                solid.report.bbox_ok,
                solid.report.hausdorff,
                solid.report.valid,
                solid.report.hash
            );
        }
    }
}
