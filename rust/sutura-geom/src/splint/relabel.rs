// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deterministic greedy relabelling of tet `is_inside` flags to remove
//! non-manifold boundary edges.
//!
//! The emitted surface is the set of shared tetrahedron facets whose two owners
//! differ in label. A tet *edge* is non-manifold when three or more of those
//! boundary facets meet along it (the in/out labels around the edge are not one
//! contiguous block). Flipping a single tet toggles the boundary status of its
//! four facets, which changes the boundary-facet count of at most twelve edges;
//! we take the flip that most reduces the number of over-subscribed edges until
//! no strictly improving flip remains. Ties are broken by lowest tet index, so
//! the result is a pure function of the input.

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

    let mut flips = 0usize;
    while flips < max_flips {
        let mut best: Option<(i32, i32, usize)> = None;
        for ti in 0..tets.len() {
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
            let improving = delta_nm < 0 || (delta_nm == 0 && delta_hole < 0);
            if !improving {
                continue;
            }
            let cand = (delta_nm, delta_hole, ti);
            if best.map_or(true, |b| cand < b) {
                best = Some(cand);
            }
        }
        let Some((_, _, ti)) = best else { break };
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

    let final_nm_edges = bcount.values().filter(|&&c| c > 2).count();
    RelabelOutcome {
        first_nm_edges,
        final_nm_edges,
        flips,
    }
}
