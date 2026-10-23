//! Diagnosis v2 phase 3: thin-wall analysis.
//!
//! The surface is sampled with `TARGET_SAMPLES` deterministic, area-weighted
//! points; from each point an inward ray (the opposite direction is tried when
//! the inward one escapes) finds the first opposite surface and the hit
//! distance is the local wall thickness.  The report is a *score* (`min`, `p5`,
//! `median`, `thin_wall_ratio`) plus regions, never a hard defect (plan I6).
//!
//! Units are not guaranteed: the normalized values divide by the bounding-box
//! diagonal, and the raw values are labelled `units='mm_assumed'` together with
//! a `threshold_mm` reference.  FDM/SLA absolute thresholds are a user/profile
//! decision, so the threshold used for the ratio is a fixed fraction of the
//! diagonal and is reported alongside it.
//!
//! Sampling must survive a face permutation: faces are visited in geometric
//! order and each triangle is cyclically rotated to a canonical start vertex,
//! so the same physical sample points (and therefore the same aggregates and
//! mask) come out of a permuted input.

use std::collections::HashMap;

use crate::winding::MeshBvh;

use super::{
    canonical_tri, face_geo_cmp, group_faces, point3_cmp, region_from_faces, region_key,
    region_key_cmp, tri_centroid, unit_normal, Bbox, Region,
};

pub const FACE_MASK_BIT: u8 = 128;

/// Deterministic sample count for `deep` (a work cap: the aggregate is an
/// estimate).  `fast` uses a fixed stride over the same sample positions.
pub const TARGET_SAMPLES: usize = 4096;
pub const FAST_SAMPLES: usize = 512;
pub const FAST_STRIDE: usize = TARGET_SAMPLES / FAST_SAMPLES;

/// Default reporting threshold as a fraction of the bounding-box diagonal.
/// The plan deliberately keeps the FDM/SLA millimetre numbers as labels only.
pub const THRESHOLD_NORM: f64 = 0.005;

pub const MAX_THIN_REGIONS: usize = 256;

const CONFIDENCE: f64 = 0.7;

#[derive(Clone, Debug, Default)]
pub struct ThinWall {
    pub available: bool,
    pub samples: usize,
    pub samples_hit: usize,
    pub min: f64,
    pub p5: f64,
    pub median: f64,
    pub min_norm: f64,
    pub p5_norm: f64,
    pub median_norm: f64,
    pub thin_samples: usize,
    pub thin_faces: usize,
    pub thin_ratio: f64,
    pub threshold_norm: f64,
    pub threshold_mm: f64,
    pub units: &'static str,
    pub sample_stride: usize,
    pub method: &'static str,
    pub estimated: bool,
    pub confidence: f64,
}

#[inline]
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[inline]
fn splitmix_unit(state: &mut u64) -> f64 {
    (splitmix64(state) >> 11) as f64 / (1u64 << 53) as f64
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * q).floor() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

