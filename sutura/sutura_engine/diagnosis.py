# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Sutura engine object analysis, templates, and classification glue.

Provides:
  - ObjectAnalysis dataclass & analyze_mesh / analyze_file
  - Pluggable object templates & profile matching (templates)
  - Classifier engine resolution & execution (mesh_classifier / v2)
  - Content-addressed analysis caching via sutura_engine.chart

Rule: This module must NOT import pymeshlab at module load time.
"""
import dataclasses
from dataclasses import dataclass
from typing import Any, Callable, Dict, List, Optional, Sequence, Tuple

import numpy as np

try:
    import defects
except ImportError:
    from sutura import defects
try:
    from sutura_engine import chart
except ImportError:
    from sutura.sutura_engine import chart
cache = chart

# Probe face count thresholds
SI_COUNT_MAX_FACES = 200000
SI_SAMPLE_FACES = 200000
SI_SAMPLE_SEED = 12345
CLOSING_SIGNAL_MAX_FACES = 200000
REPETITION_MAX_FACES = 500000

# Template tuning thresholds
OPEN_AREA_SCAN = 0.02
OPEN_AREA_RELIEF = 0.08
LARGE_LOOP_RATIO = 0.25
MANY_LOOPS = 4
SI_MODERATE = 20
SI_HEAVY = 200

CLASSIFIER_ENGINES = ('classic', 'experimental')


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
    single_side_score: float = 0.0
    relief_score: float = 0.0
    bbox_diagonal: float = 0.0
    surface_area: float = 0.0
    model: Optional[str] = None


# --- Classifier Glue --------------------------------------------------------

def resolve_classifier_engine(cli_value: Optional[str] = None) -> str:
    """Resolve the requested classifier engine: CLI > env var > 'experimental'."""
    import os
    value = cli_value
    if value is None:
        value = os.environ.get('SUTURA_CLASSIFIER_ENGINE')
    if value not in CLASSIFIER_ENGINES:
        if cli_value is not None:
            raise ValueError(f'unknown classifier engine: {cli_value!r}')
        return 'experimental'
    return value


def classify_with_engine(verts: np.ndarray, tris: np.ndarray,
                         engine: str = 'experimental',
                         extra_features: bool = False) -> Tuple[dict, str]:
    """Run classify_mesh with the selected engine. Returns (result_dict, used_engine_name)."""
    try:
        from mesh_classifier import classify_mesh as classic_classify
    except ImportError:
        from sutura.mesh_classifier import classify_mesh as classic_classify
    if engine != 'experimental':
        return classic_classify(verts, tris), 'classic'
    try:
        try:
            import mesh_classifier_v2 as v2_engine
        except ImportError:
            from sutura import mesh_classifier_v2 as v2_engine
        r = v2_engine.classify_mesh(verts, tris, extra_features=extra_features)
        if not isinstance(r, dict):
            raise ValueError('classifier result is not a dict')
        if r.get('type') not in ('mechanical', 'organic', 'unknown'):
            raise ValueError(f"unexpected classifier type: {r.get('type')!r}")
        conf = r.get('confidence')
        if not isinstance(conf, (int, float)) or not np.isfinite(conf):
            raise ValueError(f'invalid classifier confidence: {conf!r}')
        return r, 'experimental'
    except Exception:
        return classic_classify(verts, tris), 'classic'


# --- Geometry Analysis Helpers ----------------------------------------------

def _surface_area(verts: np.ndarray, tris: np.ndarray) -> float:
    """Total triangle area (pure numpy)."""
    if len(tris) == 0 or len(verts) == 0:
        return 0.0
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    a = v[t[:, 0]]
    b = v[t[:, 1]]
    c = v[t[:, 2]]
    cross = np.cross(b - a, c - a)
    return float(np.sum(0.5 * np.linalg.norm(cross, axis=1)))


def _non_manifold_edge_count(tris: np.ndarray) -> int:
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


def _loop_spanned_area(verts: np.ndarray, loop_idx: Sequence[int]) -> float:
    """Area of the near-planar polygon spanned by a boundary loop (Newell's method)."""
    idx = np.asarray(loop_idx, dtype=np.int64)
    if len(idx) < 3:
        return 0.0
    p = np.asarray(verts, dtype=np.float64)[idx]
    q = np.roll(p, -1, axis=0)
    nrm = np.array([
        np.sum((p[:, 1] - q[:, 1]) * (p[:, 2] + q[:, 2])),
        np.sum((p[:, 2] - q[:, 2]) * (p[:, 0] + q[:, 0])),
        np.sum((p[:, 0] - q[:, 0]) * (p[:, 1] + q[:, 1])),
    ])
    return float(0.5 * np.linalg.norm(nrm))


def _count_self_intersections(v: np.ndarray, t: np.ndarray) -> Tuple[int, bool]:
    """(count, estimated) self-intersecting face count via lazy pymeshlab."""
    import pymeshlab as ml
    faces = len(t)
    try:
        if faces <= SI_COUNT_MAX_FACES:
            ms = ml.MeshSet()
            ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
            ms.apply_filter('compute_selection_by_self_intersections_per_face')
            return int(ms.current_mesh().face_selection_array().sum()), False
        rng = np.random.default_rng(SI_SAMPLE_SEED)
        pick = np.sort(rng.choice(faces, size=min(SI_SAMPLE_FACES, faces), replace=False))
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=np.asarray(t[pick], dtype=np.int32)))
        ms.apply_filter('compute_selection_by_self_intersections_per_face')
        return int(ms.current_mesh().face_selection_array().sum()), True
    except Exception:
        return 0, False


