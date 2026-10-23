//! Diagnosis v2 phase 3: interior/embedded faces and fold creases.
//!
//! An *interior* surface is a closed component that sits inside another closed
//! component with the same orientation (an embedded solid inclusion).  A
//! nested component with the opposite orientation is a cavity, not an interior
//! surface: one side is solid and the other is the cavity void.
//!
//! Budget discipline (plan I3/I7): the channel never queries the whole mesh per
//! face.  It first keeps only components whose AABB overlaps another
//! component's AABB, then decides the contained component from a handful of
//! sample points queried against the *enclosing component's* surface only.  The
//! number of pairs, sampled points and per-component BVH builds is capped; a
//! truncated run is reported as `estimated` with the sampled work.
//!
//! Open components have no well-defined inside (plan I7): the ray-stab parity
//! vote against the enclosing component is used instead and the result is
//! reported as *unconfirmed* (low confidence), never as a hard defect.
//!
//! A *fold* is a crease where the two incident faces have nearly opposite
//! normals (dihedral ~pi, a zero-thickness fold).  Faces touching a boundary
//! edge are excluded: a fold there belongs to the boundary/loop channel.
//!
//! Interior and fold faces share face-mask bit 64.

use std::collections::{HashMap, HashSet};

use crate::raycast::{self, Stabber};
use crate::topology::DisjointSet;
use crate::winding::MeshBvh;

use super::{
    face_geo_cmp, group_faces, region_from_faces, tri_centroid, unit_normal, Bbox, Region,
};

pub const FACE_MASK_BIT: u8 = 64;

/// Work caps.  A candidate pair costs a handful of winding/ray-stab queries
/// against the enclosing component, not the whole mesh; these caps keep a
/// pathological assembly from eating the fast budget.  Hitting one marks the
/// channel `estimated` and leaves the counts as a deterministic sample.
pub const MAX_INTERIOR_PAIR_TESTS: usize = 100_000;
pub const MAX_INTERIOR_PAIRS: usize = 4_096;
pub const MAX_INTERIOR_POINTS: usize = 4;
pub const MAX_INTERIOR_POINTS_TOTAL: usize = 1_024;
pub const MAX_INTERIOR_BVHS: usize = 64;

/// `dot(n1, n2) < -0.94` means the normals differ by more than ~160 degrees.
pub const FOLD_COS: f64 = -0.94;

const OPEN_CONFIDENCE: f64 = 0.4;
const CLOSED_CONFIDENCE: f64 = 0.95;

#[derive(Clone, Debug)]
pub struct Interior {
    pub available: bool,
    /// Sample points actually queried (not every face is tested).
    pub candidates: usize,
    /// Candidate components that were sampled.
    pub candidate_components: usize,
    /// Confirmed interior faces (all faces of an embedded component).
    pub faces: usize,
    /// Open-component faces flagged by the parity vote (not a hard defect).
    pub low_confidence: usize,
    pub fold_faces: usize,
    pub estimated: bool,
    pub method: &'static str,
    pub confidence: f64,
}

impl Default for Interior {
    fn default() -> Self {
        Self {
            available: false,
            candidates: 0,
            candidate_components: 0,
            faces: 0,
            low_confidence: 0,
            fold_faces: 0,
            estimated: false,
            method: "none",
            confidence: 0.0,
        }
    }
}

fn component_volume(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    comp: &[usize],
    ncomp: usize,
) -> Vec<f64> {
    let mut vols = vec![0.0f64; ncomp];
    for (f, &c) in comp.iter().enumerate() {
        let t = tris[f];
        let a = verts[t[0]];
        let b = verts[t[1]];
        let c3 = verts[t[2]];
        vols[c] += (a[0] * (b[1] * c3[2] - b[2] * c3[1])
            + a[1] * (b[2] * c3[0] - b[0] * c3[2])
            + a[2] * (b[0] * c3[1] - b[1] * c3[0]))
            / 6.0;
    }
    vols
}

/// Up to `max_points` deterministic sample faces of a component (geometric
/// order, evenly spaced).  Returns `(faces, sampled)`.
fn sample_faces(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    faces: &[usize],
    max_points: usize,
) -> (Vec<usize>, bool) {
    let mut order = faces.to_vec();
    order.sort_by(|&a, &b| face_geo_cmp(verts, tris[a], tris[b]));
    if order.len() <= max_points {
        return (order, false);
    }
    let n = order.len();
    let mut out = Vec::with_capacity(max_points);
    for k in 0..max_points {
        out.push(order[k * (n - 1) / (max_points - 1)]);
    }
    out.dedup();
    (out, true)
}

