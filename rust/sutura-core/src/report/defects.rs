//! Hole and non-manifold region detection, matching `sutura/defects.py`.

use std::collections::{BTreeSet, HashMap};

use super::round_to;

#[derive(Clone, Debug, PartialEq)]
pub struct Hole {
    pub centroid: [f64; 3],
    pub diameter: f64,
    pub vertices: usize,
    pub verts_idx: Option<Vec<i64>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NonManifoldRegion {
    pub centroid: [f64; 3],
    pub faces: usize,
    pub verts_idx: Option<Vec<i64>>,
    pub faces_idx: Option<Vec<i64>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DefectReport {
    pub holes: Vec<Hole>,
    pub non_manifold: Vec<NonManifoldRegion>,
}

pub fn detect_defects(verts: &[[f32; 3]], tris: &[[i32; 3]], with_indices: bool) -> DefectReport {
    DefectReport {
        holes: detect_holes(verts, tris, with_indices),
        non_manifold: detect_non_manifold(verts, tris, with_indices),
    }
}

pub fn detect_holes(verts: &[[f32; 3]], tris: &[[i32; 3]], with_indices: bool) -> Vec<Hole> {
    let mut counts: HashMap<(i64, i64), usize> = HashMap::new();
    for t in tris {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a <= b {
                (a as i64, b as i64)
            } else {
                (b as i64, a as i64)
            };
            *counts.entry(key).or_insert(0) += 1;
        }
    }
    let mut adj: HashMap<i64, Vec<i64>> = HashMap::new();
    for (&(a, b), &count) in &counts {
        if count == 1 {
            adj.entry(a).or_default().push(b);
            adj.entry(b).or_default().push(a);
        }
    }
    if adj.is_empty() {
        return Vec::new();
    }
    for nbs in adj.values_mut() {
        nbs.sort_unstable();
    }
    let mut used: BTreeSet<i64> = BTreeSet::new();
    let mut loops: Vec<Vec<i64>> = Vec::new();
    let mut starts: Vec<i64> = adj.keys().copied().collect();
    starts.sort_unstable();
    for start in starts {
        if used.contains(&start) {
            continue;
        }
        let mut lp = vec![start];
        used.insert(start);
        let mut cur = start;
        let mut prev = i64::MIN;
        loop {
            let mut nxt = None;
            for &nb in adj.get(&cur).map(Vec::as_slice).unwrap_or(&[]) {
                if nb == prev {
                    continue;
                }
                if nb == start && lp.len() > 2 {
                    nxt = Some(start);
                    break;
                }
                if !used.contains(&nb) {
                    nxt = Some(nb);
                    break;
                }
            }
            let Some(nxt) = nxt else { break };
            if nxt == start {
                break;
            }
            used.insert(nxt);
            lp.push(nxt);
            prev = cur;
            cur = nxt;
        }
        if lp.len() >= 3 {
            loops.push(lp);
        }
    }
    loops
        .into_iter()
        .map(|lp| {
            let mut lo = [f32::INFINITY; 3];
            let mut hi = [f32::NEG_INFINITY; 3];
            let mut sum = [0f32; 3];
            for &i in &lp {
                let p = verts[i as usize];
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                    sum[k] += p[k];
                }
            }
            let n = lp.len() as f32;
            let d = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
            Hole {
                centroid: [
                    (sum[0] / n) as f64,
                    (sum[1] / n) as f64,
                    (sum[2] / n) as f64,
                ],
                diameter: round_to(((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]) as f64).sqrt(), 4),
                vertices: lp.len(),
                verts_idx: with_indices.then_some(lp),
            }
        })
        .collect()
}

pub fn detect_non_manifold(
    verts: &[[f32; 3]],
    tris: &[[i32; 3]],
    with_indices: bool,
) -> Vec<NonManifoldRegion> {
    let f = tris.len();
    if f == 0 {
        return Vec::new();
    }
    let mut edge_faces: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (fi, t) in tris.iter().enumerate() {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a <= b {
                (a as i64, b as i64)
            } else {
                (b as i64, a as i64)
            };
            edge_faces.entry(key).or_default().push(fi);
        }
    }
    let mut mask = vec![false; f];
    let mut any = false;
    for faces in edge_faces.values() {
        if faces.len() > 2 {
            any = true;
            for &fi in faces {
                mask[fi] = true;
            }
        }
    }
    if !any {
        return Vec::new();
    }
    let mut parent: Vec<usize> = (0..f).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for faces in edge_faces.values() {
        let masked: Vec<usize> = faces.iter().copied().filter(|&x| mask[x]).collect();
        for w in masked.windows(2) {
            let a = find(&mut parent, w[0]);
            let b = find(&mut parent, w[1]);
            if a != b {
                parent[a] = b;
            }
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for fi in 0..f {
        if mask[fi] {
            let r = find(&mut parent, fi);
            groups.entry(r).or_default().push(fi);
        }
    }
    let mut regions: Vec<(Vec<i64>, NonManifoldRegion)> = Vec::new();
    for mut g in groups.into_values() {
        g.sort_unstable();
        let mut vs: Vec<i64> = g.iter().flat_map(|&fi| tris[fi]).map(i64::from).collect();
        vs.sort_unstable();
        vs.dedup();
        let mut sum = [0f32; 3];
        for &i in &vs {
            let p = verts[i as usize];
            for k in 0..3 {
                sum[k] += p[k];
            }
        }
        let n = vs.len() as f32;
        let region = NonManifoldRegion {
            centroid: [
                (sum[0] / n) as f64,
                (sum[1] / n) as f64,
                (sum[2] / n) as f64,
            ],
            faces: g.len(),
            verts_idx: with_indices.then_some(vs.clone()),
            faces_idx: with_indices.then_some(g.iter().map(|&x| x as i64).collect()),
        };
        regions.push((vs, region));
    }
    regions.sort_by(|a, b| a.0.cmp(&b.0));
    regions.into_iter().map(|(_, r)| r).collect()
}
