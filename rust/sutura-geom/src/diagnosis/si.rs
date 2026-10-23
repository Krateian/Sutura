//! Diagnosis v2 phase 2: exact self-intersection classification.
//!
//! The broad phase is `arrangement::broad_phase_candidates`; each candidate is
//! classified by the exact triangle–triangle predicate into a proper crossing
//! (intra- or inter-component), a coplanar overlap, or a mere touch.  Counts
//! are never reported as zero when the mesh is above the classifier cap: the
//! channel is marked `estimated` and the count is `None`.

use std::collections::HashMap;

use rayon::prelude::*;

use crate::topology::DisjointSet;
use crate::triangle_intersection::{intersect_triangles, Triangle, TriangleIntersection};
use crate::winding::MeshBvh;

use super::{region_from_faces, Region};

/// Deterministic candidate-pair ceiling for `detail='fast'`; `deep` only uses
/// the crate-wide `SI_MAX_FACES` face cap.  Above this the scanned prefix is
/// still reported as a diagnostic hint but the public counts stay `None`.
pub const FAST_MAX_PAIRS: usize = 1_000_000;

/// Face mask bit used by this channel (phase 1 uses bits 1, 2, 4, 8).
pub const FACE_MASK_BIT: u8 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PairKind {
    Intra,
    Inter,
    Coplanar,
    Touch,
}

#[derive(Clone, Debug, Default)]
pub struct SelfIntersections {
    pub available: bool,
    pub estimated: bool,
    /// `None` means "not measured" (face or pair cap), never zero.
    pub total: Option<usize>,
    pub intra_body: Option<usize>,
    pub inter_body: Option<usize>,
    pub coplanar: Option<usize>,
    pub touch: Option<usize>,
    pub clusters: Option<usize>,
    pub candidate_pairs: usize,
    pub scanned_pairs: usize,
    /// Defect pairs (proper + coplanar) found in the scanned prefix.
    pub scanned_total: usize,
    pub reason: Option<&'static str>,
    pub method: &'static str,
}

#[inline]
fn pa_min(verts: &[[f64; 3]], t: [usize; 3], k: usize) -> f64 {
    verts[t[0]][k].min(verts[t[1]][k]).min(verts[t[2]][k])
}

#[inline]
fn pa_max(verts: &[[f64; 3]], t: [usize; 3], k: usize) -> f64 {
    verts[t[0]][k].max(verts[t[1]][k]).max(verts[t[2]][k])
}

fn shared_vertices(a: [usize; 3], b: [usize; 3]) -> usize {
    a.iter().filter(|v| b.contains(v)).count()
}

/// Signed area of a 2D polygon (CCW positive).
fn poly_area(poly: &[[f64; 2]]) -> f64 {
    let mut a = 0.0;
    for i in 0..poly.len() {
        let p = poly[i];
        let q = poly[(i + 1) % poly.len()];
        a += p[0] * q[1] - q[0] * p[1];
    }
    0.5 * a
}

