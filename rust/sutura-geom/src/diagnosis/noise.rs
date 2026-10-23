//! Diagnosis v2 phase 3: scan noise / high-frequency surface trace.
//!
//! The signal is the angle between a vertex normal (area-weighted incident
//! faces) and the average of its 1-ring neighbours' vertex normals.  A smooth
//! surface has a small deviation; a noisy scan has a large one.  Vertices on a
//! boundary or on a sharp crease are excluded so legitimate high-frequency
//! detail (text, logos, machined edges) is not reported as noise (plan A.10).
//!
//! The channel is a heuristic score, so it is always `estimated` and carries a
//! low confidence.  Valence distribution and the edge-length coefficient of
//! variation are reported alongside as supporting signals.
//!
//! Budget (plan I3): `deep` evaluates every vertex.  `fast` evaluates a fixed
//! geometric stride of at most [`FAST_MAX_VERTICES`] vertices and reports the
//! stride in `sample_stride`; its channel method names the sampling mode.
//!
//! Determinism: face normals are accumulated in geometric face order, ring
//! normals are summed in sorted order, and every aggregate comes from a sorted
//! vector, so a face/vertex permutation changes nothing.

use std::collections::{HashMap, HashSet};

use super::{
    face_geo_cmp, group_faces, point3_cmp, region_from_faces, tri_centroid, unit_normal, Region,
};

// The face mask is a u8 (bits 1/2/4/8/16 are phase 1 and 64/128 are interior
// and thin wall), so noise takes the remaining free face bit 32.  The plan's
// suggested "256" cannot be represented; keeping the mask one byte preserves
// the phase-1/2 RLE schema.
pub const FACE_MASK_BIT: u8 = 32;
/// Vertex mask bit for a noisy vertex (vertex bits 2/4/32 are already used).
pub const VERTEX_MASK_BIT: u8 = 64;

pub const MAX_NOISE_VERTICES: usize = 1_000_000;
/// Fast mode evaluates at most this many vertices (fixed geometric stride).
pub const FAST_MAX_VERTICES: usize = 8_192;
pub const MAX_NOISE_REGIONS: usize = 256;

/// Deviation above which a smooth-region vertex is considered noisy (~8.6°).
pub const DEV_THRESHOLD: f64 = 0.15;
/// `dot(n1, n2) < 0.866` is a crease sharper than 30°; those vertices are
/// excluded from the noise statistic (text/logo/machined detail).
pub const SHARP_COS: f64 = 0.866;

const CONFIDENCE: f64 = 0.6;

#[derive(Clone, Debug, Default)]
pub struct Noise {
    pub available: bool,
    pub noise_score: f64,
    pub noise_ratio: f64,
    pub flagged_vertices: usize,
    pub eligible_vertices: usize,
    pub dev_p50: f64,
    pub dev_p95: f64,
    pub valence_mean: f64,
    pub valence_std: f64,
    pub edge_cv: f64,
    pub sample_stride: usize,
    pub method: &'static str,
    pub estimated: bool,
    pub confidence: f64,
}

#[inline]
fn angle(a: [f64; 3], b: [f64; 3]) -> f64 {
    let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    dot.clamp(-1.0, 1.0).acos()
}

#[inline]
fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

#[inline]
fn normal_cmp(a: &[f64; 3], b: &[f64; 3]) -> std::cmp::Ordering {
    a[0].total_cmp(&b[0])
        .then_with(|| a[1].total_cmp(&b[1]))
        .then_with(|| a[2].total_cmp(&b[2]))
}

fn mean_std(sorted: &[f64]) -> (f64, f64) {
    if sorted.is_empty() {
        return (0.0, 0.0);
    }
    let mut sum = 0.0f64;
    for &x in sorted {
        sum += x;
    }
    let mean = sum / sorted.len() as f64;
    let mut var = 0.0f64;
    for &x in sorted {
        let d = x - mean;
        var += d * d;
    }
    (mean, (var / sorted.len() as f64).sqrt())
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * q).floor() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