def _component_count(v: np.ndarray, t: np.ndarray) -> int:
    """Connected component count via lazy pymeshlab."""
    import pymeshlab as ml
    try:
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
        topo = ms.apply_filter('get_topological_measures')
        return int(topo.get('connected_components_number', 1))
    except Exception:
        return 1


# --- Pluggable Templates ----------------------------------------------------

def _a(analysis: Any, name: str, default: float = 0.0) -> Any:
    val = getattr(analysis, name, default)
    return default if val is None else val


def _clamp01(value: Any) -> float:
    try:
        v = float(value)
    except (TypeError, ValueError):
        return 0.0
    return max(0.0, min(1.0, v))


class Template:
    """One object profile: an id, a preferred method order and a confidence function."""

    __slots__ = ('id', 'name', 'preferred', '_confidence_fn')

    def __init__(self, id: str, name: str, preferred: Sequence[int],
                 confidence_fn: Callable[[Any], float]):
        self.id = id
        self.name = name
        self.preferred = tuple(int(x) for x in preferred)
        self._confidence_fn = confidence_fn

    def confidence(self, analysis: Any) -> float:
        """Confidence (0..1) that this profile fits the object."""
        try:
            return _clamp01(self._confidence_fn(analysis))
        except Exception:
            return 0.0


# Backward compatibility alias
ObjectProfile = Template


def _mechanical_conf(a: Any) -> float:
    if _a(a, 'mesh_type') == 'mechanical':
        return _clamp01(_a(a, 'type_confidence'))
    return 0.0


def _organic_conf(a: Any) -> float:
    if _a(a, 'mesh_type') == 'organic':
        return _clamp01(_a(a, 'type_confidence'))
    return 0.0


def _single_side_scan_conf(a: Any) -> float:
    signal = _clamp01(_a(a, 'single_side_score'))
    open_ratio = _a(a, 'open_area_ratio')
    loops = _a(a, 'boundary_loops')
    largest = _a(a, 'largest_loop_ratio')
    legacy = 0.0
    if open_ratio >= OPEN_AREA_SCAN and loops <= MANY_LOOPS:
        legacy = min(1.0, open_ratio / OPEN_AREA_RELIEF)
        if largest >= LARGE_LOOP_RATIO:
            legacy = min(1.0, legacy + 0.2)
    return max(legacy, signal)


def _relief_conf(a: Any) -> float:
    signal = _clamp01(_a(a, 'relief_score'))
    open_ratio = _a(a, 'open_area_ratio')
    loops = _a(a, 'boundary_loops')
    legacy = 0.0
    if open_ratio >= OPEN_AREA_SCAN and loops <= 2:
        legacy = min(1.0, open_ratio / OPEN_AREA_RELIEF)
    return max(legacy, signal)


def _repeated_pattern_conf(a: Any) -> float:
    return _clamp01(_a(a, 'repetition_score'))


def _dense_scan_heavy_si_conf(a: Any) -> float:
    si = _a(a, 'self_intersections')
    if si < SI_HEAVY:
        return 0.0
    return min(1.0, si / (4.0 * SI_HEAVY))


_BUILTIN_TEMPLATES: List[Template] = [
    Template('mechanical', 'Mechanical part', (1, 3, 4, 5, 2), _mechanical_conf),
    Template('organic', 'Organic surface', (1, 2, 3, 13, 7, 5), _organic_conf),
    Template('single_side_scan', 'Single-sided scan', (8, 13, 7, 3, 2), _single_side_scan_conf),
    Template('relief', 'Relief / shell', (9, 10, 3, 8), _relief_conf),
    Template('repeated_pattern', 'Repeated pattern', (11, 12, 1), _repeated_pattern_conf),
    Template('dense_scan_heavy_si', 'Dense scan / heavy self-intersections', (5, 6, 13, 7, 3), _dense_scan_heavy_si_conf),
]

# Registered template registry (pluggable)
_CUSTOM_TEMPLATES: List[Template] = []


