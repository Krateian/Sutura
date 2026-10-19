#!/usr/bin/env python3
"""Regression test for topology.connected_components (disjoint-set union-find).

Verifies compatibility with a brute-force breadth-first search reference on
arbitrary graphs, edge cases, and multiple input shapes.  topology.py is
stdlib + numpy only and the reference does not import SciPy.
Usage: python3 tests/test_connected_components.py
"""
import os
import sys

REPO_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA_DIR = os.path.join(REPO_DIR, 'sutura')
if SUTURA_DIR not in sys.path:
    sys.path.insert(0, SUTURA_DIR)

import numpy as np  # noqa: E402

import topology  # noqa: E402


def test_empty_graph():
    nc, labels = topology.connected_components((0, np.zeros((0, 2), dtype=int)))
    assert nc == 0
    assert len(labels) == 0

    assert topology.connected_components((0, np.zeros((0, 2), dtype=int)), return_labels=False) == 0


def test_isolated_nodes():
    nc, labels = topology.connected_components((5, np.zeros((0, 2), dtype=int)))
    assert nc == 5
    assert np.array_equal(labels, np.arange(5))


def test_simple_triangle_and_edge():
    # Triangle: 0-1, 1-2, 2-0. Edge: 3-4. Isolated: 5.
    edges = np.array([
        [0, 1], [1, 2], [2, 0],
        [3, 4],
    ])
    nc, labels = topology.connected_components((6, edges))
    assert nc == 3
    assert labels[0] == labels[1] == labels[2] == 0
    assert labels[3] == labels[4] == 1
    assert labels[5] == 2


def test_tuple_coordinate_format():
    row = np.array([0, 1, 3])
    col = np.array([1, 2, 4])
    nc, labels = topology.connected_components((5, (row, col)))
    assert nc == 2
    assert labels[0] == labels[1] == labels[2] == 0
    assert labels[3] == labels[4] == 1


def _reference_components(n_nodes, edges):
    """Brute-force BFS labelling with the same lowest-node-first convention."""
    adj = [[] for _ in range(n_nodes)]
    for u, v in np.asarray(edges, dtype=np.int64):
        if 0 <= u < n_nodes and 0 <= v < n_nodes and u != v:
            adj[u].append(v)
            adj[v].append(u)
    labels = np.full(n_nodes, -1, dtype=np.int64)
    n_comp = 0
    for start in range(n_nodes):
        if labels[start] != -1:
            continue
        labels[start] = n_comp
        stack = [start]
        while stack:
            node = stack.pop()
            for nxt in adj[node]:
                if labels[nxt] == -1:
                    labels[nxt] = n_comp
                    stack.append(nxt)
        n_comp += 1
    return n_comp, labels


def test_randomized_parity_with_bruteforce():
    rng = np.random.default_rng(1337)
    for _ in range(50):
        n = int(rng.integers(1, 200))
        m = int(rng.integers(0, 500))
        if m > 0:
            edges = rng.integers(0, n, size=(m, 2))
        else:
            edges = np.zeros((0, 2), dtype=int)

        ref_nc, ref_lab = _reference_components(n, edges)
        top_nc, top_lab = topology.connected_components((n, edges))

        assert ref_nc == top_nc
        assert np.array_equal(ref_lab, top_lab)


def test_sparse_like_coordinate_object():
    class _Coo:
        def __init__(self, row, col, shape):
            self.row = np.asarray(row)
            self.col = np.asarray(col)
            self.shape = shape

    edges = np.array([[0, 1], [1, 2], [3, 4]])
    coo = _Coo(edges[:, 0], edges[:, 1], (6, 6))
    nc, labels = topology.connected_components(coo)
    assert nc == 3
    assert np.array_equal(labels, [0, 0, 0, 1, 1, 2])

def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('connected-components tests passed')


if __name__ == '__main__':
    main()
