// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Connected components of an undirected graph, matching Python's
//! `topology.connected_components` / SciPy's `connected_components`.
//!
//! The labelling is part of the contract, not just the count: the repeat
//! detector groups faces by component id and then walks `np.unique`'s ascending
//! order, so a different numbering changes which patch is which. Python's rule:
//!
//! 1. union by **minimum root** (`if ru < rv: parent[rv] = ru`), and
//! 2. labels are the rank of each node's root in the sorted unique roots,
//!
//! which makes component 0 the one containing node 0, component 1 the lowest
//! node not in component 0, and so on.

/// Union-find with the min-root and canonical-label rules Python uses.
fn find(parent: &mut [usize], mut x: usize) -> usize {
    let mut root = x;
    while parent[root] != root {
        root = parent[root];
    }
    while parent[x] != root {
        let next = parent[x];
        parent[x] = root;
        x = next;
    }
    root
}

/// `(n_components, labels)` for `n_nodes` nodes and undirected `edges`.
///
/// Edges with equal endpoints or out-of-range nodes are dropped, exactly as
/// `topology.connected_components` filters them (`u != v`, `0 <= u`,
/// `v < n_nodes`).
pub fn connected_components(n_nodes: usize, edges: &[(usize, usize)]) -> (usize, Vec<usize>) {
    if n_nodes == 0 {
        return (0, Vec::new());
    }
    if edges.is_empty() {
        return (n_nodes, (0..n_nodes).collect());
    }
    let mut parent: Vec<usize> = (0..n_nodes).collect();
    for &(a, b) in edges {
        let (u, v) = if a <= b { (a, b) } else { (b, a) };
        if u == v || v >= n_nodes {
            continue;
        }
        let (ru, rv) = (find(&mut parent, u), find(&mut parent, v));
        if ru < rv {
            parent[rv] = ru;
        } else if rv < ru {
            parent[ru] = rv;
        }
    }
    let roots: Vec<usize> = (0..n_nodes).map(|i| find(&mut parent, i)).collect();
    // `np.unique(roots, return_inverse=True)` gives ascending unique roots and
    // the inverse index as the label.
    let mut uniq = roots.clone();
    uniq.sort_unstable();
    uniq.dedup();
    let labels = roots
        .iter()
        .map(|r| uniq.binary_search(r).expect("root present after sorting"))
        .collect();
    (uniq.len(), labels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_nodes_is_empty() {
        let (n, labels) = connected_components(0, &[]);
        assert_eq!(n, 0);
        assert!(labels.is_empty());
    }

    #[test]
    fn no_edges_gives_every_node_its_own_component() {
        let (n, labels) = connected_components(3, &[]);
        assert_eq!(n, 3);
        assert_eq!(labels, vec![0, 1, 2]);
    }

    #[test]
    fn a_single_edge_merges_two_nodes() {
        let (n, labels) = connected_components(3, &[(0, 1)]);
        assert_eq!(n, 2);
        assert_eq!(labels, vec![0, 0, 1]);
    }

    /// Labels are the rank of each node's root among the sorted unique roots,
    /// so they follow the *lowest member* of each component -- node 4 lands in
    /// component 0 because its root is 0, and node 3 in component 2 because its
    /// root is 2. Golden values from `topology.connected_components`.
    #[test]
    fn labels_follow_the_lowest_member() {
        let (n, labels) = connected_components(5, &[(3, 2), (4, 0)]);
        assert_eq!(n, 3);
        assert_eq!(labels, vec![0, 1, 2, 2, 0]);
    }

    #[test]
    fn self_loops_and_out_of_range_edges_are_dropped() {
        let (n, labels) = connected_components(2, &[(0, 0), (0, 7)]);
        assert_eq!(n, 2);
        assert_eq!(labels, vec![0, 1]);
    }

    /// Union by minimum root, not by first-seen: chaining 2-1 then 1-0 must
    /// still put all three in the component rooted at 0.
    #[test]
    fn union_uses_the_minimum_root_when_chained() {
        let (n, labels) = connected_components(3, &[(2, 1), (1, 0)]);
        assert_eq!(n, 1);
        assert_eq!(labels, vec![0, 0, 0]);
    }

    /// A face-adjacency graph shaped like `segment_elements`' input: 8 faces
    /// joined into two quads. Golden labels from `topology.connected_components`
    /// on the same edge list, which is what the patch grouping walks.
    #[test]
    fn face_adjacency_matches_the_python_walk() {
        // Faces 0-3 form one quad, faces 4-7 another.
        let edges = [
            (0usize, 1usize),
            (1, 2),
            (2, 0),
            (2, 3),
            (3, 1),
            (4, 5),
            (5, 6),
            (6, 4),
            (6, 7),
            (7, 5),
        ];
        let (n, labels) = connected_components(8, &edges);
        assert_eq!(n, 2);
        assert_eq!(labels, vec![0, 0, 0, 0, 1, 1, 1, 1]);
    }

    /// Labels must not depend on the edge order, since `segment_elements`
    /// builds its adjacency from a `dict` whose insertion order follows the
    /// triangle scan.
    #[test]
    fn labels_are_independent_of_edge_order() {
        let edges = [(0usize, 1usize), (2, 3), (1, 2), (4, 5)];
        let mut reversed = edges.to_vec();
        reversed.reverse();
        let a = connected_components(6, &edges);
        let b = connected_components(6, &reversed);
        assert_eq!(a, b, "labelling must be order independent");
    }
}
