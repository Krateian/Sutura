//! Diagnosis v2 phase 1: topology channels (orientation, non-manifold vertices,
//! duplicates, degeneracy) and part analysis.  The output is additive: the
//! legacy `ObjectAnalysis` fields stay in Python until the later parity phase.

use std::collections::{HashMap, HashSet};

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use rayon::prelude::*;

use crate::topology::DisjointSet;
use crate::winding::MeshBvh;

#[derive(Clone, Copy, Debug)]
struct Bbox {
    lo: [f64; 3],
    hi: [f64; 3],
    any: bool,
}

impl Bbox {
    fn empty() -> Self {
        Self {
            lo: [f64::INFINITY; 3],
            hi: [f64::NEG_INFINITY; 3],
            any: false,
        }
    }
    fn add(&mut self, p: [f64; 3]) {
        for k in 0..3 {
            self.lo[k] = self.lo[k].min(p[k]);
            self.hi[k] = self.hi[k].max(p[k]);
        }
        self.any = true;
    }
    fn add_bbox(&mut self, o: &Bbox) {
        if o.any {
            self.add(o.lo);
            self.add(o.hi);
        }
    }
    fn diag(&self) -> f64 {
        if !self.any {
            return 0.0;
        }
        let d = [
            self.hi[0] - self.lo[0],
            self.hi[1] - self.lo[1],
            self.hi[2] - self.lo[2],
        ];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
    }
    fn centroid(&self) -> [f64; 3] {
        if !self.any {
            return [0.0; 3];
        }
        [
            0.5 * (self.lo[0] + self.hi[0]),
            0.5 * (self.lo[1] + self.hi[1]),
            0.5 * (self.lo[2] + self.hi[2]),
        ]
    }
    fn overlaps(&self, o: &Bbox) -> bool {
        self.any
            && o.any
            && self.lo[0] <= o.hi[0]
            && o.lo[0] <= self.hi[0]
            && self.lo[1] <= o.hi[1]
            && o.lo[1] <= self.hi[1]
            && self.lo[2] <= o.hi[2]
            && o.lo[2] <= self.hi[2]
    }
    fn contains(&self, o: &Bbox) -> bool {
        self.any
            && o.any
            && self.lo[0] <= o.lo[0]
            && self.hi[0] >= o.hi[0]
            && self.lo[1] <= o.lo[1]
            && self.hi[1] >= o.hi[1]
            && self.lo[2] <= o.lo[2]
            && self.hi[2] >= o.hi[2]
    }
    fn flat(&self) -> [f64; 6] {
        [
            self.lo[0], self.lo[1], self.lo[2], self.hi[0], self.hi[1], self.hi[2],
        ]
    }
}