def register_template(template: Template) -> None:
    """Register a new or override an existing object template."""
    for i, t in enumerate(_CUSTOM_TEMPLATES):
        if t.id == template.id:
            _CUSTOM_TEMPLATES[i] = template
            return
    _CUSTOM_TEMPLATES.append(template)


def all_templates() -> Tuple[Template, ...]:
    """Return all active templates (custom templates override built-in ones with same id)."""
    custom_ids = {t.id for t in _CUSTOM_TEMPLATES}
    res = [t for t in _BUILTIN_TEMPLATES if t.id not in custom_ids]
    res.extend(_CUSTOM_TEMPLATES)
    return tuple(res)


TEMPLATES = property(lambda: all_templates()) if False else all_templates()
SCORE_TEMPLATES = TEMPLATES


def by_id(template_id: str) -> Optional[Template]:
    """Retrieve template by id."""
    for t in all_templates():
        if t.id == template_id:
            return t
    return None


# --- Analysis Entry Points --------------------------------------------------

def analyze_mesh(verts: np.ndarray, tris: np.ndarray, engine: str = 'experimental',
                 extra_features: bool = False, model: Optional[str] = None,
                 count_self_intersections: bool = True,
                 use_cache: bool = True) -> ObjectAnalysis:
    """Analyze one mesh as numpy arrays and return an ObjectAnalysis summary."""
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    analysis = ObjectAnalysis(faces=int(len(t)), vertices=int(len(v)), model=model)
    if len(t) == 0 or len(v) == 0:
        return analysis

    # Cache lookup
    version = '0.8.1'
    try:
        from repair import VERSION
        version = VERSION
    except Exception:
        try:
            from sutura_engine import VERSION
            version = VERSION
        except Exception:
            pass

    m_hash = None
    if use_cache and cache.is_cache_enabled():
        m_hash = cache.hash_arrays(v, t)
        cached_dict = cache.get_cached_analysis(m_hash, version)
        if cached_dict is not None:
            # Rehydrate from cached dict
            for k, val in cached_dict.items():
                if hasattr(analysis, k):
                    setattr(analysis, k, val)
            analysis.model = model
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

    if len(t) <= CLOSING_SIGNAL_MAX_FACES:
        try:
            from sutura_engine.methods import closing as _closing
            analysis.single_side_score = round(float(_closing.single_side_score(v, t)[0]), 4)
            analysis.relief_score = round(float(_closing.relief_score(v, t)[0]), 4)
        except Exception:
            try:
                import closing as _closing
                analysis.single_side_score = round(float(_closing.single_side_score(v, t)[0]), 4)
                analysis.relief_score = round(float(_closing.relief_score(v, t)[0]), 4)
            except Exception:
                pass

    try:
        cls, _ = classify_with_engine(v, t, engine, extra_features=extra_features)
        analysis.mesh_type = cls.get('type', 'unknown')
        analysis.type_confidence = round(float(cls.get('confidence', 0.0)), 3)
    except Exception:
        pass

    if len(t) <= REPETITION_MAX_FACES:
        try:
            from sutura_engine.methods import repeat as _rr
            analysis.repetition_score = round(float(_rr.detect_repetition(v, t)[0]), 4)
        except Exception:
            try:
                import repeat_repair as _rr
                analysis.repetition_score = round(float(_rr.detect_repetition(v, t)[0]), 4)
            except Exception:
                pass

    if use_cache and m_hash and cache.is_cache_enabled():
        cache.put_cached_analysis(m_hash, version, to_dict(analysis))

    return analysis


def analyze_file(path: str, engine: str = 'experimental',
                 extra_features: bool = False) -> List[ObjectAnalysis]:
    """Analyze every object in a file (one for STL/OBJ, one per 3MF object)."""
    from sutura_engine.core import load_meshes
    meshes = load_meshes(path)
    return [analyze_mesh(v, t, engine=engine, extra_features=extra_features, model=name)
            for name, v, t in meshes]


def combine(objs: Sequence[ObjectAnalysis]) -> ObjectAnalysis:
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
    c.self_intersections_estimated = any(o.self_intersections_estimated for o in objs)
    c.type_confidence = min(o.type_confidence for o in objs)
    c.repetition_score = max(o.repetition_score for o in objs)
    c.single_side_score = max(o.single_side_score for o in objs)
    c.relief_score = max(o.relief_score for o in objs)
    c.bbox_diagonal = max(o.bbox_diagonal for o in objs)
    c.surface_area = sum(o.surface_area for o in objs)
    return c


def to_dict(analysis: ObjectAnalysis) -> dict:
    """JSON-safe dictionary representation of an ObjectAnalysis."""
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
        'single_side_score': round(float(analysis.single_side_score), 4),
        'relief_score': round(float(analysis.relief_score), 4),
        'bbox_diagonal': round(float(analysis.bbox_diagonal), 4),
        'surface_area': round(float(analysis.surface_area), 3),
        'model': analysis.model,
    }
