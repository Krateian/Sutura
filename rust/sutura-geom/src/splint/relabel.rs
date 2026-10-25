// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deterministic greedy relabelling of tet `is_inside` flags to remove
//! non-manifold boundary edges.
//!
//! The emitted surface is the set of shared tetrahedron facets whose two owners
//! differ in label. A tet *edge* is non-manifold when three or more of those
//! boundary facets meet along it (the in/out labels around the edge are not one
//! contiguous block). Flipping a tet toggles the boundary status of its four
//! facets, which changes the boundary-facet count of at most twelve edges; we
//! take the move that most reduces the number of over-subscribed edges until no
//! strictly improving move remains. Moves are single-tet flips plus *region*
//! flips: a whole same-label connected block of free tets is flipped at once,
//! which escapes the single-flip local minima left by pinched regions. Ties are
//! broken by lowest tet index, so the result is a pure function of the input.

use std::collections::HashMap;

const SUB: [[usize; 3]; 4] = [[1, 2, 3], [0, 2, 3], [0, 1, 3], [0, 1, 2]];

type Edge = (usize, usize);

fn edges_of(key: [usize; 3]) -> [Edge; 3] {
    [
        (key[0].min(key[1]), key[0].max(key[1])),
        (key[1].min(key[2]), key[1].max(key[2])),
        (key[0].min(key[2]), key[0].max(key[2])),
    ]
}

fn nm_weight(count: i32) -> i32 {
    i32::from(count > 2)
}

fn hole_weight(count: i32) -> i32 {
    i32::from(count == 1)
}

/// Same-label connected components of the free (non-hull) tets. `region[ti]`
/// is the component id, or `usize::MAX` for a hull/unused tet. Ids are assigned
/// in increasing start-index order, so the labelling is deterministic.
fn same_label_regions(
    adj_free: &[Vec<usize>],
    is_inside: &[bool],
    out_tet: &[bool],
) -> Vec<usize> {
    let n = is_inside.len();
    let mut region = vec![usize::MAX; n];
    let mut next = 0usize;
    let mut stack: Vec<usize> = Vec::new();
    for s in 0..n {
        if out_tet[s] || region[s] != usize::MAX {
            continue;
        }
        let r = next;
        next += 1;
        region[s] = r;
        stack.push(s);
        while let Some(c) = stack.pop() {
            for &nb in &adj_free[c] {
                if region[nb] == usize::MAX && is_inside[nb] == is_inside[c] {
                    region[nb] = r;
                    stack.push(nb);
                }
            }
        }
    }
    region
}

pub(crate) struct RelabelOutcome {
    pub(crate) first_nm_edges: usize,
    pub(crate) final_nm_edges: usize,
    pub(crate) flips: usize,
}