#[derive(Clone, Debug)]
pub struct Region {
    pub channel: String,
    pub kind: String,
    pub bbox: [f64; 6],
    pub centroid: [f64; 3],
    pub faces: usize,
    pub vertices: usize,
    pub components: Vec<usize>,
    pub severity: f64,
    pub confidence: f64,
    pub min_face: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Orientation {
    pub inconsistent_edges: usize,
    pub flipped_faces: usize,
    pub flipped_patches: usize,
    pub volume_signs: Vec<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct NmVertices {
    pub vertices: usize,
    pub bowtie: usize,
    pub pinched: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Duplicates {
    pub faces: usize,
    pub reversed_faces: usize,
    pub vertices: usize,
    pub near_vertices: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Degenerate {
    pub zero_area: usize,
    pub needle: usize,
    pub load_bearing: usize,
    pub quality_p99: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Parts {
    pub closed: usize,
    pub open: usize,
    pub debris: usize,
    pub overlap_edges: Vec<(usize, usize)>,
    pub nested_shells: Vec<(usize, usize)>,
    pub overlap_estimated: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DiagReport {
    pub orientation: Orientation,
    pub nm_vertices: NmVertices,
    pub duplicates: Duplicates,
    pub degenerate: Degenerate,
    pub parts: Parts,
    pub face_mask: Vec<u8>,
    pub vertex_mask: Vec<u8>,
    pub regions: Vec<Region>,
    pub faces: usize,
    pub vertices: usize,
    pub timings: Vec<(&'static str, f64)>,
}

#[derive(Clone, Copy)]
struct FaceGeom {
    area: f64,
    bbox: Bbox,
}

#[inline]
fn canon(a: usize, b: usize) -> (usize, usize) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Deterministic iteration order for the edge map: `HashMap` iteration is
/// salted per process, and the orientation DFS is sensitive to neighbour order
/// on a non-orientable mesh (a Mobius strip otherwise yields a different
/// `flipped_faces` count every run).
fn sorted_edge_keys(
    edges: &HashMap<(usize, usize), Vec<(usize, bool)>>,
) -> Vec<(usize, usize)> {
    let mut keys: Vec<(usize, usize)> = edges.keys().copied().collect();
    keys.sort_unstable();
    keys
}

fn build_edges(tris: &[[usize; 3]]) -> HashMap<(usize, usize), Vec<(usize, bool)>> {
    let mut edges: HashMap<(usize, usize), Vec<(usize, bool)>> = HashMap::new();
    for (fid, t) in tris.iter().enumerate() {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = canon(a, b);
            let forward = a < b;
            edges.entry(key).or_default().push((fid, forward));
        }
    }
    edges
}

fn face_geometry(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> Vec<FaceGeom> {
    tris.iter()
        .map(|t| {
            let a = verts[t[0]];
            let b = verts[t[1]];
            let c = verts[t[2]];
            let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let cr = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            let area = 0.5 * (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]).sqrt();
            let mut bbox = Bbox::empty();
            bbox.add(a);
            bbox.add(b);
            bbox.add(c);
            FaceGeom { area, bbox }
        })
        .collect()
}

fn face_volume(verts: &[[f64; 3]], t: [usize; 3]) -> f64 {
    let a = verts[t[0]];
    let b = verts[t[1]];
    let c = verts[t[2]];
    (a[0] * (b[1] * c[2] - b[2] * c[1])
        + a[1] * (b[2] * c[0] - b[0] * c[2])
        + a[2] * (b[0] * c[1] - b[1] * c[0]))
        / 6.0
}

fn components(
    nf: usize,
    edges: &HashMap<(usize, usize), Vec<(usize, bool)>>,
) -> (Vec<usize>, usize) {
    let mut ds = DisjointSet::new(nf);
    for key in sorted_edge_keys(edges) {
        let uses = &edges[&key];
        for w in uses.windows(2) {
            ds.union(w[0].0, w[1].0);
        }
    }
    let mut map = HashMap::new();
    let mut comp = vec![0usize; nf];
    let mut n = 0usize;
    for f in 0..nf {
        let r = ds.find(f);
        let id = *map.entry(r).or_insert_with(|| {
            let x = n;
            n += 1;
            x
        });
        comp[f] = id;
    }
    (comp, n)
}

/// Renumber components by a permutation-independent geometric key:
/// bounding-box centroid, face count and the per-axis minimum vertex.  The
/// `first face` order is otherwise visible through `volume_signs` and the
/// component ids in regions/overlap pairs.
fn canonical_component_ids(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    comp: &[usize],
    ncomp: usize,
) -> (Vec<usize>, usize) {
    if ncomp == 0 {
        return (comp.to_vec(), 0);
    }
    let mut bboxes = vec![Bbox::empty(); ncomp];
    let mut face_counts = vec![0usize; ncomp];
    let mut min_vertex = vec![[f64::INFINITY; 3]; ncomp];
    for (f, &c) in comp.iter().enumerate() {
        face_counts[c] += 1;
        for &v in &tris[f] {
            let p = verts[v];
            bboxes[c].add(p);
            for k in 0..3 {
                if p[k] < min_vertex[c][k] {
                    min_vertex[c][k] = p[k];
                }
            }
        }
    }
    let mut order: Vec<usize> = (0..ncomp).collect();
    order.sort_by(|&a, &b| {
        let ca = bboxes[a].centroid();
        let cb = bboxes[b].centroid();
        ca[0].total_cmp(&cb[0])
            .then_with(|| ca[1].total_cmp(&cb[1]))
            .then_with(|| ca[2].total_cmp(&cb[2]))
            .then_with(|| face_counts[a].cmp(&face_counts[b]))
            .then_with(|| min_vertex[a][0].total_cmp(&min_vertex[b][0]))
            .then_with(|| min_vertex[a][1].total_cmp(&min_vertex[b][1]))
            .then_with(|| min_vertex[a][2].total_cmp(&min_vertex[b][2]))
            .then_with(|| a.cmp(&b))
    });
    let mut remap = vec![0usize; ncomp];
    for (new, &old) in order.iter().enumerate() {
        remap[old] = new;
    }
    (comp.iter().map(|&c| remap[c]).collect(), ncomp)
}

fn adjacency_from_edges(
    nf: usize,
    edges: &HashMap<(usize, usize), Vec<(usize, bool)>>,
) -> Vec<Vec<(usize, bool)>> {
    let mut adj = vec![Vec::new(); nf];
    for key in sorted_edge_keys(edges) {
        let uses = &edges[&key];
        if uses.len() == 2 {
            let same = uses[0].1 == uses[1].1;
            let (a, b) = (uses[0].0, uses[1].0);
            adj[a].push((b, same));
            adj[b].push((a, same));
        }
    }
    adj
}

fn orientation_report(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    edges: &HashMap<(usize, usize), Vec<(usize, bool)>>,
    comp: &[usize],
    ncomp: usize,
    face_mask: &mut [u8],
    regions: &mut Vec<Region>,
) -> Orientation {
    let mut out = Orientation::default();
    let adj = adjacency_from_edges(tris.len(), edges);
    for key in sorted_edge_keys(edges) {
        let uses = &edges[&key];
        if uses.len() > 2 || (uses.len() == 2 && uses[0].1 == uses[1].1) {
            out.inconsistent_edges += 1;
        }
    }
    let mut flip = vec![false; tris.len()];
    let mut seen = vec![false; tris.len()];
    for start in 0..tris.len() {
        if seen[start] {
            continue;
        }
        seen[start] = true;
        let mut stack = vec![start];
        let mut members = Vec::new();
        while let Some(f) = stack.pop() {
            members.push(f);
            for &(nb, same) in &adj[f] {
                if !seen[nb] {
                    seen[nb] = true;
                    flip[nb] = flip[f] ^ same;
                    stack.push(nb);
                }
            }
        }
        let flips = members.iter().filter(|&&f| flip[f]).count();
        if flips * 2 > members.len() {
            for &f in &members {
                flip[f] = !flip[f];
            }
        }
    }
    out.flipped_faces = flip.iter().filter(|&&x| x).count();
    for (f, &b) in flip.iter().enumerate() {
        if b {
            face_mask[f] |= 1;
        }
    }
    let mut vol = vec![0.0f64; ncomp];
    for (f, &c) in comp.iter().enumerate() {
        vol[c] += face_volume(verts, tris[f]);
    }
    out.volume_signs = vol
        .iter()
        .map(|x| {
            if *x > 0.0 {
                1.0
            } else if *x < 0.0 {
                -1.0
            } else {
                0.0
            }
        })
        .collect();

    let mut patch_seen = vec![false; tris.len()];
    for s in 0..tris.len() {
        if !flip[s] || patch_seen[s] {
            continue;
        }
        let mut stack = vec![s];
        patch_seen[s] = true;
        let mut faces = Vec::new();
        while let Some(f) = stack.pop() {
            faces.push(f);
            for &(nb, _) in &adj[f] {
                if flip[nb] && !patch_seen[nb] {
                    patch_seen[nb] = true;
                    stack.push(nb);
                }
            }
        }
        out.flipped_patches += 1;
        regions.push(region_from_faces(
            "orientation",
            "flipped_patch",
            &faces,
            tris,
            verts,
            0.0,
            1.0,
            comp,
        ));
    }
    out
}

fn nm_vertex_report(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    edges: &HashMap<(usize, usize), Vec<(usize, bool)>>,
    comp: &[usize],
    face_mask: &mut [u8],
    vertex_mask: &mut [u8],
    regions: &mut Vec<Region>,
) -> NmVertices {
    let nv = verts.len();
    let mut incident: Vec<Vec<usize>> = vec![Vec::new(); nv];
    for (f, t) in tris.iter().enumerate() {
        for &x in t {
            incident[x].push(f);
        }
    }
    let mut vert_edges: Vec<Vec<(usize, usize)>> = vec![Vec::new(); nv];
    for (a, b) in sorted_edge_keys(edges) {
        vert_edges[a].push((a, b));
        vert_edges[b].push((a, b));
    }
    let flags: Vec<bool> = (0..nv)
        .into_par_iter()
        .map(|v| {
            let inc = &incident[v];
            if inc.len() < 2 {
                return false;
            }
            let mut local: HashMap<usize, usize> = HashMap::with_capacity(inc.len());
            for (i, &f) in inc.iter().enumerate() {
                local.insert(f, i);
            }
            let mut ds = DisjointSet::new(inc.len());
            for &(a, b) in &vert_edges[v] {
                let key = canon(a, b);
                if let Some(uses) = edges.get(&key) {
                    let ids: Vec<usize> = uses
                        .iter()
                        .filter_map(|(f, _)| local.get(f).copied())
                        .collect();
                    for w in ids.windows(2) {
                        ds.union(w[0], w[1]);
                    }
                }
            }
            let mut roots = HashSet::new();
            for i in 0..inc.len() {
                roots.insert(ds.find(i));
            }
            roots.len() > 1
        })
        .collect();
    let bad: Vec<usize> = flags
        .iter()
        .enumerate()
        .filter(|(_, b)| **b)
        .map(|(i, _)| i)
        .collect();
    for &v in &bad {
        vertex_mask[v] |= 2;
        for &f in &incident[v] {
            face_mask[f] |= 2;
        }
    }
    let mut adj_v: Vec<Vec<usize>> = vec![Vec::new(); nv];
    for (a, b) in sorted_edge_keys(edges) {
        adj_v[a].push(b);
        adj_v[b].push(a);
    }
    let mut seen = HashSet::new();
    for &v in &bad {
        if seen.contains(&v) {
            continue;
        }
        let mut stack = vec![v];
        seen.insert(v);
        let mut vs = Vec::new();
        while let Some(x) = stack.pop() {
            vs.push(x);
            for &nb in &adj_v[x] {
                if bad.contains(&nb) && seen.insert(nb) {
                    stack.push(nb);
                }
            }
        }
        let mut fs: Vec<usize> = vs.iter().flat_map(|&x| incident[x].clone()).collect();
        fs.sort_unstable();
        fs.dedup();
        regions.push(region_from_vertices(
            "nm_vertex",
            "nm_vertex_cluster",
            &vs,
            &fs,
            verts,
            1.0,
            1.0,
            comp,
        ));
    }
    NmVertices {
        vertices: bad.len(),
        bowtie: bad.len(),
        pinched: bad.len(),
    }
}

fn face_parity(t: [usize; 3], k: [usize; 3]) -> u8 {
    // 0 = even, 1 = odd permutation relative to the sorted triple.
    let p = [k[0], k[1], k[2]];
    let pos = |x: usize| p.iter().position(|&y| y == x).unwrap_or(0);
    let a = pos(t[0]);
    let b = pos(t[1]);
    let c = pos(t[2]);
    let inv = (a > b) as u8 + (a > c) as u8 + (b > c) as u8;
    inv % 2
}

fn duplicate_report(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    comp: &[usize],
    face_mask: &mut [u8],
    vertex_mask: &mut [u8],
    regions: &mut Vec<Region>,
) -> Duplicates {
    let mut out = Duplicates::default();
    let mut groups: HashMap<[usize; 3], Vec<usize>> = HashMap::new();
    for (f, t) in tris.iter().enumerate() {
        let mut k = *t;
        k.sort_unstable();
        groups.entry(k).or_default().push(f);
    }
    let mut dup_faces = HashSet::new();
    let mut group_keys: Vec<[usize; 3]> = groups.keys().copied().collect();
    group_keys.sort_unstable();
    for k in group_keys {
        let fs = &groups[&k];
        if fs.len() > 1 {
            out.faces += fs.len() - 1;
            for &f in fs {
                dup_faces.insert(f);
            }
            let parity: Vec<u8> = fs.iter().map(|&f| face_parity(tris[f], k)).collect();
            if parity.contains(&0) && parity.contains(&1) {
                out.reversed_faces += fs.len();
                for &f in fs {
                    face_mask[f] |= 4;
                }
            }
            for &f in fs {
                face_mask[f] |= 4;
            }
            let fs2: Vec<usize> = fs.clone();
            regions.push(region_from_faces(
                "duplicate_face",
                "duplicate_group",
                &fs2,
                tris,
                verts,
                0.0,
                1.0,
                comp,
            ));
        }
    }
    let mut vert_groups: HashMap<[u64; 3], Vec<usize>> = HashMap::new();
    for (i, v) in verts.iter().enumerate() {
        let k = [v[0].to_bits(), v[1].to_bits(), v[2].to_bits()];
        vert_groups.entry(k).or_default().push(i);
    }
    for vs in vert_groups.values() {
        if vs.len() > 1 {
            out.vertices += vs.len() - 1;
            for &v in vs {
                vertex_mask[v] |= 4;
            }
        }
    }
    let global = Bbox {
        lo: [0.0; 3],
        hi: [0.0; 3],
        any: false,
    };
    let diag = {
        let mut b = global;
        for v in verts {
            b.add(*v);
        }
        b.diag()
    };
    let eps = (1e-6 * diag).max(1e-9);
    let cell = eps;
    let mut grid: HashMap<[i64; 3], Vec<usize>> = HashMap::new();
    let key = |p: [f64; 3]| {
        [
            (p[0] / cell).floor() as i64,
            (p[1] / cell).floor() as i64,
            (p[2] / cell).floor() as i64,
        ]
    };
    for (i, v) in verts.iter().enumerate() {
        grid.entry(key(*v)).or_default().push(i);
    }
    let eps2 = eps * eps;
    let pairs: Vec<(usize, usize)> = (0..verts.len())
        .into_par_iter()
        .flat_map_iter(|i| {
            let v = verts[i];
            let c = key(v);
            let mut hits = Vec::new();
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if let Some(list) = grid.get(&[c[0] + dx, c[1] + dy, c[2] + dz]) {
                            for &j in list {
                                if j <= i {
                                    continue;
                                }
                                let w = verts[j];
                                let d2 = (v[0] - w[0]).powi(2)
                                    + (v[1] - w[1]).powi(2)
                                    + (v[2] - w[2]).powi(2);
                                if d2 <= eps2 {
                                    hits.push((i, j));
                                }
                            }
                        }
                    }
                }
            }
            hits
        })
        .collect();
    let mut near = HashSet::new();
    for (i, j) in pairs {
        near.insert(i);
        near.insert(j);
    }
    out.near_vertices = near.len();
    for &v in &near {
        vertex_mask[v] |= 4;
    }
    out
}

fn degenerate_report(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    comp: &[usize],
    face_geom: &[FaceGeom],
    edges: &HashMap<(usize, usize), Vec<(usize, bool)>>,
    face_mask: &mut [u8],
    regions: &mut Vec<Region>,
) -> Degenerate {
    let mut q: Vec<f64> = Vec::with_capacity(tris.len());
    for (i, t) in tris.iter().enumerate() {
        let a = verts[t[0]];
        let b = verts[t[1]];
        let c = verts[t[2]];
        let l2 = (a[0] - b[0]).powi(2)
            + (a[1] - b[1]).powi(2)
            + (a[2] - b[2]).powi(2)
            + (b[0] - c[0]).powi(2)
            + (b[1] - c[1]).powi(2)
            + (b[2] - c[2]).powi(2)
            + (c[0] - a[0]).powi(2)
            + (c[1] - a[1]).powi(2)
            + (c[2] - a[2]).powi(2);
        q.push(if l2 > 0.0 {
            4.0 * 3f64.sqrt() * face_geom[i].area / l2
        } else {
            0.0
        });
    }
    let mut qs = q.clone();
    qs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = Degenerate::default();
    out.quality_p99 = qs
        .get(((qs.len() as f64 * 0.01) as usize).min(qs.len().saturating_sub(1)))
        .copied()
        .unwrap_or(0.0);
    let mut bad = HashSet::new();
    for (i, t) in tris.iter().enumerate() {
        if face_geom[i].area <= 1e-14 {
            out.zero_area += 1;
            bad.insert(i);
            let mut bearing = false;
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                if let Some(uses) = edges.get(&canon(a, b)) {
                    if uses.len() == 2 {
                        bearing = true;
                    }
                }
            }
            if bearing {
                out.load_bearing += 1;
            }
        } else if q[i] < 0.01 {
            out.needle += 1;
            bad.insert(i);
        }
    }
    for &f in &bad {
        face_mask[f] |= 8;
    }
    let adj = adjacency_from_edges(tris.len(), edges);
    let mut seen = HashSet::new();
    for &s in &bad {
        if !seen.insert(s) {
            continue;
        }
        let mut stack = vec![s];
        let mut fs = Vec::new();
        while let Some(f) = stack.pop() {
            fs.push(f);
            for &(nb, _) in &adj[f] {
                if bad.contains(&nb) && seen.insert(nb) {
                    stack.push(nb);
                }
            }
        }
        regions.push(region_from_faces(
            "degenerate",
            "degenerate_cluster",
            &fs,
            tris,
            verts,
            1.0,
            1.0,
            comp,
        ));
    }
    out
}

fn part_report(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    edges: &HashMap<(usize, usize), Vec<(usize, bool)>>,
    face_geom: &[FaceGeom],
    comp: &[usize],
    ncomp: usize,
    regions: &mut Vec<Region>,
) -> Parts {
    let mut bboxes = vec![Bbox::empty(); ncomp];
    let mut face_counts = vec![0usize; ncomp];
    let mut vols = vec![0.0f64; ncomp];
    let mut open = vec![false; ncomp];
    for (f, &c) in comp.iter().enumerate() {
        bboxes[c].add_bbox(&face_geom[f].bbox);
        face_counts[c] += 1;
        vols[c] += face_volume(verts, tris[f]);
    }
    for key in sorted_edge_keys(edges) {
        let uses = &edges[&key];
        if uses.len() != 2 {
            for &(f, _) in uses {
                open[comp[f]] = true;
            }
        }
    }
    let mut global = Bbox::empty();
    for b in &bboxes {
        global.add_bbox(b);
    }
    let gdiag = global.diag();
    let max_vol = vols.iter().map(|v| v.abs()).fold(0.0, f64::max);
    let mut out = Parts::default();
    for c in 0..ncomp {
        if open[c] {
            out.open += 1;
        } else {
            out.closed += 1;
        }
        let small =
            bboxes[c].diag() < 0.02 * gdiag || (max_vol > 0.0 && vols[c].abs() < 1e-3 * max_vol);
        if small {
            out.debris += 1;
            let fs: Vec<usize> = (0..tris.len()).filter(|&f| comp[f] == c).collect();
            regions.push(region_from_faces(
                "part",
                "debris_part",
                &fs,
                tris,
                verts,
                1.0,
                0.9,
                comp,
            ));
        }
    }
    for a in 0..ncomp {
        for b in (a + 1)..ncomp {
            if bboxes[a].overlaps(&bboxes[b]) {
                out.overlap_edges.push((a, b));
            }
        }
    }
    out.overlap_estimated = true;
    let mut candidates = Vec::new();
    for a in 0..ncomp {
        if open[a] {
            continue;
        }
        for b in 0..ncomp {
            if a == b || open[b] {
                continue;
            }
            if bboxes[b].contains(&bboxes[a]) {
                candidates.push((a, b));
            }
        }
    }
    if !candidates.is_empty() {
        // Query the winding of `a`'s centroid against `b`'s surface only: a
        // whole-mesh BVH makes a component count itself and falsely nests a
        // part sitting in the nook of another part.
        let mut bvh_cache: HashMap<usize, MeshBvh> = HashMap::new();
        for (a, b) in candidates {
            if !bvh_cache.contains_key(&b) {
                let sub: Vec<[usize; 3]> = (0..tris.len())
                    .filter(|&f| comp[f] == b)
                    .map(|f| tris[f])
                    .collect();
                bvh_cache.insert(b, MeshBvh::from_arrays(verts, &sub));
            }
            if bvh_cache[&b].winding_at(bboxes[a].centroid()).abs() > 0.5 {
                out.nested_shells.push((a, b));
            }
        }
    }
    out
}

fn region_from_faces(
    channel: &str,
    kind: &str,
    faces: &[usize],
    tris: &[[usize; 3]],
    verts: &[[f64; 3]],
    severity: f64,
    confidence: f64,
    comp: &[usize],
) -> Region {
    let mut b = Bbox::empty();
    let mut vs = HashSet::new();
    let mut cs = HashSet::new();
    for &f in faces {
        for &v in &tris[f] {
            b.add(verts[v]);
            vs.insert(v);
        }
        if f < comp.len() {
            cs.insert(comp[f]);
        }
    }
    let mut components: Vec<usize> = cs.into_iter().collect();
    components.sort_unstable();
    Region {
        channel: channel.into(),
        kind: kind.into(),
        bbox: b.flat(),
        centroid: b.centroid(),
        faces: faces.len(),
        vertices: vs.len(),
        components,
        severity,
        confidence,
        min_face: faces.iter().copied().min().unwrap_or(usize::MAX),
    }
}

fn region_from_vertices(
    channel: &str,
    kind: &str,
    vs: &[usize],
    faces: &[usize],
    verts: &[[f64; 3]],
    severity: f64,
    confidence: f64,
    comp: &[usize],
) -> Region {
    let mut b = Bbox::empty();
    for &v in vs {
        b.add(verts[v]);
    }
    let mut cs = HashSet::new();
    for &f in faces {
        if f < comp.len() {
            cs.insert(comp[f]);
        }
    }
    let mut components: Vec<usize> = cs.into_iter().collect();
    components.sort_unstable();
    Region {
        channel: channel.into(),
        kind: kind.into(),
        bbox: b.flat(),
        centroid: b.centroid(),
        faces: faces.len(),
        vertices: vs.len(),
        components,
        severity,
        confidence,
        min_face: faces.iter().copied().min().unwrap_or(usize::MAX),
    }
}

pub fn diagnose_core(verts: &[[f64; 3]], tris: &[[usize; 3]], _detail: &str) -> DiagReport {
    let mut report = DiagReport {
        faces: tris.len(),
        vertices: verts.len(),
        ..Default::default()
    };
    if tris.is_empty() || verts.is_empty() {
        return report;
    }
    let fg = face_geometry(verts, tris);
    let edges = build_edges(tris);
    let (comp, ncomp) = components(tris.len(), &edges);
    let (comp, ncomp) = canonical_component_ids(verts, tris, &comp, ncomp);
    report.face_mask = vec![0u8; tris.len()];
    report.vertex_mask = vec![0u8; verts.len()];
    let mut regions = Vec::new();
    let t0 = std::time::Instant::now();
    report.orientation = orientation_report(
        verts,
        tris,
        &edges,
        &comp,
        ncomp,
        &mut report.face_mask,
        &mut regions,
    );
    report
        .timings
        .push(("orientation", t0.elapsed().as_secs_f64()));
    let t1 = std::time::Instant::now();
    report.nm_vertices = nm_vertex_report(
        verts,
        tris,
        &edges,
        &comp,
        &mut report.face_mask,
        &mut report.vertex_mask,
        &mut regions,
    );
    report
        .timings
        .push(("nm_vertex", t1.elapsed().as_secs_f64()));
    let t2 = std::time::Instant::now();
    report.duplicates = duplicate_report(
        verts,
        tris,
        &comp,
        &mut report.face_mask,
        &mut report.vertex_mask,
        &mut regions,
    );
    report
        .timings
        .push(("duplicate", t2.elapsed().as_secs_f64()));
    let t3 = std::time::Instant::now();
    report.degenerate = degenerate_report(
        verts,
        tris,
        &comp,
        &fg,
        &edges,
        &mut report.face_mask,
        &mut regions,
    );
    report
        .timings
        .push(("degenerate", t3.elapsed().as_secs_f64()));
    let t4 = std::time::Instant::now();
    report.parts = part_report(verts, tris, &edges, &fg, &comp, ncomp, &mut regions);
    report.timings.push(("parts", t4.elapsed().as_secs_f64()));
    regions.sort_by(|a, b| {
        a.channel
            .cmp(&b.channel)
            .then_with(|| a.bbox[0].total_cmp(&b.bbox[0]))
            .then_with(|| a.bbox[1].total_cmp(&b.bbox[1]))
            .then_with(|| a.bbox[2].total_cmp(&b.bbox[2]))
            .then_with(|| a.bbox[3].total_cmp(&b.bbox[3]))
            .then_with(|| a.bbox[4].total_cmp(&b.bbox[4]))
            .then_with(|| a.bbox[5].total_cmp(&b.bbox[5]))
            .then_with(|| a.faces.cmp(&b.faces))
            .then_with(|| a.vertices.cmp(&b.vertices))
            .then_with(|| a.min_face.cmp(&b.min_face))
            .then_with(|| a.kind.cmp(&b.kind))
    });
    regions.truncate(256);
    report.regions = regions;
    report
}

/// Python/JSON cannot represent NaN or infinity; every float in the report is
/// clamped to a finite value before it crosses the binding.
#[inline]
fn finite(x: f64) -> f64 {
    if x.is_finite() {
        x
    } else {
        0.0
    }
}

fn finite3(a: [f64; 3]) -> [f64; 3] {
    [finite(a[0]), finite(a[1]), finite(a[2])]
}

fn finite6(a: [f64; 6]) -> [f64; 6] {
    [
        finite(a[0]),
        finite(a[1]),
        finite(a[2]),
        finite(a[3]),
        finite(a[4]),
        finite(a[5]),
    ]
}

fn rle(mask: &[u8]) -> Vec<(usize, usize, u8)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < mask.len() {
        if mask[i] == 0 {
            i += 1;
            continue;
        }
        let start = i;
        let val = mask[i];
        while i < mask.len() && mask[i] == val {
            i += 1;
        }
        out.push((start, i - start, val));
    }
    out
}

fn channel_dict<'py>(
    py: Python<'py>,
    count: Option<i64>,
    estimated: bool,
    available: bool,
    severity: f64,
    confidence: f64,
    method: &str,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("count", count)?;
    d.set_item("estimated", estimated)?;
    d.set_item("available", available)?;
    d.set_item("severity", finite(severity))?;
    d.set_item("confidence", finite(confidence))?;
    d.set_item("method", method)?;
    Ok(d)
}

