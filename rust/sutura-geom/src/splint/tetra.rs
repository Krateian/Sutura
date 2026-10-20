// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tetrahedron facets, adjacency and centroids.

use std::collections::HashMap;

pub(crate) struct TetraTopology {
    /// Internal faces: sorted vertex triple + the two adjacent tet indices.
    /// Sorted by (triple, a, b) so every downstream pass has a fixed order.
    pub(crate) internal: Vec<([usize; 3], usize, usize)>,
    pub(crate) out_tet: Vec<bool>,
    pub(crate) centroids: Vec<[f64; 3]>,
    pub(crate) outer_faces: usize,
}

pub(crate) fn build(tets: &[[usize; 4]], coords: &[[f64; 3]]) -> TetraTopology {
    let n_tets = tets.len();
    let sub_faces = [[1usize, 2, 3], [0, 2, 3], [0, 1, 3], [0, 1, 2]];
    let mut facets: HashMap<[usize; 3], Vec<usize>> = HashMap::new();
    for (ti, t) in tets.iter().enumerate() {
        for sf in &sub_faces {
            let mut triple = [t[sf[0]], t[sf[1]], t[sf[2]]];
            triple.sort_unstable();
            facets.entry(triple).or_default().push(ti);
        }
    }

    let mut out_tet = vec![false; n_tets];
    let mut outer_faces = 0usize;
    let mut internal: Vec<([usize; 3], usize, usize)> = Vec::new();
    for (key, owners) in &facets {
        match owners.len() {
            1 => {
                out_tet[owners[0]] = true;
                outer_faces += 1;
            }
            2 => internal.push((*key, owners[0], owners[1])),
            _ => {}
        }
    }
    internal.sort_unstable();

    let mut centroids = vec![[0.0; 3]; n_tets];
    for (ti, t) in tets.iter().enumerate() {
        let mut c = [0.0; 3];
        for a in 0..3 {
            c[a] = (coords[t[0]][a] + coords[t[1]][a] + coords[t[2]][a] + coords[t[3]][a]) * 0.25;
        }
        centroids[ti] = c;
    }

    TetraTopology {
        internal,
        out_tet,
        centroids,
        outer_faces,
    }
}
