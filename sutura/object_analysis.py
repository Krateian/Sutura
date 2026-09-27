"""Per-object geometry analysis for the method-recommendation engine (P0).

stdlib + numpy only at import time (``defects`` has the same rule); pymeshlab
and ``repair.py`` are imported lazily inside the functions that need them, so
this module stays importable anywhere. The goal is a cheap (<~1 s for a
typical object) summary of an object's shape and damage that drives
``templates`` and ``methods.rank_methods``.

Self-intersection counting is the one potentially expensive probe: above
``SI_COUNT_MAX_FACES`` faces it is measured on a random ``SI_SAMPLE_FACES``
subset and flagged with ``self_intersections_estimated=True`` instead of
scanning the whole mesh (the caller can choose to ignore an estimate).
"""
import dataclasses
from dataclasses import dataclass
from typing import Optional

import numpy as np

import defects

# Self-intersection probe cap. Above this face count only a random sample is
# measured (see the module docstring); 200k keeps the probe under ~1 s.
SI_COUNT_MAX_FACES = 200000
SI_SAMPLE_FACES = 200000
SI_SAMPLE_SEED = 12345


@dataclass
class ObjectAnalysis:
    """A cheap per-object shape/damage summary."""
    faces: int = 0
    vertices: int = 0
    components: int = 1
    boundary_loops: int = 0
    largest_loop_len: int = 0
    largest_loop_ratio: float = 0.0
    open_area_ratio: float = 0.0
    non_manifold_edges: int = 0
    self_intersections: int = 0
    self_intersections_estimated: bool = False
    mesh_type: str = 'unknown'
    type_confidence: float = 0.0
    repetition_score: float = 0.0
    bbox_diagonal: float = 0.0
    surface_area: float = 0.0
    model: Optional[str] = None


def _repair_mod():
    """The loaded repair module, without re-executing it as ``__main__``."""
    import sys
    for name in ('repair', '__main__'):
        mod = sys.modules.get(name)
        if mod is not None and hasattr(mod, 'classify_with_engine'):
            return mod
    import repair as _repair
    return _repair


def _surface_area(verts, tris):
    """Total triangle area (pure numpy; mirrors repair.surface_area)."""
    if len(tris) == 0 or len(verts) == 0:
        return 0.0
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    a = v[t[:, 0]]
    b = v[t[:, 1]]
    c = v[t[:, 2]]
    cross = np.cross(b - a, c - a)
    return float(np.sum(0.5 * np.linalg.norm(cross, axis=1)))


def _non_manifold_edge_count(tris):
    """Number of edges shared by more than two faces (pure numpy)."""
    t = np.asarray(tris, dtype=np.int64)
    if t.size == 0:
        return 0
    v = int(t.max()) + 1
    f0, f1, f2 = t[:, 0], t[:, 1], t[:, 2]
    keys = np.concatenate([
        np.minimum(f0, f1) * v + np.maximum(f0, f1),
        np.minimum(f1, f2) * v + np.maximum(f1, f2),
        np.minimum(f0, f2) * v + np.maximum(f0, f2),
    ])
    _uniq, counts = np.unique(keys, return_counts=True)
    return int((counts > 2).sum())


def _loop_spanned_area(verts, loop_idx):
    """Area of the (near-planar) polygon spanned by a boundary loop (Newell)."""
    idx = np.asarray(loop_idx, dtype=np.int64)
    if len(idx) < 3:
        return 0.0
    p = np.asarray(verts, dtype=np.float64)[idx]
    q = np.roll(p, -1, axis=0)
    # Newell's method: N = sum (p_i - q_i) x-ish terms; |N| = 2 * area.
    nrm = np.array([
        np.sum((p[:, 1] - q[:, 1]) * (p[:, 2] + q[:, 2])),
        np.sum((p[:, 2] - q[:, 2]) * (p[:, 0] + q[:, 0])),
        np.sum((p[:, 0] - q[:, 0]) * (p[:, 1] + q[:, 1])),
    ])
    return float(0.5 * np.linalg.norm(nrm))


def _count_self_intersections(v, t):
    """(count, estimated) self-intersecting face count via pymeshlab.

    Full scan below the cap; a random face subset above it (estimated=True).
    Returns (0, False) when the probe cannot run.
    """
    import pymeshlab as ml
    faces = len(t)
    try:
        if faces <= SI_COUNT_MAX_FACES:
            ms = ml.MeshSet()
            ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
            ms.apply_filter('compute_selection_by_self_intersections_per_face')
            return int(ms.current_mesh().face_selection_array().sum()), False
        rng = np.random.default_rng(SI_SAMPLE_SEED)
        pick = np.sort(rng.choice(faces, size=min(SI_SAMPLE_FACES, faces),
                                  replace=False))
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=v,
                            face_matrix=np.asarray(t[pick], dtype=np.int32)))
        ms.apply_filter('compute_selection_by_self_intersections_per_face')
        return int(ms.current_mesh().face_selection_array().sum()), True
    except Exception:  # noqa: BLE001 - analysis is best-effort, never fatal
        return 0, False


