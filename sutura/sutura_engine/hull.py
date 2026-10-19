# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""sutura_engine.hull - Outer surface shell extraction for multi-component meshes.

Provides:
  - extract_outer_shell: unions touching/overlapping components and extracts
    the single outer shell, dissolving internal degenerate interfaces.
"""
from typing import Optional, Tuple

import numpy as np
import sutura_geom


def _split_face_components(verts, tris):
    """Split a triangle soup into edge-connected face components.

    Pure-numpy replacement for ``trimesh.Trimesh.split`` (whose graph engine
    prefers ``scipy.sparse.csgraph``).  Faces are joined when they share an
    edge used by exactly two faces.  Returns a list of ``(vertices, faces)``
    with unreferenced vertices dropped, matching trimesh's reindexed submesh.
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int64)
    n = len(tris)
    if n == 0:
        return []
    face_of_edge = np.tile(np.arange(n, dtype=np.int64), 3)
    edges = np.sort(np.concatenate(
        [tris[:, [0, 1]], tris[:, [1, 2]], tris[:, [2, 0]]], axis=0), axis=1)
    order = np.lexsort((edges[:, 1], edges[:, 0]))
    edges = edges[order]
    face_of_edge = face_of_edge[order]
    starts = np.concatenate(
        [[0], np.nonzero(np.any(edges[1:] != edges[:-1], axis=1))[0] + 1])
    sizes = np.diff(np.append(starts, len(edges)))
    pair = starts[sizes == 2]
    a = face_of_edge[pair]
    b = face_of_edge[pair + 1]

    parent = np.arange(n, dtype=np.int64)
    while len(a):
        ra = parent[a]
        rb = parent[b]
        low = np.minimum(ra, rb)
        new_parent = parent.copy()
        np.minimum.at(new_parent, ra, low)
        np.minimum.at(new_parent, rb, low)
        new_parent = new_parent[new_parent]
        if np.array_equal(new_parent, parent):
            break
        parent = new_parent
    roots = parent.copy()
    while True:
        nxt = roots[roots]
        if np.array_equal(nxt, roots):
            break
        roots = nxt
    _, labels = np.unique(roots, return_inverse=True)
    n_comp = int(labels.max()) + 1

    order = np.argsort(labels, kind='stable')
    bounds = np.searchsorted(labels[order], np.arange(n_comp + 1))
    out = []
    for label in range(n_comp):
        faces = order[bounds[label]:bounds[label + 1]]
        used = np.unique(tris[faces])
        remap = np.full(len(verts), -1, dtype=np.int64)
        remap[used] = np.arange(len(used))
        out.append((verts[used], remap[tris[faces]]))
    return out


def extract_outer_shell(verts: np.ndarray, tris: np.ndarray, dilation: Optional[float] = None) -> Tuple[np.ndarray, np.ndarray]:
    """Extract the outer surface shell of a multi-component mesh, removing internal geometry.

    For assemblies of touching or overlapping watertight parts (e.g. Rubik's cube cubies),
    slightly dilates each component around its centroid so touching interfaces dissolve,
    performs the boolean union in the Rust core (`sutura_geom.outer_shell_union`) and
    retains the single largest outer component.

    Parameters:
        verts: (N, 3) float64 array of vertex coordinates.
        tris: (M, 3) int array of triangle indices.
        dilation: float or None. Absolute dilation in mm. When None, dynamically
            computes dilation relative to the bounding box diagonal:
            `dilation = 5e-4 * bbox_diagonal` (~0.05 mm for a 100 mm object).

    Returns:
        (shell_verts, shell_tris)
    """
    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int32)
    out_v, out_t = sutura_geom.outer_shell_union(verts, tris, dilation)
    return (np.asarray(out_v, dtype=np.float64),
            np.asarray(out_t, dtype=np.int32))
