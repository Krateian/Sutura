//! Read-only small-part / possible-debris measurement, matching
//! `sutura_engine/debris.py`.

use std::collections::HashMap;

use sutura_geom::winding::MeshBvh;

use super::round_to;

const MAX_REPORTED: usize = 32;
const MAX_DISTANCE_SAMPLES: usize = 20_000;
pub const TAU_VOL: f64 = 0.005;
const ZERO_VOL_REL: f64 = 1e-9;
const DEGENERATE_REL: f64 = 1e-9;

#[derive(Clone, Debug, PartialEq)]
pub struct SmallPart {
    pub faces: usize,
    pub size_mm: f64,
    pub size_pct: Option<f64>,
    pub distance_mm: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RemovedSmallParts {
    pub count: usize,
    pub min_faces: usize,
    pub faces_total: usize,
    pub capped: bool,
    pub parts: Vec<SmallPart>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PossibleDebrisPart {
    pub faces: usize,
    pub volume_pct: f64,
    pub size_pct: f64,
    pub distance_mm: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PossibleDebris {
    pub count: usize,
    pub capped: bool,
    pub parts: Vec<PossibleDebrisPart>,
}

struct Dsu {
    parent: Vec<usize>,
}

impl Dsu {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }
    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent[ra] = rb;
        }
    }
}

struct Tables {
    labels: Vec<usize>,
    n_comp: usize,
    faces: Vec<usize>,
    vols: Vec<f64>,
    areas: Vec<f64>,
    largest: usize,
}

fn components(verts: &[[f64; 3]], tris: &[[i32; 3]]) -> (Vec<usize>, usize) {
    if tris.is_empty() || verts.is_empty() {
        return (vec![0; tris.len()], 0);
    }
    let mut dsu = Dsu::new(verts.len());
    for t in tris {
        dsu.union(t[0] as usize, t[1] as usize);
        dsu.union(t[1] as usize, t[2] as usize);
        dsu.union(t[2] as usize, t[0] as usize);
    }
    let mut map: HashMap<usize, usize> = HashMap::new();
    let mut labels = Vec::with_capacity(verts.len());
    for v in 0..verts.len() {
        let r = dsu.find(v);
        let next = map.len();
        labels.push(*map.entry(r).or_insert(next));
    }
    let face_labels = tris.iter().map(|t| labels[t[0] as usize]).collect();
    (face_labels, map.len())
}

fn face_metrics(verts: &[[f64; 3]], tris: &[[i32; 3]]) -> (Vec<f64>, Vec<f64>) {
    let mut areas = Vec::with_capacity(tris.len());
    let mut vols = Vec::with_capacity(tris.len());
    for t in tris {
        let a = verts[t[0] as usize];
        let b = verts[t[1] as usize];
        let c = verts[t[2] as usize];
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let cr = cross(ab, ac);
        areas.push(0.5 * dot(cr, cr).sqrt());
        vols.push(dot(a, cross(b, c)) / 6.0);
    }
    (areas, vols)
}

fn tables(verts: &[[f64; 3]], tris: &[[i32; 3]]) -> Option<Tables> {
    let (labels, n_comp) = components(verts, tris);
    if n_comp == 0 {
        return None;
    }
    let (area, vol) = face_metrics(verts, tris);
    let mut faces = vec![0usize; n_comp];
    let mut vols = vec![0f64; n_comp];
    let mut areas = vec![0f64; n_comp];
    for (i, &c) in labels.iter().enumerate() {
        faces[c] += 1;
        vols[c] += vol[i];
        areas[c] += area[i];
    }
    let largest = (0..n_comp)
        .max_by(|&a, &b| vols[a].abs().total_cmp(&vols[b].abs()))
        .unwrap_or(0);
    Some(Tables {
        labels,
        n_comp,
        faces,
        vols,
        areas,
        largest,
    })
}

fn comp_faces(labels: &[usize], c: usize) -> Vec<usize> {
    labels
        .iter()
        .enumerate()
        .filter(|(_, &x)| x == c)
        .map(|(i, _)| i)
        .collect()
}

fn bvid(tris: &[[i32; 3]], face_idx: &[usize]) -> Vec<i64> {
    let mut out: Vec<i64> = face_idx
        .iter()
        .flat_map(|&f| tris[f])
        .map(i64::from)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

fn edge_open(faces: &[[i32; 3]]) -> bool {
    if faces.is_empty() {
        return false;
    }
    let mut edges: Vec<(i64, i64)> = Vec::with_capacity(faces.len() * 3);
    for t in faces {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            edges.push(if a <= b {
                (i64::from(a), i64::from(b))
            } else {
                (i64::from(b), i64::from(a))
            });
        }
    }
    edges.sort_unstable();
    let mut i = 0;
    while i < edges.len() {
        let mut j = i + 1;
        while j < edges.len() && edges[j] == edges[i] {
            j += 1;
        }
        if j - i != 2 {
            return true;
        }
        i = j;
    }
    false
}

fn subset(
    verts: &[[f64; 3]],
    tris: &[[i32; 3]],
    face_idx: &[usize],
) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
    let sub: Vec<[i32; 3]> = face_idx.iter().map(|&i| tris[i]).collect();
    if sub.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut used: Vec<i64> = sub.iter().flat_map(|t| *t).map(i64::from).collect();
    used.sort_unstable();
    used.dedup();
    let mut remap: HashMap<i64, usize> = HashMap::new();
    for (i, &v) in used.iter().enumerate() {
        remap.insert(v, i);
    }
    let mv = used.iter().map(|&v| verts[v as usize]).collect();
    let mt = sub
        .iter()
        .map(|t| {
            [
                remap[&i64::from(t[0])],
                remap[&i64::from(t[1])],
                remap[&i64::from(t[2])],
            ]
        })
        .collect();
    (mv, mt)
}