/// Clip `poly` (CCW) by the half-plane left of the directed line a->b.
fn clip_half_plane(poly: &[[f64; 2]], a: [f64; 2], b: [f64; 2]) -> Vec<[f64; 2]> {
    let mut out = Vec::with_capacity(poly.len() + 2);
    if poly.is_empty() {
        return out;
    }
    let side = |p: [f64; 2]| (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
    for i in 0..poly.len() {
        let p = poly[i];
        let q = poly[(i + 1) % poly.len()];
        let sp = side(p);
        let sq = side(q);
        if sp >= 0.0 {
            out.push(p);
        }
        if (sp > 0.0 && sq < 0.0) || (sp < 0.0 && sq > 0.0) {
            let t = sp / (sp - sq);
            out.push([p[0] + t * (q[0] - p[0]), p[1] + t * (q[1] - p[1])]);
        }
    }
    out
}

/// True when two coplanar triangles overlap with positive area.  The exact
/// classifier's `CoplanarOverlap` also fires for a shared point/edge, which
/// must not be reported as an area overlap.
fn coplanar_area_overlap(pa: [[f64; 3]; 3], pb: [[f64; 3]; 3], eps: f64) -> bool {
    let u = [
        pb[0][0] - pa[0][0],
        pb[0][1] - pa[0][1],
        pb[0][2] - pa[0][2],
    ];
    let v = [
        pb[1][0] - pa[0][0],
        pb[1][1] - pa[0][1],
        pb[1][2] - pa[0][2],
    ];
    let n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let mut drop = 0usize;
    for k in 1..3 {
        if n[k].abs() > n[drop].abs() {
            drop = k;
        }
    }
    let proj = |p: [f64; 3]| match drop {
        0 => [p[1], p[2]],
        1 => [p[0], p[2]],
        _ => [p[0], p[1]],
    };
    let mut poly: Vec<[f64; 2]> = pa.iter().map(|p| proj(*p)).collect();
    let mut clip: Vec<[f64; 2]> = pb.iter().map(|p| proj(*p)).collect();
    if poly_area(&poly) < 0.0 {
        poly.reverse();
    }
    if poly_area(&clip) < 0.0 {
        clip.reverse();
    }
    for i in 0..3 {
        let a = clip[i];
        let b = clip[(i + 1) % 3];
        poly = clip_half_plane(&poly, a, b);
        if poly.len() < 3 {
            return false;
        }
    }
    poly_area(&poly) > eps
}

fn classify_pair(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    comp: &[usize],
    i: usize,
    j: usize,
) -> Option<PairKind> {
    let shared = shared_vertices(tris[i], tris[j]);
    // Edge-sharing (or identical) faces overlap on their shared boundary;
    // their defects belong to the duplicate/orientation channels.
    if shared >= 2 {
        return None;
    }
    let a = Triangle::explicit(verts[tris[i][0]], verts[tris[i][1]], verts[tris[i][2]]);
    let b = Triangle::explicit(verts[tris[j][0]], verts[tris[j][1]], verts[tris[j][2]]);
    match intersect_triangles(&a, &b) {
        TriangleIntersection::ProperSegment { .. } => {
            if shared > 0 {
                None
            } else if comp[i] == comp[j] {
                Some(PairKind::Intra)
            } else {
                Some(PairKind::Inter)
            }
        }
        TriangleIntersection::CoplanarOverlap => {
            let scale = 1e-12
                * (0..3)
                    .map(|k| {
                        let lo = pa_min(verts, tris[i], k);
                        let hi = pa_max(verts, tris[i], k);
                        hi - lo
                    })
                    .fold(0.0, f64::max);
            if coplanar_area_overlap(
                [verts[tris[i][0]], verts[tris[i][1]], verts[tris[i][2]]],
                [verts[tris[j][0]], verts[tris[j][1]], verts[tris[j][2]]],
                scale.max(1e-30),
            ) {
                Some(PairKind::Coplanar)
            } else {
                None
            }
        }
        TriangleIntersection::TouchPoint(_) | TriangleIntersection::TouchSegment { .. } => {
            if shared > 0 {
                None
            } else {
                Some(PairKind::Touch)
            }
        }
        TriangleIntersection::Disjoint => None,
    }
}

/// Public entry: classify every broad-phase candidate and add the cluster
/// regions to `regions`.  Returns the channel summary and the component pairs
/// that actually intersect (used to refine the part overlap graph).
pub fn si_report(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    comp: &[usize],
    detail: &str,
    face_mask: &mut [u8],
    regions: &mut Vec<Region>,
    bvh: Option<&MeshBvh>,
) -> (SelfIntersections, Vec<(usize, usize)>) {
    let mut out = SelfIntersections {
        available: true,
        method: "broad_phase+exact",
        ..Default::default()
    };
    let nf = tris.len();
    if nf < 2 {
        out.total = Some(0);
        out.intra_body = Some(0);
        out.inter_body = Some(0);
        out.coplanar = Some(0);
        out.touch = Some(0);
        out.clusters = Some(0);
        return (out, Vec::new());
    }
    if nf > crate::SI_MAX_FACES {
        out.estimated = true;
        out.reason = Some("face_cap");
        return (out, Vec::new());
    }

    let owned;
    let bvh = match bvh {
        Some(b) => b,
        None => {
            owned = MeshBvh::from_arrays(verts, tris);
            &owned
        }
    };
    let candidates = bvh.candidate_pairs(verts, tris);
    out.candidate_pairs = candidates.len();
    let truncated = detail == "fast" && candidates.len() > FAST_MAX_PAIRS;
    if truncated {
        out.estimated = true;
        out.reason = Some("pair_cap");
    }
    let limit = if truncated {
        FAST_MAX_PAIRS
    } else {
        candidates.len()
    };
    let pairs = &candidates[..limit];
    out.scanned_pairs = pairs.len();
    let use_exact = detail == "deep";
    if use_exact {
        out.method = "broad_phase+exact+shared";
    } else {
        out.method = "broad_phase+exact";
    }

    // In fast mode ring neighbours (any shared vertex) are skipped: they
    // cannot produce a proper interior crossing, and their coplanar overlap is
    // the duplicate channel's job.  This is the same scope as the crate's
    // `self_intersecting_faces`; `deep` classifies every candidate (including
    // vertex-sharing pairs).
    let kinds: Vec<(usize, usize, PairKind)> = pairs
        .par_iter()
        .filter_map(|&(i, j)| {
            if !use_exact && shared_vertices(tris[i], tris[j]) >= 1 {
                return None;
            }
            classify_pair(verts, tris, comp, i, j).map(|k| (i, j, k))
        })
        .collect();

    let mut intra = 0usize;
    let mut inter = 0usize;
    let mut coplanar = 0usize;
    let mut touch = 0usize;
    let mut inter_pairs: Vec<(usize, usize)> = Vec::new();
    let mut ds = DisjointSet::new(nf);
    let mut has_inter = vec![false; nf];
    let mut has_coplanar = vec![false; nf];
    for &(i, j, kind) in &kinds {
        match kind {
            PairKind::Intra => intra += 1,
            PairKind::Inter => inter += 1,
            PairKind::Coplanar => coplanar += 1,
            PairKind::Touch => touch += 1,
        }
        if kind == PairKind::Touch {
            continue;
        }
        face_mask[i] |= FACE_MASK_BIT;
        face_mask[j] |= FACE_MASK_BIT;
        ds.union(i, j);
        if kind == PairKind::Inter {
            let (a, b) = (comp[i], comp[j]);
            inter_pairs.push(if a < b { (a, b) } else { (b, a) });
            has_inter[i] = true;
            has_inter[j] = true;
        }
        if kind == PairKind::Coplanar {
            has_coplanar[i] = true;
            has_coplanar[j] = true;
        }
    }
    out.scanned_total = intra + inter + coplanar;

    let mut pair_counts: HashMap<usize, usize> = HashMap::new();
    for &(i, _, kind) in &kinds {
        if kind == PairKind::Touch {
            continue;
        }
        let root = ds.find(i);
        *pair_counts.entry(root).or_insert(0) += 1;
    }

    if !truncated {
        out.total = Some(intra + inter + coplanar);
        out.intra_body = Some(intra);
        out.inter_body = Some(inter);
        out.coplanar = Some(coplanar);
        out.touch = Some(touch);
    }

    // Group the unioned faces into clusters, ordered by their smallest face id.
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for &(i, j, kind) in &kinds {
        if kind == PairKind::Touch {
            continue;
        }
        for f in [i, j] {
            let r = ds.find(f);
            groups.entry(r).or_default().push(f);
        }
    }
    let mut cluster_list: Vec<(usize, Vec<usize>)> = groups
        .into_iter()
        .map(|(r, mut faces)| {
            faces.sort_unstable();
            faces.dedup();
            (r, faces)
        })
        .collect();
    cluster_list.sort_by_key(|(_, faces)| faces.first().copied().unwrap_or(usize::MAX));

    for (root, faces) in &cluster_list {
        let kind = if faces.iter().any(|&f| has_inter[f]) {
            "inter_body_overlap"
        } else if faces.iter().any(|&f| has_coplanar[f]) {
            "coplanar_overlap"
        } else {
            "self_intersection"
        };
        let pairs = pair_counts.get(root).copied().unwrap_or(0).max(1);
        regions.push(region_from_faces(
            "self_intersection",
            kind,
            faces,
            tris,
            verts,
            pairs as f64,
            1.0,
            comp,
        ));
    }
    if !truncated {
        out.clusters = Some(cluster_list.len());
    }

    inter_pairs.sort_unstable();
    inter_pairs.dedup();
    (out, inter_pairs)
}
