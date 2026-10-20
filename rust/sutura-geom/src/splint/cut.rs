// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Min s-t cut and the fix-A signed-field island cleanup.

use std::collections::VecDeque;

use crate::mincut::BkGraph;

const INF: f64 = 1e18;
const LAMBDA: f64 = 20.0;

pub(crate) struct CutResult {
    pub(crate) is_inside: Vec<bool>,
    pub(crate) dropped_inside_comps: usize,
    pub(crate) dropped_inside_vol: f64,
    pub(crate) filled_outside_comps: usize,
    pub(crate) filled_outside_vol: f64,
}

fn component_volume(comp: &[usize], tet_vol: &[f64]) -> f64 {
    let mut vols: Vec<f64> = comp.iter().map(|&i| tet_vol[i]).collect();
    vols.sort_by(|a, b| a.total_cmp(b));
    vols.iter().sum()
}

fn tri_area(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> f64 {
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let cross = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    0.5 * (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt()
}

/// Remove inside components that contain no deep interior (`phi < -tau`) and
/// fill outside components that touch no hull tet and contain no deep exterior
/// (`phi > +tau`). The BFS and the volume sums run in index order, so the result
/// is deterministic.
pub(crate) fn cleanup_islands(
    is_inside: &mut [bool],
    phi: &[f64],
    adj: &[Vec<usize>],
    out_tet: &[bool],
    tau: f64,
    tet_vol: &[f64],
) -> (usize, f64, usize, f64) {
    let n = is_inside.len();
    let mut visited = vec![false; n];
    let mut dropped_inside_comps = 0usize;
    let mut dropped_inside_vol = 0.0f64;

    // 1. Inside components: drop if they contain no tet with phi < -tau.
    for ti in 0..n {
        if is_inside[ti] && !visited[ti] {
            let mut comp = Vec::new();
            let mut q = VecDeque::new();
            visited[ti] = true;
            q.push_back(ti);
            while let Some(curr) = q.pop_front() {
                comp.push(curr);
                for &next in &adj[curr] {
                    if is_inside[next] && !visited[next] {
                        visited[next] = true;
                        q.push_back(next);
                    }
                }
            }
            comp.sort_unstable();
            let has_deep_interior = comp.iter().any(|&idx| phi[idx] < -tau);
            if !has_deep_interior {
                let comp_vol = component_volume(&comp, tet_vol);
                dropped_inside_comps += 1;
                dropped_inside_vol += comp_vol;
                for &idx in &comp {
                    is_inside[idx] = false;
                }
            }
        }
    }

    // 2. Outside components: fill if they touch no hull tet and contain no tet
    //    with phi > +tau.
    visited.fill(false);
    let mut filled_outside_comps = 0usize;
    let mut filled_outside_vol = 0.0f64;
    for ti in 0..n {
        if !is_inside[ti] && !visited[ti] {
            let mut comp = Vec::new();
            let mut q = VecDeque::new();
            visited[ti] = true;
            q.push_back(ti);
            while let Some(curr) = q.pop_front() {
                comp.push(curr);
                for &next in &adj[curr] {
                    if !is_inside[next] && !visited[next] {
                        visited[next] = true;
                        q.push_back(next);
                    }
                }
            }
            comp.sort_unstable();
            let touches_hull = comp.iter().any(|&idx| out_tet[idx]);
            let has_deep_exterior = comp.iter().any(|&idx| phi[idx] > tau);
            if !touches_hull && !has_deep_exterior {
                let comp_vol = component_volume(&comp, tet_vol);
                filled_outside_comps += 1;
                filled_outside_vol += comp_vol;
                for &idx in &comp {
                    is_inside[idx] = true;
                }
            }
        }
    }

    (
        dropped_inside_comps,
        dropped_inside_vol,
        filled_outside_comps,
        filled_outside_vol,
    )
}

pub(crate) fn cut_and_cleanup(
    tets: &[[usize; 4]],
    coords: &[[f64; 3]],
    internal: &[([usize; 3], usize, usize)],
    out_tet: &[bool],
    phi: &[f64],
    init_label: &[u8],
    scale: f64,
    voxel: f64,
) -> CutResult {
    let n_tets = tets.len();
    let tau = 0.25 * voxel;
    let mut g = BkGraph::new(n_tets);
    for ti in 0..n_tets {
        if out_tet[ti] {
            g.add_tweights(ti, 0.0, INF);
        } else if phi[ti] < 0.0 {
            g.add_tweights(ti, INF, 0.0);
        }
    }
    for (key, a, b) in internal {
        let area = tri_area(coords[key[0]], coords[key[1]], coords[key[2]]);
        let cap = if init_label[*a] == init_label[*b] {
            area * (1.0 + LAMBDA)
        } else {
            area
        };
        g.add_edge(*a, *b, cap, cap);
    }
    let _flow = g.maxflow();
    let mut is_inside: Vec<bool> = (0..n_tets).map(|ti| g.is_in_source(ti)).collect();

    let mut adj: Vec<Vec<usize>> = vec![Vec::with_capacity(4); n_tets];
    for &(_, a, b) in internal {
        adj[a].push(b);
        adj[b].push(a);
    }

    let tet_vol: Vec<f64> = (0..n_tets)
        .map(|ti| {
            let t = tets[ti];
            let p0 = coords[t[0]];
            let p1 = coords[t[1]];
            let p2 = coords[t[2]];
            let p3 = coords[t[3]];
            let v01 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
            let v02 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
            let v03 = [p3[0] - p0[0], p3[1] - p0[1], p3[2] - p0[2]];
            let cross = [
                v02[1] * v03[2] - v02[2] * v03[1],
                v02[2] * v03[0] - v02[0] * v03[2],
                v02[0] * v03[1] - v02[1] * v03[0],
            ];
            let v_norm = (v01[0] * cross[0] + v01[1] * cross[1] + v01[2] * cross[2]).abs() / 6.0;
            v_norm / (scale * scale * scale)
        })
        .collect();

    let (dropped_inside_comps, dropped_inside_vol, filled_outside_comps, filled_outside_vol) =
        cleanup_islands(&mut is_inside, phi, &adj, out_tet, tau, &tet_vol);

    CutResult {
        is_inside,
        dropped_inside_comps,
        dropped_inside_vol,
        filled_outside_comps,
        filled_outside_vol,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_tet_island_is_removed() {
        // 0-1 is the main body with a deep interior; 2 is a single inside
        // island connected only to the outside hull tet 3.
        let mut is_inside = vec![true, true, true, false];
        let phi = vec![-1.0, -1.0, -0.1, 0.1];
        let adj = vec![vec![1], vec![0], vec![3], vec![2]];
        let out_tet = vec![true, false, false, true];
        let tet_vol = vec![1.0, 1.0, 1.0, 1.0];
        let (dropped, dropped_vol, filled, _) =
            cleanup_islands(&mut is_inside, &phi, &adj, &out_tet, 0.25, &tet_vol);
        assert_eq!(dropped, 1);
        assert!(dropped_vol > 0.0);
        assert_eq!(filled, 0);
        assert_eq!(is_inside, vec![true, true, false, false]);
    }

    #[test]
    fn multi_body_control_is_preserved() {
        // Two disjoint solids, each with a deep interior, plus their hull
        // outside tets. Nothing may be dropped or filled.
        let mut is_inside = vec![true, true, true, true, false, false];
        let phi = vec![-1.0, -1.0, -1.0, -1.0, 0.1, 0.1];
        let adj = vec![vec![1], vec![0], vec![3], vec![2], vec![5], vec![4]];
        let out_tet = vec![true, false, true, false, true, true];
        let tet_vol = vec![1.0; 6];
        let (dropped, _, filled, _) =
            cleanup_islands(&mut is_inside, &phi, &adj, &out_tet, 0.25, &tet_vol);
        assert_eq!(dropped, 0);
        assert_eq!(filled, 0);
        assert_eq!(is_inside, vec![true, true, true, true, false, false]);
    }
}
