# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""sutura_engine.hull - Outer surface shell extraction for multi-component meshes.

Provides:
  - extract_outer_shell: unions touching/overlapping components and extracts
    the single outer shell, dissolving internal degenerate interfaces.
"""
from typing import Optional, Tuple
import numpy as np


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
    try:
        import trimesh
    except ImportError:
        return verts, tris

    verts = np.asarray(verts, dtype=np.float64)
    tris = np.asarray(tris, dtype=np.int32)

    tm = trimesh.Trimesh(vertices=verts, faces=tris, process=False)
    comps = tm.split(only_watertight=False)
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
    for comp in comps:
        c = comp.centroid
        ext = comp.extents
        scale = (ext + dilation) / np.maximum(ext, 1e-6)
        v_scaled = (comp.vertices - c) * scale + c
        v = np.ascontiguousarray(v_scaled, dtype=np.float32)
        t = np.ascontiguousarray(comp.faces, dtype=np.uint32)
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

    tm_out = trimesh.Trimesh(vertices=v_out, faces=t_out, process=False)
    sub_comps = tm_out.split(only_watertight=False)
    if not sub_comps:
        return v_out, t_out

    largest = max(sub_comps, key=lambda c: float(np.prod(c.extents)))
    return np.asarray(largest.vertices, dtype=np.float64), np.asarray(largest.faces, dtype=np.int32)