fn distance_to_main(
    verts: &[[f64; 3]],
    tris: &[[i32; 3]],
    query_vids: &[i64],
    main_faces: &[usize],
) -> Option<f64> {
    if query_vids.is_empty() || main_faces.is_empty() {
        return None;
    }
    let (mv, mt) = subset(verts, tris, main_faces);
    if mt.is_empty() {
        return None;
    }
    let mut pts: Vec<[f64; 3]> = query_vids.iter().map(|&i| verts[i as usize]).collect();
    if pts.len() > MAX_DISTANCE_SAMPLES {
        let last = pts.len() - 1;
        pts = (0..MAX_DISTANCE_SAMPLES)
            .map(|i| {
                let t = i as f64 * last as f64 / (MAX_DISTANCE_SAMPLES - 1) as f64;
                pts[t.floor() as usize]
            })
            .collect();
    }
    let bvh = MeshBvh::from_arrays(&mv, &mt);
    let mut best = f64::INFINITY;
    for p in pts {
        let (_, d, _) = bvh.closest_point(p);
        best = best.min(d);
    }
    if !best.is_finite() {
        return None;
    }
    Some(round_to(best, 4))
}

pub fn removed_small_parts(
    verts: &[[f64; 3]],
    tris: &[[i32; 3]],
    min_faces: usize,
) -> Option<RemovedSmallParts> {
    let tab = tables(verts, tris)?;
    let mut small: Vec<usize> = (0..tab.n_comp)
        .filter(|&c| tab.faces[c] > 0 && tab.faces[c] < min_faces)
        .collect();
    if small.is_empty() {
        return None;
    }
    small.sort_by_key(|&c| std::cmp::Reverse(tab.faces[c]));
    let gdiag = global_diag(verts);
    let main_faces = comp_faces(&tab.labels, tab.largest);
    let mut parts = Vec::new();
    for &c in small.iter().take(MAX_REPORTED) {
        let f_idx = comp_faces(&tab.labels, c);
        let vids = bvid(tris, &f_idx);
        let size = if vids.is_empty() {
            0.0
        } else {
            bbox_diag(verts, &vids)
        };
        parts.push(SmallPart {
            faces: tab.faces[c],
            size_mm: round_to(size, 4),
            size_pct: if gdiag > 0.0 {
                Some(round_to(100.0 * size / gdiag, 3))
            } else {
                None
            },
            distance_mm: distance_to_main(verts, tris, &vids, &main_faces),
        });
    }
    Some(RemovedSmallParts {
        count: small.len(),
        min_faces,
        faces_total: small.iter().map(|&c| tab.faces[c]).sum(),
        capped: small.len() > MAX_REPORTED,
        parts,
    })
}

pub fn possible_debris(
    verts: &[[f64; 3]],
    tris: &[[i32; 3]],
    min_faces: usize,
    tau: f64,
) -> Option<PossibleDebris> {
    let tab = tables(verts, tris)?;
    if tab.n_comp <= 1 {
        return None;
    }
    let gdiag = global_diag(verts);
    if gdiag <= 0.0 {
        return None;
    }
    let max_vol = tab.vols.iter().map(|v| v.abs()).fold(0.0, f64::max);
    if max_vol <= 0.0 {
        return None;
    }
    let zero_vol = ZERO_VOL_REL * gdiag.powi(3);
    let degenerate = DEGENERATE_REL * gdiag.powi(2);
    let main_faces = comp_faces(&tab.labels, tab.largest);
    let mut flagged = Vec::new();
    for c in 0..tab.n_comp {
        if c == tab.largest || tab.faces[c] < min_faces {
            continue;
        }
        if tab.vols[c].abs() <= zero_vol || tab.areas[c] <= degenerate {
            continue;
        }
        if tab.vols[c].abs() >= tau * max_vol {
            continue;
        }
        let f_idx = comp_faces(&tab.labels, c);
        let sub: Vec<[i32; 3]> = f_idx.iter().map(|&i| tris[i]).collect();
        if edge_open(&sub) {
            continue;
        }
        let vids = bvid(tris, &f_idx);
        let size = bbox_diag(verts, &vids);
        flagged.push(PossibleDebrisPart {
            faces: tab.faces[c],
            volume_pct: round_to(100.0 * tab.vols[c].abs() / max_vol, 4),
            size_pct: round_to(100.0 * size / gdiag, 3),
            distance_mm: distance_to_main(verts, tris, &vids, &main_faces),
        });
    }
    if flagged.is_empty() {
        return None;
    }
    flagged.sort_by(|a, b| b.volume_pct.total_cmp(&a.volume_pct));
    let count = flagged.len();
    flagged.truncate(MAX_REPORTED);
    Some(PossibleDebris {
        count,
        capped: count > MAX_REPORTED,
        parts: flagged,
    })
}

fn global_diag(verts: &[[f64; 3]]) -> f64 {
    if verts.is_empty() {
        return 0.0;
    }
    let mut lo = verts[0];
    let mut hi = verts[0];
    for p in verts {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let d = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    dot(d, d).sqrt()
}

fn bbox_diag(verts: &[[f64; 3]], ids: &[i64]) -> f64 {
    let mut lo = verts[ids[0] as usize];
    let mut hi = lo;
    for &i in ids {
        let p = verts[i as usize];
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let d = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    dot(d, d).sqrt()
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
