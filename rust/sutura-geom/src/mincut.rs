// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Robust, high-performance s-t min-cut / max-flow solver.
//!
//! Uses Dinic's algorithm with guaranteed polynomial termination (O(V^2 E))
//! and strictly monotonic level progression, completely immune to infinite loops,
//! cycles, or orphan-adoption stalls.

use std::collections::VecDeque;

#[derive(Clone, Copy, Debug)]
pub struct Edge {
    pub to: u32,
    pub rev: u32,
    pub cap: f64,
}

pub struct BkGraph {
    pub num_user_nodes: usize,
    pub source: usize,
    pub sink: usize,
    pub edges: Vec<Edge>,
    pub head: Vec<i32>,
    pub next: Vec<i32>,
    level: Vec<i32>,
    ptr: Vec<i32>,
    reachable_from_source: Vec<bool>,
}

impl BkGraph {
    pub fn new(num_nodes: usize) -> Self {
        let source = num_nodes;
        let sink = num_nodes + 1;
        let total = num_nodes + 2;

        Self {
            num_user_nodes: num_nodes,
            source,
            sink,
            edges: Vec::new(),
            head: vec![-1; total],
            next: Vec::new(),
            level: vec![-1; total],
            ptr: vec![-1; total],
            reachable_from_source: vec![false; total],
        }
    }

    /// Add capacity from Source to node u and from node u to Sink.
    pub fn add_tweights(&mut self, u: usize, cap_s: f64, cap_t: f64) {
        if u < self.num_user_nodes {
            if cap_s > 0.0 {
                self.add_directed_edge(self.source, u, cap_s);
            }
            if cap_t > 0.0 {
                self.add_directed_edge(u, self.sink, cap_t);
            }
        }
    }

    /// Add undirected/bidirectional edge between u and v with respective capacities.
    pub fn add_edge(&mut self, u: usize, v: usize, cap_uv: f64, cap_vu: f64) {
        if u < self.num_user_nodes && v < self.num_user_nodes && u != v {
            self.add_bidirectional_edge(u, v, cap_uv, cap_vu);
        }
    }

    fn add_directed_edge(&mut self, from: usize, to: usize, cap: f64) {
        let forward_idx = self.edges.len() as u32;
        let reverse_idx = forward_idx + 1;

        self.edges.push(Edge { to: to as u32, rev: reverse_idx, cap });
        self.next.push(self.head[from]);
        self.head[from] = forward_idx as i32;

        self.edges.push(Edge { to: from as u32, rev: forward_idx, cap: 0.0 });
        self.next.push(self.head[to]);
        self.head[to] = reverse_idx as i32;
    }

    fn add_bidirectional_edge(&mut self, from: usize, to: usize, cap_fwd: f64, cap_rev: f64) {
        let forward_idx = self.edges.len() as u32;
        let reverse_idx = forward_idx + 1;

        self.edges.push(Edge { to: to as u32, rev: reverse_idx, cap: cap_fwd });
        self.next.push(self.head[from]);
        self.head[from] = forward_idx as i32;

        self.edges.push(Edge { to: from as u32, rev: forward_idx, cap: cap_rev });
        self.next.push(self.head[to]);
        self.head[to] = reverse_idx as i32;
    }

    /// Dinic's BFS: construct level graph.
    fn bfs(&mut self) -> bool {
        self.level.fill(-1);
        self.level[self.source] = 0;

        let mut queue = VecDeque::with_capacity(self.head.len());
        queue.push_back(self.source);

        while let Some(u) = queue.pop_front() {
            let u_lvl = self.level[u];
            let mut e = self.head[u];
            while e != -1 {
                let edge = self.edges[e as usize];
                let v = edge.to as usize;
                if edge.cap > 1e-12 && self.level[v] == -1 {
                    self.level[v] = u_lvl + 1;
                    if v == self.sink {
                        return true;
                    }
                    queue.push_back(v);
                }
                e = self.next[e as usize];
            }
        }

        self.level[self.sink] != -1
    }