pub fn interior_report(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    edges: &HashMap<(usize, usize), Vec<(usize, bool)>>,
    comp: &[usize],
    ncomp: usize,
    face_mask: &mut [u8],
    regions: &mut Vec<Region>,
) -> Interior {
    let nf = tris.len();
    let mut out = Interior {
        available: nf > 0,
        ..Default::default()
    };
    if nf == 0 || ncomp == 0 {
        return out;
    }
    let normals: Vec<[f64; 3]> = tris.iter().map(|&t| unit_normal(verts, t)).collect();
    let mut global = Bbox::empty();
    for p in verts {
        global.add(*p);
    }
    let diag = global.diag().max(1e-12);
    let eps = (1e-4 * diag).max(1e-12);

    // Per-component openness, bbox and face list.
    let mut open = vec![false; ncomp];
    for uses in edges.values() {
        if uses.len() != 2 {
            for &(f, _) in uses {
                open[comp[f]] = true;
            }
        }
    }
    let mut bboxes = vec![Bbox::empty(); ncomp];
    let mut faces_by_comp: Vec<Vec<usize>> = vec![Vec::new(); ncomp];
    for (f, &c) in comp.iter().enumerate() {
        faces_by_comp[c].push(f);
        for &v in &tris[f] {
            bboxes[c].add(verts[v]);
        }
    }
    let vols = component_volume(verts, tris, comp, ncomp);

    // Fold creases.  Marking booleans and taking a max are commutative, so the
    // unsorted `edges` map is safe here; only the grouping needs a sorted walk.
    let mut boundary_face = vec![false; nf];
    for uses in edges.values() {
        if uses.len() != 2 {
            for &(f, _) in uses {
                boundary_face[f] = true;
            }
        }
    }
    let mut fold_face = vec![false; nf];
    let mut fold_sev = vec![0.0f64; nf];
    for uses in edges.values() {
        if uses.len() != 2 {
            continue;
        }
        let (a, b) = (uses[0].0, uses[1].0);
        if boundary_face[a] || boundary_face[b] {
            continue;
        }
        let d = normals[a][0] * normals[b][0]
            + normals[a][1] * normals[b][1]
            + normals[a][2] * normals[b][2];
        if d < FOLD_COS {
            let sev = ((-d - (-FOLD_COS)) / (1.0 + FOLD_COS)).clamp(0.0, 1.0);
            fold_face[a] = true;
            fold_face[b] = true;
            fold_sev[a] = fold_sev[a].max(sev);
            fold_sev[b] = fold_sev[b].max(sev);
        }
    }

    // Candidate pairs: AABB overlap with one bbox containing the other.  A
    // partially interpenetrating pair is the self-intersection channel's job;
    // here only a fully contained component can be an interior/cavity shell.
    let mut pairs: Vec<(usize, usize)> = Vec::new(); // (contained, enclosing)
    let mut tested = 0usize;
    'scan: for a in 0..ncomp {
        for b in (a + 1)..ncomp {
            if tested >= MAX_INTERIOR_PAIR_TESTS {
                out.estimated = true;
                break 'scan;
            }
            tested += 1;
            if bboxes[a].contains(&bboxes[b]) {
                pairs.push((b, a));
            } else if bboxes[b].contains(&bboxes[a]) {
                pairs.push((a, b));
            }
        }
    }
    if pairs.len() > MAX_INTERIOR_PAIRS {
        pairs.truncate(MAX_INTERIOR_PAIRS);
        out.estimated = true;
    }

    let mut bvh_cache: HashMap<usize, MeshBvh> = HashMap::new();
    let mut bvhs_built = 0usize;
    let mut points_used = 0usize;
    let mut decided = vec![false; ncomp];
    let mut confirmed: Vec<usize> = Vec::new();
    let mut unconfirmed: Vec<usize> = Vec::new();
    let mut did_gwn = false;
    let mut did_raystab = false;
    let stabber = Stabber::new(
        &global_aabb(verts),
        raycast::DEFAULT_DIRS,
        0,
        true,
        raycast::DEFAULT_ESCAPE_WEIGHT,
    );

    for &(a, b) in &pairs {
        if decided[a] {
            continue;
        }
        let (samples, sampled) = sample_faces(verts, tris, &faces_by_comp[a], MAX_INTERIOR_POINTS);
        if samples.is_empty() {
            decided[a] = true;
            continue;
        }
        out.estimated |= sampled;
        if points_used + samples.len() > MAX_INTERIOR_POINTS_TOTAL {
            out.estimated = true;
            break;
        }
        if !bvh_cache.contains_key(&b) {
            if bvhs_built >= MAX_INTERIOR_BVHS {
                out.estimated = true;
                break;
            }
            let sub: Vec<[usize; 3]> = faces_by_comp[b].iter().map(|&f| tris[f]).collect();
            bvh_cache.insert(b, MeshBvh::from_arrays(verts, &sub));
            bvhs_built += 1;
        }
        points_used += samples.len();
        out.candidate_components += 1;
        out.candidates += samples.len();
        let bvh = &bvh_cache[&b];

        if open[a] {
            did_raystab = true;
            let mut inside = 0usize;
            let mut unresolved = 0usize;
            for &f in &samples {
                let c = tri_centroid(verts, tris[f]);
                let n = normals[f];
                let p_plus = [c[0] + n[0] * eps, c[1] + n[1] * eps, c[2] + n[2] * eps];
                let p_minus = [c[0] - n[0] * eps, c[1] - n[1] * eps, c[2] - n[2] * eps];
                let v1 = stabber.vote(bvh, p_plus);
                let v2 = stabber.vote(bvh, p_minus);
                if v1.unresolved || v2.unresolved {
                    unresolved += 1;
                }
                if v1.inside || v2.inside {
                    inside += 1;
                }
            }
            // Majority of the sampled points lies in the enclosing solid.  An
            // open patch is inherently `unknown`, so it is only reported as
            // unconfirmed and never painted as a hard defect (plan I7).
            if inside * 2 > samples.len() && unresolved * 2 <= samples.len() {
                decided[a] = true;
                for &f in &faces_by_comp[a] {
                    unconfirmed.push(f);
                }
            }
            continue;
        }

        did_gwn = true;
        let mut inside = 0usize;
        let mut sign_sum = 0.0f64;
        let mut consistent = true;
        for &f in &samples {
            let c = tri_centroid(verts, tris[f]);
            let w = bvh.winding_at(c);
            if w.abs() > 0.5 {
                inside += 1;
            }
            if sign_sum == 0.0 {
                sign_sum = w;
            } else if w.signum() != sign_sum.signum() {
                consistent = false;
            }
        }
        let vol_sign = if vols[a] >= 0.0 { 1.0 } else { -1.0 };
        // Every sample must be inside the enclosing component and all must
        // agree on the orientation.  Same orientation as the enclosing solid
        // = embedded material (interior); opposite = a cavity, not interior.
        if inside == samples.len() && consistent && sign_sum.signum() == vol_sign {
            decided[a] = true;
            for &f in &faces_by_comp[a] {
                confirmed.push(f);
            }
        }
    }

    out.faces = confirmed.len();
    out.low_confidence = unconfirmed.len();
    out.fold_faces = fold_face.iter().filter(|&&x| x).count();
    if did_gwn {
        out.method = "gwn";
        out.confidence = CLOSED_CONFIDENCE;
    } else if did_raystab {
        out.method = "raystab";
        out.confidence = OPEN_CONFIDENCE;
    } else if out.fold_faces > 0 {
        out.confidence = CLOSED_CONFIDENCE;
    }

    // Confirmed interior is painted; unconfirmed interior is only reported as
    // a low-confidence region (plan I7) and never becomes a hard issue.
    for &f in &confirmed {
        face_mask[f] |= FACE_MASK_BIT;
    }
    for group in group_faces(&confirmed, edges) {
        regions.push(region_from_faces(
            "interior",
            "interior_surface",
            &group,
            tris,
            verts,
            group.len() as f64,
            CLOSED_CONFIDENCE,
            comp,
        ));
    }
    for group in group_faces(&unconfirmed, edges) {
        regions.push(region_from_faces(
            "interior",
            "interior_unconfirmed",
            &group,
            tris,
            verts,
            group.len() as f64,
            OPEN_CONFIDENCE,
            comp,
        ));
    }

    let folds: Vec<usize> = (0..nf).filter(|&f| fold_face[f]).collect();
    if !folds.is_empty() {
        let fold_member: HashSet<usize> = folds.iter().copied().collect();
        let mut fold_ds = DisjointSet::new(folds.len());
        let mut fold_index: HashMap<usize, usize> = HashMap::with_capacity(folds.len());
        for (i, &f) in folds.iter().enumerate() {
            fold_index.insert(f, i);
        }
        let mut keys: Vec<(usize, usize)> = edges.keys().copied().collect();
        keys.sort_unstable();
        for key in keys {
            let uses = &edges[&key];
            if uses.len() != 2 {
                continue;
            }
            let (a, b) = (uses[0].0, uses[1].0);
            if fold_member.contains(&a) && fold_member.contains(&b) {
                fold_ds.union(fold_index[&a], fold_index[&b]);
            }
        }
        let mut fold_groups: HashMap<usize, Vec<usize>> = HashMap::new();
        for &f in &folds {
            let r = fold_ds.find(fold_index[&f]);
            fold_groups.entry(r).or_default().push(f);
        }
        let mut fold_regions: Vec<(f64, Vec<usize>)> = fold_groups
            .into_values()
            .map(|mut g| {
                g.sort_unstable();
                let sev = g.iter().map(|&f| fold_sev[f]).fold(0.0, f64::max);
                (sev, g)
            })
            .collect();
        fold_regions.sort_by(|a, b| a.1.cmp(&b.1));
        for (sev, group) in fold_regions {
            for &f in &group {
                face_mask[f] |= FACE_MASK_BIT;
            }
            regions.push(region_from_faces(
                "interior",
                "fold_crease",
                &group,
                tris,
                verts,
                sev,
                CLOSED_CONFIDENCE,
                comp,
            ));
        }
    }

    out
}

