//! Point-cloud nearest-neighbour queries.
//!
//! `nearest_points` is the SciPy-free replacement for
//! `scipy.spatial.cKDTree.query`.  It builds an immutable KD-tree once and
//! answers batches of queries.  Distances are exact f64 Euclidean distances;
//! exact ties are broken by the smallest source index so the result is
//! deterministic across runs and query order.

use kiddo::{ImmutableKdTree, SquaredEuclidean};
use rayon::prelude::*;
use std::cmp::Ordering;
use std::num::NonZeroUsize;

/// Reusable immutable point-cloud tree (item indices are the source indices).
pub type PointTree = ImmutableKdTree<f64, 3>;

/// Build a tree for repeated query batches.
pub fn build_tree(points: &[[f64; 3]]) -> Option<PointTree> {
    if points.is_empty() {
        return None;
    }
    ImmutableKdTree::new_from_slice(points).ok()
}

/// Query a prebuilt tree.
///
/// Returns `(distances, indices, neighbours)` with the first two vectors flat
/// in query-major order (`query_index * neighbours + neighbour_index`).
/// `neighbours` is `k` clamped to `[1, point_count]`.
pub fn query_neighbours(
    tree: &PointTree,
    point_count: usize,
    queries: &[[f64; 3]],
    k: usize,
) -> (Vec<f64>, Vec<i32>, usize) {
    let take = k.max(1).min(point_count);
    let probe = (take + 1).min(point_count);
    let probe_nz = NonZeroUsize::new(probe).expect("probe count is positive");

    let rows: Vec<(Vec<f64>, Vec<i32>)> = queries
        .par_iter()
        .map(|q| {
            let mut items = tree
                .query(q)
                .nearest_n::<SquaredEuclidean<f64>>(probe_nz)
                .execute();
            items.sort_by(|a, b| {
                a.distance
                    .total_cmp(&b.distance)
                    .then_with(|| a.item.cmp(&b.item))
            });

            // If the first item beyond the requested k is tied with the last
            // requested item, collect every item at that distance so the
            // smallest-index tie-break is exact.
            if items.len() > take
                && items[take].distance.total_cmp(&items[take - 1].distance) == Ordering::Equal
            {
                let radius = items[take - 1].distance;
                let mut all = tree
                    .query(q)
                    .within::<SquaredEuclidean<f64>>(radius)
                    .execute();
                all.sort_by(|a, b| {
                    a.distance
                        .total_cmp(&b.distance)
                        .then_with(|| a.item.cmp(&b.item))
                });
                items = all;
            }
            items.truncate(take);

            let mut dist = Vec::with_capacity(items.len());
            let mut idx = Vec::with_capacity(items.len());
            for it in items {
                dist.push(it.distance.sqrt());
                idx.push(it.item as i32);
            }
            (dist, idx)
        })
        .collect();

    let mut dist = Vec::with_capacity(rows.len() * take);
    let mut idx = Vec::with_capacity(rows.len() * take);
    for (mut d, mut i) in rows {
        d.resize(take, f64::INFINITY);
        i.resize(take, -1);
        dist.extend(d);
        idx.extend(i);
    }
    (dist, idx, take)
}

/// Nearest neighbours of every query point.
///
/// Convenience wrapper that builds the tree and queries it once.  Use
/// `build_tree` + `query_neighbours` for repeated batches.
pub fn nearest_points(
    points: &[[f64; 3]],
    queries: &[[f64; 3]],
    k: usize,
) -> (Vec<f64>, Vec<i32>, usize) {
    match build_tree(points) {
        None => (Vec::new(), Vec::new(), 0),
        Some(tree) => query_neighbours(&tree, points.len(), queries, k),
    }
}

#[cfg(test)]
mod tests {
    use super::nearest_points;

    fn brute(points: &[[f64; 3]], q: [f64; 3], k: usize) -> Vec<(f64, i32)> {
        let mut all: Vec<(f64, i32)> = points
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let d = ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2))
                    .sqrt();
                (d, i as i32)
            })
            .collect();
        all.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        all.truncate(k.min(points.len()));
        all
    }

    #[test]
    fn matches_brute_force_and_breaks_ties_by_index() {
        let points = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
        ];
        let queries = [[0.0, 0.0, 0.0], [0.5, 0.0, 0.0], [0.0, 0.0, 2.0]];
        let (dist, idx, take) = nearest_points(&points, &queries, 2);
        assert_eq!(take, 2);
        for (qi, q) in queries.iter().enumerate() {
            let expected = brute(&points, *q, 2);
            for j in 0..2 {
                let pos = qi * 2 + j;
                assert_eq!(idx[pos], expected[j].1, "query {qi} slot {j}");
                assert!((dist[pos] - expected[j].0).abs() < 1e-12);
            }
        }
        // The origin is an exact five-way tie: index 0 must win.
        assert_eq!(idx[0], 0);
    }

    #[test]
    fn clamps_k_to_point_count_and_handles_empty_queries() {
        let points = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]];
        let (dist, idx, take) = nearest_points(&points, &[[0.0; 3]], 12);
        assert_eq!(take, 2);
        assert_eq!(dist, vec![0.0, 1.0]);
        assert_eq!(idx, vec![0, 1]);

        let (d, i, t) = nearest_points(&points, &[], 3);
        assert_eq!(t, 2);
        assert!(d.is_empty() && i.is_empty());
    }

    #[test]
    fn k_one_returns_one_column() {
        let points = [[0.0, 0.0, 0.0], [2.0, 0.0, 0.0]];
        let (dist, idx, take) = nearest_points(&points, &[[1.0, 0.0, 0.0]], 1);
        assert_eq!(take, 1);
        assert_eq!(idx, vec![0]);
        assert!((dist[0] - 1.0).abs() < 1e-12);
    }
}