    /// Dinic's DFS: push blocking flow along level graph.
    fn dfs(&mut self, u: usize, pushed: f64) -> f64 {
        if pushed <= 1e-12 || u == self.sink {
            return pushed;
        }

        let u_lvl = self.level[u];
        let mut e = self.ptr[u];

        while e != -1 {
            let edge = self.edges[e as usize];
            let v = edge.to as usize;

            if self.level[v] == u_lvl + 1 && edge.cap > 1e-12 {
                let tr = self.dfs(v, pushed.min(edge.cap));
                if tr > 1e-12 {
                    self.edges[e as usize].cap -= tr;
                    let rev = edge.rev as usize;
                    self.edges[rev].cap += tr;
                    return tr;
                }
            }

            e = self.next[e as usize];
            self.ptr[u] = e;
        }

        0.0
    }

    /// Compute max-flow and identify min-cut partition.
    pub fn maxflow(&mut self) -> f64 {
        let mut total_flow = 0.0;
        let mut iterations = 0;
        let max_iterations = self.head.len() * 2 + 10_000;

        while self.bfs() {
            iterations += 1;
            if iterations > max_iterations {
                eprintln!("WARNING: Dinic reached max iterations cap ({})", max_iterations);
                break;
            }

            for i in 0..self.head.len() {
                self.ptr[i] = self.head[i];
            }

            loop {
                let pushed = self.dfs(self.source, f64::INFINITY);
                if pushed <= 1e-12 {
                    break;
                }
                total_flow += pushed;
            }
        }

        // BFS to find all nodes reachable from Source in residual graph
        self.reachable_from_source.fill(false);
        let mut queue = VecDeque::new();
        self.reachable_from_source[self.source] = true;
        queue.push_back(self.source);

        while let Some(u) = queue.pop_front() {
            let mut e = self.head[u];
            while e != -1 {
                let edge = self.edges[e as usize];
                let v = edge.to as usize;
                if edge.cap > 1e-12 && !self.reachable_from_source[v] {
                    self.reachable_from_source[v] = true;
                    queue.push_back(v);
                }
                e = self.next[e as usize];
            }
        }

        total_flow
    }

    /// Returns whether node u belongs to the Source partition S.
    pub fn is_in_source(&self, u: usize) -> bool {
        if u < self.num_user_nodes {
            self.reachable_from_source[u]
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bk_simple_line() {
        // S -> 0 -> 1 -> T
        let mut g = BkGraph::new(2);
        g.add_tweights(0, 10.0, 0.0);
        g.add_tweights(1, 0.0, 5.0);
        g.add_edge(0, 1, 2.0, 0.0);

        let max_flow = g.maxflow();
        assert!((max_flow - 2.0).abs() < 1e-6);
        assert!(g.is_in_source(0));
        assert!(!g.is_in_source(1));
    }

    #[test]
    fn test_bk_diamond_source_cut() {
        // S -> 0, 1; 0 -> 2; 1 -> 2; 2 -> T
        let mut g = BkGraph::new(3);
        g.add_tweights(0, 10.0, 0.0);
        g.add_tweights(1, 10.0, 0.0);
        g.add_edge(0, 2, 3.0, 0.0);
        g.add_edge(1, 2, 4.0, 0.0);
        g.add_tweights(2, 0.0, 10.0);

        let max_flow = g.maxflow();
        assert!((max_flow - 7.0).abs() < 1e-6);
        assert!(g.is_in_source(0));
        assert!(g.is_in_source(1));
        assert!(!g.is_in_source(2));
    }

    #[test]
    fn test_bk_diamond_sink_cut() {
        // S -> 0, 1; 0 -> 2; 1 -> 2; 2 -> T
        let mut g = BkGraph::new(3);
        g.add_tweights(0, 10.0, 0.0);
        g.add_tweights(1, 10.0, 0.0);
        g.add_edge(0, 2, 3.0, 0.0);
        g.add_edge(1, 2, 4.0, 0.0);
        g.add_tweights(2, 0.0, 5.0);

        let max_flow = g.maxflow();
        assert!((max_flow - 5.0).abs() < 1e-6);
        assert!(g.is_in_source(0));
        assert!(g.is_in_source(1));
        assert!(g.is_in_source(2));
    }
}
