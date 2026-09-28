# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Adapter for PyMeshLab / VCG Library (Visual Computing Lab - ISTI - CNR).

Wraps the Stage 1 PyMeshLab filter chain:
  - Non-manifold repair
  - Hole closing
  - Debris and degenerate triangle removal
  - Coherent orientation
  - Topological measurements

Notice: PyMeshLab is dual-licensed under GPL. This adapter isolates PyMeshLab
calls and ensures pymeshlab is only imported lazily at execution time.
"""
from typing import Any, Dict, Optional, Tuple
import numpy as np


def is_pymeshlab_available() -> bool:
    """Return True if pymeshlab can be imported in the current Python environment."""
    try:
        import pymeshlab  # noqa: F401
        return True
    except (ImportError, Exception):
        return False


def get_meshset() -> Any:
    """Create a new pymeshlab.MeshSet instance lazily."""
    import pymeshlab as ml
    return ml.MeshSet()


def mesh_from_arrays(verts: np.ndarray, tris: np.ndarray) -> Any:
    """Create a pymeshlab.Mesh from numpy vertex and triangle matrices."""
    import pymeshlab as ml
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    return ml.Mesh(vertex_matrix=v, face_matrix=t)


def apply_stage1_filters(verts: np.ndarray, tris: np.ndarray,
                         params: Dict[str, Any]) -> Tuple[np.ndarray, np.ndarray, dict]:
    """Execute the Stage 1 VCG filter chain on in-memory mesh arrays."""
    import pymeshlab as ml

    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))

    before = ms.apply_filter('get_topological_measures')

    maxholesize = params.get('maxholesize', 300)
    mincomponentsize = params.get('mincomponentsize', 8)

    filters = [
        ('meshing_remove_duplicate_faces', {}),
        ('meshing_repair_non_manifold_edges', {}),
        ('meshing_repair_non_manifold_vertices', {}),
        ('meshing_re_orient_faces_coherently', {}),
        ('meshing_close_holes', {'maxholesize': maxholesize}),
        ('meshing_remove_connected_component_by_face_number', {'mincomponentsize': mincomponentsize}),
        ('meshing_remove_duplicate_faces', {}),
        ('meshing_remove_unreferenced_vertices', {}),
    ]

    for name, fparams in filters:
        try:
            ms.apply_filter(name, **fparams)
        except Exception:
            pass

    after = ms.apply_filter('get_topological_measures')
    m = ms.current_mesh()
    out_v = np.asarray(m.vertex_matrix(), dtype=np.float32)
    out_t = np.asarray(m.face_matrix(), dtype=np.int32)

    stats = {
        'holes_before': int(before.get('boundary_edges_number', 0)),
        'holes_remaining': int(after.get('boundary_edges_number', 0)),
        'two_manifold': bool(after.get('is_mesh_two_manifold', False)),
        'components': int(after.get('connected_components_number', 1)),
        'faces_before': len(t),
        'faces_after': len(out_t),
    }
    return out_v, out_t, stats
