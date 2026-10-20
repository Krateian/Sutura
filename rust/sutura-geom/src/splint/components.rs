// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deterministic component counts for the raw cut.

use std::collections::HashMap;

/// Number of connected components when faces are joined **only across edges
/// used by exactly two faces** — the same rule as `split_components` in
/// `manifold.rs`, so the count is what Manifold's Stage-2 import will see.
///
/// Non-manifold edges (3+ faces) and pinch vertices do not join shells; that is
/// deliberately different from a plain edge/vertex-connected count. The count
/// is an integer; HashMap iteration only affects union roots, not the number of
/// components.
pub(crate) fn cut_shells(tris: &[[usize; 3]]) -> usize {
    let n = tris.len();
    if n == 0 {
        return 0;
    }
    let mut edges: HashMap<(usize, usize), [usize; 2]> = HashMap::with_capacity(n * 3);
    let mut counts: HashMap<(usize, usize), u8> = HashMap::with_capacity(n * 3);
    for (fi, t) in tris.iter().enumerate() {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a < b { (a, b) } else { (b, a) };
            let c = counts.entry(key).or_insert(0);
            *c = c.saturating_add(1);
            if *c <= 2 {
                edges.entry(key).or_insert([fi, usize::MAX])[usize::from(*c == 2)] = fi;
            }
        }
    }
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for (key, c) in &counts {
        if *c == 2 {
            let [a, b] = edges[key];
            let ra = find(&mut parent, a);
            let rb = find(&mut parent, b);
            if ra != rb {
                parent[ra] = rb;
            }
        }
    }
    let mut roots = 0usize;
    for i in 0..n {
        if find(&mut parent, i) == i {
            roots += 1;
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_connected_pair_is_one_shell() {
        assert_eq!(cut_shells(&[[0, 1, 2], [1, 3, 2]]), 1);
    }

    #[test]
    fn non_manifold_edge_does_not_join_faces() {
        // Three faces share edge (0,1): the edge is non-manifold, so every face
        // remains its own shell.
        assert_eq!(cut_shells(&[[0, 1, 2], [0, 1, 3], [0, 1, 4]]), 3);
    }

    #[test]
    fn shared_vertex_does_not_join_faces() {
        assert_eq!(cut_shells(&[[0, 1, 2], [0, 3, 4]]), 2);
    }

    #[test]
    fn disjoint_faces_and_empty_input() {
        assert_eq!(cut_shells(&[[0, 1, 2], [3, 4, 5]]), 2);
        assert_eq!(cut_shells(&[]), 0);
    }

    #[test]
    fn cube_is_one_shell() {
        let t = [
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [3, 7, 6],
            [3, 6, 2],
            [0, 4, 7],
            [0, 7, 3],
            [1, 2, 6],
            [1, 6, 5],
        ];
        assert_eq!(cut_shells(&t), 1);
    }
}