fn mask_dict<'py>(py: Python<'py>, mask: &[u8], encoding: &str) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("encoding", encoding)?;
    d.set_item("length", mask.len())?;
    let runs_vec = rle(mask);
    let estimated_bytes = runs_vec.len() * 24 + 16;
    if estimated_bytes > 1_000_000 {
        d.set_item("omitted", true)?;
        d.set_item("reason", "size")?;
        return Ok(d);
    }
    let runs = PyList::empty(py);
    for (s, n, v) in runs_vec {
        runs.append((s, n, v))?;
    }
    d.set_item("runs", runs)?;
    d.set_item("omitted", false)?;
    Ok(d)
}

pub fn report_to_pydict<'py>(
    py: Python<'py>,
    r: &DiagReport,
    detail: &str,
    masks: bool,
    seconds: f64,
) -> PyResult<Bound<'py, PyDict>> {
    let root = PyDict::new(py);
    root.set_item("schema_version", 1)?;
    root.set_item("detail", detail)?;
    let ch = PyDict::new(py);
    ch.set_item(
        "orientation",
        channel_dict(
            py,
            Some((r.orientation.inconsistent_edges + r.orientation.flipped_faces) as i64),
            false,
            true,
            r.orientation.flipped_faces as f64,
            1.0,
            "edge_table+bfs",
        )?,
    )?;
    ch.set_item(
        "nm_vertex",
        channel_dict(
            py,
            Some(r.nm_vertices.vertices as i64),
            false,
            true,
            r.nm_vertices.vertices as f64,
            1.0,
            "vertex_fan",
        )?,
    )?;
    ch.set_item(
        "duplicate",
        channel_dict(
            py,
            Some((r.duplicates.faces + r.duplicates.near_vertices) as i64),
            true,
            true,
            (r.duplicates.faces + r.duplicates.near_vertices) as f64,
            0.9,
            "hash+grid",
        )?,
    )?;
    ch.set_item(
        "degenerate",
        channel_dict(
            py,
            Some((r.degenerate.zero_area + r.degenerate.needle) as i64),
            false,
            true,
            (r.degenerate.zero_area + r.degenerate.needle) as f64,
            0.95,
            "area+quality",
        )?,
    )?;
    ch.set_item(
        "self_intersection",
        channel_dict(py, None, true, false, 0.0, 0.0, "phase2")?,
    )?;
    ch.set_item(
        "loops",
        channel_dict(py, None, true, false, 0.0, 0.0, "phase2")?,
    )?;
    ch.set_item(
        "parts",
        channel_dict(
            py,
            Some((r.parts.debris + r.parts.overlap_edges.len()) as i64),
            true,
            true,
            r.parts.debris as f64,
            0.8,
            "components+aabb",
        )?,
    )?;
    ch.set_item(
        "interior",
        channel_dict(py, None, true, false, 0.0, 0.0, "phase3")?,
    )?;
    ch.set_item(
        "thin_wall",
        channel_dict(py, None, true, false, 0.0, 0.0, "phase3")?,
    )?;
    ch.set_item(
        "noise",
        channel_dict(py, None, true, false, 0.0, 0.0, "phase3")?,
    )?;
    root.set_item("channels", ch)?;

    let o = PyDict::new(py);
    o.set_item("inconsistent_edges", r.orientation.inconsistent_edges)?;
    o.set_item("flipped_faces", r.orientation.flipped_faces)?;
    o.set_item("flipped_patches", r.orientation.flipped_patches)?;
    o.set_item(
        "volume_signs",
        r.orientation
            .volume_signs
            .iter()
            .map(|x| finite(*x))
            .collect::<Vec<f64>>(),
    )?;
    root.set_item("orientation", o)?;

    let n = PyDict::new(py);
    n.set_item("vertices", r.nm_vertices.vertices)?;
    n.set_item("bowtie", r.nm_vertices.bowtie)?;
    n.set_item("pinched", r.nm_vertices.pinched)?;
    root.set_item("nm_vertices", n)?;

    let d = PyDict::new(py);
    d.set_item("faces", r.duplicates.faces)?;
    d.set_item("reversed_faces", r.duplicates.reversed_faces)?;
    d.set_item("vertices", r.duplicates.vertices)?;
    d.set_item("near_vertices", r.duplicates.near_vertices)?;
    root.set_item("duplicates", d)?;

    let g = PyDict::new(py);
    g.set_item("zero_area", r.degenerate.zero_area)?;
    g.set_item("needle", r.degenerate.needle)?;
    g.set_item("load_bearing", r.degenerate.load_bearing)?;
    g.set_item("quality_p99", finite(r.degenerate.quality_p99))?;
    root.set_item("degenerate", g)?;

    let p = PyDict::new(py);
    p.set_item("closed", r.parts.closed)?;
    p.set_item("open", r.parts.open)?;
    p.set_item("debris", r.parts.debris)?;
    p.set_item("overlap_edges", r.parts.overlap_edges.clone())?;
    p.set_item("nested_shells", r.parts.nested_shells.clone())?;
    p.set_item("overlap_estimated", r.parts.overlap_estimated)?;
    root.set_item("parts", p)?;

    if masks {
        root.set_item("face_mask", mask_dict(py, &r.face_mask, "rle")?)?;
        root.set_item("vertex_mask", mask_dict(py, &r.vertex_mask, "rle")?)?;
    }
    let regions = PyList::empty(py);
    for g in &r.regions {
        let d = PyDict::new(py);
        d.set_item("channel", &g.channel)?;
        d.set_item("kind", &g.kind)?;
        d.set_item("bbox", finite6(g.bbox))?;
        d.set_item("centroid", finite3(g.centroid))?;
        d.set_item("faces", g.faces)?;
        d.set_item("vertices", g.vertices)?;
        d.set_item("components", g.components.clone())?;
        d.set_item("severity", finite(g.severity))?;
        d.set_item("confidence", finite(g.confidence))?;
        regions.append(d)?;
    }
    root.set_item("regions", regions)?;
    let meta = PyDict::new(py);
    meta.set_item("faces", r.faces)?;
    meta.set_item("vertices", r.vertices)?;
    meta.set_item("seconds", finite(seconds))?;
    meta.set_item("phase", 1)?;
    meta.set_item("fast_budget_s", 1.0)?;
    meta.set_item("fast_budget_met", seconds <= 1.0)?;
    let timings = PyDict::new(py);
    for (name, sec) in &r.timings {
        timings.set_item(name, finite(*sec))?;
    }
    meta.set_item("channel_seconds", timings)?;
    meta.set_item(
        "channels_available",
        vec![
            "orientation",
            "nm_vertex",
            "duplicate",
            "degenerate",
            "parts",
        ],
    )?;
    root.set_item("meta", meta)?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let v = vec![
            [0., 0., 0.],
            [1., 0., 0.],
            [1., 1., 0.],
            [0., 1., 0.],
            [0., 0., 1.],
            [1., 0., 1.],
            [1., 1., 1.],
            [0., 1., 1.],
        ];
        let f = vec![
            [0, 1, 2],
            [0, 2, 3],
            [4, 6, 5],
            [4, 7, 6],
            [0, 4, 5],
            [0, 5, 1],
            [3, 2, 6],
            [3, 6, 7],
            [0, 3, 7],
            [0, 7, 4],
            [1, 5, 6],
            [1, 6, 2],
        ];
        (v, f)
    }

    #[test]
    fn clean_cube_has_no_phase1_defects() {
        let (v, f) = cube();
        let r = diagnose_core(&v, &f, "fast");
        assert_eq!(r.orientation.inconsistent_edges, 0);
        assert_eq!(r.orientation.flipped_faces, 0);
        assert_eq!(r.nm_vertices.vertices, 0);
        assert_eq!(r.duplicates.faces, 0);
        assert_eq!(r.degenerate.zero_area, 0);
        assert_eq!(r.parts.closed, 1);
        assert_eq!(r.parts.open, 0);
    }

    #[test]
    fn flipped_face_and_duplicate_are_reported() {
        let (v, mut f) = cube();
        f[0] = [0, 2, 1];
        let r = diagnose_core(&v, &f, "fast");
        assert!(r.orientation.flipped_faces > 0);
        let mut f2 = cube().1;
        f2.push(f2[1]);
        let r2 = diagnose_core(&v, &f2, "fast");
        assert!(r2.duplicates.faces > 0);
    }

    #[test]
    fn zero_area_face_is_degenerate() {
        let (mut v, f) = cube();
        v[1] = v[0];
        let r = diagnose_core(&v, &f, "fast");
        assert!(r.degenerate.zero_area > 0);
    }

    #[test]
    fn two_separate_cubes_are_two_parts() {
        let (v, f) = cube();
        let mut v2 = v.clone();
        for p in &mut v2 {
            p[0] += 3.0;
        }
        let mut vv = v.clone();
        vv.extend(v2);
        let mut ff = f.clone();
        let base = v.len();
        for t in &f {
            ff.push([t[0] + base, t[1] + base, t[2] + base]);
        }
        let r = diagnose_core(&vv, &ff, "fast");
        assert_eq!(r.parts.closed, 2);
        assert!(r.parts.overlap_edges.is_empty());
    }


    fn box_mesh(lo: f64, hi: f64) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let (v, f) = cube();
        let v = v
            .iter()
            .map(|p| [lo + p[0] * (hi - lo), lo + p[1] * (hi - lo), lo + p[2] * (hi - lo)])
            .collect();
        (v, f)
    }

    /// Closed L-shaped prism: a non-convex part whose bounding box has an
    /// empty nook.
    fn l_prism() -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let poly = [
            [0.0, 0.0],
            [3.0, 0.0],
            [3.0, 1.0],
            [1.0, 1.0],
            [1.0, 3.0],
            [0.0, 3.0],
        ];
        let mut v = Vec::with_capacity(12);
        for z in [0.0, 1.0] {
            for p in &poly {
                v.push([p[0], p[1], z]);
            }
        }
        let mut f = Vec::new();
        for t in [[0, 1, 2], [0, 2, 3], [0, 3, 4], [0, 4, 5]] {
            // Top (z = 1) keeps the CCW order (+z); the bottom is reversed.
            f.push([t[0] + 6, t[1] + 6, t[2] + 6]);
            f.push([t[2], t[1], t[0]]);
        }
        for i in 0..6 {
            let j = (i + 1) % 6;
            f.push([i, j, j + 6]);
            f.push([i, j + 6, i + 6]);
        }
        (v, f)
    }

    fn append_meshes(
        a: (Vec<[f64; 3]>, Vec<[usize; 3]>),
        b: (Vec<[f64; 3]>, Vec<[usize; 3]>),
    ) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let base = a.0.len();
        let mut v = a.0;
        v.extend(b.0);
        let mut f = a.1;
        for t in b.1 {
            f.push([t[0] + base, t[1] + base, t[2] + base]);
        }
        (v, f)
    }

    fn two_cubes(offset: [f64; 3]) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let mut second = cube();
        for p in &mut second.0 {
            for k in 0..3 {
                p[k] += offset[k];
            }
        }
        append_meshes(cube(), second)
    }

    #[test]
    fn nested_shell_does_not_count_the_part_itself() {
        let lshape = l_prism();
        // A cube in the empty nook: inside the L's bbox, outside its solid.
        let nook = box_mesh(0.0, 1.0);
        let nook = (
            nook.0
                .iter()
                .map(|p| [1.5 + p[0], 1.5 + p[1], 0.2 + 0.6 * p[2]])
                .collect(),
            nook.1,
        );
        let (v, f) = append_meshes(lshape.clone(), nook);
        let r = diagnose_core(&v, &f, "fast");
        assert_eq!(r.parts.closed, 2);
        assert!(r.parts.nested_shells.is_empty());

        // The same cube inside the L arm is genuinely nested.
        let arm = box_mesh(0.0, 1.0);
        let arm = (
            arm.0
                .iter()
                .map(|p| [0.2 + 0.6 * p[0], 0.2 + 0.6 * p[1], 0.2 + 0.6 * p[2]])
                .collect(),
            arm.1,
        );
        let (v2, f2) = append_meshes(lshape, arm);
        let r2 = diagnose_core(&v2, &f2, "fast");
        assert_eq!(r2.parts.closed, 2);
        assert!(!r2.parts.nested_shells.is_empty());
    }

    #[test]
    fn component_ids_are_face_order_independent() {
        let (v, f) = two_cubes([3.0, 0.0, 0.0]);
        let a = diagnose_core(&v, &f, "fast");
        let mut fp: Vec<[usize; 3]> = f.iter().rev().cloned().collect();
        fp.swap(1, 7);
        let b = diagnose_core(&v, &fp, "fast");
        assert_eq!(a.orientation.volume_signs, b.orientation.volume_signs);
        assert_eq!(a.parts.overlap_edges, b.parts.overlap_edges);
    }

    #[test]
    fn face_order_does_not_change_counts() {
        let (v, f) = cube();
        let a = diagnose_core(&v, &f, "fast");
        let mut fp: Vec<[usize; 3]> = f.iter().rev().cloned().collect();
        fp.swap(0, 3);
        let b = diagnose_core(&v, &fp, "fast");
        assert_eq!(a.orientation.flipped_faces, b.orientation.flipped_faces);
        assert_eq!(a.nm_vertices.vertices, b.nm_vertices.vertices);
        assert_eq!(a.duplicates.faces, b.duplicates.faces);
        assert_eq!(a.degenerate.zero_area, b.degenerate.zero_area);
    }
}
