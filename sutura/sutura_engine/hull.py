# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""sutura_engine.hull - Outer surface shell extraction for multi-component meshes.

Provides:
  - extract_outer_shell: unions touching/overlapping components and extracts
    the single outer shell, dissolving internal degenerate interfaces.
"""
from typing import Optional, Tuple
import numpy as np


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


def _area_weighted_centroid(verts, tris):
    """Area-weighted average of the triangle centres (trimesh's ``centroid``)."""
    tri = verts[tris]
    cross = np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0])
    area = 0.5 * np.linalg.norm(cross, axis=1)
    centres = tri.mean(axis=1)
    total = float(area.sum())
    if total <= 0.0:
        return verts.mean(axis=0)
    return (centres * area[:, None]).sum(axis=0) / total


def extract_outer_shell(verts: np.ndarray, tris: np.ndarray, dilation: Optional[float] = None) -> Tuple[np.ndarray, np.ndarray]:
    """Extract the outer surface shell of a multi-component mesh, removing internal geometry.

    For assemblies of touching or overlapping watertight parts (e.g. Rubik's cube cubies),
    slightly dilates each component around its centroid so touching interfaces dissolve,
    performs boolean union via manifold3d, and retains the single largest outer component.

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

    comps = _split_face_components(verts, tris)
    if len(comps) <= 1:
        return verts, tris

    diag = float(np.linalg.norm(np.ptp(verts, axis=0)))
    if dilation is None:
        dilation = 5e-4 * (diag if diag > 1e-6 else 100.0)

    try:
        import manifold3d
    except ImportError:
        return verts, tris

    mf_list = []
    for comp_v, comp_t in comps:
        c = _area_weighted_centroid(comp_v, comp_t)
        ext = np.ptp(comp_v, axis=0)
        scale = (ext + dilation) / np.maximum(ext, 1e-6)
        v_scaled = (comp_v - c) * scale + c
        v = np.ascontiguousarray(v_scaled, dtype=np.float32)
        t = np.ascontiguousarray(comp_t, dtype=np.uint32)
        try:
            mf = manifold3d.Manifold(mesh=manifold3d.Mesh(vert_properties=v, tri_verts=t))
            if mf.status() == manifold3d.Error.NoError and not mf.is_empty():
                mf_list.append(mf)
        except Exception:
            continue

    if not mf_list:
        return verts, tris

    united = mf_list[0]
    for mf in mf_list[1:]:
        united = united + mf

    out_m = united.to_mesh()
    v_out = np.asarray(out_m.vert_properties, dtype=np.float64)
    t_out = np.asarray(out_m.tri_verts, dtype=np.int64)

    sub_comps = _split_face_components(v_out, t_out)
    if not sub_comps:
        return v_out, t_out.astype(np.int32)

    largest_v, largest_t = max(
        sub_comps, key=lambda c: float(np.prod(np.ptp(c[0], axis=0))))
    return (np.asarray(largest_v, dtype=np.float64),
            np.asarray(largest_t, dtype=np.int32))
