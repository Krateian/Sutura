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

/// Main's raw fast cap, and the threshold at or below which fast mode takes
/// main's exact path unchanged.  A mesh whose broad phase is above it was
/// already `estimated` in main, so only such a mesh is eligible for the
/// coplanar-aware prefilter.
pub const FAST_MAX_RAW_PAIRS: usize = 1_000_000;

/// Deterministic ceiling for the **prefiltered** pair list in `detail='fast'`,
/// applied only when the raw broad phase exceeded [`FAST_MAX_RAW_PAIRS`] (i.e.
/// the pre-prefilter fast mode was already `estimated`).  This can never turn
/// a formerly exact result into `None`.
pub const FAST_MAX_FILTERED_PAIRS: usize = 100_000;

/// Plane normals whose cross product norm is below this are treated as
/// parallel by the diagnosis wrapper: the pair is kept (coplanar overlaps and
/// touches must reach the exact classifier).  Same threshold as the repair
/// filter's internal parallel branch.
pub const PARALLEL_PLANE_EPS_SQ: f64 = 1e-22;

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
    /// Pairs left after the fast coplanar-aware prefilter; `None` unless the
    /// fast path prefilters (raw broad phase above [`FAST_MAX_RAW_PAIRS`]).
    pub filtered_pairs: Option<usize>,
    /// `Some("coplanar_aware_plane")` only when the fast path prefilters.
    pub prefilter: Option<&'static str>,
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

#[inline]
fn tri_plane(verts: &[[f64; 3]], t: [usize; 3]) -> [f64; 4] {
    let a = verts[t[0]];
    let b = verts[t[1]];
    let c = verts[t[2]];
    let e0 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let e1 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        e0[1] * e1[2] - e0[2] * e1[1],
        e0[2] * e1[0] - e0[0] * e1[2],
        e0[0] * e1[1] - e0[1] * e1[0],
    ];
    [n[0], n[1], n[2], -(n[0] * a[0] + n[1] * a[1] + n[2] * a[2])]
}

#[inline]
fn moller_interval(v: &[[f64; 3]; 3], d: [f64; 3], dir: [f64; 3]) -> (f64, f64) {
    let p = [
        v[0][0] * dir[0] + v[0][1] * dir[1] + v[0][2] * dir[2],
        v[1][0] * dir[0] + v[1][1] * dir[1] + v[1][2] * dir[2],
        v[2][0] * dir[0] + v[2][1] * dir[1] + v[2][2] * dir[2],
    ];
    let s0 = d[0] > 0.0;
    let s1 = d[1] > 0.0;
    let s2 = d[2] > 0.0;
    let (t0, t1) = if s0 == s1 {
        let denom0 = d[0] - d[2];
        let denom1 = d[1] - d[2];
        let frac0 = if denom0.abs() > 1e-15 {
            d[0] / denom0
        } else {
            0.5
        };
        let frac1 = if denom1.abs() > 1e-15 {
            d[1] / denom1
        } else {
            0.5
        };
        (p[0] + (p[2] - p[0]) * frac0, p[1] + (p[2] - p[1]) * frac1)
    } else if s0 == s2 {
        let denom0 = d[0] - d[1];
        let denom2 = d[2] - d[1];
        let frac0 = if denom0.abs() > 1e-15 {
            d[0] / denom0
        } else {
            0.5
        };
        let frac2 = if denom2.abs() > 1e-15 {
            d[2] / denom2
        } else {
            0.5
        };
        (p[0] + (p[1] - p[0]) * frac0, p[2] + (p[1] - p[2]) * frac2)
    } else {
        let denom1 = d[1] - d[0];
        let denom2 = d[2] - d[0];
        let frac1 = if denom1.abs() > 1e-15 {
            d[1] / denom1
        } else {
            0.5
        };
        let frac2 = if denom2.abs() > 1e-15 {
            d[2] / denom2
        } else {
            0.5
        };
        (p[1] + (p[0] - p[1]) * frac1, p[2] + (p[0] - p[2]) * frac2)
    };
    if t0 < t1 {
        (t0, t1)
    } else {
        (t1, t0)
    }
}