/// Relabel in place. `internal` are the shared facets (sorted triple + the two
/// owning tet indices). Hull tets are never flipped; they anchor the outside.
pub(crate) fn greedy_relabel(
    tets: &[[usize; 4]],
    internal: &[([usize; 3], usize, usize)],
    out_tet: &[bool],
    is_inside: &mut [bool],
    phi: &[f64],
    tau: f64,
    max_flips: usize,
) -> RelabelOutcome {
    let mut face_index: HashMap<[usize; 3], usize> = HashMap::with_capacity(internal.len());
    for (i, (key, _, _)) in internal.iter().enumerate() {
        face_index.insert(*key, i);
    }

    // Per tet: the internal-facet index of each of its four facets, if shared.
    let tet_facets: Vec<[Option<usize>; 4]> = tets
        .iter()
        .map(|t| {
            let mut slots = [None; 4];
            for (k, sf) in SUB.iter().enumerate() {
                let mut key = [t[sf[0]], t[sf[1]], t[sf[2]]];
                key.sort_unstable();
                slots[k] = face_index.get(&key).copied();
            }
            slots
        })
        .collect();

    let mut is_boundary = vec![false; internal.len()];
    let mut bcount: HashMap<Edge, i32> = HashMap::new();
    for (i, (key, a, b)) in internal.iter().enumerate() {
        if is_inside[*a] != is_inside[*b] {
            is_boundary[i] = true;
            for e in edges_of(*key) {
                *bcount.entry(e).or_insert(0) += 1;
            }
        }
    }
    let first_nm_edges = bcount.values().filter(|&&c| c > 2).count();

    // Free-tet adjacency, for the same-label region moves.
    let mut adj_free: Vec<Vec<usize>> = vec![Vec::new(); tets.len()];
    for (_, a, b) in internal {
        if !out_tet[*a] && !out_tet[*b] {
            adj_free[*a].push(*b);
            adj_free[*b].push(*a);
        }
    }

    let n = tets.len();
    let mut flips = 0usize;
    while flips < max_flips {
        // --- best single-tet flip ---
        let mut best_single: Option<(i32, i32, usize)> = None;
        for ti in 0..n {
            if out_tet[ti] {
                continue;
            }
            let mut edge_delta: HashMap<Edge, i32> = HashMap::new();
            for fi in tet_facets[ti].iter().flatten() {
                let key = internal[*fi].0;
                let sign = if is_boundary[*fi] { -1 } else { 1 };
                for e in edges_of(key) {
                    *edge_delta.entry(e).or_insert(0) += sign;
                }
            }
            let mut delta_nm = 0i32;
            let mut delta_hole = 0i32;
            for (e, d) in &edge_delta {
                let before = bcount.get(e).copied().unwrap_or(0);
                let after = before + d;
                delta_nm += nm_weight(after) - nm_weight(before);
                delta_hole += hole_weight(after) - hole_weight(before);
            }
            if delta_nm < 0 || (delta_nm == 0 && delta_hole < 0) {
                let cand = (delta_nm, delta_hole, ti);
                if best_single.map_or(true, |b| cand < b) {
                    best_single = Some(cand);
                }
            }
        }

        // --- best region flip (a whole same-label connected block) ---
        let region = same_label_regions(&adj_free, is_inside, out_tet);
        let mut region_delta: HashMap<(usize, Edge), i32> = HashMap::new();
        let mut region_min: HashMap<usize, usize> = HashMap::new();
        for (fi, (key, a, b)) in internal.iter().enumerate() {
            let ra = region[*a];
            let rb = region[*b];
            if ra == rb {
                continue; // facet internal to one region: flip leaves it unchanged
            }
            let sign = if is_boundary[fi] { -1 } else { 1 };
            if ra != usize::MAX {
                for e in edges_of(*key) {
                    *region_delta.entry((ra, e)).or_insert(0) += sign;
                }
            }
            if rb != usize::MAX {
                for e in edges_of(*key) {
                    *region_delta.entry((rb, e)).or_insert(0) += sign;
                }
            }
        }
        for (i, r) in region.iter().enumerate() {
            if *r != usize::MAX {
                region_min
                    .entry(*r)
                    .and_modify(|m| {
                        if i < *m {
                            *m = i;
                        }
                    })
                    .or_insert(i);
            }
        }
        // A region move is only allowed inside the ambiguous band: every tet of
        // the region must have `|phi| <= tau`. Otherwise the degenerate "flip
        // the whole solid to empty" move trivially removes all boundary edges.
        let mut region_ok: HashMap<usize, bool> = HashMap::new();
        for (i, r) in region.iter().enumerate() {
            if *r != usize::MAX {
                let ok = region_ok.entry(*r).or_insert(true);
                if phi[i].abs() > tau {
                    *ok = false;
                }
            }
        }
        let mut per_region: HashMap<usize, (i32, i32)> = HashMap::new();
        for ((r, e), d) in &region_delta {
            let before = bcount.get(e).copied().unwrap_or(0);
            let after = before + d;
            let ent = per_region.entry(*r).or_insert((0, 0));
            ent.0 += nm_weight(after) - nm_weight(before);
            ent.1 += hole_weight(after) - hole_weight(before);
        }
        // (delta_nm, delta_hole, min_tet_index, region_id); min index makes the
        // choice unique, so HashMap iteration order cannot affect the result.
        let mut best_region: Option<(i32, i32, usize, usize)> = None;
        for (r, (dnm, dh)) in &per_region {
            if !region_ok.get(r).copied().unwrap_or(false) {
                continue;
            }
            if *dnm < 0 || (*dnm == 0 && *dh < 0) {
                let cand = (*dnm, *dh, region_min[r], *r);
                if best_region.map_or(true, |b| cand < b) {
                    best_region = Some(cand);
                }
            }
        }

        let use_region = match (best_single, best_region) {
            (Some(s), Some(r)) => (r.0, r.1, r.2) < (s.0, s.1, s.2),
            (None, Some(_)) => true,
            (Some(_), None) => false,
            (None, None) => break,
        };

        if use_region {
            let r = best_region.unwrap().3;
            let mut affected: Vec<usize> = Vec::new();
            for (fi, (_, a, b)) in internal.iter().enumerate() {
                if (region[*a] == r) != (region[*b] == r) {
                    affected.push(fi);
                }
            }
            for ti in 0..n {
                if region[ti] == r {
                    is_inside[ti] = !is_inside[ti];
                    flips += 1;
                }
            }
            for fi in affected {
                let (key, a, b) = internal[fi];
                let new_b = is_inside[a] != is_inside[b];
                if new_b != is_boundary[fi] {
                    let sign = if new_b { 1 } else { -1 };
                    for e in edges_of(key) {
                        *bcount.entry(e).or_insert(0) += sign;
                    }
                    is_boundary[fi] = new_b;
                }
            }
        } else {
            let ti = best_single.unwrap().2;
            is_inside[ti] = !is_inside[ti];
            for fi in tet_facets[ti].iter().flatten() {
                let key = internal[*fi].0;
                let sign = if is_boundary[*fi] { -1 } else { 1 };
                is_boundary[*fi] = !is_boundary[*fi];
                for e in edges_of(key) {
                    *bcount.entry(e).or_insert(0) += sign;
                }
            }
            flips += 1;
        }
    }

    let final_nm_edges = bcount.values().filter(|&&c| c > 2).count();
    RelabelOutcome {
        first_nm_edges,
        final_nm_edges,
        flips,
    }
}