def _component_count(v, t):
    import pymeshlab as ml
    try:
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
        topo = ms.apply_filter('get_topological_measures')
        return int(topo.get('connected_components_number', 1))
    except Exception:  # noqa: BLE001
        return 1


def analyze_mesh(verts, tris, engine='experimental', extra_features=False,
                 model=None, count_self_intersections=True):
    """Analyze one mesh as numpy arrays and return an :class:`ObjectAnalysis`."""
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    analysis = ObjectAnalysis(faces=int(len(t)), vertices=int(len(v)),
                              model=model)
    if len(t) == 0 or len(v) == 0:
        return analysis

    diag = float(np.linalg.norm(v.max(axis=0) - v.min(axis=0)))
    analysis.bbox_diagonal = round(diag, 6)
    analysis.surface_area = round(_surface_area(v, t), 3)
    analysis.non_manifold_edges = _non_manifold_edge_count(t)

    holes = defects.detect_holes(v, t, with_indices=True)
    analysis.boundary_loops = len(holes)
    if holes:
        analysis.largest_loop_len = int(max(len(h['verts_idx']) for h in holes))
        max_diam = float(max(h['diameter'] for h in holes))
        if diag > 0:
            analysis.largest_loop_ratio = round(max_diam / diag, 4)
        spanned = sum(_loop_spanned_area(v, h['verts_idx']) for h in holes)
        if analysis.surface_area > 0:
            analysis.open_area_ratio = round(spanned / analysis.surface_area, 4)

    analysis.components = _component_count(v, t)
    if count_self_intersections:
        sec, estimated = _count_self_intersections(v, t)
        analysis.self_intersections = sec
        analysis.self_intersections_estimated = bool(estimated)

    try:
        cls, _engine = _repair_mod().classify_with_engine(
            v, t, engine, extra_features=extra_features)
        analysis.mesh_type = cls.get('type', 'unknown')
        analysis.type_confidence = round(float(cls.get('confidence', 0.0)), 3)
    except Exception:  # noqa: BLE001
        pass

    # repetition_score is a P5 stub: no repetition detector yet.
    analysis.repetition_score = 0.0
    return analysis


def analyze_file(path, engine='experimental', extra_features=False):
    """Analyze every object of a file: one entry for STL/OBJ, one per 3MF object."""
    meshes = _repair_mod().load_meshes(path)
    return [analyze_mesh(v, t, engine=engine, extra_features=extra_features,
                         model=name) for name, v, t in meshes]


def combine(objs):
    """Worst-case merge of several object analyses (for ranking a whole file)."""
    if not objs:
        return ObjectAnalysis()
    if len(objs) == 1:
        return objs[0]
    c = dataclasses.replace(objs[0])
    c.model = None
    c.faces = max(o.faces for o in objs)
    c.vertices = max(o.vertices for o in objs)
    c.components = max(o.components for o in objs)
    c.boundary_loops = sum(o.boundary_loops for o in objs)
    c.largest_loop_len = max(o.largest_loop_len for o in objs)
    c.largest_loop_ratio = max(o.largest_loop_ratio for o in objs)
    c.open_area_ratio = max(o.open_area_ratio for o in objs)
    c.non_manifold_edges = max(o.non_manifold_edges for o in objs)
    c.self_intersections = max(o.self_intersections for o in objs)
    c.self_intersections_estimated = any(
        o.self_intersections_estimated for o in objs)
    c.type_confidence = min(o.type_confidence for o in objs)
    c.repetition_score = max(o.repetition_score for o in objs)
    c.bbox_diagonal = max(o.bbox_diagonal for o in objs)
    c.surface_area = sum(o.surface_area for o in objs)
    return c


def to_dict(analysis):
    """JSON-safe dict of an analysis (floats rounded for readability)."""
    return {
        'faces': int(analysis.faces),
        'vertices': int(analysis.vertices),
        'components': int(analysis.components),
        'boundary_loops': int(analysis.boundary_loops),
        'largest_loop_len': int(analysis.largest_loop_len),
        'largest_loop_ratio': round(float(analysis.largest_loop_ratio), 4),
        'open_area_ratio': round(float(analysis.open_area_ratio), 4),
        'non_manifold_edges': int(analysis.non_manifold_edges),
        'self_intersections': int(analysis.self_intersections),
        'self_intersections_estimated': bool(analysis.self_intersections_estimated),
        'mesh_type': analysis.mesh_type,
        'type_confidence': round(float(analysis.type_confidence), 3),
        'repetition_score': round(float(analysis.repetition_score), 3),
        'bbox_diagonal': round(float(analysis.bbox_diagonal), 4),
        'surface_area': round(float(analysis.surface_area), 3),
        'model': analysis.model,
    }