/// Diagnosis fast prefilter: `true` only when the pair is provably disjoint.
///
/// Mirrors the repair float filter with two diagnosis-only conservativeness
/// guards: parallel planes and vertices on the other plane are kept (the exact
/// classifier must still report coplanar overlaps and touches), and the
/// projected-interval gap uses a relative tolerance so a float-noise
/// near-touch is not rejected.  The repair path's filter is not modified.
#[inline]
fn prefilter_disjoint(pa: [f64; 4], pb: [f64; 4], va: [[f64; 3]; 3], vb: [[f64; 3]; 3]) -> bool {
    let d_dir = [
        pa[1] * pb[2] - pa[2] * pb[1],
        pa[2] * pb[0] - pa[0] * pb[2],
        pa[0] * pb[1] - pa[1] * pb[0],
    ];
    let len_sq = d_dir[0] * d_dir[0] + d_dir[1] * d_dir[1] + d_dir[2] * d_dir[2];
    if len_sq < PARALLEL_PLANE_EPS_SQ {
        return false;
    }
    const PLANE_EPS: f64 = 1e-11;
    let side = |p: [f64; 3], pl: [f64; 4]| pl[0] * p[0] + pl[1] * p[1] + pl[2] * p[2] + pl[3];
    let d2 = [side(vb[0], pa), side(vb[1], pa), side(vb[2], pa)];
    let d1 = [side(va[0], pb), side(va[1], pb), side(va[2], pb)];
    // A vertex on the other plane makes the interval formula degenerate (two
    // zero distances) and means a possible touch/coplanar edge: keep it.
    if d1.iter().any(|d| d.abs() <= PLANE_EPS) || d2.iter().any(|d| d.abs() <= PLANE_EPS) {
        return false;
    }
    if (d2[0] > PLANE_EPS && d2[1] > PLANE_EPS && d2[2] > PLANE_EPS)
        || (d2[0] < -PLANE_EPS && d2[1] < -PLANE_EPS && d2[2] < -PLANE_EPS)
    {
        return true;
    }
    if (d1[0] > PLANE_EPS && d1[1] > PLANE_EPS && d1[2] > PLANE_EPS)
        || (d1[0] < -PLANE_EPS && d1[1] < -PLANE_EPS && d1[2] < -PLANE_EPS)
    {
        return true;
    }
    let i1 = moller_interval(&va, d1, d_dir);
    let i2 = moller_interval(&vb, d2, d_dir);
    let magnitude =
        i1.0.abs()
            .max(i1.1.abs())
            .max(i2.0.abs())
            .max(i2.1.abs())
            .max(1.0);
    let gap_eps = 1e-9 * magnitude;
    i1.1 < i2.0 - gap_eps || i2.1 < i1.0 - gap_eps
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
    let use_exact = detail == "deep";

    // Fast mode drops pairs the conservative plane filter proves disjoint only
    // once the raw broad phase exceeds main's fast cap, where it measurably
    // pays off.  At or below the cap the prefilter costs more than it saves, so
    // take exactly main's path (no filter, no extra allocations) and match its
    // work and output.  The survivor cap below applies only to the >1M branch.
    let prefilter_fast = !use_exact && candidates.len() > FAST_MAX_RAW_PAIRS;

    let mut filtered: Vec<(usize, usize)> = Vec::new();
    let pairs_all: &[(usize, usize)] = if prefilter_fast {
        out.prefilter = Some("coplanar_aware_plane");
        let planes: Vec<[f64; 4]> = tris.iter().map(|&t| tri_plane(verts, t)).collect();
        let points: Vec<[[f64; 3]; 3]> = tris
            .iter()
            .map(|t| [verts[t[0]], verts[t[1]], verts[t[2]]])
            .collect();
        filtered = candidates
            .iter()
            .copied()
            .filter(|&(i, j)| {
                let (a, b) = (tris[i], tris[j]);
                if shared_vertices(a, b) >= 1 {
                    return false;
                }
                !prefilter_disjoint(planes[i], planes[j], points[i], points[j])
            })
            .collect();
        out.filtered_pairs = Some(filtered.len());
        &filtered
    } else {
        &candidates
    };
    // Survivor scan guard: only a mesh whose raw broad phase already exceeded
    // FAST_MAX_RAW_PAIRS (i.e. was `estimated` in main) may have its filtered
    // survivors capped.  A mesh at or below the cap never prefilters, so its
    // counts stay exact exactly as in main.
    let truncated = prefilter_fast && pairs_all.len() > FAST_MAX_FILTERED_PAIRS;
    if truncated {
        out.estimated = true;
        out.reason = Some("pair_cap");
    }
    let limit = if truncated {
        FAST_MAX_FILTERED_PAIRS
    } else {
        pairs_all.len()
    };
    let pairs = &pairs_all[..limit];
    out.scanned_pairs = pairs.len();
    if use_exact {
        out.method = "broad_phase+exact+shared";
    } else if prefilter_fast {
        out.method = "broad_phase+prefilter+exact";
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triangle_intersection::{intersect_triangles, Triangle, TriangleIntersection};

    fn plane_of(v: [[f64; 3]; 3]) -> [f64; 4] {
        tri_plane(&v, [0, 1, 2])
    }

    fn flags(va: [[f64; 3]; 3], vb: [[f64; 3]; 3]) -> (bool, bool) {
        let shared = va.iter().filter(|p| vb.iter().any(|q| p == &q)).count();
        let pa = plane_of(va);
        let pb = plane_of(vb);
        // The fast scope skips adjacent pairs before the plane filter (same as
        // the repair path); the filter's domain is non-adjacent pairs.
        let kept = shared > 0 || !prefilter_disjoint(pa, pb, va, vb);
        let ta = Triangle::explicit(va[0], va[1], va[2]);
        let tb = Triangle::explicit(vb[0], vb[1], vb[2]);
        let exact_disjoint = matches!(
            intersect_triangles(&ta, &tb),
            TriangleIntersection::Disjoint
        );
        (kept, exact_disjoint)
    }

    #[test]
    fn prefilter_cases() {
        let a = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        // Clearly disjoint and non-parallel: rejected, exact Disjoint.
        let far = [[10.0, 10.0, 10.0], [11.0, 10.0, 10.0], [10.0, 11.0, 12.0]];
        let (kept, disp) = flags(a, far);
        assert!(!kept && disp);
        // Proper crossing: kept, exact not Disjoint.
        let cross = [[0.25, 0.25, -1.0], [0.25, 0.25, 1.0], [0.75, 0.30, 0.1]];
        let (kept, disp) = flags(a, cross);
        assert!(kept && !disp);
        // Coplanar overlap: parallel planes are kept for the exact classifier.
        let coplanar = [[0.2, 0.2, 0.0], [1.2, 0.2, 0.0], [0.2, 1.2, 0.0]];
        let (kept, disp) = flags(a, coplanar);
        assert!(kept && !disp);
        // Coplanar but disjoint: conservatively kept (false positive is safe).
        let coplanar_far = [[5.0, 5.0, 0.0], [6.0, 5.0, 0.0], [5.0, 6.0, 0.0]];
        let (kept, disp) = flags(a, coplanar_far);
        assert!(kept && disp);
        // Touch at a shared vertex.
        let touch = [[0.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 1.0, 1.0]];
        let (kept, disp) = flags(a, touch);
        assert!(kept && !disp);
        // Shared edge.
        let shared_edge = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 1.0]];
        let (kept, _disp) = flags(a, shared_edge);
        assert!(kept);
        // Near-touching (offset 1e-12): kept.
        let near = [[0.2, 0.2, 1e-12], [0.8, 0.2, 1e-12], [0.5, 0.8, 1e-12]];
        let (kept, _disp) = flags(a, near);
        assert!(kept);
        // Sliver almost in the plane: kept.
        let sliver = [[0.2, 0.2, 0.0], [0.8, 0.2, 0.0], [0.5, 0.5, 1e-9]];
        let (kept, _disp) = flags(a, sliver);
        assert!(kept);
    }

    fn read_binary_stl(path: &std::path::Path) -> Option<(Vec<[f64; 3]>, Vec<[usize; 3]>)> {
        use std::collections::HashMap;
        let bytes = std::fs::read(path).ok()?;
        if bytes.len() < 84 {
            return read_ascii_stl(&bytes);
        }
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        if bytes.len() != 84 + n * 50 {
            return read_ascii_stl(&bytes);
        }
        let mut verts: Vec<[f64; 3]> = Vec::with_capacity(n * 3);
        let mut tris: Vec<[usize; 3]> = Vec::with_capacity(n);
        let mut weld: HashMap<(u32, u32, u32), usize> = HashMap::with_capacity(n * 3);
        for k in 0..n {
            let base = 84 + k * 50 + 12;
            let mut idx = [0usize; 3];
            for v in 0..3 {
                let p = base + v * 12;
                let x = f32::from_le_bytes([bytes[p], bytes[p + 1], bytes[p + 2], bytes[p + 3]]);
                let y =
                    f32::from_le_bytes([bytes[p + 4], bytes[p + 5], bytes[p + 6], bytes[p + 7]]);
                let z =
                    f32::from_le_bytes([bytes[p + 8], bytes[p + 9], bytes[p + 10], bytes[p + 11]]);
                let key = (x.to_bits(), y.to_bits(), z.to_bits());
                idx[v] = *weld.entry(key).or_insert_with(|| {
                    let i = verts.len();
                    verts.push([x as f64, y as f64, z as f64]);
                    i
                });
            }
            tris.push(idx);
        }
        Some((verts, tris))
    }

    fn read_ascii_stl(bytes: &[u8]) -> Option<(Vec<[f64; 3]>, Vec<[usize; 3]>)> {
        use std::collections::HashMap;
        let text = std::str::from_utf8(bytes).ok()?;
        let mut verts: Vec<[f64; 3]> = Vec::new();
        let mut tris: Vec<[usize; 3]> = Vec::new();
        let mut weld: HashMap<(u32, u32, u32), usize> = HashMap::new();
        let mut cur: Vec<usize> = Vec::with_capacity(3);
        for line in text.lines() {
            let p: Vec<&str> = line.split_whitespace().collect();
            if p.first() == Some(&"vertex") && p.len() >= 4 {
                let x = p[1].parse::<f32>().ok()?;
                let y = p[2].parse::<f32>().ok()?;
                let z = p[3].parse::<f32>().ok()?;
                let key = (x.to_bits(), y.to_bits(), z.to_bits());
                let idx = *weld.entry(key).or_insert_with(|| {
                    let i = verts.len();
                    verts.push([x as f64, y as f64, z as f64]);
                    i
                });
                cur.push(idx);
                if cur.len() == 3 {
                    tris.push([cur[0], cur[1], cur[2]]);
                    cur.clear();
                }
            }
        }
        if cur.is_empty() && !tris.is_empty() {
            Some((verts, tris))
        } else {
            None
        }
    }

    fn kind_index(k: PairKind) -> usize {
        match k {
            PairKind::Intra => 0,
            PairKind::Inter => 1,
            PairKind::Coplanar => 2,
            PairKind::Touch => 3,
        }
    }

    /// Conservativeness gate: every pair the prefilter rejects must be exactly
    /// Disjoint, and the class counts over the filtered set must equal the
    /// unfiltered exact counts.  Runs the tracked real-world sample set (plus
    /// the synthetic cases above); ignored by default because it classifies
    /// every broad-phase pair of 40 meshes.
    #[test]
    #[ignore = "real-world prefilter gate; run with --release --ignored"]
    fn prefilter_matches_unfiltered_on_corpus() {
        use rayon::prelude::*;
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/real-world-samples");
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .expect("real-world-samples directory")
            .filter_map(|e| {
                let p = e.ok()?.path();
                if p.extension().and_then(|s| s.to_str()) == Some("stl") {
                    Some(p)
                } else {
                    None
                }
            })
            .collect();
        files.sort();
        assert!(
            files.len() >= 40,
            "expected the real-world sample set, found {}",
            files.len()
        );
        let mut grand_pairs = 0usize;
        let mut grand_rejected = 0usize;
        for path in files {
            let (verts, tris) =
                read_binary_stl(&path).unwrap_or_else(|| panic!("not a binary STL: {path:?}"));
            let edges = super::super::build_edges_for_tests(&tris);
            let (comp, _n) = super::super::components_for_tests(tris.len(), &edges);
            let bvh = MeshBvh::from_arrays(&verts, &tris);
            let candidates = bvh.candidate_pairs(&verts, &tris);
            let planes: Vec<[f64; 4]> = tris.iter().map(|&t| tri_plane(&verts, t)).collect();
            let points: Vec<[[f64; 3]; 3]> = tris
                .iter()
                .map(|t| [verts[t[0]], verts[t[1]], verts[t[2]]])
                .collect();
            let rows: Vec<(usize, usize, bool, Option<PairKind>, bool)> = candidates
                .par_iter()
                .map(|&(i, j)| {
                    let shared = shared_vertices(tris[i], tris[j]);
                    // Adjacent pairs are outside the plane filter's domain: the
                    // fast scope skips them before the filter (and did so before
                    // this change), so they never contribute a class.
                    let rejected = shared == 0
                        && prefilter_disjoint(planes[i], planes[j], points[i], points[j]);
                    let kind = if shared == 0 {
                        classify_pair(&verts, &tris, &comp, i, j)
                    } else {
                        None
                    };
                    let exact_disjoint = if rejected {
                        let (a, b) = (tris[i], tris[j]);
                        let ta = Triangle::explicit(verts[a[0]], verts[a[1]], verts[a[2]]);
                        let tb = Triangle::explicit(verts[b[0]], verts[b[1]], verts[b[2]]);
                        matches!(
                            intersect_triangles(&ta, &tb),
                            TriangleIntersection::Disjoint
                        )
                    } else {
                        true
                    };
                    (i, j, rejected, kind, exact_disjoint)
                })
                .collect();
            let mut all = [0usize; 4];
            let mut filtered = [0usize; 4];
            let mut rejected = 0usize;
            let mut shared_pairs = 0usize;
            for (i, j, is_rej, kind, exact_disjoint) in rows {
                if shared_vertices(tris[i], tris[j]) > 0 {
                    shared_pairs += 1;
                }
                if is_rej {
                    rejected += 1;
                    if !exact_disjoint {
                        let a = tris[i];
                        let b = tris[j];
                        let shared = shared_vertices(a, b);
                        let pa = tri_plane(&verts, a);
                        let pb = tri_plane(&verts, b);
                        let cross = [
                            pa[1] * pb[2] - pa[2] * pb[1],
                            pa[2] * pb[0] - pa[0] * pb[2],
                            pa[0] * pb[1] - pa[1] * pb[0],
                        ];
                        let len_sq =
                            cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2];
                        let side = |p: [f64; 3], pl: [f64; 4]| {
                            pl[0] * p[0] + pl[1] * p[1] + pl[2] * p[2] + pl[3]
                        };
                        let d2 = [
                            side(verts[b[0]], pa),
                            side(verts[b[1]], pa),
                            side(verts[b[2]], pa),
                        ];
                        let d1 = [
                            side(verts[a[0]], pb),
                            side(verts[a[1]], pb),
                            side(verts[a[2]], pb),
                        ];
                        panic!(
                            "prefilter rejected a non-disjoint pair in {path:?}: i={i} j={j} shared={shared} kind={kind:?} len_sq={len_sq:e} d2={d2:?} d1={d1:?} va={:?} vb={:?}",
                            [verts[a[0]], verts[a[1]], verts[a[2]]],
                            [verts[b[0]], verts[b[1]], verts[b[2]]]
                        );
                    }
                }
                if let Some(k) = kind {
                    all[kind_index(k)] += 1;
                    if !is_rej {
                        filtered[kind_index(k)] += 1;
                    }
                }
            }
            assert_eq!(all, filtered, "class counts differ for {path:?}");
            grand_pairs += candidates.len();
            grand_rejected += rejected;
            let non_shared = candidates.len() - shared_pairs;
            println!(
                "{:>28}: candidates={} shared={} non_shared={} rejected={} kept={} classes={:?}",
                path.file_name().unwrap().to_string_lossy(),
                candidates.len(),
                shared_pairs,
                non_shared,
                rejected,
                non_shared - rejected,
                all
            );
        }
        println!(
            "TOTAL: pairs={grand_pairs} rejected={grand_rejected} kept={}",
            grand_pairs - grand_rejected
        );
    }
}