pub fn noise_report(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    edges: &HashMap<(usize, usize), Vec<(usize, bool)>>,
    detail: &str,
    face_mask: &mut [u8],
    vertex_mask: &mut [u8],
    regions: &mut Vec<Region>,
) -> Noise {
    let nv = verts.len();
    let nf = tris.len();
    let deep = detail == "deep";
    let mut out = Noise {
        eligible_vertices: nv,
        method: if deep {
            "ring_deviation"
        } else {
            "ring_deviation+stride"
        },
        estimated: true,
        confidence: CONFIDENCE,
        ..Default::default()
    };
    if nv > MAX_NOISE_VERTICES {
        out.available = false;
        return out;
    }
    out.available = true;
    if nv == 0 || nf == 0 {
        return out;
    }

    // Face normals/areas, accumulated into vertex normals in geometric face
    // order (permutation-invariant float sums, no per-vertex face lists).
    let normals: Vec<[f64; 3]> = tris.iter().map(|&t| unit_normal(verts, t)).collect();
    let mut areas = vec![0.0f64; nf];
    for (f, &t) in tris.iter().enumerate() {
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
        areas[f] = 0.5 * (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]).sqrt();
    }
    let centroids: Vec<[f64; 3]> = tris.iter().map(|&t| tri_centroid(verts, t)).collect();
    let mut face_order: Vec<usize> = (0..nf).collect();
    face_order.sort_by(|&a, &b| {
        point3_cmp(centroids[a], centroids[b]).then_with(|| face_geo_cmp(verts, tris[a], tris[b]))
    });
    let mut vnorm = vec![[0.0f64; 3]; nv];
    let mut used = vec![false; nv];
    for &f in &face_order {
        for &v in &tris[f] {
            used[v] = true;
            let w = areas[f];
            vnorm[v][0] += normals[f][0] * w;
            vnorm[v][1] += normals[f][1] * w;
            vnorm[v][2] += normals[f][2] * w;
        }
    }
    for v in 0..nv {
        let n = vnorm[v];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len > 0.0 {
            vnorm[v] = [n[0] / len, n[1] / len, n[2] / len];
        }
    }

    // Vertex sample: deep = every vertex, fast = fixed geometric stride.
    let mut sampled = vec![true; nv];
    let stride = if deep {
        1
    } else {
        ((nv + FAST_MAX_VERTICES - 1) / FAST_MAX_VERTICES).max(1)
    };
    if !deep {
        let mut vorder: Vec<usize> = (0..nv).collect();
        vorder.sort_by(|&a, &b| {
            point3_cmp(verts[a], verts[b]).then_with(|| normal_cmp(&vnorm[a], &vnorm[b]))
        });
        sampled.fill(false);
        for i in (0..nv).step_by(stride) {
            sampled[vorder[i]] = true;
        }
    }
    out.sample_stride = stride;

    // One sorted edge walk: lengths, valence, boundary/crease flags and the
    // neighbour list of every evaluated vertex (deep) or sampled vertex (fast).
    let mut nbr: Vec<Vec<usize>> = vec![Vec::new(); nv];
    let mut degree = vec![0usize; nv];
    let mut boundary = vec![false; nv];
    let mut sharp = vec![false; nv];
    let mut keys: Vec<(usize, usize)> = edges.keys().copied().collect();
    keys.sort_unstable();
    let mut edge_lengths: Vec<f64> = Vec::with_capacity(keys.len());
    for key in keys {
        let uses = &edges[&key];
        let (a, b) = key;
        degree[a] += 1;
        degree[b] += 1;
        edge_lengths.push(dist(verts[a], verts[b]));
        if uses.len() != 2 {
            boundary[a] = true;
            boundary[b] = true;
            continue;
        }
        let d = normals[uses[0].0][0] * normals[uses[1].0][0]
            + normals[uses[0].0][1] * normals[uses[1].0][1]
            + normals[uses[0].0][2] * normals[uses[1].0][2];
        if d < SHARP_COS {
            sharp[a] = true;
            sharp[b] = true;
        }
        if sampled[a] {
            nbr[a].push(b);
        }
        if sampled[b] {
            nbr[b].push(a);
        }
    }

    let mut devs: Vec<(f64, usize)> = Vec::with_capacity(nv / stride + 1);
    for v in 0..nv {
        if !sampled[v] || !used[v] || boundary[v] || sharp[v] || nbr[v].is_empty() {
            continue;
        }
        let mut ring: Vec<[f64; 3]> = nbr[v].iter().map(|&u| vnorm[u]).collect();
        ring.sort_by(normal_cmp);
        let mut acc = [0.0f64; 3];
        for n in &ring {
            acc[0] += n[0];
            acc[1] += n[1];
            acc[2] += n[2];
        }
        let len = (acc[0] * acc[0] + acc[1] * acc[1] + acc[2] * acc[2]).sqrt();
        if len <= 0.0 {
            continue;
        }
        let ring_n = [acc[0] / len, acc[1] / len, acc[2] / len];
        let dev = angle(vnorm[v], ring_n);
        if dev.is_finite() {
            devs.push((dev, v));
        }
    }
    devs.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let dev_values: Vec<f64> = devs.iter().map(|x| x.0).collect();

    let flagged: Vec<usize> = devs
        .iter()
        .filter(|(d, _)| *d > DEV_THRESHOLD)
        .map(|(_, v)| *v)
        .collect();
    out.eligible_vertices = devs.len();
    out.flagged_vertices = flagged.len();
    out.dev_p50 = percentile(&dev_values, 0.50);
    out.dev_p95 = percentile(&dev_values, 0.95);
    out.noise_score = (out.dev_p95 / (std::f64::consts::PI * 0.5)).clamp(0.0, 1.0);
    out.noise_ratio = if devs.is_empty() {
        0.0
    } else {
        flagged.len() as f64 / devs.len() as f64
    };

    // Valence distribution (over vertices that carry faces) and edge CV.
    let mut valence: Vec<f64> = (0..nv)
        .filter(|&v| used[v])
        .map(|v| degree[v] as f64)
        .collect();
    valence.sort_by(|a, b| a.total_cmp(b));
    let (vm, vs) = mean_std(&valence);
    out.valence_mean = vm;
    out.valence_std = vs;
    edge_lengths.sort_by(|a, b| a.total_cmp(b));
    let (em, es) = mean_std(&edge_lengths);
    out.edge_cv = if em > 0.0 { es / em } else { 0.0 };

    for &v in &flagged {
        if v < vertex_mask.len() {
            vertex_mask[v] |= VERTEX_MASK_BIT;
        }
    }
    let flagged_set: HashSet<usize> = flagged.iter().copied().collect();
    let mut face_list: Vec<usize> = Vec::new();
    for (f, &t) in tris.iter().enumerate() {
        if t.iter().any(|v| flagged_set.contains(v)) {
            face_list.push(f);
        }
    }
    for &f in &face_list {
        face_mask[f] |= FACE_MASK_BIT;
    }

    // Severity per region = the largest deviation among the region's vertices.
    let dev_by_vertex: HashMap<usize, f64> = devs.iter().map(|&(d, v)| (v, d)).collect();
    let mut scored: Vec<(f64, [f64; 3], Vec<usize>)> = group_faces(&face_list, edges)
        .into_iter()
        .map(|g| {
            let mut sev = 0.0f64;
            for &f in &g {
                for &v in &tris[f] {
                    if let Some(&d) = dev_by_vertex.get(&v) {
                        sev = sev.max(d);
                    }
                }
            }
            let key = g
                .iter()
                .map(|&f| tri_centroid(verts, tris[f]))
                .min_by(|a, b| point3_cmp(*a, *b))
                .unwrap_or([0.0; 3]);
            (sev, key, g)
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| a.1[0].total_cmp(&b.1[0]))
            .then_with(|| a.1[1].total_cmp(&b.1[1]))
            .then_with(|| a.1[2].total_cmp(&b.1[2]))
    });
    if scored.len() > MAX_NOISE_REGIONS {
        scored.truncate(MAX_NOISE_REGIONS);
    }
    for (sev, _, group) in scored {
        regions.push(region_from_faces(
            "noise",
            "noise_patch",
            &group,
            tris,
            verts,
            sev / (std::f64::consts::PI * 0.5),
            CONFIDENCE,
            &[],
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnosis::build_edges_for_tests;

    fn hash01(i: usize, j: usize) -> f64 {
        let mut x = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (j as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        x ^= x >> 30;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 27;
        x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^= x >> 31;
        (x >> 11) as f64 / (1u64 << 53) as f64
    }

    fn grid(n: usize, jitter: f64) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let mut v = Vec::with_capacity((n + 1) * (n + 1));
        for j in 0..=n {
            for i in 0..=n {
                let z = if jitter == 0.0 {
                    0.0
                } else {
                    (hash01(i, j) - 0.5) * 2.0 * jitter
                };
                v.push([i as f64, j as f64, z]);
            }
        }
        let mut f = Vec::new();
        let w = n + 1;
        for j in 0..n {
            for i in 0..n {
                let a = j * w + i;
                let b = a + 1;
                let c = a + w;
                let d = c + 1;
                f.push([a, b, d]);
                f.push([a, d, c]);
            }
        }
        (v, f)
    }

    fn run(v: &[[f64; 3]], f: &[[usize; 3]], detail: &str) -> Noise {
        let edges = build_edges_for_tests(f);
        let mut fm = vec![0u8; f.len()];
        let mut vm = vec![0u8; v.len()];
        let mut regions = Vec::new();
        noise_report(v, f, &edges, detail, &mut fm, &mut vm, &mut regions)
    }

    #[test]
    fn jittered_grid_scores_higher_than_smooth() {
        let (vs, fs) = grid(16, 0.0);
        let (vj, fj) = grid(16, 0.05);
        let smooth = run(&vs, &fs, "deep");
        let noisy = run(&vj, &fj, "deep");
        assert!(smooth.available);
        assert_eq!(smooth.noise_score, 0.0);
        assert!(noisy.noise_score > smooth.noise_score, "{noisy:?}");
        assert!(noisy.edge_cv > 0.0);
        let fast = run(&vj, &fj, "fast");
        assert!(fast.method.contains("stride"));
        assert!(fast.estimated);
    }

    #[test]
    fn cube_vertices_are_sharp_and_excluded() {
        let v = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
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
        let r = run(&v, &f, "deep");
        assert_eq!(r.eligible_vertices, 0);
        assert_eq!(r.flagged_vertices, 0);
        assert_eq!(r.noise_score, 0.0);
    }
}
