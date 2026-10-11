#!/usr/bin/env python3
"""Regression test for topology.connected_components (disjoint-set union-find).

Verifies full compatibility and parity with
scipy.sparse.csgraph.connected_components on arbitrary graphs, edge cases, and
multiple input shapes.  topology.py is stdlib + numpy only, so this suite runs
without the Rust extension; scipy is used only as an independent oracle and the
parity checks are skipped if it is absent.
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


def test_scipy_sparse_coo_and_csr_formats():
    try:
        import scipy.sparse as sp
    except ImportError:
        print('skip test_scipy_sparse_coo_and_csr_formats (scipy not installed)')
        return
    edges = np.array([[0, 1], [1, 2], [3, 4]])
    coo = sp.coo_matrix((np.ones(len(edges)), (edges[:, 0], edges[:, 1])), shape=(6, 6))
    csr = coo.tocsr()

    nc_coo, lab_coo = topology.connected_components(coo)
    nc_csr, lab_csr = topology.connected_components(csr)
    assert nc_coo == nc_csr == 3
    assert np.array_equal(lab_coo, lab_csr)


def test_randomized_parity_with_scipy():
    try:
        import scipy.sparse as sp
        import scipy.sparse.csgraph as csgraph
    except ImportError:
        print('skip test_randomized_parity_with_scipy (scipy not installed)')
        return

    rng = np.random.default_rng(1337)
    for _ in range(50):
        n = int(rng.integers(1, 200))
        m = int(rng.integers(0, 500))
        if m > 0:
            edges = rng.integers(0, n, size=(m, 2))
        else:
            edges = np.zeros((0, 2), dtype=int)

        adj = sp.coo_matrix((np.ones(len(edges)), (edges[:, 0], edges[:, 1])), shape=(n, n))
        sc_nc, sc_lab = csgraph.connected_components(adj, directed=False)
        top_nc, top_lab = topology.connected_components((n, edges))

        assert sc_nc == top_nc
        assert np.array_equal(sc_lab, top_lab)


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('connected-components tests passed')


if __name__ == '__main__':
    main()