fn global_aabb(verts: &[[f64; 3]]) -> crate::winding::Aabb {
    let mut b = crate::winding::Aabb::empty();
    for p in verts {
        b.expand_point(*p);
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnosis::{build_edges_for_tests, components_for_tests};

    fn cube(lo: f64, hi: f64) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let v = vec![
            [lo, lo, lo],
            [hi, lo, lo],
            [hi, hi, lo],
            [lo, hi, lo],
            [lo, lo, hi],
            [hi, lo, hi],
            [hi, hi, hi],
            [lo, hi, hi],
        ];
        let f = cube_faces(0);
        (v, f)
    }

    fn cube_faces(base: usize) -> Vec<[usize; 3]> {
        let b = base;
        vec![
            [b, b + 1, b + 2],
            [b, b + 2, b + 3],
            [b + 4, b + 6, b + 5],
            [b + 4, b + 7, b + 6],
            [b, b + 4, b + 5],
            [b, b + 5, b + 1],
            [b + 3, b + 2, b + 6],
            [b + 3, b + 6, b + 7],
            [b, b + 3, b + 7],
            [b, b + 7, b + 4],
            [b + 1, b + 5, b + 6],
            [b + 1, b + 6, b + 2],
        ]
    }

    fn run(
        verts: &[[f64; 3]],
        tris: &[[usize; 3]],
        flip_inner: Option<(usize, usize)>,
    ) -> Interior {
        let mut tris = tris.to_vec();
        if let Some((start, end)) = flip_inner {
            for t in &mut tris[start..end] {
                t.swap(1, 2);
            }
        }
        let edges = build_edges_for_tests(&tris);
        let (comp, ncomp) = components_for_tests(tris.len(), &edges);
        let mut mask = vec![0u8; tris.len()];
        let mut regions = Vec::new();
        interior_report(verts, &tris, &edges, &comp, ncomp, &mut mask, &mut regions)
    }

    #[test]
    fn embedded_cube_is_interior() {
        let (mut v, mut f) = cube(0.0, 10.0);
        let (vi, fi) = cube(3.0, 7.0);
        let base = v.len();
        v.extend_from_slice(&vi);
        f.extend(fi.iter().map(|t| [t[0] + base, t[1] + base, t[2] + base]));
        let r = run(&v, &f, None);
        assert!(r.available);
        assert_eq!(r.faces, 12, "{r:?}");
        assert_eq!(r.method, "gwn");
    }

    #[test]
    fn cavity_shell_is_not_interior() {
        let (mut v, mut f) = cube(0.0, 10.0);
        let (vi, fi) = cube(3.0, 7.0);
        let base = v.len();
        v.extend_from_slice(&vi);
        f.extend(fi.iter().map(|t| [t[0] + base, t[1] + base, t[2] + base]));
        let r = run(&v, &f, Some((12, 24)));
        assert_eq!(r.faces, 0, "{r:?}");
    }

    #[test]
    fn flattened_tetrahedron_has_a_fold() {
        // A closed tetrahedron with the apex just below the base plane.  The
        // base face and the side face share edge AB with nearly opposite
        // normals, and neither face touches a boundary edge.
        let v = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.5, 1.0, 0.0],
            [0.5, 0.5, -0.001],
        ];
        let f = vec![[0, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3]];
        let edges = build_edges_for_tests(&f);
        let (comp, ncomp) = components_for_tests(f.len(), &edges);
        let mut mask = vec![0u8; f.len()];
        let mut regions = Vec::new();
        let r = interior_report(&v, &f, &edges, &comp, ncomp, &mut mask, &mut regions);
        assert!(r.fold_faces >= 2, "{r:?}");
        assert!(regions.iter().any(|g| g.kind == "fold_crease"));
    }

    #[test]
    fn open_patch_inside_solid_is_unconfirmed() {
        let (mut v, mut f) = cube(0.0, 10.0);
        let base = v.len();
        v.extend_from_slice(&[[4.0, 4.0, 5.0], [6.0, 4.0, 5.0], [4.0, 6.0, 5.0]]);
        f.push([base, base + 1, base + 2]);
        let r = run(&v, &f, None);
        assert_eq!(r.faces, 0, "{r:?}");
        assert!(r.low_confidence >= 1, "{r:?}");
        assert_eq!(r.method, "raystab");
    }
}
