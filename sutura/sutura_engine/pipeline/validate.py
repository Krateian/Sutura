# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Read-only validation and dry-run analysis (P1a split).
Moved verbatim from sutura/repair.py; no logic change."""
import os

import numpy as np

import topology
import triage
from confidence import estimate_confidence_pre_repair
from defects import detect as detect_defects
from sutura_engine import health as method_health
from sutura_engine.pipeline.mesh_io import (
    BRIDGE, check_units, load_meshes, read_3mf_units, scan_bad_coordinates,
)
from sutura_engine.pipeline.presets import (
    classify_with_engine, resolve_mode_params, resolve_si_mode,
)
from sutura_engine.pipeline.utils import (
    boundary_loop_stats, signed_volume, surface_area,
)


def validate_mesh_from_arrays(verts, tris, engine='experimental',
                              declared_unit=None, si_mode=None):
    """Analyze one mesh WITHOUT repairing it.

    Combines defects.detect(), the mesh classifier and cheap pymeshlab
    measures (self-intersecting faces, connected components) into a
    read-only report. Never writes anything. ``declared_unit`` feeds the
    non-blocking unit-warning heuristic (3MF only). ``si_mode`` selects the
    self-intersection policy; under 'off' the count is reported as None
    ('not measured') and is not required by the watertight predicate."""
    import pymeshlab as ml
    si_mode = resolve_si_mode(si_mode)
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    if len(t) == 0 or len(v) == 0:
        raise ValueError('input mesh is empty (no triangles)')
    if not np.isfinite(v).all():
        raise ValueError('input mesh contains NaN or infinite coordinates')

    cls, classifier_engine = classify_with_engine(verts, tris, engine)
    d = detect_defects(verts, tris)
    holes = d['holes']
    nm = d['non_manifold']

    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
    topo = ms.apply_filter('get_topological_measures')
    if si_mode == 'off':
        self_intersections = None
    elif topology.native_si_enabled():
        _, count = topology.self_intersecting_faces(v, t)
        self_intersections = max(int(count), 0)
    else:
        ms.apply_filter('compute_selection_by_self_intersections_per_face')
        self_intersections = int(ms.current_mesh().face_selection_array().sum())

    vol = signed_volume(v, t)
    bridge = bool(os.path.exists(BRIDGE))
    validation = {
        'vertices': int(v.shape[0]),
        'faces': int(t.shape[0]),
        'holes': holes,
        'non_manifold': nm,
        'self_intersections': self_intersections,
        'connected_components': int(topo.get('connected_components_number', 1)),
        'watertight': bool(len(holes) == 0 and len(nm) == 0
                           and self_intersections in (0, None)),
        'signed_volume': round(float(vol), 6),
        'surface_area': round(surface_area(v, t), 3),
        'orientation': 'inverted' if vol < 0 else 'consistent',
    }
    _u = check_units(v, declared_unit)
    if _u:
        validation['unit_warning'] = True
        validation['unit_hint'] = _u['unit_hint']
    est = estimate_confidence_pre_repair({
        'detected_type': cls['type'],
        'detected_confidence': cls['confidence'],
        'stage2_bridge_available': bridge,
        'holes': len(holes),
        'non_manifold': len(nm),
        'self_intersections': self_intersections,
    })
    return {'validation': validation,
            'detected_type': cls['type'],
            'detected_confidence': cls['confidence'],
            'classifier_engine': classifier_engine,
            'stage2_bridge_available': bridge,
            'estimated_confidence': est['score'],
            'estimated_confidence_label': est['label'],
            'estimated_confidence_factors': est['factors']}


def validate_file(src, engine='experimental', si_mode=None):
    """Validate a mesh file without repairing it. Returns the report dict;
    a hard error (missing/malformed input) is a dict with an 'error' key."""
    result = {'input': src}
    try:
        if not os.path.exists(src):
            raise ValueError('file not found: %s' % src)
        bad_coords = scan_bad_coordinates(src)
        if bad_coords:
            raise ValueError('input mesh contains %s' % bad_coords)
        meshes = load_meshes(src)
        if not meshes:
            raise ValueError('no mesh objects found in 3MF')
        units = read_3mf_units(src) if os.path.splitext(src)[1].lower() == '.3mf' else {}
        reports = []
        for name, vs, ts in meshes:
            declared = units.get(name) if name is not None else None
            rep = validate_mesh_from_arrays(vs, ts, engine, declared_unit=declared,
                                            si_mode=si_mode)
            if name is not None:
                rep['model'] = name
            reports.append(rep)
        if len(reports) == 1:
            result.update(reports[0])
        else:
            result['objects'] = len(reports)
            result['object_reports'] = reports
            result['detected_type'] = reports[0].get('detected_type')
            result['detected_confidence'] = reports[0].get('detected_confidence')
    except Exception as e:
        result['error'] = 'validation failed: %s' % e
    return result


def dry_run_mesh_from_arrays(verts, tris, mode='auto', profile=None, engine='experimental',
                             declared_unit=None, triage_spec=None, si_mode=None,
                             flap=False):
    """Report what a repair WOULD do for one mesh, without doing it.

    Detects the type, resolves the mode/thresholds and counts the holes /
    debris / self-intersections. The debris count reuses the same
    meshing_remove_connected_component_by_face_number filter the repair
    chain runs, applied to an in-memory scratch mesh - nothing is written.
    ``declared_unit`` feeds the non-blocking unit-warning heuristic
    (3MF only). ``triage_spec`` is the resolved intensity preset (Balanced
    when None); Stage 1 thresholds are unaffected by it. ``flap`` is reported
    as ``flap_step`` in the plan so --dry-run shows the pre-pass (no work is
    done)."""
    import pymeshlab as ml
    si_mode = resolve_si_mode(si_mode)
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    if len(t) == 0 or len(v) == 0:
        raise ValueError('input mesh is empty (no triangles)')
    if not np.isfinite(v).all():
        raise ValueError('input mesh contains NaN or infinite coordinates')

    cls, classifier_engine = classify_with_engine(verts, tris, engine)
    d = detect_defects(verts, tris)
    holes = d['holes']
    nm = d['non_manifold']
    params, tuning = resolve_mode_params(mode, cls['type'], cls['confidence'],
                                         profile=profile)
    # keep --dry-run's would_apply in sync with the real repair: the mesh-
    # sensitive maxholesize raise must be visible in the plan too.
    params['maxholesize'] = max(params['maxholesize'],
                                2 * boundary_loop_stats(verts, tris)[1])

    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
    topo = ms.apply_filter('get_topological_measures')
    # "found" counts describe the INPUT mesh (like defects.detect does), so
    # self-intersections are measured before the debris-removal step below.
    if si_mode == 'off':
        self_intersections = None
    elif topology.native_si_enabled():
        _, count = topology.self_intersecting_faces(v, t)
        self_intersections = max(int(count), 0)
    else:
        ms.apply_filter('compute_selection_by_self_intersections_per_face')
        self_intersections = int(ms.current_mesh().face_selection_array().sum())
        ms.apply_filter('set_selection_none')
    before_faces = ms.current_mesh().face_number()
    try:
        m = ms.current_mesh()
        v_cur = m.vertex_matrix()
        t_cur = m.face_matrix()
        v_new, t_new = topology.remove_connected_components(
            v_cur, t_cur, min_faces=params['mincomponentsize'], remove_unref=True)
        if len(t_new) == 0:
            ms.apply_filter('meshing_remove_connected_component_by_face_number',
                            mincomponentsize=params['mincomponentsize'], removeunref=True)
        elif len(t_new) < len(t_cur):
            ms.clear()
            ms.add_mesh(ml.Mesh(vertex_matrix=v_new, face_matrix=t_new.astype(np.int32)))
    except Exception:
        ms.apply_filter('meshing_remove_connected_component_by_face_number',
                        mincomponentsize=params['mincomponentsize'], removeunref=True)
    debris_faces = max(before_faces - ms.current_mesh().face_number(), 0)

    est = estimate_confidence_pre_repair({
        'detected_type': cls['type'],
        'detected_confidence': cls['confidence'],
        'tuning_applied': tuning,
        'mode': mode,
        'holes': len(holes),
        'non_manifold': len(nm),
        'self_intersections': self_intersections,
        'stage2_bridge_available': bool(os.path.exists(BRIDGE)),
    })

    result = {
        'repair_mode': mode,
        'repair_profile': profile,
        'detected_type': cls['type'],
        'detected_confidence': cls['confidence'],
        'classifier_engine': classifier_engine,
        'tuning_applied': tuning,
        'would_apply': params,
        'holes_found': len(holes),
        'largest_hole_diameter': round(max((h['diameter'] for h in holes), default=0.0), 4),
        'non_manifold_regions': len(nm),
        'debris_faces_removable': debris_faces,
        'self_intersections': self_intersections,
        'connected_components': int(topo.get('connected_components_number', 1)),
        'stage2_bridge_available': bool(os.path.exists(BRIDGE)),
        'estimated_confidence': est['score'],
        'estimated_confidence_label': est['label'],
        'estimated_confidence_factors': est['factors'],
        'flap_step': bool(flap),
    }
    result.update(triage.triage_report_fields(triage_spec))
    _u = check_units(v, declared_unit)
    if _u:
        result['unit_warning'] = True
        result['unit_hint'] = _u['unit_hint']
    return result


def dry_run_file(src, mode='auto', profile=None, engine='experimental',
                 triage_spec=None, si_mode=None, flap=False):
    """Dry-run one mesh file. Returns the report dict; a hard error is a
    dict with an 'error' key. Never writes any output file."""
    result = {'input': src}
    try:
        if not os.path.exists(src):
            raise ValueError('file not found: %s' % src)
        bad_coords = scan_bad_coordinates(src)
        if bad_coords:
            raise ValueError('input mesh contains %s' % bad_coords)
        meshes = load_meshes(src)
        if not meshes:
            raise ValueError('no mesh objects found in 3MF')
        units = read_3mf_units(src) if os.path.splitext(src)[1].lower() == '.3mf' else {}
        reports = []
        for name, vs, ts in meshes:
            declared = units.get(name) if name is not None else None
            rep = dry_run_mesh_from_arrays(vs, ts, mode, profile, engine,
                                           declared_unit=declared,
                                           triage_spec=triage_spec,
                                           si_mode=si_mode, flap=flap)
            if name is not None:
                rep['model'] = name
            reports.append(rep)
        if len(reports) == 1:
            result.update(reports[0])
        else:
            result['objects'] = len(reports)
            result['object_reports'] = reports
    except Exception as e:
        result['error'] = 'dry run failed: %s' % e
    return result