pub fn thin_report(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    edges: &HashMap<(usize, usize), Vec<(usize, bool)>>,
    bvh: Option<&MeshBvh>,
    detail: &str,
    face_mask: &mut [u8],
    regions: &mut Vec<Region>,
) -> ThinWall {
    let deep = detail == "deep";
    let stride = if deep { 1 } else { FAST_STRIDE };
    let mut out = ThinWall {
        threshold_norm: THRESHOLD_NORM,
        units: "mm_assumed",
        sample_stride: stride,
        method: if deep {
            "ray_thickness"
        } else {
            "ray_thickness+stride"
        },
        estimated: true,
        confidence: CONFIDENCE,
        ..Default::default()
    };
    let nf = tris.len();
    if nf == 0 || bvh.is_none() {
        return out;
    }
    let bvh = bvh.unwrap();
    let mut global = Bbox::empty();
    for p in verts {
        global.add(*p);
    }
    let diag = global.diag().max(1e-12);
    out.threshold_mm = THRESHOLD_NORM * diag;

    // Geometry in a permutation-independent order.  The precomputed centroid
    // is the primary key (cheap comparisons); the full geometric comparator
    // only runs on exact centroid ties.
    let centroids: Vec<[f64; 3]> = tris.iter().map(|&t| tri_centroid(verts, t)).collect();
    let mut order: Vec<usize> = (0..nf).collect();
    order.sort_by(|&a, &b| {
        point3_cmp(centroids[a], centroids[b]).then_with(|| face_geo_cmp(verts, tris[a], tris[b]))
    });
    let mut areas = vec![0.0f64; nf];
    let mut prefix = Vec::with_capacity(nf);
    let mut acc = 0.0f64;
    for &f in &order {
        let a = verts[tris[f][0]];
        let b = verts[tris[f][1]];
        let c = verts[tris[f][2]];
        let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let cr = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        let area = 0.5 * (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]).sqrt();
        areas[f] = area;
        acc += area;
        prefix.push(acc);
    }
    if !(acc > 0.0) {
        out.available = true;
        return out;
    }

    let inv_n = 1.0 / TARGET_SAMPLES as f64;
    let eps = (1e-5 * diag).max(1e-12);
    let merge_eps = 1e-6 * diag;
    let max_t = 2.0 * diag;
    let threshold_raw = THRESHOLD_NORM * diag;
    let mut thicknesses: Vec<f64> = Vec::with_capacity(TARGET_SAMPLES);
    let mut thin_faces: Vec<usize> = Vec::new();
    let mut face_thickness: HashMap<usize, f64> = HashMap::new();
    let mut thin_samples = 0usize;

    for k in (0..TARGET_SAMPLES).step_by(stride) {
        let target = (k as f64 + 0.5) * inv_n * acc;
        let pos = prefix.partition_point(|&x| x < target).min(nf - 1);
        let f = order[pos];
        let t = canonical_tri(verts, tris[f]);
        let a = verts[t[0]];
        let b = verts[t[1]];
        let c = verts[t[2]];
        let mut state = (k as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(1);
        let u = splitmix_unit(&mut state);
        let v = splitmix_unit(&mut state);
        let (u, v) = if u + v > 1.0 {
            (1.0 - u, 1.0 - v)
        } else {
            (u, v)
        };
        let p = [
            a[0] + u * (b[0] - a[0]) + v * (c[0] - a[0]),
            a[1] + u * (b[1] - a[1]) + v * (c[1] - a[1]),
            a[2] + u * (b[2] - a[2]) + v * (c[2] - a[2]),
        ];
        let n = unit_normal(verts, t);
        if n == [0.0, 0.0, 0.0] {
            continue;
        }
        let origin_in = [p[0] - n[0] * eps, p[1] - n[1] * eps, p[2] - n[2] * eps];
        let inward = [-n[0], -n[1], -n[2]];
        let mut query = bvh.intersect_ray(origin_in, inward, max_t, merge_eps);
        let mut t_hit = if query.crossings > 0 && query.nearest_t.is_finite() {
            Some(query.nearest_t + eps)
        } else {
            let origin_out = [p[0] + n[0] * eps, p[1] + n[1] * eps, p[2] + n[2] * eps];
            query = bvh.intersect_ray(origin_out, n, max_t, merge_eps);
            if query.crossings > 0 && query.nearest_t.is_finite() {
                Some(query.nearest_t + eps)
            } else {
                None
            }
        };
        if let Some(th) = t_hit {
            // A hit at (almost) zero is the sample's own face; ignore it.
            if th <= eps * 1.5 {
                t_hit = None;
            }
        }
        let Some(thickness) = t_hit else {
            continue;
        };
        thicknesses.push(thickness);
        let prev = face_thickness.get(&f).copied().unwrap_or(f64::INFINITY);
        if thickness < prev {
            face_thickness.insert(f, thickness);
        }
        if thickness < threshold_raw {
            thin_samples += 1;
            thin_faces.push(f);
        }
    }

    thicknesses.sort_by(|a, b| a.total_cmp(b));
    out.available = true;
    out.samples = TARGET_SAMPLES / stride;
    out.samples_hit = thicknesses.len();
    out.min = thicknesses.first().copied().unwrap_or(0.0);
    out.p5 = percentile(&thicknesses, 0.05);
    out.median = percentile(&thicknesses, 0.50);
    out.min_norm = out.min / diag;
    out.p5_norm = out.p5 / diag;
    out.median_norm = out.median / diag;
    out.thin_samples = thin_samples;
    out.thin_ratio = if thicknesses.is_empty() {
        0.0
    } else {
        thin_samples as f64 / thicknesses.len() as f64
    };

    thin_faces.sort_unstable();
    thin_faces.dedup();
    out.thin_faces = thin_faces.len();
    for &f in &thin_faces {
        face_mask[f] |= FACE_MASK_BIT;
    }

    // Deterministic region cap: smallest thickness first, then the canonical
    // geometric key and only then the raw face id.  A face-index tie-break
    // alone would let a permutation change which equal-thickness regions
    // survive the cap.
    let mut keyed: Vec<(f64, super::RegionKey, usize, Vec<usize>)> =
        group_faces(&thin_faces, edges)
            .into_iter()
            .map(|group| {
                let min_th = group
                    .iter()
                    .filter_map(|f| face_thickness.get(f))
                    .fold(f64::INFINITY, |x, &y| x.min(y));
                let key = region_key(verts, tris, &group);
                let min_face = group.iter().copied().min().unwrap_or(usize::MAX);
                (min_th, key, min_face, group)
            })
            .collect();
    keyed.sort_by(|a, b| {
        a.0.total_cmp(&b.0)
            .then_with(|| region_key_cmp(&a.1, &b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    if keyed.len() > MAX_THIN_REGIONS {
        keyed.truncate(MAX_THIN_REGIONS);
    }
    for (min_th, _, _, group) in keyed {
        let sev = if min_th.is_finite() {
            (1.0 - min_th / threshold_raw).clamp(0.0, 1.0)
        } else {
            0.0
        };
        regions.push(region_from_faces(
            "thin_wall",
            "thin_wall_patch",
            &group,
            tris,
            verts,
            sev,
            CONFIDENCE,
            // Component ids are not needed for a sampled region; an empty list
            // keeps the region channel-agnostic (and permutation-stable).
            &[],
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnosis::{build_edges_for_tests, components_for_tests};

    fn box_tris(nx: usize, ny: usize, nz: f64) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let _ = (nx, ny);
        let v = vec![
            [0.0, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            [10.0, 10.0, 0.0],
            [0.0, 10.0, 0.0],
            [0.0, 0.0, nz],
            [10.0, 0.0, nz],
            [10.0, 10.0, nz],
            [0.0, 10.0, nz],
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

    fn run(v: &[[f64; 3]], f: &[[usize; 3]], detail: &str) -> (ThinWall, Vec<u8>, Vec<Region>) {
        let edges = build_edges_for_tests(f);
        let (_comp, _n) = components_for_tests(f.len(), &edges);
        let bvh = MeshBvh::from_arrays(v, f);
        let mut mask = vec![0u8; f.len()];
        let mut regions = Vec::new();
        let r = thin_report(v, f, &edges, Some(&bvh), detail, &mut mask, &mut regions);
        (r, mask, regions)
    }

    #[test]
    fn thin_slab_is_detected() {
        let (v, f) = box_tris(1, 1, 0.05);
        let (r, mask, regions) = run(&v, &f, "deep");
        assert!(r.available);
        assert!((r.min - 0.05).abs() < 1e-6, "{r:?}");
        assert!(r.thin_ratio > 0.0, "{r:?}");
        assert!(r.thin_faces > 0);
        assert!(mask.iter().any(|&b| b & FACE_MASK_BIT != 0));
        assert!(regions.iter().any(|g| g.kind == "thin_wall_patch"));
        assert_eq!(r.units, "mm_assumed");
        assert!(r.min_norm > 0.0);
        let (fast, _m, _r) = run(&v, &f, "fast");
        assert_eq!(fast.samples, FAST_SAMPLES);
        assert_eq!(fast.sample_stride, FAST_STRIDE);
        assert!(fast.method.contains("stride"));
        assert!(fast.estimated);
    }

    #[test]
    fn solid_cube_has_no_thin_wall() {
        let (v, f) = box_tris(1, 1, 1.0);
        let (r, mask, _regions) = run(&v, &f, "deep");
        assert!(r.available);
        assert!((r.min - 1.0).abs() < 1e-6, "{r:?}");
        assert_eq!(r.thin_faces, 0);
        assert!(!mask.iter().any(|&b| b & FACE_MASK_BIT != 0));
    }
}
