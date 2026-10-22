#!/usr/bin/env python3
"""Face-order independence regression for core defect/topology outputs.

The input face order of a triangle mesh is arbitrary.  Two code paths used to
(``defects.detect_non_manifold``) or could (``topology._mesh_topology_numpy``)
let the output order depend on it:

  * ``defects.detect_non_manifold`` returned its region list in the insertion
    order of ``groups.values()`` (which follows face index) -- fixed by sorting
    the regions on their permutation-invariant vertex ids.
  * ``_mesh_topology_numpy`` walks ``boundary_directed.keys()``; the insertion
    order is already canonical (edge-key order), so this test is a regression
    lock, not a behaviour change.

Usage: python3 tests/test_order_independence.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402

import topology  # noqa: E402
import defects  # noqa: E402

_SEEDS = (1, 2, 3, 4, 5)


def _permute(t, seed):
    return t[np.random.default_rng(seed).permutation(len(t))]


def _fan_cluster(offset):
    # three triangles sharing edge (0, 1): that edge has three incident faces,
    # i.e. a non-manifold edge, so all three form one defect region.
    v = np.array([[0, 0, 0], [1, 0, 0], [0.5, 1, 0], [0.5, -1, 0], [0.5, 0, 1]],
                 dtype=np.float64) + offset
    t = np.array([[0, 1, 2], [0, 1, 3], [0, 1, 4]], dtype=np.int64)
    return v, t


def _two_fan_clusters():
    v0, t0 = _fan_cluster(np.array([0.0, 0.0, 0.0]))
    v1, t1 = _fan_cluster(np.array([10.0, 0.0, 0.0]))
    return np.vstack([v0, v1]), np.vstack([t0, t1 + len(v0)])


def _two_triangles():
    v = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0],
                  [10, 0, 0], [11, 0, 0], [10, 1, 0]], dtype=np.float64)
    t = np.array([[0, 1, 2], [3, 4, 5]], dtype=np.int64)
    return v, t


def _open_cube():
    v = np.array([
        (0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0),
        (0, 0, 1), (1, 0, 1), (1, 1, 1), (0, 1, 1)], dtype=np.float32)
    t = np.array([
        (0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7),
        (0, 1, 5), (0, 5, 4), (1, 2, 6), (1, 6, 5),
        (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7)], dtype=np.int64)
    return v, t[:-1]


def _canon_regions(regions):
    # centroid + face count + (permutation-invariant) sorted vertex ids; the
    # input face indices in ``faces_idx`` are intentionally NOT compared.
    return [(tuple(round(float(c), 9) for c in r['centroid']), int(r['faces']),
             tuple(int(i) for i in r.get('verts_idx', ()))) for r in regions]


def test_detect_non_manifold_order_independent():
    v, t = _two_fan_clusters()
    topology.set_engine('python')
    try:
        base = _canon_regions(defects.detect_non_manifold(v, t, with_indices=True))
        assert len(base) == 2, base
        # the two regions are far apart, so their order is observable
        assert base[0][0][0] != base[1][0][0], base
        for seed in _SEEDS:
            got = _canon_regions(
                defects.detect_non_manifold(v, _permute(t, seed), with_indices=True))
            assert got == base, (seed, got, base)
    finally:
        topology.set_engine('auto')


def test_detect_non_manifold_single_region():
    v, t = _fan_cluster(np.array([0.0, 0.0, 0.0]))
    topology.set_engine('python')
    try:
        base = _canon_regions(defects.detect_non_manifold(v, t, with_indices=True))
        assert len(base) == 1, base
        for seed in _SEEDS:
            got = _canon_regions(
                defects.detect_non_manifold(v, _permute(t, seed), with_indices=True))
            assert got == base, (seed, got, base)
    finally:
        topology.set_engine('auto')


def test_mesh_topology_order_independent():
    meshes = [_two_triangles(), _open_cube(), _two_fan_clusters()]
    topology.set_engine('python')
    try:
        for v, t in meshes:
            base = topology._mesh_topology_numpy(v, t)
            for seed in _SEEDS:
                got = topology._mesh_topology_numpy(v, _permute(t, seed))
                assert got == base, (seed, got, base)
    finally:
        topology.set_engine('auto')
    # exercise the default (Rust) path too, when the extension is importable
    if topology._rust_kernel_available():
        for v, t in meshes:
            base = topology._geom.mesh_topology(v.astype(np.float32),
                                                t.astype(np.int64))
            base = dict(base)
            for seed in _SEEDS:
                got = dict(topology._geom.mesh_topology(
                    v.astype(np.float32), _permute(t, seed).astype(np.int64)))
                assert got == base, (seed, got, base)


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('order-independence tests passed')


if __name__ == '__main__':
    main()
