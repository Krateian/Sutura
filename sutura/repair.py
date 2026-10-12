#!/usr/bin/env python3
"""Sutura Triage Engine: a staged PyMeshLab + manifold3d mesh-repair pipeline with 16 ranked repair methods.

Stage 1 (PyMeshLab/VCG): clean up, orient, close holes, drop debris.
Stage 2 (manifold3d): rebuild the closed mesh as a watertight solid and
merge overlapping shells.

Multi-object 3MF files are repaired per object and written back, so no
object is lost. Meshes are handled in memory as numpy arrays to preserve
the original vertex structure. Output is a new file; the input is never
overwritten.
"""
import sys
import os
import re
import json
import math
import struct
import tempfile
import shutil
import zipfile
import subprocess
import importlib.util
import time
import hashlib
from collections import defaultdict, OrderedDict
import numpy as np

from classification import classify, issue_label
from confidence import (estimate_confidence_pre_repair, repair_confidence)
from defects import detect as detect_defects
from mesh_classifier import classify_mesh
import repair_score
import history
import local_exact
import topology
import triage
import methods as method_registry
from sutura_engine import health as method_health
from sutura_engine.pipeline.utils import (
    surface_area, boundary_loop_stats, signed_volume, _bbox_diagonal,
    _edge_uses, _faces_preserved, _referenced_only, _canonical_face_keys,
    _count_vertex_components, _internal_wall_count, _exact_si_face_count,
    _count_si, _hausdorff_rel,
)
from sutura_engine.pipeline.presets import (
   CAD_LIKENESS_THRESHOLD, CLASSIFIER_ENGINES, DECIMATE_HD_MARGIN,
   DEEP_ESTIMATE_FTETWILD_A, DEEP_ESTIMATE_FTETWILD_B,
   DEEP_ESTIMATE_LOCAL_BASE, DEEP_ESTIMATE_LOCAL_PER_KFACE,
   DEEP_ESTIMATE_LOCAL_PER_REGION, DEEP_REPAIR_CONFIG, DEEP_REPAIR_DEFAULT,
   DEEP_REPAIR_ENV, DEEP_REPAIR_MODES, DENSE_MIN_FACES, DENSE_QUALITY,
   DENSE_RATIO, DENSE_TARGET_LADDER, DRESSING_DEFAULT_ENABLED,
   DRESSING_DEFECT_SETS, DRESSING_MAX_NORMAL_ANGLE_P95,
   DRESSING_MAX_PARTS_SLACK, DRESSING_MAX_VOLUME_DELTA,
   ExtremeRemovedAllError, FLAP_DEFAULT_ENABLED, FLAP_ENV,
   FTETWILD_ABORT_MESSAGE, FTETWILD_ABORT_MESSAGE_TR, FTETWILD_MAX_FACES,
   FTETWILD_MAX_HAUSDORFF_REL, FTETWILD_OPTIMIZE_ENV,
   FTETWILD_RETRY_PARAMS, FTETWILD_TIMEOUT, GRAFT_MAX_FACES, GRAFT_TIMEOUT,
   HAUSDORFF_SAMPLES, LOCAL_MAX_LOOP_LEN, LOCAL_MAX_NM_EDGES,
   LOCAL_MAX_REGIONS, LOCAL_MAX_REMOVED_FACES, LOCAL_MAX_SECONDS,
   LOCAL_REMESH_FAIR_STEPS, LOCAL_REMESH_MAX_HOLE, LOCAL_REMESH_REFINE,
   MECH_TUNE_GATE, MODE_PARAMS, ORG_TUNE_GATE, PROFILES, REPAIR_MODES,
   REPAIR_PROFILES, SI_MODES, SI_MODE_DEFAULT, SI_MODE_ENV, SUTURA_DIR,
   TAMPONADE_DEFAULT_ENABLED, TAMPONADE_ENV, TOPOMETRICS, VENV311,
   _BALANCED_INTENSITY, _TYPE_PARAMS, _dressing_default_enabled,
   _flap_default_enabled, _tamponade_default_enabled, cad_likeness,
   classify_with_engine, estimate_deep_repair_time, ftetwild_params,
   resolve_classifier_engine, resolve_deep_repair,
   resolve_deep_repair_flags, resolve_dressing, resolve_dressing_defects,
   resolve_dressing_drain, resolve_dressing_rmax_scale,
   resolve_dressing_scale, resolve_dressing_sigma_scale, resolve_flap,
   resolve_ftetwild, resolve_ftetwild_optimize, resolve_graft,
   resolve_mode_params, resolve_si_mode, resolve_tamponade,
   tuning_applied_for
)
from sutura_engine.pipeline.mesh_io import (
    write_obj, check_units, read_3mf_units, read_obj, stl_write_binary,
    should_extract_outer_shell, scan_bad_coordinates, _scan_bin_stl,
    obj_has_material_refs, parse_3mf_meshes, build_mesh_block,
    _3MF_MAX_ENTRY_BYTES, _read_zip_entry, load_meshes, save_mesh,
    _resolve_bridge, _resolve_ftetwild_bridge, _resolve_indirect_bridge,
    BRIDGE, FTETWILD_BRIDGE, INDIRECT_BRIDGE,
)
from sutura_engine.pipeline.stage1 import (
   _refine_hole_enabled, _close_holes_step, stage1_cleanup_chain,
   stage1_chain, delete_fallback_chain, apply_chain,
   _remove_unref_vertices_ms, _invert_faces_ms, _re_orient_faces_ms,
   _repair_nme_ms, extreme_extra_passes, SI_EXCISE_MAX_ROUNDS,
   SI_EXCISE_MAX_FACE_FRACTION, SI_EXCISE_REFINE_EDGE_PCT,
   SI_EXCISE_REFINE_MAX_FACES, SI_EXCISE_DEFAULT_BUDGET,
   SI_EXCISE_BUDGET_BY_INTENSITY, si_excise_enabled, _si_excise_budget,
   si_excise_recap, LOCAL_EXACT_DEFAULT_ENABLED,
   LOCAL_EXACT_BUDGET_BY_INTENSITY, LOCAL_EXACT_DEFAULT_BUDGET,
   local_exact_enabled, _local_exact_budget, _should_run_local_exact,
   _si_count_and_mask, _si_count, join_small_components
)
from sutura_engine.pipeline.stage2 import (
    run_stage2, maybe_run_stage2, _split_pinched_vertices, PINCH_SPLIT_MAX_PASSES,
)
from sutura_engine.pipeline.p_weld import (
    weld_reload_equivalent, _repair_welded_topology, reload_strict_holes_nm,
    _separate_weld_collisions, _close_small_holes, p_weld_final,
    enforce_reload_verdict, P_WELD_MAX_NUDGE_ATTEMPTS, P_WELD_MAX_HOLE,
    P_WELD_MIN_FACE_FRACTION,
)
from sutura_engine.pipeline.tiers import ftetwild as _ftetwild_mod
from sutura_engine.pipeline.tiers.ftetwild import (
    ftetwild_available, run_ftetwild, _ftetwild_manifold_postprocess,
    _ftetwild_candidate, _decimate_boundary,
    _ftetwild_decimate_and_postprocess, _ftetwild_boundary,
    _ftetwild_attempt, _ftetwild_tier,
)
from sutura_engine.pipeline.tiers.local_remesh import (
    _damaged_region, _umbrella_fair, _local_remesh_tier, _local_si_rise,
)
from sutura_engine.pipeline.tiers.flap import (
    FLAP_SKIP_KEYS, _flap_adopt, _flap_max_orig_move, _reload_component_count,
    _flap_final_verdict, _flap_final_needs_off, _flap_off_preferred,
    FLAP_TIMEOUT_BY_INTENSITY, FLAP_DEFAULT_BUDGET, _FLAP_CACHE,
    _FLAP_CACHE_MAX, _flap_cache_key, _flap_step, _flap_step_impl,
    FLAP_DEBRIS_VOLUME_EPS, FLAP_DEBRIS_VOLUME_REL, FLAP_DEBRIS_FLATNESS,
    drop_flap_debris,
)
from sutura_engine.pipeline.tiers.closing import closing_ladder
from sutura_engine.pipeline.tiers.tamponade import (
    TAMP_COLLAPSE_BBOX_FRAC, TAMP_LEAK_TRIGGER_FRAC, TAMP_LEAK_MAX_FACES,
    _tamponade_shape_leak, _tamponade_final_needs_off,
    _tamponade_off_preferred, _dump_tamp_mesh, tamponade_tier,
)


VERSION = "0.9.1"


def repair_mesh_from_arrays(verts, tris, tmpdir, mode='auto', profile=None,
                            engine='experimental', declared_unit=None,
                            join_components=False, autorefine=False,
                            ftetwild=False, indirect_autorefine=False,
                            extra_features=False, deep_repair=None,
                            triage_spec=None, engines=None, engine_chain=None,
                            closing=None, proxy_template=False, repeat=None,
                            repeat_source=None, repeat_target=None,
                             wall_thicken=False, wall_min_thickness=None,
                             graft=False, flap=False, dressing=False,
                             dressing_drain=None, dressing_defects=None,
                             dressing_rmax_scale=None, dressing_sigma_scale=None,
                             tamponade=False,
                             si_mode=None, dressing_force_adopt=False):
    """Repair one mesh given as numpy arrays. Returns (report, verts, tris).

    ``mode`` is one of REPAIR_MODES: 'auto' (the default) uses the mesh
    classifier + confidence gate exactly as before; the fixed modes
    (low/medium/aggressive/extreme) use the MODE_PARAMS thresholds directly.
    ``profile`` (optional, only effective when mode is 'auto') selects a
    named threshold preset (see PROFILES). ``engine`` selects the classifier
    engine ('experimental' default; 'classic' opt-in). ``declared_unit`` is
    the 3MF <model unit="..."> attribute (None for STL/OBJ) fed to the
    non-blocking unit-warning heuristic. ``join_components`` enables the
    experimental join-small-components prototype (flag-gated; NOT the default).
    ``autorefine`` enables the experimental self-intersection subdivision
    prototype (Lazard & Valque 2025; flag-gated; NEVER deletes input faces).
    ``ftetwild`` selects the fTetWild fallback tier, which tetrahedralizes the
    input and extracts a watertight boundary: ``'auto'`` (the CLI/GUI
    default) runs it only when fTetWild is installed and stage 1 still leaves
    holes or non-manifold edges; ``True`` (``--experimental-fallback-ftetwild``)
    also runs it on a closed result that still self-intersects and reports an
    explicit skip when fTetWild is missing; ``False`` disables it (library
    default, ``--no-fallback-ftetwild``).
    ``indirect_autorefine`` enables the experimental exact indirect-predicate
    arrangement-lite split (rust/sutura-geom; flag-gated; same adopt/fallback
    guard as ``autorefine``).
    ``extra_features`` enables the opt-in edge-tiebreak classifier head.
    ``deep_repair`` selects the deep-repair ladder mode ('off'/'local'/
    'full', see ``deep_repair_ladder``); None (library default) keeps the
    pre-ladder behaviour and adds no ``deep_repair`` report.
    ``si_mode`` selects the self-intersection policy ('repair'/'report'/'off',
    see ``resolve_si_mode``); None resolves to the default 'report', which
    measures/reports SI without escalating or failing on SI alone.
    ``triage_spec`` is the resolved intensity preset (sutura/triage.py) that
    supplies the post-Stage-1 knobs (deep-repair tier timeout/size cap,
    decimation ladder, Hausdorff sample count); None resolves to Balanced
    (the pre-triage behaviour). It never changes Stage 1.
    ``engines``/``engine_chain`` are the configs and resolved chain from
    ``load_engine_run``; None (library default) runs no external engine and
    keeps the output byte-identical to the pre-engines behaviour.
    ``closing`` selects the standalone scan-closing tier (registry methods 8/9):
    ``'poisson'`` (screened Poisson reconstruction, ``sutura/closing.py``) or
    ``'flat_back'`` (flat back plane + side walls); ``proxy_template`` selects
    the proxy-template rebuild (registry method 10, ``sutura/proxy_repair.py``).
    Both run on the ORIGINAL input arrays (like the fTetWild tier) and the
    candidate is adopted only when it is strict-watertight (no holes, no
    non-manifold edges) and no worse than the stage-1 baseline; None/False
    (library default) keeps the output byte-identical to today.
    ``flap`` enables the surface-based hole filler (``sutura_engine.flap``) as
    the FIRST Stage-1 step: a pre-pass on the input arrays as they stand before
    the VCG chain (normally the original surface, though outer-shell extraction
    may already have replaced them), so the deep-repair tiers that work from
    those arrays also see its patches. It is adopted only when the reload-honest
    damage (holes + non-manifold edges) does not rise; the chain's own
    ``meshing_close_holes`` remains the fallback for any loop it skips or
    rejects. That gate is topological-only and pre-chain, so ``repair_file``
    adds a final safety net that re-runs with Flap OFF when the final result is
    not strict-watertight or has more parts than the input; False (library
    default) keeps the output byte-identical.
    ``tamponade`` enables the interior-plug tier (method #17,
    ``sutura_engine.tamponade``): it runs as a PRE-PASS on the cleaned input
    before the Flap pre-pass and before Stage-1's hole closing, so a genuine
    crater is sealed with the reference-surface plug rather than the generic
    Stage-1 cap, and a second-chance call runs after the Stage-1/Flap closing
    ladder and before Graft/fTetWild when the pre-pass did not adopt. It plugs
    each open boundary from the inside without deleting an input face, and is
    adopted only when the reload-honest holes + non-manifold count strictly
    falls without raising the component count or losing more than 1% of the
    pre-tier faces; a watertight adoption then skips the deep tiers. False
    (library default) keeps the output byte-identical.
    """
    import pymeshlab as ml
    method_health.reset_if_unscoped()
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)

    if len(t) == 0 or len(v) == 0:
        raise ValueError('input mesh is empty (no triangles)')
    if not np.isfinite(v).all():
        raise ValueError('input mesh contains NaN or infinite coordinates')

    stats = {'stage1': {}}
    stats['timings'] = {}
    _t_start = time.perf_counter()
    if triage_spec is None:
        triage_spec = triage.resolve_intensity(None)
    stats.update(triage.triage_report_fields(triage_spec))
    si_mode = resolve_si_mode(si_mode)
    stats['si_mode'] = si_mode

    # before_stage1 engines run on the ORIGINAL input, before anything else
    # (including the unit heuristic and the classifier, which then see the
    # engine output). Adoption uses the same holes/non-manifold guard as the
    # fTetWild tier; a shape-changing output is flagged, not rejected.
    if engines:
        _entries = []
        if engine_placement_order(engines, engine_chain, 'before_stage1'):
            _base_ms = ml.MeshSet()
            _base_ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
            _base_topo = _base_ms.apply_filter('get_topological_measures')
            _cv, _ct, _adopted = run_engines(
                ml, engines, engine_chain, 'before_stage1', tmpdir, v, t,
                boundary_loop_stats(v, t)[0],
                _base_topo.get('non_two_manifold_edges', 0), triage_spec, _entries)
            if _adopted:
                v = np.asarray(_cv, dtype=np.float32)
                t = np.asarray(_ct, dtype=np.int32)
                verts, tris = v, t
        _record_engine_entries(stats, _entries)

    # Non-blocking unit warning: the size-based Stage 1 thresholds assume
    # millimetres, so a mesh that was probably authored in inches/cm deserves
    # a visible warning (reported, never changes the repair).
    _u = check_units(v, declared_unit)
    if _u:
        stats['unit_warning'] = True
        stats['unit_hint'] = _u['unit_hint']

    # Mesh-type-aware Stage 1 tuning (organic vs mechanical): resolved through
    # the shared resolve_mode_params so repair and --dry-run stay in sync.
    _cls, stats['classifier_engine'] = classify_with_engine(verts, tris, engine, extra_features=extra_features)
    stats['detected_type'] = _cls['type']
    stats['detected_confidence'] = _cls['confidence']
    stats['repair_mode'] = mode
    if profile is not None:
        stats['repair_profile'] = profile
    _p, stats['tuning_applied'] = resolve_mode_params(
        mode, _cls['type'], _cls['confidence'], profile=profile)
    # Mesh-sensitive maxholesize: a fixed value (1000) skips any boundary
    # loop longer than that (VCG counts each hole edge twice), so large
    # scan holes stay open. Raise to cover the input's largest loop, never
    # below the mode/type base. Measured on the 75-model corpus: closes
    # large loops without ever degrading a mesh (7 cases improved, 0 worse).
    _p['maxholesize'] = max(_p['maxholesize'],
                            2 * boundary_loop_stats(v, t)[1])

    # Outer-shell extraction pre-step (dense_scan_heavy_si / mechanical templates):
    # For inputs with multiple overlapping/nested closed components (e.g. Rubik-style
    # assemblies), dissolves internal interfaces via slight dilation
    # (dilation = 5e-4 * bbox_diagonal, ~0.05 mm for 100 mm object) and boolean union.
    if mode == 'auto' and should_extract_outer_shell(verts, tris, mesh_type=_cls['type']):
        _shell_adopted = False
        _shell_phase = None
        _shell_reason = None
        _sinfo = None
        if tamponade:
            _t_shell_start = time.time()
            _t_analyze_s = 0.0
            _t_topo_s = 0.0
            _shell_analyze = None
            with method_health.tier_guard('shell_analyze', 'import') as _gi:
                import sutura_geom
                _shell_analyze = getattr(sutura_geom, 'shell_analyze', None)
            if _gi.error is not None:
                _shell_phase = 'error'
                _shell_reason = str(_gi.error)
            if _shell_analyze is None:
                # Stale extension without the shell-analyze binding: skip the
                # prepass cleanly (report the API gap, not a caught
                # AttributeError) and fall through to extract_outer_shell.
                if _shell_phase is None:
                    _shell_phase = 'api_unavailable'
                    _shell_reason = 'shell_analyze_unavailable'
                    method_health.record_missing_symbol(
                        'shell_analyze', 'import', 'shell_analyze')
            else:
                with method_health.tier_guard('shell_analyze', 'tier') as _gs:
                    import defects as _defects
                    _t0 = time.time()
                    _cand_v, _cand_t, _sinfo = _shell_analyze(
                        np.asarray(verts, dtype=np.float64),
                        np.asarray(tris, dtype=np.int32),
                    )
                    _t_analyze_s = time.time() - _t0
                    _t0 = time.time()
                    _in_holes = boundary_loop_stats(verts, tris)[0]
                    _in_nm = len(_defects.detect(verts, tris).get('non_manifold', []))
                    _in_parts = _count_vertex_components(len(verts), tris)
                    _t_topo_s = time.time() - _t0

                    # Phase P-a: DISJOINT parts (60916-type):
                    # parts are strictly disjoint: no cross-part proper SI, volume overlap < 0.1%,
                    # no buried faces, no coincident opposite walls, no internal walls.
                    _is_p_a = _sinfo.get('is_disjoint', False)

                    if _is_p_a:
                        _shell_adopted = True
                        _shell_phase = 'P-a_disjoint'
                        _shell_reason = 'disjoint_parts_no_overlap'

                    # Phase P-b: exactly coincident opposite walls -> dissolve both (C2)
                    elif (_sinfo['coincident_opp_count'] > 0
                            and not _sinfo['has_internal_walls']
                            and _sinfo['internal_walls_count'] == 0
                            and _sinfo['unresolved_coincident_count'] == 0
                            and _sinfo['reason'] == 'ok'):
                        _cand_holes = boundary_loop_stats(_cand_v, _cand_t)[0]
                        _cand_nm = len(_defects.detect(_cand_v, _cand_t).get('non_manifold', []))
                        _cand_parts = _count_vertex_components(len(_cand_v), _cand_t)
                        if _cand_holes <= _in_holes and _cand_nm <= _in_nm and _cand_parts <= _in_parts:
                            _shell_adopted = True
                            _shell_phase = 'P-b_coincident'
                            _shell_reason = 'coincident_opposite_dissolved'
                            verts = np.asarray(_cand_v, dtype=np.float32)
                            tris = np.asarray(_cand_t, dtype=np.int32)
                            v, t = verts, tris
                        else:
                            _shell_phase = 'P-b_coincident'
                            _shell_reason = 'rejected_holes_or_parts_rose'

                    # Phase P-c: penetrating bodies (100045-type) -> exact clip + stitch (C1)
                    elif not _sinfo['has_internal_walls'] and _sinfo['reason'] == 'ok':
                        _cand_holes = boundary_loop_stats(_cand_v, _cand_t)[0]
                        _cand_nm = len(_defects.detect(_cand_v, _cand_t).get('non_manifold', []))
                        _cand_parts = _count_vertex_components(len(_cand_v), _cand_t)
                        if _cand_holes <= _in_holes and _cand_nm <= _in_nm and _cand_parts <= _in_parts:
                            _shell_adopted = True
                            _shell_phase = 'P-c_penetrating'
                            _shell_reason = 'penetrating_bodies_clipped_and_stitched'
                            verts = np.asarray(_cand_v, dtype=np.float32)
                            tris = np.asarray(_cand_t, dtype=np.int32)
                            v, t = verts, tris
                        else:
                            _shell_phase = 'P-c_penetrating'
                            _shell_reason = 'rejected_holes_or_parts_rose'
                    else:
                        _shell_phase = 'P-c_penetrating' if _sinfo['reason'] == 'unresolved_coplanar_overlapping' else 'fallback'
                        _shell_reason = _sinfo['reason']
                if _gs.error is not None:
                    _shell_adopted = False
                    _shell_phase = 'error'
                    _shell_reason = str(_gs.error)

            _t_shell_total = time.time() - _t_shell_start
            stats['tamponade_shell_prepass'] = {
                'shell_adopted': _shell_adopted,
                'shell_phase': _shell_phase,
                'shell_reason': _shell_reason,
                'time_s': round(_t_shell_total, 4),
                'shell_analyze_s': round(_t_analyze_s, 4),
                'topo_stats_s': round(_t_topo_s, 4),
                'outer_count': (_sinfo or {}).get('outer_count', 0),
                'buried_count': (_sinfo or {}).get('buried_count', 0),
                'cavity_count': (_sinfo or {}).get('cavity_count', 0),
                'coincident_opp_count': (_sinfo or {}).get('coincident_opp_count', 0),
                'coincident_same_count': (_sinfo or {}).get('coincident_same_count', 0),
                'internal_walls_count': (_sinfo or {}).get('internal_walls_count', 0),
                'unresolved_coincident_count': (_sinfo or {}).get('unresolved_coincident_count', 0),
                'has_internal_walls': (_sinfo or {}).get('has_internal_walls', False),
                'is_disjoint': (_sinfo or {}).get('is_disjoint', False),
            }
            stats['shell_adopted'] = _shell_adopted
            stats['shell_phase'] = _shell_phase
            stats['shell_reason'] = _shell_reason

        # Any phase that is not adopted -> today's path unchanged
        if not _shell_adopted:
            with method_health.tier_guard('outer_shell', 'tier') as _go:
                import repeat_repair as _rr
                _shell_v, _shell_t = _rr.extract_outer_shell(verts, tris)
                if len(_shell_t) > 0:
                    verts = np.asarray(_shell_v, dtype=np.float32)
                    tris = np.asarray(_shell_t, dtype=np.int32)
                    v, t = verts, tris
                    stats['outer_shell_extracted'] = True
            # on error the guard records it and the input is kept unchanged

    before_ms = ml.MeshSet()
    before_ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
    before = before_ms.apply_filter('get_topological_measures')
    if topology.native_measures_enabled():
        before_geom = topology.mesh_geometric_measures(v, t)
    else:
        before_geom = before_ms.apply_filter('get_geometric_measures')
    before_volume = before_geom.get('mesh_volume', 0)
    before_area = surface_area(v, t)
    before_verts = before.get('vertices_number', 0)
    before_faces = before.get('faces_number', 0)
    components_before = before.get('connected_components_number', 0)
    holes_before = boundary_loop_stats(v, t)[0]
    nm_before = before.get('non_two_manifold_edges', 0)

    # Tamponade prepass (lead decision, T4 step 2): when the tier is ON, seal
    # holes on the CLEANED input BEFORE the Flap pre-pass and before Stage 1's
    # generic meshing_close_holes, so a genuine crater is closed with the
    # reference-surface plug rather than the low-quality Stage-1 cap. Stage 1
    # then only re-closes the loops Tamponade skipped or rejected. Runs on the
    # same hygiene chain (no hole closing, no component removal) the chain
    # itself starts with, so the surface matches. With the tier OFF this block
    # is a no-op and the pipeline is byte-identical. The post-chain
    # tamponade_tier call below is skipped once this prepass has sealed the
    # mesh, so its stats record is preserved.
    if tamponade is True:
        _t_pre = time.perf_counter()
        _tms = ml.MeshSet()
        _tms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
        apply_chain(_tms, stage1_cleanup_chain(ml))
        _tafter = _tms.apply_filter('get_topological_measures')
        _tms, _tafter = tamponade_tier(
            ml, _tms, _tafter, stats, tmpdir, tamponade=True,
            intensity=(getattr(triage_spec, 'base', None)
                       or getattr(triage_spec, 'name', None)),
            spec=triage_spec, si_mode=si_mode, prepass=True)
        if (stats.get('tamponade') or {}).get('adopted'):
            v = np.asarray(_tms.current_mesh().vertex_matrix(), dtype=np.float32)
            t = np.asarray(_tms.current_mesh().face_matrix(), dtype=np.int32)
            verts, tris = v, t
        stats['timings']['prepass'] = round(time.perf_counter() - _t_pre, 3)

    # Flap: surface-based hole fill (on by default in the CLI). It is the FIRST Stage-1
    # step, a pre-pass on the input as it stands here (normally the original
    # surface; note the outer-shell extraction above may already have replaced
    # v,t for mode='auto', so this is not unconditionally the raw input). The
    # chain's own meshing_close_holes remains the fallback for any loop Flap
    # skips or rejects, and a candidate is adopted only when the reload-honest
    # damage (holes + non-manifold edges) does not rise. Running here -- rather
    # than post-chain -- is what lets the deep-repair Graft tier, which also
    # works from these arrays, see the flap patches: measured on ornate-frame,
    # the standalone Flap->Graft output (Orca 2 parts, 3494 SI faces, volume
    # 58762) is reproduced exactly, whereas a post-chain flap cannot reach Graft
    # and left the output byte-identical to flap-OFF. This gate is pre-chain and
    # topological-only, so repair_file adds a final safety net (re-run with Flap
    # OFF when the final result is not strict-watertight or has more parts than
    # the input). The reported 'before' metrics above are still the input's.
    if flap:
        _fms = ml.MeshSet()
        _fms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
        _fafter = _fms.apply_filter('get_topological_measures')
        _fms, _fafter = _flap_step(ml, _fms, _fafter, stats, None)
        if (stats.get('flap') or {}).get('adopted'):
            v = np.asarray(_fms.current_mesh().vertex_matrix(), dtype=np.float32)
            t = np.asarray(_fms.current_mesh().face_matrix(), dtype=np.int32)
            verts, tris = v, t

    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))

    applied, skipped = apply_chain(
        ms, stage1_chain(ml, **_p, join_components=join_components))
    after = ms.apply_filter('get_topological_measures')

    # --experimental-autorefine prototype: resolve self-intersections on the
    # INPUT mesh by subdividing the intersecting triangles along their
    # intersection segments (Lazard & Valque 2025), NEVER deleting input faces.
    # When enabled, the SAME chain is re-run on the autorefine-preprocessed
    # input and the better final result is adopted (adopt/fallback guard:
    # autorefine is only used when its final output has holes AND non-manifold
    # regions no worse than the baseline chain — its float64 construction can
    # leave non-manifold edges on dense-SI scans, see
    # docs/alpha-wrap-feasibility-2026-09.md section 4a). Flag-gated, NOT the
    # default.
    stats['experimental_autorefine'] = False
    if autorefine and len(t) > 0:
        ar_v = np.asarray(v, dtype=np.float64)
        ar_t = np.asarray(t, dtype=np.int64)
        ar_rep = {'skipped': False}
        with method_health.tier_guard('autorefine', 'tier') as _ga:
            # Imported here, inside the guard: autorefine uses
            # sutura_geom.orient3d exact predicates, and any unexpected error
            # must be caught and reported, never crashing the repair.
            import autorefine as _autorefine
            ar_v, ar_t, ar_info = _autorefine.autorefine(ar_v, ar_t)
            ar_rep.update(ar_info)
            if len(ar_t) > 0:
                ms_cand = ml.MeshSet()
                ms_cand.add_mesh(ml.Mesh(vertex_matrix=np.asarray(ar_v, np.float32),
                                         face_matrix=np.asarray(ar_t, np.int32)))
                apply_chain(ms_cand, stage1_chain(ml, **_p, join_components=join_components))
                cand_after = ms_cand.apply_filter('get_topological_measures')
                cand_holes = boundary_loop_stats(
                    ms_cand.current_mesh().vertex_matrix(),
                    ms_cand.current_mesh().face_matrix())[0]
                cand_nm = cand_after.get('non_two_manifold_edges', 0)
                base_holes = boundary_loop_stats(
                    ms.current_mesh().vertex_matrix(),
                    ms.current_mesh().face_matrix())[0]
                base_nm = after.get('non_two_manifold_edges', 0)
                adopted = bool(cand_holes <= base_holes and cand_nm <= base_nm)
                ar_rep['adopted'] = adopted
                if adopted:
                    ms = ms_cand
                    after = cand_after
        if _ga.error is not None:
            ar_rep['error'] = str(_ga.error)
        stats['experimental_autorefine'] = ar_rep

    # --experimental-indirect-autorefine (Phase B): exact arrangement-lite
    # self-intersection split via the rust/sutura-geom extension (B4).  The
    # INPUT mesh is split along its proper-intersection segments with exact
    # indirect predicates (broad phase + exact classifier + per-triangle 2D
    # CDT + exact-rational welding), then the SAME stage-1 chain is re-run on
    # the split output.  Adopt/fallback guard identical to autorefine: the
    # indirect result is adopted only when its final holes AND non-manifold
    # edge counts are no worse than the baseline chain's.  Flag-gated, NOT the
    # default; reports an explicit skip when sutura_geom is unavailable.
    stats['experimental_indirect_autorefine'] = False
    if indirect_autorefine and len(t) > 0:
        ia_rep = {'skipped': False}
        with method_health.tier_guard('indirect_autorefine', 'import') as _gii:
            import indirect_bridge
        if _gii.error is not None:
            ia_rep['skipped'] = True
            ia_rep['error'] = 'indirect bridge unavailable: %s' % _gii.error
        else:
            with method_health.tier_guard('indirect_autorefine', 'tier') as _gi:
                ia_v, ia_t, al_info = indirect_bridge.arrangement_lite_arrays(v, t)
                ia_rep.update(al_info)
                if len(ia_t) > 0:
                    ms_cand = ml.MeshSet()
                    ms_cand.add_mesh(ml.Mesh(vertex_matrix=np.asarray(ia_v, np.float32),
                                             face_matrix=np.asarray(ia_t, np.int32)))
                    apply_chain(ms_cand, stage1_chain(ml, **_p, join_components=join_components))
                    cand_after = ms_cand.apply_filter('get_topological_measures')
                    cand_holes = boundary_loop_stats(
                        ms_cand.current_mesh().vertex_matrix(),
                        ms_cand.current_mesh().face_matrix())[0]
                    cand_nm = cand_after.get('non_two_manifold_edges', 0)
                    base_holes = boundary_loop_stats(
                        ms.current_mesh().vertex_matrix(),
                        ms.current_mesh().face_matrix())[0]
                    base_nm = after.get('non_two_manifold_edges', 0)
                    adopted = bool(cand_holes <= base_holes and cand_nm <= base_nm)
                    ia_rep['adopted'] = adopted
                    if adopted:
                        ms = ms_cand
                        after = cand_after
            if _gi.error is not None:
                if isinstance(_gi.error, ImportError):
                    ia_rep['skipped'] = True
                    ia_rep['error'] = ('indirect autorefine skipped: sutura_geom '
                                       'extension unavailable (%s)' % _gi.error)
                else:
                    ia_rep['error'] = str(_gi.error)
        stats['experimental_indirect_autorefine'] = ia_rep

    if after.get('non_two_manifold_edges', 0) > 0 or after.get('non_two_manifold_vertices', 0) > 0:
        fb = ml.MeshSet()
        fb.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
        fapplied, fskipped = apply_chain(
            fb, delete_fallback_chain(ml, **_p, join_components=join_components))
        fafter = fb.apply_filter('get_topological_measures')
        if (fafter.get('non_two_manifold_edges', 0) + fafter.get('non_two_manifold_vertices', 0)
                < after.get('non_two_manifold_edges', 0) + after.get('non_two_manifold_vertices', 0)):
            ms, after, applied, skipped = fb, fafter, fapplied, fskipped

    # --experimental-join-components prototype: instead of deleting small
    # components (the mincomponentsize filter was dropped from the chain
    # above), move them onto the nearest larger component and re-close.
    stats['experimental_join_components'] = False
    if join_components:
        moved, remaining = join_small_components(
            ms, ml, _p['mincomponentsize'], _p['maxholesize'])
        stats['experimental_join_components'] = {'moved': moved,
                                                 'remaining_small': remaining}
        after = ms.apply_filter('get_topological_measures')

    if after.get('faces_number', 0) == 0:
        if mode == 'extreme':
            raise ExtremeRemovedAllError(
                'Extreme mode removed all geometry (small connected component '
                'below the size threshold); try a less aggressive mode (e.g. Auto)')
        raise ValueError('all faces are degenerate; nothing to repair')

    # Guarded SI excise + refined re-cap (Full Mend path). Runs after the
    # Stage-1 chain (+ delete-fallback + join), before the deep-repair ladder.
    # Gated to deep_repair == 'full' (Quick is 'off'; the library default None
    # is untouched) and skipped for mode == 'extreme', which keeps its legacy
    _shell_adopted_for_si = (stats.get('tamponade_shell_prepass') or {}).get('shell_adopted', False)
    if deep_repair == 'full' and mode != 'extreme' and si_excise_enabled() and not _shell_adopted_for_si:
        ms, _si_rec = si_excise_recap(
            ms, ml, maxholesize=_p['maxholesize'],
            time_budget=_si_excise_budget(triage_spec))
        # Only report when a round was actually accepted (self-intersections
        # strictly reduced, holes/nm not worse); a mesh the excision cannot
        # improve -- including any 0-SI mesh -- keeps the pre-existing report
        # and output byte-identical.
        if _si_rec.get('applied'):
            stats['si_excise'] = _si_rec
            after = ms.apply_filter('get_topological_measures')

    # Extreme-only extra passes (Stage C): self-intersection cleanup + one more
    # run of the main chain to close the holes / drop the debris the removal
    # exposed. ONLY for mode == 'extreme'; every other mode is untouched.
    # meshing_isotropic_explicit_remeshing (full remesh) is deliberately out of
    # scope: it can unpredictably change topology.
    stats['extreme_passes_applied'] = False
    if mode == 'extreme':
        applied_extra, si_found, si_removed, applied2, skipped2 = \
            extreme_extra_passes(ms, ml, _p)
        stats['extreme_passes_applied'] = applied_extra
        applied += applied2
        if skipped2:
            skipped.update(skipped2)
        if si_found:
            stats['self_intersections_found'] = si_found
            stats['self_intersections_removed'] = si_removed
        after = ms.apply_filter('get_topological_measures')
        if after.get('faces_number', 0) == 0:
            raise ExtremeRemovedAllError(
                'Extreme mode removed all geometry (small connected component '
                'below the size threshold); try a less aggressive mode (e.g. Auto)')

    # fTetWild fallback tier (FAZ17): guaranteed-correct last-resort
    # solidifier. Tetrahedralize the ORIGINAL input surface and extract its
    # boundary as a watertight, SI-free triangle mesh (fTetWild, MPL-2.0, via
    # the pytetwild bridge in venv311). Adopt only when the extracted surface
    # is no worse on the same holes+non-manifold metric used by the autorefine
    # guard.
    # - 'auto' (default when installed): only when stage 1 still leaves holes
    #   or non-manifold edges. Measured on the 40 real-world samples: strict
    #   watertight 31 -> 39, and a mesh stage 1 already closed is never
    #   remeshed (its residual self-intersections are left alone).
    # - True (--experimental-fallback-ftetwild): also when the closed result
    #   still self-intersects (slow: up to FTETWILD_TIMEOUT per mesh).
    # after_stage1 engines run on the current stage-1 result, before the deep
    # ladder; replace_ftetwild engines replace the built-in fTetWild tier
    # inside the ladder.
    if engines:
        _entries = []
        if engine_placement_order(engines, engine_chain, 'after_stage1'):
            _cur_v = np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float32)
            _cur_t = np.asarray(ms.current_mesh().face_matrix(), dtype=np.int32)
            _cv, _ct, _adopted = run_engines(
                ml, engines, engine_chain, 'after_stage1', tmpdir, _cur_v, _cur_t,
                boundary_loop_stats(_cur_v, _cur_t)[0],
                after.get('non_two_manifold_edges', 0), triage_spec, _entries)
            if _adopted:
                ms = ml.MeshSet()
                ms.add_mesh(ml.Mesh(vertex_matrix=_cv, face_matrix=_ct))
                after = ms.apply_filter('get_topological_measures')
        _record_engine_entries(stats, _entries)

    # Registry methods 8/9/10 (P-INT): standalone closing / proxy-template
    # modules. They run on the ORIGINAL input arrays, before the deep-repair
    # ladder, and adopt only a strict-watertight, no-worse candidate (see
    # ``closing_ladder``).
    stats['timings']['stage1'] = round(time.perf_counter() - _t_start, 3)
    _t_tiers = time.perf_counter()
    ms, after = closing_ladder(ml, ms, after, stats, v, t, tmpdir,
                               closing=closing, proxy_template=proxy_template)

    # Registry method 17 (Tamponade): interior hole plug + SI melt. When the
    # tier is ON it normally runs as a pre-pass (before Flap / Stage 1, see
    # above) so its reference plug wins over the generic cap; this post-chain
    # call is then skipped. It is kept for the OFF path and as a second chance
    # when the prepass ran but did not adopt (e.g. no reference found there).
    # Adopts only when the reload-honest holes+non-manifold strictly falls
    # without raising parts or losing healthy faces.
    if not (stats.get('tamponade') or {}).get('adopted'):
        ms, after = tamponade_tier(ml, ms, after, stats, tmpdir,
                                   tamponade=(tamponade is True),
                                   intensity=(getattr(triage_spec, 'base', None)
                                              or getattr(triage_spec, 'name', None)),
                                   spec=triage_spec, si_mode=si_mode)
    # A watertight Tamponade adoption already closed the mesh: skip the deep
    # tiers (the same "a watertight candidate supersedes" logic Graft adoption
    # uses), but still record the ladder's own report.
    _tamp_watertight = bool(
        (stats.get('tamponade') or {}).get('adopted')
        and (stats.get('tamponade') or {}).get('reload_watertight')
        and (stats.get('tamponade') or {}).get('candidate_parts', 1) <= 1)

    # Registry method 13 (Graft / shell wrap): the morphology last-resort tier,
    # run on the ORIGINAL input arrays before the fTetWild ladder. Adopts only a
    # reload-watertight candidate; records the fidelity verdict + warnings under
    # ``stats['graft']``.
    ms, after = graft_tier(ml, ms, after, stats, v, t, tmpdir,
                           graft=(graft is True) and not _tamp_watertight,
                           intensity=(getattr(triage_spec, 'base', None)
                                      or getattr(triage_spec, 'name', None)),
                           spec=triage_spec)

    ms, after = deep_repair_ladder(ml, ms, after, stats, v, t, tmpdir,
                                   mode=deep_repair,
                                   ftetwild=(ftetwild if not _tamp_watertight
                                             else False),
                                   spec=triage_spec, engines=engines,
                                   engine_chain=engine_chain,
                                   graft=(graft if not _tamp_watertight
                                          else False),
                                   dressing=(dressing if not _tamp_watertight
                                             else False),
                                   dressing_drain=dressing_drain,
                                   dressing_defects=dressing_defects,
                                   dressing_rmax_scale=dressing_rmax_scale,
                                   dressing_sigma_scale=dressing_sigma_scale,
                                   si_mode=si_mode,
                                   dressing_force_adopt=dressing_force_adopt)

    # Registry methods 11/12 (P-REP): repeated-element transplant, after the
    # whole stage-1 chain (the transplant needs a watertight M).
    ms, after = repeat_tier(ml, ms, after, stats, mode=repeat,
                            source_point=repeat_source,
                            target_point=repeat_target)

    # Registry method 15 (P-WALL): opt-in thin-wall thicken-to-min. Runs on the
    # ORIGINAL input arrays (like the closing tier) and adopts only a
    # strict-watertight, no-worse candidate; never automatic.
    if wall_thicken:
        ms, after = wall_thicken_tier(ml, ms, after, stats, v, t,
                                      min_thickness=wall_min_thickness)
    stats['timings']['deep_repair'] = round(time.perf_counter() - _t_tiers, 3)

    # A closed result with no non-manifold edge can still have pinched
    # ("bowtie") vertices, e.g. when the final close_holes fan-fills a hole at
    # a vertex shared by two boundary loops; stage 2 then never runs and a
    # strictly watertight mesh is reported as a warning (thingi10k_145065).
    ms, after, stats['pinched_vertices_split'] = _split_pinched_vertices(ml, ms, after)

    # final_fallback engines run once, on the ORIGINAL input, after every
    # built-in tier (including replace_ftetwild) and after the pinch split.
    if engines:
        _entries = []
        if engine_placement_order(engines, engine_chain, 'final_fallback'):
            _cur_v = np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float32)
            _cur_t = np.asarray(ms.current_mesh().face_matrix(), dtype=np.int32)
            _cv, _ct, _adopted = run_engines(
                ml, engines, engine_chain, 'final_fallback', tmpdir, v, t,
                boundary_loop_stats(_cur_v, _cur_t)[0],
                after.get('non_two_manifold_edges', 0), triage_spec, _entries)
            if _adopted:
                ms = ml.MeshSet()
                ms.add_mesh(ml.Mesh(vertex_matrix=_cv, face_matrix=_ct))
                after = ms.apply_filter('get_topological_measures')
        _record_engine_entries(stats, _entries)

    holes_after = boundary_loop_stats(ms.current_mesh().vertex_matrix(),
                                      ms.current_mesh().face_matrix())[0]
    nm_after = after.get('non_two_manifold_edges', 0)
    _deep_repair_offer(stats, holes_after, nm_after, len(t), spec=triage_spec)

    stats['stage1']['holes_closed'] = max(holes_before - holes_after, 0)
    stats['stage1']['holes_remaining'] = holes_after
    stats['stage1']['non_manifold_edges_fixed'] = max(nm_before - nm_after, 0)
    stats['stage1']['non_manifold_edges_remaining'] = nm_after
    stats['stage1']['two_manifold'] = bool(after.get('is_mesh_two_manifold'))
    stats['stage1']['components'] = after.get('connected_components_number')
    stats['stage1']['faces_after'] = after.get('faces_number')
    stats['stage1']['faces_before'] = before.get('faces_number')
    stats['stage1']['faces_removed'] = max(before.get('faces_number', 0) - after.get('faces_number', 0), 0)
    stats['stage1']['applied_filters'] = applied
    if skipped:
        stats['stage1']['skipped'] = skipped

    if topology.native_measures_enabled():
        m_cur = ms.current_mesh()
        cur_v = np.asarray(m_cur.vertex_matrix(), dtype=np.float32)
        cur_t = np.asarray(m_cur.face_matrix(), dtype=np.int32)
        geom = topology.mesh_geometric_measures(cur_v, cur_t)
        if geom.get('mesh_volume', 0) < 0:
            _invert_faces_ms(ms, ml)
            m_cur = ms.current_mesh()
            cur_v = np.asarray(m_cur.vertex_matrix(), dtype=np.float32)
            cur_t = np.asarray(m_cur.face_matrix(), dtype=np.int32)
            geom = topology.mesh_geometric_measures(cur_v, cur_t)
    else:
        geom = ms.apply_filter('get_geometric_measures')
        if geom.get('mesh_volume', 0) < 0:
            _invert_faces_ms(ms, ml)
            geom = ms.apply_filter('get_geometric_measures')
            if geom.get('mesh_volume', 0) < 0:
                _invert_faces_ms(ms, ml)
                geom = ms.apply_filter('get_geometric_measures')
    after_volume = geom.get('mesh_volume', 0)

    new_verts = np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float32)
    new_tris = np.asarray(ms.current_mesh().face_matrix(), dtype=np.int32)
    fin = ms.apply_filter('get_topological_measures')
    after_area = surface_area(new_verts, new_tris)
    after_verts = fin.get('vertices_number', 0)
    after_faces = fin.get('faces_number', 0)

    volume_change_pct = 0.0
    if before_volume and after_volume:
        volume_change_pct = (after_volume - before_volume) / abs(before_volume) * 100
    surface_change_pct = 0.0
    if before_area and after_area:
        surface_change_pct = (after_area - before_area) / abs(before_area) * 100
    stats['stage1']['volume_before'] = float(before_volume)
    stats['stage1']['volume_after'] = float(after_volume)
    stats['stage1']['volume_change_percent'] = round(volume_change_pct, 2)
    stats['stage1']['surface_area_before'] = round(before_area, 3)
    stats['stage1']['surface_area_after'] = round(after_area, 3)
    stats['stage1']['surface_area_change_percent'] = round(surface_change_pct, 2)
    if abs(volume_change_pct) > 15:
        stats['stage1']['volume_warning'] = (
            'Volume changed by %.1f%% - verify in slicer before printing.'
            % abs(volume_change_pct))

    stats['stage1'].update({
        'two_manifold': bool(fin.get('is_mesh_two_manifold')),
        'holes_remaining': boundary_loop_stats(new_verts, new_tris)[0],
        'components': fin.get('connected_components_number'),
        'components_before': int(components_before),
        'vertices_before': int(before_verts),
        'vertices_after': int(after_verts),
        'faces_before': int(before_faces),
        'faces_after': int(after_faces),
    })

    # Self-intersection count on the FINAL stage-1 output (repair_score.py's
    # health metric). We reuse the existing `ms` MeshSet: ms.current_mesh()
    # IS the repaired mesh right now (new_verts/new_tris were pulled from it
    # above), so building a fresh MeshSet and reloading arrays would be wasted
    # work on large meshes. The filter only sets the face-selection array and
    # does not alter geometry, so this cannot disturb anything downstream.
    #
    # Assumption (documented, not silent): stage 2 (manifold3d) only runs on a
    # watertight closed mesh and REBUILDS the watertight solid, so its output
    # is assumed free of self-intersections by construction -- therefore stage
    # 2's output is NOT re-measured here. When stage 2 runs and writes the
    # final mesh, this value is a conservative stage-1-based upper bound on
    # the health signal, which is fine for scoring.
    # Under --si-mode off the policy is 'not measured': skip the (estimated)
    # count entirely and report None. The guarded si_excise pass keeps its own
    # pymeshlab count independently, so it is unaffected.
    if si_mode == 'off':
        stats['stage1']['self_intersections_remaining'] = None
    elif topology.native_si_enabled():
        _, count = topology.self_intersecting_faces(new_verts, new_tris)
        stats['stage1']['self_intersections_remaining'] = max(int(count), 0)
    else:
        ms.apply_filter('compute_selection_by_self_intersections_per_face')
        stats['stage1']['self_intersections_remaining'] = int(
            ms.current_mesh().face_selection_array().sum())
    stats['method_errors'] = method_health.current().to_dicts()
    return stats, new_verts, new_tris


# ---------------------------------------------------------------------------
# External repair engines (sutura/engines.py, user-installed third-party
# binaries). Engine configs are loaded ONCE per run and slotted into the
# pipeline at their named placement. Every engine output passes through the
# SAME holes/non-manifold guard as the fTetWild tier, and an adopted output
# far from the input is flagged (``shape_changed``) exactly like fTetWild.
# With no engines configured and no chain.toml the whole layer is inert: the
# repair is byte-identical to the pre-engines behaviour.
ENGINE_MESH_FORMATS = ('stl', 'obj', 'ply')


def load_engine_run(engines_dir=None):
    """Load engine configs and resolve the execution chain, ONCE per run.

    Returns ``(engines, chain, warnings)``. ``sutura/engines.py`` is treated
    as optional: a stale install without it, or a broken config, only yields a
    warning and behaves as if no engine were configured (never a crash and
    never a repair failure)."""
    try:
        import engines as _engines
    except Exception as e:  # noqa: BLE001
        return {}, [], ['external engines unavailable: %s' % e]
    try:
        eng, warns = _engines.load_all_engines(engines_dir)
        chain, cwarns = _engines.resolve_chain(eng, config_dir=engines_dir)
        return eng, chain, warns + cwarns
    except Exception as e:  # noqa: BLE001 - ambient config never fails a repair
        return {}, [], ['failed to load engines: %s' % e]


def engine_placement_order(engines, chain, placement):
    """Enabled engine names for one placement, in chain order."""
    if not engines:
        return []
    order = list(chain or [])
    names = [n for n in order if n in engines
             and engines[n].enabled and engines[n].placement == placement]
    # Engines present in the config dir but omitted from a custom chain.toml
    # are not run; a default chain (build_default_chain) includes them all.
    return names


def _read_engine_mesh(path):
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.load_new_mesh(path)
    return (np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float32),
            np.asarray(ms.current_mesh().face_matrix(), dtype=np.int32))


def _write_engine_mesh(path, verts, tris):
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float32),
                        face_matrix=np.asarray(tris, np.int32)))
    ms.save_current_mesh(path)


def run_engines(ml, engines, chain, placement, tmpdir, in_v, in_t,
                base_holes, base_nm, spec, entries):
    """Run the enabled engines at ``placement`` on ``(in_v, in_t)``.

    Engines run in chain order; the first output that passes the same
    holes/non-manifold guard as the fTetWild tier is adopted and returned.
    Returns ``(cand_v, cand_t, adopted)`` (cand arrays None when nothing was
    adopted). One report entry per engine is appended to ``entries``."""
    import engines as _engines
    for name in engine_placement_order(engines, chain, placement):
        eng = engines[name]
        entry = {'name': name, 'placement': placement, 'adopted': False}
        with method_health.tier_guard('engine:' + name, placement) as _g:
            in_path = os.path.join(tmpdir,
                                   'engine_%s_in.%s' % (name, eng.input_format))
            out_path = os.path.join(tmpdir,
                                    'engine_%s_out.%s' % (name, eng.output_format))
            _write_engine_mesh(in_path, in_v, in_t)
            eng_tmp = os.path.join(tmpdir, 'engine_%s_tmp' % name)
            os.makedirs(eng_tmp, exist_ok=True)
            res = _engines.run_engine(eng, in_path, out_path, tmp_dir=eng_tmp)
            entry['rc'] = res.get('returncode')
            entry['time'] = round(float(res.get('duration_sec') or 0.0), 3)
            entry['stdout_tail'] = res.get('stdout_tail', '')
            entry['stderr_tail'] = res.get('stderr_tail', '')
            if not res.get('success'):
                entry['reject_reason'] = res.get('error') or 'engine failed'
                entries.append(entry)
                continue
            ok, verr = _engines.validate_output(eng, in_path, out_path)
            if not ok:
                entry['reject_reason'] = verr
                entries.append(entry)
                continue
            cand_v, cand_t = _read_engine_mesh(out_path)
            if len(cand_t) == 0:
                entry['reject_reason'] = 'engine output has no faces'
                entries.append(entry)
                continue
            cand_holes = boundary_loop_stats(cand_v, cand_t)[0]
            cand_ms = ml.MeshSet()
            cand_ms.add_mesh(ml.Mesh(vertex_matrix=cand_v, face_matrix=cand_t))
            cand_after = cand_ms.apply_filter('get_topological_measures')
            if cand_after.get('faces_number', 0) == 0:
                entry['reject_reason'] = 'engine removed all geometry'
                entries.append(entry)
                continue
            cand_nm = cand_after.get('non_two_manifold_edges', 0)
            entry['output_holes'] = int(cand_holes)
            entry['output_non_manifold'] = int(cand_nm)
            if cand_holes <= base_holes and cand_nm <= base_nm:
                hd, _ = _hausdorff_rel(ml, in_v, in_t, cand_v, cand_t,
                                       samples=spec.ftetwild_hausdorff_samples)
                entry['hausdorff_rel'] = hd
                entry['shape_changed'] = bool(
                    hd is not None and hd > FTETWILD_MAX_HAUSDORFF_REL)
                entry['adopted'] = True
                entries.append(entry)
                return cand_v, cand_t, True
            entry['reject_reason'] = 'holes_nm'
        if _g.error is not None:
            entry['reject_reason'] = 'error: %s' % _g.error
        entries.append(entry)
    return None, None, False


def _record_engine_entries(stats, entries):
    """Append engine entries to ``stats['engines']`` and flag shape changes."""
    if not entries:
        return
    stats.setdefault('engines', []).extend(entries)
    if any(e.get('shape_changed') for e in entries):
        stats['shape_changed'] = True


def graft_tier(ml, ms, after, stats, v, t, tmpdir, graft=False, intensity=None,
               spec=None):
    """Morphology shell-wrap tier for registry method 13 (Graft).

    Runs ``sutura_engine.graft.shell_wrap`` on the ORIGINAL input arrays (like
    the closing and fTetWild tiers).  The candidate is adopted only when it is
    strict-watertight after the save/reload-equivalent weld.  ``stats['graft']``
    records the fidelity verdict (``fidelity_ok``, ``hausdorff_healthy``,
    ``detail_max_mm``/``detail_area_moved``) and the EN/TR detail-loss
    warnings the CLI/GUI surface; the auto policy reads ``fidelity_ok`` to
    decide whether to also try fTetWild.

    The tier is bounded two ways: ``spec`` (the Triage ``IntensitySpec``)
    supplies ``graft_max_faces`` (a hard input cap; a single ``morph_close``
    attempt is not interruptible) and ``graft_timeout`` (an OUTER wall-clock
    abort checked between ladder attempts).  The r-ladder itself stops on a
    DETERMINISTIC work budget inside ``shell_wrap`` (a pure function of the
    input grid + preset), so the produced mesh does not depend on CPU speed or
    load.  Inputs above the cap, a work-budget stop, and a wall-clock abort are
    reported honestly (``skipped_budget`` / ``budget_limited_by`` / ``aborted``
    / ``reject_reason``) and keep the stage-1 result.  An abort is
    all-or-nothing: the partial ladder is discarded, never returned.  Never
    raises.
    """
    if not graft:
        return ms, after
    cap = getattr(spec, 'graft_max_faces', GRAFT_MAX_FACES) if spec is not None \
        else GRAFT_MAX_FACES
    timeout = getattr(spec, 'graft_timeout', GRAFT_TIMEOUT) if spec is not None \
        else GRAFT_TIMEOUT
    rec = {'tier': 'graft', 'ran': False, 'adopted': False,
           'watertight': False, 'fidelity_ok': None,
           'input_coverage': None, 'coverage_ok': None,
           'hausdorff_healthy': None, 'detail_max_mm': None,
           'detail_area_moved': None, 'r_used': None, 'mode': None,
           'faces': None, 'holes': None, 'non_manifold': None,
           'warnings': [], 'reason': None,
           'skipped_budget': False, 'timed_out': False,
           'low_memory': False, 'aborted': False,
           'grid_fallback': None,
           'budget_limited_by': None, 'work_budget': None,
           'reject_reason': None,
           'input_faces': None,
           'max_faces': int(cap) if cap is not None else None,
           'time_budget': (float(timeout) if timeout else None)}
    with method_health.tier_guard('graft', 'tier') as _g:
        try:
            from sutura_engine import graft as _graft
        except Exception:  # noqa: BLE001
            import shell_wrap as _graft
        in_v = np.asarray(v, dtype=np.float64)
        in_t = np.asarray(t, dtype=np.int64)
        rec['input_faces'] = int(len(in_t))
        if cap is not None and len(in_t) > int(cap):
            # Hard input cap: a morph_close call is not interruptible, so do
            # not start the ladder on a mesh too large to finish in budget.
            rec['skipped_budget'] = True
            rec['reject_reason'] = 'too_large'
            rec['holes'] = (stats.get('stage1') or {}).get('holes_remaining')
            rec['non_manifold'] = (stats.get('stage1') or {}).get(
                'non_manifold_edges_remaining')
            rec['reason'] = ('input has %d faces, above the Graft cap of %d; '
                             'stage-1 result kept' % (len(in_t), int(cap)))
            stats['graft'] = rec
            return ms, after
        cand_v, cand_t, grec = _graft.shell_wrap(in_v, in_t, ml=ml,
                                                 intensity=intensity,
                                                 time_budget=timeout)
        rec['ran'] = True
        rec['budget_exceeded'] = bool(grec.get('budget_exceeded'))
        rec['budget_limited_by'] = grec.get('budget_limited_by')
        rec['work_budget'] = grec.get('work_budget')
        rec['grid_fallback'] = grec.get('grid_fallback')
        if grec.get('aborted'):
            # Outer wall-clock abort: Graft was stopped for time and the
            # stage-1 result is kept (never a partial-ladder mesh).
            rec['aborted'] = True
            rec['timed_out'] = True
            rec['reject_reason'] = 'aborted_time'
            rec['reason'] = grec.get('reason')
            rec['warnings'] = list(grec.get('warnings') or [])
            rec['holes'] = (stats.get('stage1') or {}).get('holes_remaining')
            rec['non_manifold'] = (stats.get('stage1') or {}).get(
                'non_manifold_edges_remaining')
            stats['graft'] = rec
            return ms, after
        if rec['budget_exceeded']:
            # Deterministic inner work budget: keep stage-1 if not adopted.
            rec['reject_reason'] = 'budget'
        if grec.get('low_memory'):
            # RAM guard: the deterministic grid budget did not fit, so Graft
            # refused rather than coarsening.  Keep the stage-1 result and
            # surface the clear message (never a silently different shape).
            rec['low_memory'] = True
            rec['reject_reason'] = 'low_memory'
            rec['reason'] = grec.get('reason')
            rec['warnings'] = list(grec.get('warnings') or [])
            rec['holes'] = (stats.get('stage1') or {}).get('holes_remaining')
            rec['non_manifold'] = (stats.get('stage1') or {}).get(
                'non_manifold_edges_remaining')
            stats['graft'] = rec
            return ms, after
        rec['fidelity_ok'] = grec.get('fidelity_ok')
        rec['input_coverage'] = grec.get('input_coverage')
        rec['coverage_ok'] = grec.get('coverage_ok')
        rec['hausdorff_healthy'] = grec.get('hausdorff_healthy')
        rec['detail_max_mm'] = grec.get('detail_max_mm')
        rec['detail_area_moved'] = grec.get('detail_area_moved')
        rec['r_used'] = grec.get('r_used')
        rec['mode'] = grec.get('mode')
        rec['seconds'] = grec.get('seconds')
        rec['warnings'] = list(grec.get('warnings') or [])
        if len(cand_t):
            cand_v, cand_t = weld_reload_equivalent(cand_v, cand_t)
            cand_v, cand_t = _repair_welded_topology(ml, cand_v, cand_t)
            cand_v = np.asarray(cand_v, dtype=np.float32)
            cand_t = np.asarray(cand_t, dtype=np.int32)
            det = detect_defects(cand_v, cand_t)
            cand_holes = len(det['holes'])
            cand_nm = len(det['non_manifold'])
            rec['holes'] = cand_holes
            rec['non_manifold'] = cand_nm
            rec['faces'] = int(len(cand_t))
            if len(cand_t) and cand_holes == 0 and cand_nm == 0:
                ms = ml.MeshSet()
                ms.add_mesh(ml.Mesh(vertex_matrix=cand_v, face_matrix=cand_t))
                after = ms.apply_filter('get_topological_measures')
                rec['adopted'] = True
                rec['watertight'] = False  # stage 2 confirms at the top level
                rec['candidate_watertight'] = True
                rec['reason'] = 'strict-watertight candidate (stage 2 pending)'
            elif rec['reason'] is None:
                rec['reason'] = ('candidate not strict-watertight '
                                 '(holes=%d non-manifold=%d)'
                                 % (cand_holes, cand_nm))
        else:
            rec['reason'] = 'graft returned no geometry'
        if rec['adopted']:
            # A watertight candidate was found before the work budget ran out.
            rec['timed_out'] = False
            rec['reject_reason'] = None
        elif rec.get('budget_exceeded'):
            # Deterministic inner work budget spent without a watertight
            # candidate: keep stage 1.  Machine-independent (a pure function of
            # the input grid + preset), unlike the old wall-clock truncation.
            rec['skipped_budget'] = True
            if rec['holes'] is None:
                rec['holes'] = (stats.get('stage1') or {}).get(
                    'holes_remaining')
                rec['non_manifold'] = (stats.get('stage1') or {}).get(
                    'non_manifold_edges_remaining')
            rec['reason'] = ('Graft hit its deterministic work budget (%s '
                             'cell-attempts) before a watertight candidate; '
                             'stage-1 result kept'
                             % (rec.get('work_budget') or '?'))
    if _g.error is not None:
        # PyO3 surfaces a Rust panic as pyo3_runtime.PanicException, which
        # subclasses BaseException (not Exception); a tier must degrade to its
        # reason field rather than abort the whole repair.
        rec['reason'] = 'error: %s: %s' % (type(_g.error).__name__, _g.error)
    stats['graft'] = rec
    return ms, after


# CLI flags the Dressing suggestion points the user at: the plain opt-in, and
# the shape-gate override that keeps a gate-rejected (but watertight) coat.
DRESSING_SUGGEST_FLAG = '--experimental-dressing'
DRESSING_FORCE_ADOPT_FLAG = '--dressing-force-adopt'


def _obj_closed_result(r):
    """True when one object's stage-1 output is a closed manifold (local copy
    of ``classification._obj_closed`` so repair.py does not import a private
    helper)."""
    s1 = r.get('stage1', {}) if isinstance(r, dict) else {}
    return bool(s1.get('two_manifold')) and s1.get('holes_remaining', 0) == 0


def _not_watertight_result(report):
    """True when the FINAL repaired geometry is open or non-manifold.

    The verdict is the post-ladder topology in ``stage1`` (``two_manifold`` /
    ``holes_remaining`` / ``non_manifold_edges_remaining``), which
    ``repair_mesh_from_arrays`` rewrites after every tier, so it describes the
    mesh actually saved.  A residual self-intersection count is NOT a
    "not watertight" signal: SI is reported, not repaired, under the default
    ``si_mode='report'`` (a closed, SI-carrying result must not get a "Not
    watertight" hint).  A hard error or a declined budget save is not a "try
    Dressing" situation either.  Multi-object 3MF is judged across every
    object."""
    if not isinstance(report, dict):
        return False
    if report.get('error') or report.get('status') == 'budget_declined':
        return False
    reports = report.get('object_reports')
    if reports:
        return any(not _obj_closed_result(r) for r in reports)
    s1 = report.get('stage1')
    if not isinstance(s1, dict) or not s1:
        return False
    if not s1.get('two_manifold'):
        return True
    if s1.get('holes_remaining', 0):
        return True
    if s1.get('non_manifold_edges_remaining', 0):
        return True
    return False


def _dressing_record(report):
    """The Dressing record of a single-mesh or first-run multi-object report."""
    dress = report.get('dressing')
    if isinstance(dress, dict):
        return dress
    for r in report.get('object_reports') or []:
        if isinstance(r.get('dressing'), dict):
            return r['dressing']
    return None


def _dressing_gate_detail(sug):
    """Human parenthesis with the gate numbers a rejected coat failed."""
    parts = []
    v = sug.get('volume_delta_rel')
    if v is not None:
        parts.append('volume %.2f%%' % (100.0 * v))
    n = sug.get('normal_angle_p95')
    if n is not None:
        parts.append('normal p95 %.1f deg' % n)
    if not parts:
        r = sug.get('reason')
        if r:
            parts.append(str(r))
    return ' (%s)' % ', '.join(parts) if parts else ''


def _dressing_suggestions(report):
    """Return the Dressing opt-in suggestion(s) for a still-broken result.

    Empty list when the repair errored / was budget-declined, the result is
    watertight, or Dressing was already adopted.  When Dressing ran but the
    quality gate rejected it, the suggestion points at
    ``--dressing-force-adopt``; otherwise it points at the plain
    ``--experimental-dressing`` opt-in.  Never raises."""
    try:
        if not _not_watertight_result(report):
            return []
        dress = _dressing_record(report)
        if isinstance(dress, dict) and dress.get('adopted'):
            return []
        suggestion = {
            'method': 'dressing',
            'flag': DRESSING_SUGGEST_FLAG,
            'force': False,
            'reason': 'dressing_not_run',
            'warning': 'may_deform',
        }
        if isinstance(dress, dict) and dress.get('ran'):
            suggestion['flag'] = DRESSING_FORCE_ADOPT_FLAG
            suggestion['force'] = True
            suggestion['reason'] = dress.get('reason') or 'gate_rejected'
            suggestion['volume_delta_rel'] = dress.get('volume_delta_rel')
            suggestion['normal_angle_p95'] = dress.get('normal_angle_p95')
            suggestion['fidelity_ok'] = dress.get('fidelity_ok')
        return [suggestion]
    except Exception:  # noqa: BLE001 - a suggestion never breaks a repair
        return []


def _dressing_suggestion_text(sug):
    """English CLI lines for one Dressing suggestion (CLI/human report)."""
    if sug.get('force'):
        return ('  \u26a0 Not watertight. Dressing ran but its result was '
                'rejected by the quality gate%s.\n'
                '    Force it with %s if you accept the deformation '
                '(the shape may deform).'
                % (_dressing_gate_detail(sug), sug.get('flag')))
    return ('  \u26a0 Not watertight. You can try Dressing: %s (watertight, '
            'but the shape may deform / fine detail may be lost).'
            % sug.get('flag'))


def dressing_tier(ml, ms, after, stats, v, t, tmpdir, dressing=False,
                  intensity=None, drain=None, defects=None,
                  r_max_scale=None, sigma_scale=None, force_adopt=False):
    """Variable-viscosity coat tier for registry method 16 (Dressing).

    Runs ``sutura_engine.dressing.dressing_coat`` on the ORIGINAL input arrays
    (like the closing / Graft / fTetWild tiers).  The candidate is adopted only
    when it is strict-watertight after the save/reload-equivalent weld, free of
    exact self-intersections and inside the preset's coat->input fidelity gate.
    ``drain`` selects the healthy-region erode-back (``None`` = preset default,
    a mode name, or an explicit delta in mm).  ``defects`` selects the mask
    (``'all'`` default, ``'holes_nm'``); ``r_max_scale`` / ``sigma_scale``
    multiply the resolved ``r_max`` / ``sigma``.  ``force_adopt``
    (``--dressing-force-adopt``) skips the shape-preservation gates (volume,
    component count, healthy-surface normal angle, coat fidelity) while STILL
    requiring a strict-watertight, self-intersection-free candidate, so the
    caller can keep a deformed coat it explicitly asked for.
    ``stats['dressing']`` records
    the parameters, the deviations, the exact-SI verdict and the EN/TR warnings
    the CLI/GUI surface.  Never raises.
    """
    if not dressing:
        return ms, after
    rec = {'tier': 'dressing', 'ran': False, 'adopted': False,
           'watertight': False, 'fidelity_ok': None, 'engine': None,
           'voxel': None, 'r_base': None, 'r_max': None, 'sigma': None,
           'faces': None, 'faces_coat': None, 'decimated': False,
           'drain': None, 'defects': None, 'r_max_scale': None,
           'sigma_scale': None, 'cleanup': None,
           'hausdorff_rel_max': None, 'hausdorff_input_to_coat': None,
           'normal_angle_p50': None, 'normal_angle_p95': None,
           'normal_angle_n': 0,
           'volume_before': None, 'volume_after': None,
           'volume_delta_rel': None, 'components_before': None,
           'components_after': None,
           'volume_ok': None, 'parts_ok': None, 'normal_ok': None,
           'si_before': None, 'si_after': None, 'si_exact_unknown': False,
           'holes': None, 'non_manifold': None,
           'within_time_budget': None, 'seconds': None,
           'forced': False,
           'warnings': [], 'reason': None}
    try:
        from sutura_engine import dressing as _dressing
    except Exception as e:  # noqa: BLE001
        rec['reason'] = 'sutura_engine.dressing unavailable: %s' % e
        stats['dressing'] = rec
        return ms, after
    with method_health.tier_guard('dressing', 'tier') as _g:
        in_v = np.asarray(v, dtype=np.float64)
        in_t = np.asarray(t, dtype=np.int64)
        cand_v, cand_t, drec = _dressing.dressing_coat(
            in_v, in_t, ml=ml, intensity=intensity, drain=drain,
            defects=defects, r_max_scale=r_max_scale, sigma_scale=sigma_scale)
        rec['ran'] = True
        rec['drain'] = drec.get('drain')
        rec['defects'] = drec.get('defects')
        rec['r_max_scale'] = drec.get('r_max_scale')
        rec['sigma_scale'] = drec.get('sigma_scale')
        rec['engine'] = drec.get('engine')
        rec['voxel'] = drec.get('voxel')
        rec['r_base'] = drec.get('r_base')
        rec['r_max'] = drec.get('r_max')
        rec['sigma'] = drec.get('sigma')
        rec['faces_coat'] = drec.get('faces_coat')
        rec['cleanup'] = drec.get('cleanup')
        rec['decimated'] = bool(drec.get('decimated'))
        rec['fidelity_ok'] = drec.get('fidelity_ok')
        rec['hausdorff_rel_max'] = drec.get('hausdorff_rel_max')
        rec['hausdorff_input_to_coat'] = drec.get('hausdorff_input_to_coat')
        rec['si_before'] = drec.get('si_before')
        rec['si_after'] = drec.get('si_after')
        rec['si_exact_unknown'] = bool(drec.get('si_exact_unknown'))
        rec['within_time_budget'] = drec.get('within_time_budget')
        rec['seconds'] = drec.get('seconds')
        rec['warnings'] = list(drec.get('warnings') or [])
        rec['normal_angle_p50'] = drec.get('normal_angle_p50')
        rec['normal_angle_p95'] = drec.get('normal_angle_p95')
        rec['normal_angle_n'] = int(drec.get('normal_angle_n') or 0)
        if len(cand_t) == 0:
            rec['reason'] = 'dressing returned no geometry'
        else:
            cand_v, cand_t = weld_reload_equivalent(cand_v, cand_t)
            cand_v, cand_t = _repair_welded_topology(ml, cand_v, cand_t)
            cand_v = np.asarray(cand_v, dtype=np.float32)
            cand_t = np.asarray(cand_t, dtype=np.int32)
            det = detect_defects(cand_v, cand_t)
            cand_holes = len(det['holes'])
            cand_nm = len(det['non_manifold'])
            rec['holes'] = cand_holes
            rec['non_manifold'] = cand_nm
            rec['faces'] = int(len(cand_t))
            si_after = drec.get('si_after')
            si_ok = si_after in (0, None)

            # global-shape guards vs the mesh the ladder held before Dressing:
            # a component-count explosion or a large volume change is not a
            # repair.  The raw input signed volume is meaningless when the input
            # is itself damaged/open, so the current result is the reference.
            vol_before = parts_before = None
            try:
                cur_v = np.asarray(ms.current_mesh().vertex_matrix(),
                                   dtype=np.float64)
                cur_t = np.asarray(ms.current_mesh().face_matrix(),
                                   dtype=np.int64)
                vol_before = _dressing.signed_volume(cur_v, cur_t)
                parts_before = _dressing.count_components(cur_t)
            except Exception:  # noqa: BLE001 - guards degrade to "unknown"
                pass
            vol_after = drec.get('volume_after')
            parts_after = drec.get('components_after')
            rec['volume_before'] = (round(vol_before, 6)
                                    if vol_before is not None else None)
            rec['volume_after'] = vol_after
            rec['components_before'] = parts_before
            rec['components_after'] = parts_after
            if vol_before is not None and vol_after is not None:
                denom = max(abs(vol_before), 1e-12)
                rec['volume_delta_rel'] = abs(vol_after - vol_before) / denom
            vol_ok = (rec['volume_delta_rel'] is None
                      or rec['volume_delta_rel'] <= DRESSING_MAX_VOLUME_DELTA)
            parts_ok = (parts_before is None or parts_after is None
                        or parts_after <= parts_before + DRESSING_MAX_PARTS_SLACK)
            na95 = rec['normal_angle_p95']
            normal_ok = (na95 is None
                         or na95 <= DRESSING_MAX_NORMAL_ANGLE_P95)
            rec['volume_ok'] = bool(vol_ok)
            rec['parts_ok'] = bool(parts_ok)
            rec['normal_ok'] = bool(normal_ok)
            shape_ok = (drec.get('fidelity_ok') is not False
                        and vol_ok and parts_ok and normal_ok)
            rec['shape_ok'] = bool(shape_ok)

            if (cand_holes == 0 and cand_nm == 0 and si_ok
                    and (shape_ok or force_adopt)):
                ms = ml.MeshSet()
                ms.add_mesh(ml.Mesh(vertex_matrix=cand_v, face_matrix=cand_t))
                after = ms.apply_filter('get_topological_measures')
                rec['adopted'] = True
                rec['forced'] = bool(force_adopt and not shape_ok)
                rec['watertight'] = False  # stage 2 confirms at the top level
                rec['candidate_watertight'] = True
                rec['reason'] = ('strict-watertight candidate (stage 2 pending'
                                 + (', shape gates forced)' if rec['forced']
                                    else ')'))
            else:
                reasons = []
                if cand_holes or cand_nm:
                    reasons.append('holes=%d non-manifold=%d'
                                   % (cand_holes, cand_nm))
                if not si_ok:
                    reasons.append('exact self-intersections=%s' % si_after)
                if not force_adopt:
                    if drec.get('fidelity_ok') is False:
                        reasons.append('fidelity %.3f%% > gate'
                                       % (100.0 * (rec['hausdorff_rel_max'] or 0)))
                    if not vol_ok:
                        reasons.append('volume delta %.2f%% > gate'
                                       % (100.0 * (rec['volume_delta_rel'] or 0)))
                    if not parts_ok:
                        reasons.append('components %s > %s + %d'
                                       % (parts_after, parts_before,
                                          DRESSING_MAX_PARTS_SLACK))
                    if not normal_ok:
                        reasons.append('normal p95 %.1f deg > gate'
                                       % (na95 or 0.0))
                rec['reason'] = ('candidate not adopted (%s)'
                                 % ', '.join(reasons) if reasons
                                 else 'candidate not adopted')
    if _g.error is not None:
        rec['reason'] = 'error: %s: %s' % (type(_g.error).__name__, _g.error)
    stats['dressing'] = rec
    return ms, after


def wall_thicken_tier(ml, ms, after, stats, v, t, min_thickness=None):
    """Thin-wall thicken-to-min tier for registry method 15 (P-WALL).

    ``wall_thickness.thicken_to_min`` measures the per-vertex wall thickness
    from an SDF grid and thickens walls below ``min_thickness`` (default: 1% of
    the bbox diagonal) with a morphological closing of the solid. It runs on
    the ORIGINAL input arrays; the candidate is cleaned with PyMeshLab, then
    adopted only when it is strict-watertight and no worse than the stage-1
    baseline on holes + non-manifold edges. Records ``stats['wall_thicken']``
    (before/after minimum thickness, counts, notes); never raises.
    """
    rec = {'tier': 'wall_thicken', 'ran': True, 'adopted': False,
           'watertight': False, 'min_before': None, 'min_after': None,
           'min_thickness': None, 'delta': None, 'thin_before': None,
           'faces': None, 'notes': [], 'reason': None}
    with method_health.tier_guard('wall_thicken', 'tier') as _g:
        import wall_thickness as _wt
        base_v = np.asarray(ms.current_mesh().vertex_matrix())
        base_t = np.asarray(ms.current_mesh().face_matrix())
        base_holes = boundary_loop_stats(base_v, base_t)[0]
        base_nm = int(after.get('non_two_manifold_edges', 0))
        in_v = np.asarray(v, dtype=np.float64)
        in_t = np.asarray(t, dtype=np.int64)
        cand_v, cand_t, rep = _wt.thicken_to_min(
            in_v, in_t, min_thickness=min_thickness)
        rec['min_thickness'] = rep.get('min_thickness')
        rec['min_before'] = rep.get('min_before')
        rec['thin_before'] = rep.get('thin_before')
        rec['delta'] = rep.get('delta')
        rec['notes'] = list(rep.get('notes') or [])
        if 'error' in rep:
            rec['reason'] = 'module error: %s' % rep['error']
        if len(cand_t):
            cand_v, cand_t = _clean_and_orient(ml, cand_v, cand_t)
            cand_v = np.asarray(cand_v, dtype=np.float32)
            cand_t = np.asarray(cand_t, dtype=np.int32)
            det = detect_defects(cand_v, cand_t)
            cand_holes = len(det['holes'])
            cand_nm = len(det['non_manifold'])
            rec['holes'] = cand_holes
            rec['non_manifold'] = cand_nm
            rec['faces'] = int(len(cand_t))
            rec['min_after'] = rep.get('min_after')
            rec['thin_after'] = rep.get('thin_after')
            if (cand_holes == 0 and cand_nm == 0
                    and cand_holes + cand_nm <= base_holes + base_nm):
                ms = ml.MeshSet()
                ms.add_mesh(ml.Mesh(vertex_matrix=cand_v, face_matrix=cand_t))
                after = ms.apply_filter('get_topological_measures')
                rec['adopted'] = True
                rec['candidate_watertight'] = True
                rec['reason'] = 'walls thickened (stage 2 pending)'
            elif rec['reason'] is None:
                rec['reason'] = ('candidate not strict-watertight '
                                 '(holes=%d non-manifold=%d)'
                                 % (cand_holes, cand_nm))
        elif rec['reason'] is None:
            rec['reason'] = 'module returned no geometry'
    if _g.error is not None:
        rec['ran'] = False
        rec['reason'] = 'error: %s' % _g.error
    stats['wall_thicken'] = rec
    return ms, after


def _clean_and_orient(ml, verts, tris):
    """PyMeshLab cleanup of a generated surface: dedup + coherent orientation."""
    try:
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float64),
                            face_matrix=np.asarray(tris, np.int32)))
        for filt in ('meshing_remove_duplicate_vertices',
                     'meshing_remove_duplicate_faces',
                     'meshing_remove_unreferenced_vertices',
                     'meshing_repair_non_manifold_edges',
                     'meshing_re_orient_faces_coherently'):
            try:
                ms.apply_filter(filt)
            except Exception:
                pass
        cur = ms.current_mesh()
        return (np.asarray(cur.vertex_matrix(), dtype=np.float64),
                np.asarray(cur.face_matrix(), dtype=np.int32))
    except Exception:
        return verts, tris


# One-sided (original -> result, outside the repaired boxes) Hausdorff guard
# for the repeated-element transplant tier: the transplanted copy must not move
# the untouched geometry. Mirrors methods.GENERATIVE_MAX_HAUSDORFF_REL.
REPEAT_MAX_HAUSDORFF_OUTSIDE = 0.01


def repeat_tier(ml, ms, after, stats, mode=None, source_point=None,
                target_point=None):
    """Repeated-element transplant tier (registry methods 11/12, P-REP).

    Runs AFTER the whole stage-1 chain because the transplant needs a
    watertight (closed 2-manifold) ``M``: if stage 1 left holes or non-manifold
    edges the tier is skipped. ``mode='auto'`` detects the repeated pattern
    (``repeat_repair.detect_repetition``) and repairs the damaged copies;
    ``mode='manual'`` transplants the element nearest ``source_point`` onto the
    one nearest ``target_point``. The candidate is adopted only when it is
    reload-equivalent strict-watertight, no worse than the stage-1 baseline,
    and the untouched geometry did not move (one-sided Hausdorff outside the
    repaired boxes <= ``REPEAT_MAX_HAUSDORFF_OUTSIDE``). Records
    ``stats['repeat']``; never raises.
    """
    if not mode:
        return ms, after
    rec = {'mode': mode, 'ran': False, 'adopted': False, 'watertight': False,
           'pattern_type': None, 'positions_repaired': None,
           'hausdorff_outside': None, 'notes': [], 'reason': None}
    with method_health.tier_guard('repeat', 'tier') as _g:
        cur_v = np.asarray(ms.current_mesh().vertex_matrix())
        cur_t = np.asarray(ms.current_mesh().face_matrix())
        holes = boundary_loop_stats(cur_v, cur_t)[0]
        nm = int(after.get('non_two_manifold_edges', 0))
        if holes or nm or not after.get('is_mesh_two_manifold'):
            rec['reason'] = ('repeated-element transplant needs a watertight '
                             'input; stage 1 left holes=%d non-manifold=%d'
                             % (holes, nm))
            stats['repeat'] = rec
            return ms, after
        rec['ran'] = True
        import repeat_repair as _rr
        if mode == 'manual':
            if source_point is None or target_point is None:
                rec['reason'] = ('manual repeat repair needs a source and a '
                                 'target point')
                stats['repeat'] = rec
                return ms, after
            rv, rt, rep = _rr.repair_repeat_manual(
                np.asarray(cur_v, dtype=np.float64),
                np.asarray(cur_t, dtype=np.int64),
                np.asarray(source_point, dtype=np.float64),
                np.asarray(target_point, dtype=np.float64))
        else:
            rv, rt, rep = _rr.repair_repeat_auto(
                np.asarray(cur_v, dtype=np.float64),
                np.asarray(cur_t, dtype=np.int64))
        rec['notes'] = list(rep.get('notes') or [])
        rec['pattern_type'] = rep.get('pattern_type')
        rec['positions_repaired'] = rep.get('positions_repaired')
        rec['hausdorff_outside'] = rep.get('hausdorff_outside')
        if 'error' in rep:
            rec['reason'] = 'module error: %s' % rep['error']
        if not rep.get('repaired') or not len(rt):
            if rec['reason'] is None:
                notes = rep.get('notes') or []
                rec['reason'] = notes[0] if notes else 'nothing to repair'
            stats['repeat'] = rec
            return ms, after
        # Validate the raw manifold3d transplant (in-memory). unlike the
        # closing tiers it is NOT float32-welded here: the CSG output has
        # coincident cut-seam vertices whose pymeshlab weld-repair opens holes,
        # while the later stage-2 manifold3d rebuild makes the mesh reload-safe
        # as a side effect. The reload-equivalent counts are still recorded for
        # transparency; the top-level P-HONEST verdict remains authoritative.
        cand_v = np.asarray(rv, dtype=np.float32)
        cand_t = np.asarray(rt, dtype=np.int32)
        det = detect_defects(cand_v, cand_t)
        cand_holes = len(det['holes'])
        cand_nm = len(det['non_manifold'])
        rec['holes'] = cand_holes
        rec['non_manifold'] = cand_nm
        try:
            rh, rnm = reload_strict_holes_nm(rv, rt)
            rec['reload_holes'] = int(rh)
            rec['reload_non_manifold'] = int(rnm)
        except Exception:  # noqa: BLE001 - reporting only
            pass
        hd = rep.get('hausdorff_outside')
        if (len(cand_t) and cand_holes == 0 and cand_nm == 0
                and cand_holes + cand_nm <= holes + nm
                and hd is not None and hd <= REPEAT_MAX_HAUSDORFF_OUTSIDE):
            ms = ml.MeshSet()
            ms.add_mesh(ml.Mesh(vertex_matrix=cand_v, face_matrix=cand_t))
            after = ms.apply_filter('get_topological_measures')
            rec['adopted'] = True
            rec['watertight'] = False
            rec['candidate_watertight'] = True
            rec['reason'] = 'repeated element(s) transplanted (stage 2 pending)'
        elif rec['reason'] is None:
            if hd is not None and hd > REPEAT_MAX_HAUSDORFF_OUTSIDE:
                rec['reason'] = ('untouched geometry moved (Hausdorff %.3f > %.3f)'
                                 % (hd, REPEAT_MAX_HAUSDORFF_OUTSIDE))
            else:
                rec['reason'] = ('candidate not strict-watertight '
                                 '(holes=%d non-manifold=%d)'
                                 % (cand_holes, cand_nm))
    if _g.error is not None:
        rec['ran'] = False
        rec['reason'] = 'error: %s' % _g.error
    stats['repeat'] = rec
    return ms, after


def _dressing_wanted(dressing, holes, nm, si, si_mode='report'):
    """Whether the Dressing fallback should run on the current result.

    ``dressing is True`` (forced) always runs -- it overrides ``si_mode``.
    ``'auto'`` (the default switch) runs when the result still has holes or
    non-manifold edges; a POSITIVE exact self-intersection count also triggers
    it only under ``si_mode == 'repair'`` (``si is None`` = unmeasurable never
    triggers).
    """
    if dressing is True:
        return True
    if dressing == 'auto':
        si_damage = (si_mode == 'repair' and si is not None and si > 0)
        return bool(holes > 0 or nm > 0 or si_damage)
    return False


def _dressing_damage(ms, after, measure_si=True):
    """``(holes, non_manifold, exact_si)`` of the current result.

    ``exact_si`` is ``None`` when the classifier cannot measure it (empty/too
    large mesh, or a build without the Rust extension) or when ``measure_si``
    is False (``--si-mode off``).  Used to decide whether the Dressing fallback
    should run on the result the ladder currently holds (after Graft), so under
    the default 'report' mode an SI-only residual is reported but not escalated.
    """
    m = ms.current_mesh()
    holes = boundary_loop_stats(m.vertex_matrix(), m.face_matrix())[0]
    nm = int(after.get('non_two_manifold_edges', 0))
    si = None
    if measure_si:
        try:
            from sutura_engine import dressing as _d
            si = _d.si_face_count(
                np.asarray(m.vertex_matrix(), dtype=np.float64),
                np.asarray(m.face_matrix(), dtype=np.int64))
        except Exception:  # noqa: BLE001 - unknown SI never blocks a repair
            si = None
    return int(holes), nm, si


def deep_repair_ladder(ml, ms, after, stats, v, t, tmpdir, mode=None,
                       ftetwild=False, spec=None, engines=None, engine_chain=None,
                       graft=False, dressing=False, dressing_drain=None,
                       dressing_defects=None, dressing_rmax_scale=None,
                       dressing_sigma_scale=None, si_mode='report',
                       dressing_force_adopt=False):
    """Single entry point of the deep-repair ladder, called after the stage-1
    chain. ``mode`` is one of DEEP_REPAIR_MODES, or None for the library
    default (no deep-repair report; ``ftetwild`` alone decides, as before
    the ladder existed). 'local' runs the local re-mesh tier; 'full' and None
    run the fTetWild tier (``ftetwild``: 'auto'/True/False, unchanged
    semantics); 'off' runs nothing. ``spec`` is the resolved intensity spec
    (defaults to Balanced); it supplies the tier's timeout, size cap,
    decimation ladder and Hausdorff sample count. Records
    ``stats['deep_repair']`` (mode, tiers run, per-tier reports); the
    ``available`` offer is filled in by ``_deep_repair_offer`` after the final
    stage-1 measurement. Returns ``(ms, after)``."""
    if spec is None:
        spec = triage.PRESETS[triage.DEFAULT_INTENSITY]
    tiers_run = []
    cad = None
    dmg = None
    cur_holes = boundary_loop_stats(ms.current_mesh().vertex_matrix(),
                                    ms.current_mesh().face_matrix())[0]
    cur_nm = int(after.get('non_two_manifold_edges', 0))
    if mode == 'local':
        if cur_holes > 0 or cur_nm > 0:
            ms, after, local_rep = _local_remesh_tier(ml, ms, after)
            tiers_run.append('local')
        else:
            local_rep = None
    else:
        local_rep = None
    if mode in (None, 'full'):
        # replace_ftetwild engines REPLACE the built-in fTetWild tier; they
        # receive the original input, exactly like fTetWild. With a custom
        # chain.toml that omits 'ftetwild' the built-in tier is skipped too.
        replace_names = engine_placement_order(engines, engine_chain,
                                               'replace_ftetwild')
        if replace_names:
            _entries = []
            _cv, _ct, _adopted = run_engines(
                ml, engines, engine_chain, 'replace_ftetwild', tmpdir, v, t,
                cur_holes, cur_nm, spec, _entries)
            _record_engine_entries(stats, _entries)
            if _adopted:
                ms = ml.MeshSet()
                ms.add_mesh(ml.Mesh(vertex_matrix=_cv, face_matrix=_ct))
                after = ms.apply_filter('get_topological_measures')
                tiers_run.append('replace_ftetwild')
        elif (not engine_chain) or ('ftetwild' in engine_chain):
            # Graft (shell wrap) is tried BEFORE the fTetWild tier. When it is
            # reload-watertight and its healthy fidelity passes, it is adopted
            # and fTetWild is skipped. When it closes the mesh but fidelity
            # fails, fTetWild is also tried and the watertight result with the
            # lower healthy deviation wins.
            #
            # CAD guard hook: the geometric CAD/machined-vs-organic score is
            # recorded for every input.  The ladder keeps Graft first for ALL
            # inputs (which satisfies the guard: a CAD-like part gets the
            # verbatim Graft hybrid before Dressing and Dressing only runs when
            # damage remains); the calibrated detector will drive the ordering.
            cad = cad_likeness(v, t)
            graft_ms = graft_after = None
            graft_dev = None
            graft_ok = False
            graft_cov_failed = False
            # Only run the last-resort Graft when stage 1 still leaves holes or
            # non-manifold edges (the same trigger fTetWild's 'auto' uses).  A
            # closed stage-1 result is already the final mesh: Graft must not
            # run on it, or already-watertight repairs would change output and
            # pay an unnecessary tier.
            if graft in (True, 'auto') and (cur_holes > 0 or cur_nm > 0):
                _g_ms, _g_after = graft_tier(ml, ms, after, stats, v, t, tmpdir,
                                             graft=True,
                                             intensity=(getattr(spec, 'base', None)
                                                        or getattr(spec, 'name', None)),
                                             spec=spec)
                _g = stats.get('graft') or {}
                if _g.get('adopted'):
                    # Adopt only a watertight candidate that is faithful AND
                    # covers the input (G12). A coverage failure (a collapsed
                    # fragment that still passed the healthy-deviation gate,
                    # thingi10k_100827) is held aside like a fidelity failure so
                    # the ladder falls through to fTetWild/next tier; it is
                    # never preferred over fTetWild by the deviation tie-break.
                    if (_g.get('fidelity_ok') is not False
                            and _g.get('coverage_ok') is not False):
                        ms, after = _g_ms, _g_after
                        graft_ok = True
                        tiers_run.append('graft')
                    else:
                        graft_ms, graft_after = _g_ms, _g_after
                        graft_dev = _g.get('hausdorff_healthy')
                        graft_cov_failed = (_g.get('coverage_ok') is False)

            # Dressing (#16) sits AFTER Graft and BEFORE fTetWild: its
            # volumetric skin targets the whole-shell folds and heavy
            # self-intersections Graft's hybrid leaves behind.  When the switch
            # is on it is an Auto fallback, tried whenever the result the ladder
            # now holds (after Graft) still has holes, non-manifold edges or
            # exact self-intersections; an explicit force runs regardless.
            dressing_ok = False
            _dh = _dnm = 0
            _dsi = None
            if dressing == 'auto':
                _dh, _dnm, _dsi = _dressing_damage(ms, after,
                                                   measure_si=(si_mode != 'off'))
                dmg = {'holes': _dh, 'non_manifold': _dnm,
                       'self_intersections': _dsi}
            if _dressing_wanted(dressing, _dh, _dnm, _dsi, si_mode):
                _d_ms, _d_after = dressing_tier(
                    ml, ms, after, stats, v, t, tmpdir, dressing=True,
                    intensity=(getattr(spec, 'base', None)
                               or getattr(spec, 'name', None)),
                    drain=dressing_drain, defects=dressing_defects,
                    r_max_scale=dressing_rmax_scale,
                    sigma_scale=dressing_sigma_scale,
                    force_adopt=dressing_force_adopt)
                if (stats.get('dressing') or {}).get('adopted'):
                    ms, after = _d_ms, _d_after
                    dressing_ok = True
                    tiers_run.append('dressing')

            if dressing_ok:
                stats['experimental_ftetwild'] = False
                if graft_ms is not None:
                    stats['graft']['adopted'] = False
                    stats['graft']['reason'] = 'superseded by Dressing'
            elif graft_ok:
                stats['experimental_ftetwild'] = False
            else:
                ms, after = _ftetwild_tier(ml, ms, after, stats, v, t, tmpdir,
                                           ftetwild, spec=spec)
                if (stats.get('experimental_ftetwild') or {}).get('ran'):
                    tiers_run.append('ftetwild')
                if graft_ms is not None:
                    ft_dev = (stats.get('experimental_ftetwild')
                              or {}).get('hausdorff_rel')
                    ft_adopted = bool((stats.get('experimental_ftetwild')
                                       or {}).get('adopted'))
                    if (not ft_adopted
                            or graft_cov_failed
                            or (ft_dev is not None and graft_dev is not None
                                and ft_dev < graft_dev)):
                        stats['graft']['adopted'] = False
                        stats['graft']['reason'] = (
                            'superseded by fTetWild (input coverage below '
                            'threshold)' if graft_cov_failed else
                            'superseded by fTetWild (lower healthy deviation)')
                        tiers_run.append('graft')
                    else:
                        ms, after = graft_ms, graft_after
                        stats['graft']['adopted'] = True
                        stats['graft']['preferred_over_ftetwild'] = True
                        if 'ftetwild' in tiers_run:
                            tiers_run.remove('ftetwild')
                        tiers_run.append('graft')
    else:
        # mode 'off'/'local': Dressing is run whenever it is explicitly enabled
        # (method #16 sets deep_repair='off'), regardless of remaining holes, so
        # an explicit choice is always honoured; the auto 'full' ladder stays
        # conservative and only tries it when stage 1 left damage.
        if dressing is True:
            _d_ms, _d_after = dressing_tier(
                ml, ms, after, stats, v, t, tmpdir, dressing=True,
                intensity=(getattr(spec, 'base', None)
                           or getattr(spec, 'name', None)),
                drain=dressing_drain, defects=dressing_defects,
                r_max_scale=dressing_rmax_scale,
                sigma_scale=dressing_sigma_scale,
                force_adopt=dressing_force_adopt)
            if (stats.get('dressing') or {}).get('adopted'):
                ms, after = _d_ms, _d_after
                tiers_run.append('dressing')
        stats['experimental_ftetwild'] = False
    if mode is not None:
        stats['deep_repair'] = {
            'mode': mode,
            'si_mode': si_mode,
            'holes_before': int(cur_holes),
            'nm_before': cur_nm,
            'tiers_run': tiers_run,
            'local': local_rep,
            'ftetwild': stats.get('experimental_ftetwild') or None,
            'graft': stats.get('graft') or None,
            'dressing': stats.get('dressing') or None,
            'cad_likeness': cad,
            'dressing_damage': dmg,
            'available': None,
        }
    return ms, after


def _deep_repair_offer(stats, holes, nm, n_faces, spec=None):
    """Fill ``deep_repair.available`` and ``final_tier`` once the final
    stage-1 counts are known: when holes or non-manifold edges remain, list
    the tiers this mode did not run (fTetWild only when installed) with a
    rough time estimate."""
    if spec is None:
        spec = triage.PRESETS[triage.DEFAULT_INTENSITY]
    dr = stats.get('deep_repair')
    if not dr:
        return
    final = 'stage1'
    if (dr.get('ftetwild') or {}).get('adopted'):
        final = 'ftetwild'
    elif (stats.get('graft') or {}).get('adopted'):
        final = 'graft'
    elif (stats.get('dressing') or {}).get('adopted'):
        final = 'dressing'
    elif (dr.get('local') or {}).get('adopted'):
        final = 'local'
    dr['final_tier'] = final
    if holes == 0 and nm == 0:
        return
    within_cap = (spec.ftetwild_max_faces is None
                  or n_faces <= spec.ftetwild_max_faces)
    tiers = []
    if dr['mode'] == 'off':
        tiers.append('local')
    if (dr['mode'] in ('off', 'local') and _ftetwild_mod.ftetwild_available()
            and within_cap):
        tiers.append('ftetwild')
    if tiers:
        dr['available'] = {
            'holes_remaining': int(holes),
            'nm_remaining': int(nm),
            'tiers': tiers,
            'estimate_s': estimate_deep_repair_time(
                n_faces, holes + nm, tiers, spec.ftetwild_timeout),
        }


def _geom_change_pct(s1):
    """Geometry-change magnitude % for a stage1 dict, or None.

    ``max(|volume_change_percent|, |surface_area_change_percent|)`` — the
    worst geometric distortion. Reuses metrics the repair already computed
    (no recomputation)."""
    vals = []
    for key in ('volume_change_percent', 'surface_area_change_percent'):
        v = s1.get(key)
        if isinstance(v, (int, float)) and not isinstance(v, bool):
            vals.append(abs(float(v)))
    return max(vals) if vals else None


def _budget_message(budget):
    """Human-readable 'metric value (budget)' list for an exceeded budget."""
    parts = []
    for e in budget.get('exceeded', []):
        if e['metric'] == 'geometry_change':
            parts.append('geometry change %.2f%% (budget %.2f%%)'
                         % (e['value'], e['budget']))
        elif e['metric'] == 'risk':
            parts.append('risk %d (budget %d)' % (e['value'], e['budget']))
    return '; '.join(parts)


def budget_check(result, max_geom_change=None, max_risk=None):
    """Compare the repair's actual geometry change / risk against budgets.

    Both metrics are reused, never recomputed:
      - geometry change % = max(|volume|, |surface area|) change (stage1)
      - repair risk score (0-100) = repair_score.compute_scores() output
    For multi-object 3MF the WORST object drives each metric (per-object
    stage1 / repair_risk values); the report still carries the full per-object
    numbers.

    Returns None when no budget is set (no behaviour change); otherwise:
      {'geometry_change_pct': float|None, 'risk_score': int|None,
       'max_geometry_change_pct': float|None, 'max_risk_score': float|None,
       'within_budget': bool, 'exceeded': [{metric, value, budget}, ...]}.
    """
    if max_geom_change is None and max_risk is None:
        return None
    reports = result.get('object_reports')
    if reports:
        geoms, risks = [], []
        for rep in reports:
            g = _geom_change_pct(rep.get('stage1') or {})
            if g is not None:
                geoms.append(g)
            r = rep.get('repair_risk')
            if isinstance(r, (int, float)) and not isinstance(r, bool):
                risks.append(float(r))
        geom = max(geoms) if geoms else None
        risk = max(risks) if risks else None
    else:
        geom = _geom_change_pct(result.get('stage1') or {})
        risk = result.get('repair_risk')
        if not isinstance(risk, (int, float)) or isinstance(risk, bool):
            risk = None
        elif risk is not None:
            risk = float(risk)

    exceeded = []
    if max_geom_change is not None and geom is not None \
            and geom > float(max_geom_change):
        exceeded.append({'metric': 'geometry_change',
                         'value': round(geom, 2),
                         'budget': float(max_geom_change)})
    if max_risk is not None and risk is not None and risk > float(max_risk):
        exceeded.append({'metric': 'risk',
                         'value': int(round(risk)),
                         'budget': float(max_risk)})
    return {
        'geometry_change_pct': None if geom is None else round(geom, 2),
        'risk_score': None if risk is None else int(round(risk)),
        'max_geometry_change_pct':
            float(max_geom_change) if max_geom_change is not None else None,
        'max_risk_score': float(max_risk) if max_risk is not None else None,
        'within_budget': not exceeded,
        'exceeded': exceeded,
    }


def _confirm_budget_save(budget):
    """Prompt the user to confirm saving despite an exceeded budget.

    Only prompts on an interactive terminal (stdin is a TTY). Non-interactive
    callers (the GUI subprocess, scripts, file-manager integration) never
    hang and return False so the output is declined instead of saved
    silently."""
    if not sys.stdin.isatty():
        return False
    try:
        print('WARNING: the repair exceeded your budget (%s).'
              % _budget_message(budget), file=sys.stderr)
        answer = input('Save anyway? [y/N] ').strip().lower()
    except (EOFError, KeyboardInterrupt):
        return False
    return answer in ('y', 'yes')


def run_after_stage2_engines(ml, report, new_v, new_t, tmpdir, engines,
                             engine_chain, spec):
    """Run after_stage2 engines on the stage-2 result and adopt under the
    shared guard. Returns the (possibly replaced) ``(new_v, new_t)`` and
    appends one report entry per engine to ``report['engines']``."""
    if not engine_placement_order(engines, engine_chain, 'after_stage2'):
        return new_v, new_t
    entries = []
    base_holes = boundary_loop_stats(new_v, new_t)[0]
    base_ms = ml.MeshSet()
    base_ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(new_v, np.float32),
                             face_matrix=np.asarray(new_t, np.int32)))
    base_nm = base_ms.apply_filter('get_topological_measures').get(
        'non_two_manifold_edges', 0)
    cand_v, cand_t, adopted = run_engines(
        ml, engines, engine_chain, 'after_stage2', tmpdir, new_v, new_t,
        base_holes, base_nm, spec, entries)
    _record_engine_entries(report, entries)
    if adopted:
        return cand_v, cand_t
    return new_v, new_t


def repair_file(src, out, tmpdir, mode='auto', profile=None, engine='experimental',
                join_components=False, autorefine=False, ftetwild=False,
                indirect_autorefine=False, extra_features=False, deep_repair=None,
                triage_spec=None, engines=None, engine_chain=None,
                closing=None, proxy_template=False, repeat=None,
                repeat_source=None, repeat_target=None,
                 wall_thicken=False, wall_min_thickness=None, graft=False,
                 flap=False,
                 dressing=False, dressing_drain=None, dressing_defects=None,
                 dressing_rmax_scale=None, dressing_sigma_scale=None,
                 tamponade=False,
                 si_mode=None, dressing_force_adopt=False):
    """Repair a single STL/OBJ/3MF file. Returns the report dict."""
    import pymeshlab as ml

    bad_coords = scan_bad_coordinates(src)
    if bad_coords:
        raise ValueError('input mesh contains %s' % bad_coords)

    load_ms = ml.MeshSet()
    load_ms.load_new_mesh(src)
    verts = np.asarray(load_ms.current_mesh().vertex_matrix(), dtype=np.float32)
    tris = np.asarray(load_ms.current_mesh().face_matrix(), dtype=np.int32)

    ext = os.path.splitext(src)[1].lower()
    declared_unit = None
    if ext == '.3mf':
        units = read_3mf_units(src)
        if units:
            declared_unit = next(iter(units.values()))

    def _run(flap_value, run_tamponade=None):
        """Repair the loaded mesh with Flap on/off, through P-HONEST.

        ``run_tamponade`` defaults to the outer flag; the Flap-OFF safety-net
        re-run passes False so the Tamponade tier is not evaluated a
        second time for a fallback candidate that is usually discarded (the
        net keeps Flap-ON unless OFF is strictly better, and skipping the tier
        can only make OFF look no better, so the chosen result never worsens).
        """
        if run_tamponade is None:
            run_tamponade = tamponade
        report, new_v, new_t = repair_mesh_from_arrays(
            verts, tris, tmpdir, mode=mode, profile=profile, engine=engine,
            declared_unit=declared_unit, join_components=join_components,
            autorefine=autorefine, ftetwild=ftetwild,
            indirect_autorefine=indirect_autorefine,
            extra_features=extra_features, deep_repair=deep_repair,
            triage_spec=triage_spec, engines=engines, engine_chain=engine_chain,
            closing=closing, proxy_template=proxy_template, repeat=repeat,
            repeat_source=repeat_source, repeat_target=repeat_target,
            wall_thicken=wall_thicken, wall_min_thickness=wall_min_thickness,
            graft=graft, flap=flap_value,
            dressing=dressing, dressing_drain=dressing_drain,
            dressing_defects=dressing_defects,
            dressing_rmax_scale=dressing_rmax_scale,
            dressing_sigma_scale=dressing_sigma_scale,
            tamponade=run_tamponade,
            si_mode=si_mode,
            dressing_force_adopt=dressing_force_adopt)
        report['_fp'] = history.mesh_fingerprint(verts, tris)
        report['defects'] = detect_defects(verts, tris)
        if ext == '.obj':
            report['material_discarded'] = obj_has_material_refs(src)

        # Stage 2 applies to watertight results; shared helper keeps the single-
        # mesh path and per-object 3MF repair on the same code (same gate, same
        # OBJ round-trip, same explicit skip reporting).
        new_v, new_t = maybe_run_stage2(report, new_v, new_t, tmpdir)

        if engines:
            new_v, new_t = run_after_stage2_engines(
                ml, report, new_v, new_t, tmpdir, engines, engine_chain,
                triage_spec or triage.resolve_intensity(None))

        # P-WELD: reload-safe final pass. STL re-welds exactly-coincident
        # float32 vertices on load, so a clean index-topology mesh can reload
        # non-manifold (the stage-2 / closing seam case). Separate those
        # coincidences (or, when the welded mesh is itself repairable, repair
        # it) and adopt only a strictly reload-watertight result; never make
        # the output worse.
        if ext == '.stl':
            new_v, new_t, _pwel = p_weld_final(ml, new_v, new_t)
            if _pwel is not None:
                report['p_weld'] = _pwel

        # Localized exact self-union of residual SI clusters (experimental, OFF
        # by default). Runs on the post-Stage-2 + post-P-WELD mesh; a no-op that
        # returns the input arrays unchanged when disabled or when no cluster is
        # accepted.
        if _should_run_local_exact(report, triage_spec):
            _lse_spec = triage_spec or triage.resolve_intensity(None)
            new_v, new_t, _lse = local_exact.local_exact_self_union(
                ml, new_v, new_t, time_budget=_local_exact_budget(_lse_spec))
            if _lse.get('ran'):
                report['local_exact'] = _lse

        # Flap debris cleanup (only when Flap was adopted): drop the flat,
        # effectively zero-volume closed shells the repair left behind, so a
        # Flap-ON result is never worse than flap-OFF on the part count. Solid
        # parts are never dropped and the cleanup is adopted only when the
        # reload holes/non-manifold count does not rise; every dropped
        # component is reported. See drop_flap_debris.
        if (report.get('flap') or {}).get('adopted'):
            _cl_v, _cl_t, _cl_info = drop_flap_debris(verts, tris, new_v, new_t)
            if _cl_info:
                _bh, _bnm = reload_strict_holes_nm(new_v, new_t)
                _ch, _cnm = reload_strict_holes_nm(_cl_v, _cl_t)
                if _ch <= _bh and _cnm <= _bnm:
                    new_v, new_t = _cl_v, _cl_t
                    report['flap']['debris_cleanup'] = _cl_info

        # P-HONEST: the verdict must describe the mesh actually saved, not the
        # in-memory index topology. The final mesh is chosen above; judge it in
        # the save/reload-equivalent form and let that verdict win.
        enforce_reload_verdict(report, new_v, new_t)
        return report, new_v, new_t

    _t_file = time.perf_counter()
    report, new_v, new_t = _run(flap)

    # HIGH-1 safety net: the Flap adoption gate is topological-only and decided
    # on the pre-chain mesh, so it cannot guarantee the FINAL result beats
    # flap-OFF. When Flap was adopted and the final mesh is not strict-
    # watertight, or has more parts than the input, re-run this file ONCE with
    # Flap OFF and keep the better result. Rare (never fires on the 41-sample
    # regression, where Flap-ON is 41/41 watertight) and it reuses the same
    # code path, so it cannot diverge from a plain flap-OFF repair.
    if flap and (report.get('flap') or {}).get('adopted'):
        try:
            on_reason, on_metrics = _flap_final_needs_off(
                verts, tris, new_v, new_t)
        except Exception:  # noqa: BLE001 - the net never crashes a repair
            on_reason, on_metrics = None, None
        if on_reason:
            on_flap = dict(report['flap'])
            fallback = {'ran': False, 'used': 'on', 'reason': on_reason,
                        'on': on_metrics}
            use_off = False
            try:
                off_report, off_v, off_t = _run(False, run_tamponade=False)
                _, off_metrics = _flap_final_needs_off(
                    verts, tris, off_v, off_t)
                fallback['ran'] = True
                fallback['off'] = off_metrics
                fallback['tamponade_skipped'] = bool(tamponade)
                use_off = _flap_off_preferred(
                    (on_reason, on_metrics), (None, off_metrics))
            except Exception as e:  # noqa: BLE001 - the net never crashes
                fallback['error'] = str(e)
            fallback['used'] = 'off' if use_off else 'on'
            if use_off:
                on_flap['adopted_final'] = False
                on_flap['final_fallback'] = fallback
                off_report['flap'] = on_flap
                # Carry the primary run's tier verdict when it did not change
                # the geometry (a no-op record); an adopted tier changed the
                # flap-ON mesh, which is not the mesh now being reported.
                _on_tamp = report.get('tamponade')
                if tamponade and _on_tamp and not _on_tamp.get('adopted'):
                    off_report['tamponade'] = _on_tamp
                report, new_v, new_t = off_report, off_v, off_t
            else:
                report['flap']['final_fallback'] = fallback

    # Tamponade final safety net (G11): the Tamponade adoption gate is
    # topological-only and decided on the pre-chain / stage-1 mesh, so it
    # cannot guarantee the FINAL result keeps the input's shape. When a cheap
    # bounding-box check flags a collapse (the saved result no longer spans the
    # input), re-run this file ONCE with Tamponade OFF and keep the candidate
    # that covers more of the input without regressing damage or part count.
    # A no-op unless Tamponade was actually adopted and the final collapsed.
    if tamponade and (report.get('tamponade') or {}).get('adopted'):
        try:
            on_reason, on_metrics = _tamponade_final_needs_off(
                verts, tris, new_v, new_t)
        except Exception:  # noqa: BLE001 - the net never crashes a repair
            on_reason, on_metrics = None, None
        if on_reason:
            on_tamp = dict(report['tamponade'])
            fallback = {'ran': False, 'used': 'on', 'reason': on_reason,
                        'on': on_metrics}
            use_off = False
            try:
                off_report, off_v, off_t = _run(flap, run_tamponade=False)
                _, off_metrics = _tamponade_final_needs_off(
                    verts, tris, off_v, off_t, force_metrics=True)
                fallback['ran'] = True
                fallback['off'] = off_metrics
                use_off = _tamponade_off_preferred(on_metrics, off_metrics)
            except Exception as e:  # noqa: BLE001 - the net never crashes
                fallback['error'] = str(e)
            fallback['used'] = 'off' if use_off else 'on'
            if use_off:
                on_tamp['adopted_final'] = False
                on_tamp['final_fallback'] = fallback
                off_report['tamponade'] = on_tamp
                report, new_v, new_t = off_report, off_v, off_t
            else:
                report['tamponade']['final_fallback'] = fallback

    report.setdefault('timings', {})
    report['timings']['file_total'] = round(time.perf_counter() - _t_file, 3)
    report['timings']['flap_off_rerun'] = bool(
        (report.get('flap') or {}).get('final_fallback', {}).get('ran'))
    report['timings']['tamponade_off_rerun'] = bool(
        (report.get('tamponade') or {}).get('final_fallback', {}).get('ran'))
    save_mesh(out, new_v, new_t)
    return report


def repair_3mf(src, out, tmpdir, mode='auto', profile=None, engine='experimental',
               join_components=False, autorefine=False, ftetwild=False,
               indirect_autorefine=False, extra_features=False, deep_repair=None,
               triage_spec=None, engines=None, engine_chain=None,
               closing=None, proxy_template=False, repeat=None,
               repeat_source=None, repeat_target=None,
               wall_thicken=False, wall_min_thickness=None, graft=False,
               flap=False,
               dressing=False, dressing_drain=None, dressing_defects=None,
               dressing_rmax_scale=None, dressing_sigma_scale=None,
               tamponade=False,
               si_mode=None, dressing_force_adopt=False):
    """Repair every object mesh in a 3MF archive, preserving structure.

    Per-object Stage 2: any object that stage 1 closes (two-manifold with no
    remaining holes) is passed through the shared ``maybe_run_stage2`` helper,
    so a closed object gets a manifold3d watertight rebuild exactly like a
    single-mesh file (the helper sets the per-object ``stage2`` report and
    returns the rebuilt arrays). Objects that remain open after stage 1 are
    written as stage-1 output, mirroring the single-mesh behaviour.
    """
    import pymeshlab as ml
    meshes = parse_3mf_meshes(src)
    if not meshes:
        return {'error': 'no mesh objects found in 3MF'}

    with zipfile.ZipFile(src) as z:
        items = []
        for name in z.namelist():
            items.append((name, _read_zip_entry(z, name)))

    units = read_3mf_units(src)
    reports = []
    cache = {}
    for model_name, blocks in meshes.items():
        declared = units.get(model_name, 'millimeter')
        for idx, (verts, tris, span) in enumerate(blocks):
            key = (verts.tobytes(), tris.tobytes())
            if key in cache:
                # byte-identical geometry -> reuse the repair AND its report;
                # each object still gets its own per-object report (incl. the
                # stage2 outcome, which is a pure function of the geometry).
                new_v, new_t, rep = cache[key]
                reports.append(dict(rep))
            else:
                rep, new_v, new_t = repair_mesh_from_arrays(
                    verts, tris, tmpdir, mode=mode, profile=profile,
                    engine=engine, declared_unit=declared,
                    join_components=join_components,
                    autorefine=autorefine,
                    ftetwild=ftetwild,
                    indirect_autorefine=indirect_autorefine,
                    extra_features=extra_features,
                    deep_repair=deep_repair,
                    triage_spec=triage_spec, engines=engines,
                    engine_chain=engine_chain,
                    closing=closing, proxy_template=proxy_template,
                    repeat=repeat, repeat_source=repeat_source,
                    repeat_target=repeat_target, wall_thicken=wall_thicken,
                    wall_min_thickness=wall_min_thickness, graft=graft,
                    flap=flap,
                    dressing=dressing, dressing_drain=dressing_drain,
                    dressing_defects=dressing_defects,
                    dressing_rmax_scale=dressing_rmax_scale,
                    dressing_sigma_scale=dressing_sigma_scale,
                    tamponade=tamponade,
                    si_mode=si_mode,
                    dressing_force_adopt=dressing_force_adopt)
                rep['defects'] = detect_defects(verts, tris)
                rep['_fp'] = history.mesh_fingerprint(verts, tris)
                # Per-object stage 2 first: closed objects get a watertight
                # rebuild (same helper/gate as the single-mesh path), and the
                # confidence/health/risk below then see the stage2 outcome.
                new_v, new_t = maybe_run_stage2(rep, new_v, new_t, tmpdir)
                if engines:
                    new_v, new_t = run_after_stage2_engines(
                        ml, rep, new_v, new_t, tmpdir, engines, engine_chain,
                        triage_spec or triage.resolve_intensity(None))
                if _should_run_local_exact(rep, triage_spec):
                    _lse_spec = triage_spec or triage.resolve_intensity(None)
                    new_v, new_t, _lse = local_exact.local_exact_self_union(
                        ml, new_v, new_t,
                        time_budget=_local_exact_budget(_lse_spec))
                    if _lse.get('ran'):
                        rep['local_exact'] = _lse
                # Flap debris cleanup (only when Flap was adopted); see the
                # single-mesh path and drop_flap_debris.
                if (rep.get('flap') or {}).get('adopted'):
                    _cl_v, _cl_t, _cl_info = drop_flap_debris(
                        verts, tris, new_v, new_t)
                    if _cl_info:
                        _bh, _bnm = reload_strict_holes_nm(new_v, new_t)
                        _ch, _cnm = reload_strict_holes_nm(_cl_v, _cl_t)
                        if _ch <= _bh and _cnm <= _bnm:
                            new_v, new_t = _cl_v, _cl_t
                            rep['flap']['debris_cleanup'] = _cl_info
                # P-HONEST: judge the per-object mesh actually written into the
                # archive (reload-equivalent form) before the confidence/score
                # below, so those see the honest verdict too.
                enforce_reload_verdict(rep, new_v, new_t)
                _rc = repair_confidence(rep)
                rep['repair_confidence'] = _rc['score']
                rep['repair_confidence_label'] = _rc['label']
                rep['repair_confidence_factors'] = _rc['factors']
                _rs = repair_score.compute_scores(rep)
                rep['repair_health'] = _rs['health']
                rep['repair_health_factors'] = _rs['health_factors']
                rep['repair_risk'] = _rs['risk']
                rep['repair_risk_factors'] = _rs['risk_factors']
                rep['repair_status'] = _rs['status']
                rep['repair_status_code'] = _rs['status_code']
                reports.append(rep)
                cache[key] = (new_v, new_t, rep)

            for i, (fname, fdata) in enumerate(items):
                if fname != model_name:
                    continue
                xml = fdata.decode('utf-8', errors='replace')
                rebuilt = list(xml)
                rebuilt[span[0]:span[1]] = build_mesh_block(new_v, new_t)
                items[i] = (fname, ''.join(rebuilt).encode('utf-8'))

    with zipfile.ZipFile(out, 'w', zipfile.ZIP_DEFLATED) as z:
        for fname, fdata in items:
            z.writestr(fname, fdata)

    agg = {'objects': len(meshes), 'object_reports': reports}
    if reports:
        # backward compat: the top-level stage1 stays object-0's (classification
        # now evaluates object_reports directly for multi-object files).
        agg['stage1'] = reports[0].get('stage1', {})
        agg['objects_watertight'] = sum(
            1 for r in reports
            if r.get('stage1', {}).get('two_manifold')
            and r.get('stage1', {}).get('holes_remaining', 0) == 0
            and bool(r.get('stage2', {}).get('ok')))
        agg['objects_stage2_ok'] = sum(
            1 for r in reports if bool(r.get('stage2', {}).get('ok')))
        s2 = next((r['stage2'] for r in reports if 'stage2' in r), None)
        if s2 is not None:
            agg['stage2'] = s2
        # Flap aggregate (only when Flap was requested, so an OFF multi-object
        # file keeps the pre-flap aggregate byte-identical): lets a 3MF batch be
        # scanned for the Flap outcome without walking object_reports.
        _flaps = [r['flap'] for r in reports
                  if isinstance(r.get('flap'), dict)]
        if _flaps:
            agg['flap'] = {
                'objects': len(_flaps),
                'ran': sum(1 for f in _flaps if f.get('ran')),
                'adopted': sum(1 for f in _flaps if f.get('adopted')),
                'final_fallback_off': sum(
                    1 for f in _flaps
                    if (f.get('final_fallback') or {}).get('used') == 'off'),
            }
    return agg


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


def human_defects(r):
    """Render the defect list for --human mode (only with --defects)."""
    d = r.get('defects')
    if not d:
        return None
    lines = ['', 'Defects (input):']
    holes = d.get('holes', [])
    nm = d.get('non_manifold', [])
    if not holes and not nm:
        lines.append('  none')
    for h in holes:
        c = h['centroid']
        lines.append('  hole: centroid=(%.3f, %.3f, %.3f), diameter=%.3f mm, %d verts'
                     % (c[0], c[1], c[2], h['diameter'], h['vertices']))
    for r_ in nm:
        c = r_['centroid']
        lines.append('  non-manifold: centroid=(%.3f, %.3f, %.3f), %d faces'
                     % (c[0], c[1], c[2], r_['faces']))
    return '\n'.join(lines)


def human_report(r, show_defects=False, show_diff=False):
    if 'error' in r and 'stage1' not in r:
        return 'ERROR: %s' % r['error']
    s1 = r.get('stage1', {})
    lines = []
    lines.append('Input : %s' % r.get('input'))
    lines.append('Output: %s' % r.get('output'))
    lines.append('Mode  : %s' % r.get('repair_mode', 'auto'))
    if r.get('unit_warning'):
        lines.append('WARNING: %s' % r.get('unit_hint'))
    if r.get('status') == 'budget_declined':
        lines.append('ERROR: %s' % r.get('error'))
    if r.get('classifier_engine') is not None:
        lines.append('Classifier: %s' % r['classifier_engine'])
    if r.get('repair_profile'):
        lines.append('Profile: %s' % r['repair_profile'])
    lines.append('Intensity: %s' % r.get('triage_intensity', 'balanced'))
    dt = r.get('detected_type')
    if dt:
        conf = r.get('detected_confidence', 0.0)
        tuning = r.get('tuning_applied')
        line = 'Type  : %s (confidence %.2f)' % (dt, conf)
        if tuning is not None:
            line += ' - tuned thresholds' if tuning else ' - default thresholds (below confidence gate)'
        lines.append(line)
    rc = r.get('repair_confidence')
    if rc is not None:
        lines.append('Confidence: %d/100 (%s)'
                     % (rc, str(r.get('repair_confidence_label', '?')).title()))
    rh = r.get('repair_health')
    rk = r.get('repair_risk')
    if rh is not None or rk is not None:
        lines.append('Health: %s/100   Risk: %s/100   Status: %s' % (
            'n/a' if rh is None else rh,
            'n/a' if rk is None else rk,
            r.get('repair_status', 'n/a')))
    mh = r.get('method_health')
    if mh:
        lines.append(method_health.human_line(mh))
        for _e in mh.get('errors') or []:
            lines.append('  ! %s [%s]: %s' % (
                _e.get('method'), _e.get('stage'), _e.get('message')))
    b = r.get('budget')
    if b is not None:
        lines.append('Budget: geometry change %s%% (limit %s) · risk %s '
                     '(limit %s) · %s' % (
                         'n/a' if b.get('geometry_change_pct') is None
                         else b.get('geometry_change_pct'),
                         'n/a' if b.get('max_geometry_change_pct') is None
                         else b.get('max_geometry_change_pct'),
                         'n/a' if b.get('risk_score') is None
                         else b.get('risk_score'),
                         'n/a' if b.get('max_risk_score') is None
                         else b.get('max_risk_score'),
                         'EXCEEDED' if not b.get('within_budget')
                         else 'within budget'))
    if r.get('material_discarded'):
        lines.append('Material: input OBJ has mtllib/usemtl references which '
                     'are not preserved in the repaired output.')
    lines.append('')
    lines.append('Stage 1 (MeshLab):')
    lines.append('  Holes closed            : %d' % s1.get('holes_closed', 0))
    lines.append('  Holes remaining         : %d' % s1.get('holes_remaining', 0))
    lines.append('  Non-manifold edges fixed: %d' % s1.get('non_manifold_edges_fixed', 0))
    lines.append('  Faces removed           : %d' % s1.get('faces_removed', 0))
    lines.append('  Connected components    : %d' % s1.get('components', 0))
    lines.append('  Two-manifold            : %s' % ('YES' if s1.get('two_manifold') else 'NO'))
    pv = r.get('pinched_vertices_split')
    if pv:
        lines.append('  Pinched vertices split  : %d -> %d (%d pass(es))%s'
                     % (pv.get('before', 0), pv.get('after', 0), pv.get('passes', 0),
                        '' if pv.get('adopted') else ', not kept'))
    pw = r.get('p_weld')
    if pw:
        state = ('%s adopted' % pw.get('method')) if pw.get('applied') \
            else 'no improvement (kept reload-welded state)'
        lines.append('  Reload-safe final pass  : holes %d -> %d, non-manifold %d -> %d (%s)'
                     % (pw.get('holes_before', 0), pw.get('holes_after', 0),
                        pw.get('non_manifold_before', 0),
                        pw.get('non_manifold_after', 0), state))
    le_r = r.get('local_exact')
    if le_r and le_r.get('ran'):
        if le_r.get('applied'):
            state = '%d/%d cluster(s) accepted' % (le_r.get('accepted', 0),
                                                   le_r.get('clusters', 0))
        else:
            state = 'no cluster accepted (%s)' % (le_r.get('reason') or '?')
        lines.append('  Local exact self-union : SI %d -> %s, %s in %.2fs'
                     % (le_r.get('si_before', 0), le_r.get('si_after'),
                        state, le_r.get('seconds', 0)))
    if r.get('extreme_passes_applied'):
        lines.append('  Extreme passes          : applied (%d self-intersecting face(s) removed)'
                     % r.get('self_intersections_removed', 0))
    elif r.get('repair_mode') == 'extreme':
        lines.append('  Extreme passes          : none needed (no self-intersections)')
    ar_r = r.get('experimental_autorefine')
    if ar_r:
        if ar_r.get('skipped'):
            lines.append('  Autorefine (experiment) : skipped (no triangles)')
        elif 'error' in ar_r:
            lines.append('  Autorefine (experiment) : error (%s)' % ar_r['error'][:60])
        else:
            conv = 'yes' if ar_r.get('converged') else ('no (capped)' if ar_r.get('capped') else 'no')
            adopted = ' adopted' if ar_r.get('adopted') else ' NOT adopted (kept stage-1 output)'
            lines.append('  Autorefine (experiment) : SI pairs %d -> %d, %d pass(es), '
                         'converged=%s%s' % (ar_r.get('si_before', 0),
                                             ar_r.get('si_after', 0),
                                             ar_r.get('iterations', 0), conv, adopted))
    ft_r = r.get('experimental_ftetwild')
    if ft_r:
        if ft_r.get('reject_reason') == 'too_large':
            lines.append('  fTetWild fallback : skipped (%d faces, limit %d)'
                         % (ft_r.get('input_faces', 0), ft_r.get('max_faces', 0)))
        elif ft_r.get('aborted'):
            lines.append('  fTetWild fallback : %s'
                         % (ft_r.get('reason')
                            or 'stopped for time; previous stage kept'))
        elif ft_r.get('ran') and 'error' in ft_r:
            lines.append('  fTetWild fallback : error (%s)'
                         % ft_r['error'][:60])
        elif ft_r.get('ran'):
            if ft_r.get('adopted') and ft_r.get('shape_changed'):
                adopted = (' adopted, SHAPE CHANGED (Hausdorff %.1f%% of the '
                           'diagonal)' % (100 * (ft_r.get('hausdorff_rel') or 0)))
            elif ft_r.get('adopted'):
                adopted = ' adopted'
            else:
                adopted = ' NOT adopted (kept stage-1 output)'
            pp = ' + manifold3d post-process' if ft_r.get('manifold_postprocessed') else ''
            if ft_r.get('adopted_attempt') == 'optimize':
                pp += ' (second attempt, optimisation on)'
            if ft_r.get('ftetwild_decimated'):
                pp += ', decimated %d->%d faces' % (
                    ft_r.get('ftetwild_faces_raw', 0),
                    ft_r.get('ftetwild_faces_decimated',
                             ft_r.get('ftetwild_faces_final', 0)))
            lines.append('  fTetWild fallback : %d faces in %.2fs%s, '
                         'holes=%s non-manifold=%s%s' % (
                             ft_r.get('output_faces', 0), ft_r.get('time', 0), pp,
                             ft_r.get('output_holes'), ft_r.get('output_non_manifold'),
                             adopted))
    fl_r = r.get('flap')
    if fl_r and fl_r.get('ran'):
        if fl_r.get('adopted'):
            state = ' adopted'
        else:
            state = ' NOT adopted (kept stage-1 output)'
        lines.append('  Flap (surface fill)     : filled %s/%s loops, '
                     '%s patch faces in %.2fs, max original move %.3g%s'
                     % (fl_r.get('loops_filled'), fl_r.get('loops_found'),
                        fl_r.get('patch_faces'), fl_r.get('time_s') or 0,
                        fl_r.get('max_orig_vertex_move_mm') or 0, state))
    tp_r = r.get('tamponade')
    if tp_r and tp_r.get('ran'):
        if tp_r.get('adopted'):
            state = ' adopted'
        else:
            state = ' NOT adopted (%s)' % (tp_r.get('reason') or '?')
        lines.append('  Tamponade (plug)        : %s plugs, %s plug faces, '
                     'holes=%s nm=%s, healthy kept %s in %.2fs%s%s'
                     % (tp_r.get('plugs'), tp_r.get('plug_faces'),
                        tp_r.get('candidate_holes'),
                        tp_r.get('candidate_non_manifold'),
                        tp_r.get('healthy_faces_kept'),
                        tp_r.get('seconds') or 0, state,
                        '' if not tp_r.get('plugs_rolled_back')
                        else ', rolled back %s'
                        % tp_r.get('plugs_rolled_back')))
    gr = r.get('graft')
    if gr and gr.get('ran'):
        if gr.get('adopted'):
            state = ' adopted%s' % (
                '' if gr.get('fidelity_ok') is not False
                else ' (LOW FIDELITY: %s)' % (gr.get('warnings') or [{}])[0].get(
                    'message_en', 'detail loss'))
        else:
            state = ' NOT adopted (kept stage-1 output)'
        lines.append('  Graft (shell wrap)      : %s faces in %.2fs, r=%s, %s%s'
                     % (gr.get('faces', 0), gr.get('seconds', 0),
                        gr.get('r_used'), gr.get('mode'), state))
        _gf = gr.get('grid_fallback')
        if _gf:
            lines.append('    Graft grid            : %s grid does not fit '
                         'in RAM; used Balanced grid'
                         % (_gf.get('intensity') or 'preset').capitalize())
        elif gr.get('low_memory'):
            lines.append('    Graft grid            : %s'
                         % (gr.get('reason') or 'low memory'))
        elif gr.get('aborted'):
            lines.append('    Graft                 : %s'
                         % (gr.get('reason')
                            or 'stopped for time; stage-1 result kept'))
        elif gr.get('budget_limited_by') == 'work':
            lines.append('    Graft                 : hit its deterministic '
                         'work budget; stage-1 result kept')
    dr_r = r.get('dressing')
    if dr_r and dr_r.get('ran'):
        if dr_r.get('adopted'):
            state = ' adopted%s' % (
                '' if dr_r.get('fidelity_ok') is not False
                else ' (LOW FIDELITY: %.2f%% of diag)'
                % (100.0 * (dr_r.get('hausdorff_rel_max') or 0)))
        else:
            state = ' NOT adopted (%s)' % (dr_r.get('reason') or '?')
        _dd = dr_r.get('drain') or {}
        _drain_txt = ('drain=%s' % _dd.get('mode')
                      if _dd and _dd.get('drained')
                      else 'drain=off')
        lines.append('  Dressing (viscosity)    : %s faces in %.2fs, '
                     'voxel=%s, r=%s, %s, holes=%s nm=%s%s'
                     % (dr_r.get('faces') or dr_r.get('faces_coat', 0),
                        dr_r.get('seconds', 0), dr_r.get('voxel'),
                        dr_r.get('r_base'), _drain_txt, dr_r.get('holes'),
                        dr_r.get('non_manifold'), state))
    eng_r = r.get('engines')
    if eng_r:
        lines.append('')
        lines.append('External engines:')
        for e in eng_r:
            if e.get('adopted'):
                if e.get('shape_changed'):
                    state = ('adopted, SHAPE CHANGED (Hausdorff %.1f%% of the '
                             'diagonal)' % (100 * (e.get('hausdorff_rel') or 0)))
                else:
                    state = 'adopted'
            else:
                state = 'NOT adopted (%s)' % (e.get('reject_reason') or '?')
            lines.append('  %s [%s] : %s, rc=%s, %.2fs' % (
                e.get('name'), e.get('placement'), state, e.get('rc'),
                e.get('time', 0)))
    dr = r.get('deep_repair')
    if dr:
        lr = dr.get('local')
        if lr:
            if 'error' in lr:
                lines.append('  Local re-mesh (experiment) : error (%s)' % lr['error'][:60])
            else:
                res = ('adopted' if lr.get('adopted')
                       else 'NOT adopted (%s)' % lr.get('reject_reason', '?'))
                lines.append('  Local re-mesh (experiment) : %d region(s), %d face(s) removed, %s added, '
                             'holes %s -> %s, non-manifold %s -> %s, %s' % (
                                 lr.get('regions', 0), lr.get('faces_removed', 0),
                                 lr.get('faces_added', '-'), lr.get('holes_before'),
                                 lr.get('holes_after', '-'), lr.get('nm_before'),
                                 lr.get('nm_after', '-'), res))
        av = dr.get('available')
        if av:
            est = ', '.join('%s ~%ss' % (k, v) for k, v in av['estimate_s'].items())
            lines.append('  Deep repair available : %d hole(s), %d non-manifold '
                         'edge(s) left; %s' % (av['holes_remaining'],
                                               av['nm_remaining'], est))
    cl_r = r.get('closing')
    if cl_r:
        if not cl_r.get('ran'):
            lines.append('  Closing (method %s) : not run (%s)'
                         % (cl_r.get('tier'), cl_r.get('reason')))
        else:
            state = ('adopted' if cl_r.get('adopted')
                     else 'NOT adopted (%s)' % (cl_r.get('reason') or '?'))
            note = (' - %s' % '; '.join(cl_r.get('notes') or [])
                    if cl_r.get('notes') else '')
            lines.append('  Closing (method %s) : %s faces, holes=%s '
                         'non-manifold=%s, %s%s' % (
                             cl_r.get('tier'), cl_r.get('faces'),
                             cl_r.get('holes'), cl_r.get('non_manifold'),
                             state, note))
    rp_r = r.get('repeat')
    if rp_r:
        if not rp_r.get('ran'):
            lines.append('  Repeat repair (method %s) : not run (%s)'
                         % (rp_r.get('mode'), rp_r.get('reason')))
        else:
            state = ('transplanted' if rp_r.get('adopted')
                     else 'NOT adopted (%s)' % (rp_r.get('reason') or '?'))
            lines.append('  Repeat repair (method %s) : pattern=%s, %s '
                         'position(s) repaired, %s' % (
                             rp_r.get('mode'), rp_r.get('pattern_type'),
                             rp_r.get('positions_repaired'), state))
    ia_r = r.get('experimental_indirect_autorefine')
    if ia_r:
        if ia_r.get('skipped'):
            lines.append('  Indirect autorefine (experiment) : skipped (%s)'
                         % (ia_r.get('error') or 'no triangles')[:60])
        elif 'error' in ia_r:
            lines.append('  Indirect autorefine (experiment) : error (%s)'
                         % ia_r['error'][:60])
        else:
            adopted = ' adopted' if ia_r.get('adopted') else ' NOT adopted (kept stage-1 output)'
            lines.append('  Indirect autorefine (experiment) : SI pairs %d -> %d, '
                         'faces %d -> %d%s' % (ia_r.get('si_before', 0),
                                               ia_r.get('si_after', 0),
                                               ia_r.get('faces_before', 0),
                                               ia_r.get('faces_after', 0),
                                               adopted))
    if show_diff:
        lines.append('  Vertices                : %s -> %s' % (
            s1.get('vertices_before', 0), s1.get('vertices_after', 0)))
        lines.append('  Faces                   : %s -> %s' % (
            s1.get('faces_before', 0), s1.get('faces_after', 0)))
        if s1.get('surface_area_change_percent') is not None:
            lines.append('  Surface area change     : %s%%' % s1.get('surface_area_change_percent'))
    if s1.get('volume_change_percent') is not None:
        lines.append('  Volume change          : %s%%' % s1.get('volume_change_percent'))
    if s1.get('volume_warning'):
        lines.append('')
        lines.append('  WARNING: %s' % s1['volume_warning'])
    if 'stage2' in r:
        s2 = r['stage2']
        lines.append('')
        lines.append('Stage 2 (Manifold):')
        if 'error' in s2:
            if s2['error'].startswith('Stage 2 skipped'):
                lines.append('  SKIPPED: %s' % s2['error'])
            else:
                lines.append('  ERROR: %s' % s2['error'])
        else:
            for k, v in s2.items():
                lines.append('  %s: %s' % (k, v))
    if 'objects' in r:
        lines.append('')
        lines.append('3MF objects repaired: %d' % r['objects'])
        if r.get('objects_watertight') is not None:
            lines.append('Objects watertight: %d/%d' % (
                r.get('objects_watertight'), len(r.get('object_reports', []))))
        for i, rep in enumerate(r.get('object_reports', [])):
            s1o = rep.get('stage1', {})
            s2o = rep.get('stage2') or {}
            if s2o.get('ok'):
                label = 'watertight'
            elif s1o.get('two_manifold') and s1o.get('holes_remaining', 0) == 0:
                label = ('stage 2 skipped'
                         if s2o.get('error', '').startswith('Stage 2 skipped')
                         else 'stage 2 error')
            else:
                label = 'partial'
            lines.append('  object %d: %s (%d hole(s) remaining, two-manifold=%s)' % (
                i, label,
                s1o.get('holes_remaining', 0), 'YES' if s1o.get('two_manifold') else 'NO'))
            if rep.get('unit_warning'):
                lines.append('    WARNING: %s' % rep.get('unit_hint'))
            if show_diff:
                lines.append('    vertices %s -> %s, faces %s -> %s, surface %s%%' % (
                    s1o.get('vertices_before', 0), s1o.get('vertices_after', 0),
                    s1o.get('faces_before', 0), s1o.get('faces_after', 0),
                    s1o.get('surface_area_change_percent', 0)))
            if show_defects:
                ds = human_defects(rep)
                if ds:
                    lines.append(ds)
    elif show_defects:
        ds = human_defects(r)
        if ds:
            lines.append(ds)
    for _sug in r.get('suggestions', []):
        if _sug.get('method') == 'dressing':
            lines.append('')
            lines.append(_dressing_suggestion_text(_sug))
    return '\n'.join(lines)


def human_validate(r, show_defects=False):
    """Render a validate report for --human mode."""
    if 'error' in r and 'validation' not in r:
        return 'ERROR: %s' % r['error']
    lines = ['Input : %s' % r.get('input')]
    if 'object_reports' in r:
        lines.append('3MF objects validated: %d' % r.get('objects', 0))
        for i, rep in enumerate(r.get('object_reports', [])):
            v = rep.get('validation', {})
            lines.append('  object %d (%s): %d verts, %d faces, %d hole(s), '
                         '%d non-manifold region(s), %d self-intersection(s), watertight=%s' % (
                i, rep.get('model', '?'), v.get('vertices', 0), v.get('faces', 0),
                len(v.get('holes', [])), len(v.get('non_manifold', [])),
                v.get('self_intersections', 0), 'YES' if v.get('watertight') else 'NO'))
            if v.get('unit_warning'):
                lines.append('    WARNING: %s' % v.get('unit_hint'))
        return '\n'.join(lines)
    v = r.get('validation', {})
    if v.get('unit_warning'):
        lines.append('WARNING: %s' % v.get('unit_hint'))
    lines.append('Type  : %s (confidence %.2f)' % (
        r.get('detected_type', '?'), r.get('detected_confidence', 0.0)))
    if r.get('classifier_engine') is not None:
        lines.append('Classifier: %s' % r['classifier_engine'])
    ec = r.get('estimated_confidence')
    if ec is not None:
        lines.append('Estimated confidence: %d/100 (%s) — actual result may differ after repair'
                     % (ec, str(r.get('estimated_confidence_label', '?')).title()))
    lines.append('')
    lines.append('Validation:')
    lines.append('  Vertices            : %d' % v.get('vertices', 0))
    lines.append('  Faces               : %d' % v.get('faces', 0))
    lines.append('  Holes               : %d' % len(v.get('holes', [])))
    lines.append('  Non-manifold regions: %d' % len(v.get('non_manifold', [])))
    lines.append('  Self-intersections  : %d' % v.get('self_intersections', 0))
    lines.append('  Connected components: %d' % v.get('connected_components', 0))
    lines.append('  Watertight          : %s' % ('YES' if v.get('watertight') else 'NO'))
    lines.append('  Signed volume       : %.6f' % v.get('signed_volume', 0.0))
    lines.append('  Surface area        : %.3f' % v.get('surface_area', 0.0))
    lines.append('  Orientation         : %s' % v.get('orientation', '?'))
    if show_defects:
        for h in v.get('holes', []):
            c = h['centroid']
            lines.append('    hole: centroid=(%.3f, %.3f, %.3f), diameter=%.3f, %d verts'
                         % (c[0], c[1], c[2], h['diameter'], h['vertices']))
        for nm in v.get('non_manifold', []):
            c = nm['centroid']
            lines.append('    non-manifold: centroid=(%.3f, %.3f, %.3f), %d faces'
                         % (c[0], c[1], c[2], nm['faces']))
    return '\n'.join(lines)


def human_dry_run(r):
    """Render a --dry-run report for --human mode."""
    if 'error' in r and 'repair_mode' not in r:
        return 'ERROR: %s' % r['error']
    lines = ['Input : %s' % r.get('input'),
             'Dry-run: no output file will be written']
    if 'object_reports' in r:
        lines.append('3MF objects: %d' % r.get('objects', 0))
        for i, rep in enumerate(r.get('object_reports', [])):
            lines.append('  object %d (%s): mode=%s, type=%s, holes=%d, '
                         'non-manifold=%d, debris=%d face(s), self-intersections=%d' % (
                i, rep.get('model', '?'), rep.get('repair_mode', '?'),
                rep.get('detected_type', '?'), rep.get('holes_found', 0),
                rep.get('non_manifold_regions', 0), rep.get('debris_faces_removable', 0),
                rep.get('self_intersections', 0)))
            if rep.get('unit_warning'):
                lines.append('    WARNING: %s' % rep.get('unit_hint'))
        return '\n'.join(lines)
    lines.append('Mode  : %s' % r.get('repair_mode', 'auto'))
    if r.get('unit_warning'):
        lines.append('WARNING: %s' % r.get('unit_hint'))
    lines.append('Type  : %s (confidence %.2f)' % (
        r.get('detected_type', '?'), r.get('detected_confidence', 0.0)))
    if r.get('classifier_engine') is not None:
        lines.append('Classifier: %s' % r['classifier_engine'])
    pa = r.get('would_apply', {})
    tuning = 'tuned thresholds' if r.get('tuning_applied') else 'default thresholds'
    lines.append('Tuning: %s' % tuning)
    ec = r.get('estimated_confidence')
    if ec is not None:
        lines.append('Estimated confidence: %d/100 (%s) — actual result may differ after repair'
                     % (ec, str(r.get('estimated_confidence_label', '?')).title()))
    lines.append('Would apply: mincomponentsize=%d, maxholesize=%d' % (
        pa.get('mincomponentsize', 0), pa.get('maxholesize', 0)))
    lines.append('Found  : %d hole(s) (largest %.3f), %d non-manifold region(s), '
                 '%d self-intersection(s), %d debris face(s) (< mincomponentsize)' % (
        r.get('holes_found', 0), r.get('largest_hole_diameter', 0.0),
        r.get('non_manifold_regions', 0), r.get('self_intersections', 0),
        r.get('debris_faces_removable', 0)))
    s2 = r.get('stage2_bridge_available')
    lines.append('Stage 2: %s' % (
        'would run if stage 1 closes the mesh (bridge available)' if s2
        else 'not available on this system'))
    if r.get('flap_step'):
        lines.append('Flap  : would run first (pre-pass on the input)')
    return '\n'.join(lines)


def process_file(src, human, mode='auto', profile=None, no_history=False,
                 out=None, engine='experimental', max_geom_change=None,
                 max_risk=None, force=False, join_components=False,
                 autorefine=False, ftetwild=False, indirect_autorefine=False,
                 extra_features=False, deep_repair=None, triage_spec=None,
                 engines=None, engine_chain=None, engine_warnings=None,
                 methods=None, engine_filter=None, repeat_source=None,
                 repeat_target=None, wall_min_thickness=None, graft=False,
                 flap=False,
                 dressing=False, dressing_drain=None, dressing_defects=None,
                 dressing_rmax_scale=None, dressing_sigma_scale=None,
                 tamponade=False,
                 si_mode=None, dressing_force_adopt=False):
    """Repair one file. Returns (result_dict, category).

    ``max_geom_change``/``max_risk`` (optional repair budgets) gate the save:
    when the actual geometry change % / repair risk exceeds a budget, the
    temp output is only moved into place after explicit confirmation (an
    interactive ``[y/N]`` prompt on a TTY) or ``force``. Without a budget or
    within budget the save is unconditional (reporting the numbers).
    ``join_components`` enables the experimental join-small-components
    prototype (flag-gated). ``autorefine`` enables the experimental
    self-intersection subdivision prototype (flag-gated). ``ftetwild`` enables
    the experimental fTetWild fallback tier (flag-gated). ``indirect_autorefine``
    enables the experimental exact indirect-predicate arrangement-lite split
    (flag-gated).
    ``triage_spec`` is the resolved intensity preset (sutura/triage.py),
    forwarded to the repair; None resolves to Balanced.
    ``engines``/``engine_chain``/``engine_warnings`` are the ``load_engine_run``
    output; when the first two are None the engines are loaded here (once per
    call). Load warnings are recorded in the report and never fail a repair.
    ``methods`` is the optional user-tagged method list (``--methods``); None
    means auto mode (today's default pipeline first, ranked fallbacks only when
    it fails), implemented by ``sutura/methods.py``.
    ``engine_filter`` is the optional list of external-engine names
    (``--engines``): when given, only those engines run (at their configured
    placement); None keeps the default of running every enabled engine.
    """
    if not os.path.exists(src):
        return ({'input': src, 'error': 'file not found: %s' % src}, 'error')

    if engines is None and engine_chain is None:
        engines, engine_chain, engine_warnings = load_engine_run()
    if engine_filter is not None:
        _want = set(engine_filter)
        engines = {n: c for n, c in (engines or {}).items() if n in _want}
        engine_chain = [n for n in (engine_chain or []) if n in _want]

    stem, ext = os.path.splitext(src)
    if out is None:
        out = stem + '_fixed' + ext

    tmpdir = tempfile.mkdtemp(prefix='sutura-')
    tmp_out = os.path.join(tmpdir, 'out' + ext)
    result = {'input': src, 'output': out}
    if engine_warnings:
        result['engine_warnings'] = list(engine_warnings)
    t0 = time.perf_counter()
    # One central error-isolation scope for this file: every tier/method run
    # records its failure through sutura_engine.health; the run is summarised
    # below as result['method_health'].
    method_health.begin()
    try:
        # Method registry / exec policy (sutura/methods.py): with methods=None
        # this is auto mode -- it runs today's default pipeline first and only
        # escalates to ranked methods when that baseline is not strict-
        # watertight, so an untagged repair stays byte-identical to today.
        result.update(method_registry.repair_with_methods(
            src, tmp_out, tmpdir, methods=methods,
            # An explicit --engines tag ("only these engines") must not silently
            # auto-escalate to extra repair methods; --methods is unaffected.
            auto_escalation=(engine_filter is None),
            mode=mode, profile=profile,
            engine=engine, join_components=join_components,
            autorefine=autorefine, ftetwild=ftetwild,
            indirect_autorefine=indirect_autorefine,
            extra_features=extra_features, deep_repair=deep_repair,
            triage_spec=triage_spec, engines=engines,
            engine_chain=engine_chain, repeat_source=repeat_source,
            repeat_target=repeat_target, wall_min_thickness=wall_min_thickness,
            graft=graft, flap=flap,
            dressing=dressing, dressing_drain=dressing_drain,
            dressing_defects=dressing_defects,
            dressing_rmax_scale=dressing_rmax_scale,
            dressing_sigma_scale=dressing_sigma_scale,
            tamponade=tamponade,
            si_mode=si_mode,
            dressing_force_adopt=dressing_force_adopt))

        # Post-repair confidence + Health/Risk scores (needed for the budget
        # gating below). Multi-object 3MF carries these per object already.
        # Fail-silent: repair_score.compute_scores never raises.
        if 'object_reports' not in result:
            _rc = repair_confidence(result)
            result['repair_confidence'] = _rc['score']
            result['repair_confidence_label'] = _rc['label']
            result['repair_confidence_factors'] = _rc['factors']
            try:
                _rs = repair_score.compute_scores(result)
                result['repair_health'] = _rs['health']
                result['repair_health_factors'] = _rs['health_factors']
                result['repair_risk'] = _rs['risk']
                result['repair_risk_factors'] = _rs['risk_factors']
                result['repair_status'] = _rs['status']
                result['repair_status_code'] = _rs['status_code']
            except Exception:
                pass

        # Repair budget: compare actual geometry change + risk against the
        # user's budgets; an exceeded budget requires confirmation before the
        # temp output is moved into place (or --force). Never saved silently.
        budget = budget_check(result, max_geom_change, max_risk)
        if budget is not None:
            result['budget'] = budget
        if budget is not None and not budget['within_budget']:
            if force or _confirm_budget_save(budget):
                os.replace(tmp_out, out)
            else:
                # Unambiguous, machine-detectable outcome: distinct from a
                # generic 'error' (corrupt file, crash) so the GUI can offer
                # a --force re-run without wrongly offering it for other
                # hard failures.
                result['status'] = 'budget_declined'
                result['error'] = (
                    'Save declined: the repair exceeded your budget (%s). '
                    'No output was written; re-run with --force to save '
                    'anyway.' % _budget_message(budget))
                if 'object_reports' not in result:
                    _rc = repair_confidence(result)
                    result['repair_confidence'] = _rc['score']
                    result['repair_confidence_label'] = _rc['label']
                    result['repair_confidence_factors'] = _rc['factors']
        elif os.path.exists(tmp_out):
            os.replace(tmp_out, out)
    except ExtremeRemovedAllError as e:
        result['error'] = str(e)
        result['extreme_removed_object'] = True
    except Exception as e:
        result['error'] = 'repair failed: %s' % e
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)
    # Central method-health section: availability of every registered method
    # overlaid with this run's recorded failures (the wrapper wrote them into
    # the scope opened above).
    result['method_health'] = method_health.build_report()
    method_health.end()
    elapsed_ms = int((time.perf_counter() - t0) * 1000)

    category, issues, _summary = classify(result)
    result['category'] = category
    result['issues'] = issues
    # The Dressing opt-in hint is kept SEPARATE from the issue codes: it is a
    # suggestion (a next action), not a defect in this result, so the category
    # and the batch issue_counts stay unchanged. Only set when non-empty.
    _suggestions = _dressing_suggestions(result)
    if _suggestions:
        result['suggestions'] = _suggestions

    _fps = history.pop_fingerprints(result)
    if not no_history:
        history.write(result, VERSION, elapsed_ms, _fps)
    return result, category


def _confirm_cli(action, yes):
    """Require confirmation for a destructive CLI action (install/uninstall),
    unless ``--yes``. Non-interactive without --yes refuses (never hangs)."""
    if yes:
        return True
    if not sys.stdin.isatty():
        return False
    try:
        ans = input('Proceed with %s? [y/N] ' % action).strip().lower()
    except EOFError:
        return False
    return ans in ('y', 'yes')


def run_engines_cli(args, human=False):
    """``sutura engines list|check``: validate the TOML configs and resolve
    the binaries WITHOUT running anything. Returns an exit code."""
    sub = args[0] if args else 'list'
    if sub not in ('list', 'check'):
        print(json.dumps({'error': "engines: subcommand must be 'list' or 'check'"}))
        return 1
    try:
        import engines as _engines
    except Exception as e:  # noqa: BLE001
        print(json.dumps({'error': 'engines module unavailable: %s' % e}))
        return 1
    eng, chain, warnings = load_engine_run()
    ordered = [n for n in chain if n in eng]
    ordered += [n for n in sorted(eng) if n not in ordered]
    items = []
    problems = list(warnings)
    for name in ordered:
        cfg = eng[name]
        found = bool(_engines.is_engine_available(cfg))
        item = {
            'name': cfg.name,
            'placement': cfg.placement,
            'enabled': cfg.enabled,
            'in_chain': name in chain,
            'binary_found': found,
            'command': cfg.command,
            'input_format': cfg.input_format,
            'output_format': cfg.output_format,
            'timeout': cfg.timeout,
            'source': str(cfg.source_path) if cfg.source_path else None,
        }
        items.append(item)
        if cfg.enabled and not found:
            problems.append("engine '%s': executable %r not found"
                            % (name, cfg.command[0] if cfg.command else '?'))
    report = {
        'config_dir': str(_engines.get_engines_dir()),
        'chain': chain,
        'engines': items,
        'warnings': problems,
    }
    if sub == 'check':
        report['ok'] = not problems
    if human:
        print('Engines (%s):' % report['config_dir'])
        if not items:
            print('  none configured')
        for it in items:
            print('  %-20s %-16s enabled=%-5s binary=%s%s' % (
                it['name'], it['placement'], it['enabled'],
                'found' if it['binary_found'] else 'MISSING',
                '' if it['in_chain'] else ' (not in chain)'))
        for w in problems:
            print('  ! %s' % w)
    else:
        print(json.dumps(report, ensure_ascii=False))
    return 0 if not (sub == 'check' and problems) else 1


def run_ftetwild_cli(args, dry_run=False, yes=False, human=False):
    """``sutura ftetwild status|install|uninstall`` via ``ftetwild_manager``.

    ``--dry-run`` never touches the environment (it prints the plan/estimate);
    install/uninstall require ``--yes`` or an interactive confirmation, and
    always show the download / installed / freed size first. Returns an exit
    code."""
    sub = args[0] if args else 'status'
    if sub not in ('status', 'install', 'uninstall'):
        print(json.dumps({'error': "ftetwild: subcommand must be "
                                   "'status', 'install' or 'uninstall'"}))
        return 1
    try:
        import ftetwild_manager as mgr
    except Exception as e:  # noqa: BLE001
        print(json.dumps({'error': 'ftetwild manager unavailable: %s' % e}))
        return 1

    if sub == 'status':
        st = mgr.status()
        st['estimate'] = mgr.estimate()
        if human:
            if not st.get('supported'):
                print('fTetWild: unsupported (%s)' % st.get('reason'))
            elif st.get('installed'):
                print('fTetWild: installed v%s (%s) at %s'
                      % (st.get('version'), st.get('size_human'), st.get('location')))
            else:
                print('fTetWild: not installed (supported; download ~%s, '
                      'installed ~%s)' % (st['estimate']['download_human'],
                                          st['estimate']['installed_human']))
        else:
            print(json.dumps(st, ensure_ascii=False))
        return 0 if st.get('supported') else 1

    if sub == 'install':
        est = mgr.estimate()
        if dry_run:
            plan = mgr.install(dry_run=True)
            print(json.dumps({'estimate': est, 'plan': plan}, ensure_ascii=False))
            return 0 if plan.get('ok') else 1
        print('fTetWild install: download ~%s, installed ~%s'
              % (est['download_human'], est['installed_human']), file=sys.stderr)
        if not _confirm_cli('fTetWild install', yes):
            print(json.dumps({'error': 'install cancelled (use --yes to confirm)'}))
            return 1
        res = mgr.install(progress_cb=lambda s: print(s, file=sys.stderr))
        print(json.dumps({'result': res, 'estimate': est}, ensure_ascii=False))
        return 0 if res.get('ok') else 1

    # uninstall
    plan = mgr.uninstall(dry_run=True)
    if dry_run:
        print(json.dumps({'plan': plan}, ensure_ascii=False))
        return 0 if plan.get('ok') else 1
    print('fTetWild uninstall: %d package(s), frees ~%s'
          % (len(plan.get('packages', [])), plan.get('freed_human', '0 B')),
          file=sys.stderr)
    if not _confirm_cli('fTetWild uninstall', yes):
        print(json.dumps({'error': 'uninstall cancelled (use --yes to confirm)'}))
        return 1
    res = mgr.uninstall(progress_cb=lambda s: print(s, file=sys.stderr))
    print(json.dumps({'result': res}, ensure_ascii=False))
    return 0 if res.get('ok') else 1


def main():
    try:
        from sutura_engine import paths as _paths
        _data = _paths.ensure_data_dir()
        for _err in _data.get('errors', []):
            sys.stderr.write('sutura: user-data migration warning: %s\n' % _err)
    except Exception:
        pass
    import argparse
    parser = argparse.ArgumentParser(
        prog='sutura',
        description='Sutura Triage Engine: staged robust mesh repair for STL, OBJ, and 3MF files.',
        epilog=(
            'subcommands (first argument):\n'
            '  validate FILE          read-only mesh validation (no repair, no output file)\n'
            '  clear-cache            clear content-addressed Chart cache (~/.cache/sutura/)\n'
            '  clear-learning         reset learning-triage history (triage_learning.json)\n'
            '  export-history         view or export anonymous technical usage history\n'
            '  engines list|check     inspect configured third-party repair engines\n'
            '  ftetwild status|install|uninstall  manage optional fTetWild fallback extra\n'
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('files', nargs='*', metavar='FILE',
                        help='input mesh file(s)')
    parser.add_argument('-o', '--output', metavar='OUTPUT',
                        help='output file (only valid with a single input)')
    parser.add_argument('--human', action='store_true',
                        help='print a human-readable report')
    parser.add_argument('--defects', action='store_true',
                        help='with --human, also list input defects (holes / '
                             'non-manifold regions). JSON always includes defects.')
    parser.add_argument('--diff', action='store_true',
                        help='with --human, also show before/after geometry '
                             'diff (vertices, faces, surface area change). '
                             'JSON always includes these fields.')
    parser.add_argument('--mode', choices=REPAIR_MODES, default='auto',
                        help='repair mode: low/medium/aggressive/extreme use '
                             'fixed Stage 1 thresholds; auto (default) uses '
                             'the mesh classifier + confidence gate.')
    parser.add_argument('--classifier-engine', choices=CLASSIFIER_ENGINES,
                        default=None,
                        help='mesh classifier engine: experimental (default) '
                             'is the RANSAC + trained-head engine; classic '
                             'selects the original heuristic. Also selectable '
                             'via SUTURA_CLASSIFIER_ENGINE.')
    parser.add_argument('--profile', choices=REPAIR_PROFILES, default=None,
                        help='named Stage 1 threshold preset: mechanical, '
                             'organic, scan, miniature or fast. Only effective '
                             'with mode auto; an explicit fixed mode wins.')
    parser.add_argument('--intensity', default=None, metavar='NAME',
                        help='Triage Engine intensity for the effort after '
                             'Stage 1: a built-in preset (quick, balanced '
                             '(default; byte-identical to the historical '
                             'defaults), thorough, extreme) or a named user '
                             'profile from ~/.config/sutura/profiles.json. '
                             'Stage 1 is NOT affected (use --mode/--profile). '
                             'Also from SUTURA_INTENSITY or the "intensity" '
                             'key of ~/.config/sutura/config.json. See '
                             '--list-intensities.')
    parser.add_argument('--list-intensities', action='store_true',
                        help='list the built-in intensity presets and the '
                             'user profiles with their effective values, then '
                             'exit (no repair)')
    parser.add_argument('--list-methods', action='store_true',
                        help='list the repair methods (num, id, family, '
                             'availability) and exit (no repair)')
    parser.add_argument('--analyze', action='store_true',
                        help='analyze the input object(s) -- defects, mesh '
                             'type, self-intersection load -- and print the '
                             'ranked method recommendations plus the external '
                             'engines. Writes no output file.')
    parser.add_argument('--methods', default=None, metavar='LIST',
                        help='tag the repair methods to try, in order, e.g. '
                             '--methods 2,3,5. Without it the repair is auto: '
                             "today's default pipeline first, with ranked "
                             'fallbacks only when it fails. BREAKING (0.7.0): '
                             "in 0.6.0 '13'/'graft' meant the manual "
                             "picked-points method; that is now #12 "
                             "'transplant_plus', and '13'/'graft' is the new "
                             'Graft (shell wrap).')
    parser.add_argument('--engines', default=None, metavar='LIST',
                        help='tag the configured external engines to run, e.g. '
                             '--engines meshfix,my-engine. When given, only '
                             'those enabled engines run (at their configured '
                             'placement); without it every enabled engine runs '
                             'as configured. Engines are never mixed into the '
                             'method ranking (see `engines list`).')
    parser.add_argument('--repeat-source', default=None, metavar='X,Y,Z',
                        help='method 12 (repeat_manual): the 3D point on the '
                             'healthy repeated element to copy from.')
    parser.add_argument('--wall-min-thickness', type=float, default=None,
                        metavar='T',
                        help='target minimum wall thickness for the opt-in '
                             'Wall Thicken method (#15); default 1%% of the '
                             'bounding-box diagonal. Ignored unless #15 is '
                             'tagged via --methods.')
    parser.add_argument('--repeat-target', default=None, metavar='X,Y,Z',
                        help='method 12 (repeat_manual): the 3D point on the '
                             'damaged repeated element to replace.')
    parser.add_argument('--json', action='store_true',
                        help='force machine-readable JSON output (the default '
                             'format; overrides --human)')
    parser.add_argument('--no-learning-triage', action='store_true',
                        help='disable the bounded local-history ranking bonus '
                             '(learning triage) for this run')
    parser.add_argument('--no-history', action='store_true',
                        help='do not write the anonymous usage history record '
                             '(mesh geometry + repair results only, never file '
                             'names or paths)')
    parser.add_argument('--no-cache', action='store_true',
                        help='disable content-addressed Sutura Chart cache (~/.cache/sutura/)')
    parser.add_argument('--diagnostics', metavar='ZIP_FILE', default=None,
                        help='collect system diagnostics, extension symbols, method '
                             'availability, sanitized config, and last repair history '
                             'into a ZIP archive and exit')
    parser.add_argument('--self-repair', action='store_true',
                        help='verify installed application files against the release '
                             'manifest and safely download replacements')
    parser.add_argument('--self-check', action='store_true',
                        help='run health and self-check diagnostics on all registered repair methods')
    parser.add_argument('--max-geometry-change', type=float, default=None,
                        metavar='PCT',
                        help='repair budget: warn/block when the actual geometry '
                             'change (max of |volume| and |surface area| change '
                             '%%) exceeds PCT. On a TTY you are prompted to '
                             'confirm; non-interactive runs decline the save '
                             'unless --force is given (0 disables the budget)')
    parser.add_argument('--max-risk', type=float, default=None, metavar='SCORE',
                        help='repair budget: warn/block when the repair risk '
                             'score (0-100) exceeds SCORE. Same confirmation '
                             'behaviour as --max-geometry-change (0 disables)')
    parser.add_argument('--force', action='store_true',
                        help='with --max-geometry-change/--max-risk: save the '
                             'output even when the budget is exceeded, without '
                             'asking')
    parser.add_argument('--last', type=int, default=0, metavar='N',
                        help='export-history: only the last N records')
    parser.add_argument('--clear', action='store_true',
                        help='export-history: clear the history file')
    parser.add_argument('--summary-only', action='store_true',
                        help='export-history: print only the summary')
    parser.add_argument('--dry-run', action='store_true',
                        help='do not repair: report what would be done and '
                             'write no output file')
    parser.add_argument('-y', '--yes', action='store_true',
                        help='confirm self-repair or ftetwild install/uninstall '
                             'without an interactive prompt')
    parser.add_argument('--experimental-join-components', action='store_true',
                        help='experimental prototype: move small connected '
                             'components onto the nearest larger component '
                             'instead of deleting them (changes geometry; '
                             'NOT the default behaviour, evaluation only)')
    parser.add_argument('--experimental-autorefine', action='store_true',
                        help='experimental prototype: resolve self-'
                             'intersections by subdividing the intersecting '
                             'triangles along their intersection segments '
                             '(Lazard & Valque 2025) instead of deleting '
                             'faces. NEVER deletes input faces; on moderate-'
                             'SI meshes it reduces SI, on dense-SI scans it '
                             'is limited by float64 construction (see '
                             'docs/alpha-wrap-feasibility-2026-09.md). NOT '
                             'the default behaviour, evaluation only')
    parser.add_argument('--no-fallback-ftetwild', action='store_true',
                        help='disable the fTetWild fallback tier. By default, '
                             'when the optional fTetWild extra is installed '
                             '(SUTURA_WITH_FTETWILD=1) and the stage-1 chain '
                             'still leaves holes or non-manifold edges, the '
                             'ORIGINAL input is tetrahedralized and its '
                             'boundary extracted as a watertight, SI-free '
                             'surface (fTetWild via pytetwild, MPL-2.0); it is '
                             'adopted only when no worse on holes+non-manifold '
                             'than the stage-1 result. Without the extra this '
                             'flag changes nothing')
    parser.add_argument('--experimental-fallback-ftetwild', action='store_true',
                        help='run the fTetWild fallback tier also when the '
                             'stage-1 result is closed but still '
                             'self-intersects (slow: up to %d s per mesh; '
                             'remeshes such meshes), and report an explicit '
                             'skip when fTetWild is not installed' % FTETWILD_TIMEOUT)
    parser.add_argument('--ftetwild-optimize', action='store_true',
                        help='let fTetWild also optimise the quality of its '
                             'tetrahedra (off by default: only the boundary '
                             'surface is used, and the optimisation mostly '
                             'lengthens repairs of complex parts). Default from '
                             '%s or the "ftetwild_optimize" key of '
                             '~/.config/sutura/config.json' % FTETWILD_OPTIMIZE_ENV)
    parser.add_argument('--no-graft', action='store_true',
                        help='disable the Graft (shell wrap) tier (#13). By '
                             'default the deep-repair ladder tries Graft '
                             'before fTetWild on the original input and adopts '
                             'its reload-watertight result; when Graft reports '
                             'fidelity_ok=false, fTetWild is also tried and '
                             'the lower healthy-deviation result wins')
    parser.add_argument('--experimental-graft', action='store_true',
                        help='force the Graft (shell wrap) tier (#13) even when '
                             'the deep-repair ladder would not run it (e.g. '
                             'with --deep-repair off). Primarily for its own '
                             'evaluation; the auto path (default) already '
                             'prefers Graft over fTetWild')
    parser.add_argument('--flap', action='store_true',
                        help='run the Flap surface-based hole filler as the '
                             'FIRST Stage-1 step (a pre-pass on the input, '
                             'before the VCG chain): every open boundary loop '
                             'is covered with a minimum-area triangulation '
                             'refined to the surrounding edge length and faired '
                             'with a thin-plate solve that continues the '
                             'neighbouring surface. Original triangles are kept '
                             'verbatim and the result is adopted only when the '
                             'reload-honest holes + non-manifold count does not '
                             'worsen; repair_file re-runs with Flap OFF if the '
                             'final mesh still ends up worse. On by default; '
                             'env: SUTURA_FLAP / SUTURA_FLAP_DEFAULT')
    parser.add_argument('--no-flap', action='store_true',
                        help='disable the Flap hole filler (it is on by default)')
    parser.add_argument('--tamponade', action='store_true',
                        help='enable the Tamponade tier (#17): it is ON by '
                             'default (this flag is kept for compatibility); '
                             'disable with --no-tamponade. Runs first as a '
                             'pre-pass on the cleaned input before Flap and '
                             'Stage 1 hole closing, with a second chance after '
                             'Flap and before Graft/fTetWild: plugs each open '
                             'hole from the INSIDE (Stent inflates from inside '
                             'and freezes at the shell; large holes split into '
                             'several Stents; no input face is deleted) and '
                             'melts residual self-intersections in rolled-back '
                             'layers. Adopted only when the reload-honest holes '
                             '+ non-manifold count strictly falls without '
                             'raising the part count or losing more than 1%% of '
                             'the healthy faces; a final safety net re-runs '
                             'without it if the result collapses or leaks; '
                             'env: SUTURA_TAMPONADE / SUTURA_TAMPONADE_DEFAULT')
    parser.add_argument('--no-tamponade', action='store_true',
                        help='disable the Tamponade tier (#17); it is on by '
                             'default, so this turns it off')
    parser.add_argument('--no-dressing', action='store_true',
                        help='disable the Dressing (viscosity coat) tier (#16); '
                             'this is the default (Dressing is opt-in)')
    parser.add_argument('--experimental-dressing', action='store_true',
                        help='enable the Dressing (viscosity coat) tier (#16): '
                             'extract a variable-thickness isosurface of the '
                             'generalized-winding signed field (2-manifold and '
                             'self-intersection-free by construction). Opt-in '
                             'until measured on the corpus; tried after Graft '
                             'and before fTetWild')
    parser.add_argument('--dressing-drain', default=None,
                        help='Dressing (#16) healthy-region erode-back: a mode '
                             '(none, half, full, deep), an explicit delta_r in '
                             'mm, or unset for the intensity preset default '
                             '(Quick off; Balanced/Thorough full; Extreme deep). '
                             'Applied in the level set (F = s - r + delta_r), '
                             'never by vertex projection')
    parser.add_argument('--dressing-defects', choices=DRESSING_DEFECT_SETS,
                        default=None,
                        help='Dressing (#16) viscosity-mask defect set: "all" '
                             '(default) = holes + non-manifold + '
                             'self-intersections, "holes_nm" = holes + '
                             'non-manifold only (self-intersections are '
                             'ignored). Env: SUTURA_DRESSING_DEFECTS')
    parser.add_argument('--dressing-rmax-scale', type=float, default=None,
                        metavar='F',
                        help='Dressing (#16) multiplier on the resolved r_max '
                             '(bridging radius); default 1.0. A value < 1 gives '
                             'a narrower coat. Env: SUTURA_DRESSING_RMAX_SCALE')
    parser.add_argument('--dressing-sigma-scale', type=float, default=None,
                        metavar='F',
                        help='Dressing (#16) multiplier on the resolved sigma '
                             '(defect-influence width); default 1.0. Env: '
                             'SUTURA_DRESSING_SIGMA_SCALE')
    parser.add_argument('--dressing-force-adopt', action='store_true',
                        help='Dressing (#16) skip the shape-preservation '
                             'gates (volume / component count / healthy-surface '
                             'normal angle / coat fidelity) and adopt a '
                             'gate-rejected coat that is still strictly '
                             'watertight and self-intersection-free. The shape '
                             'may deform; offered by the non-watertight '
                             'suggestion. Implies --experimental-dressing')
    parser.add_argument('--deep-repair', choices=DEEP_REPAIR_MODES, default=None,
                        help='deep-repair ladder after the fast repair, when '
                             'holes or non-manifold edges remain: "full" '
                             '(default) runs the fTetWild tier as before, '
                             '"local" re-meshes only the damaged regions '
                             '(experimental prototype, the rest of the '
                             'surface is kept unchanged), "off" runs no tier '
                             'and only reports what is available with a '
                             'rough time estimate. Default from %s or the '
                             '"deep_repair" key of ~/.config/sutura/config.json'
                             % DEEP_REPAIR_ENV)
    parser.add_argument('--si-mode', choices=SI_MODES, default=None,
                        help='self-intersection policy: "report" (default) '
                             'measures and reports self-intersections but '
                             'never escalates the deep-repair ladder or fails '
                             'a result on self-intersections alone (holes and '
                             'non-manifold edges always count); "repair" treats '
                             'a residual self-intersection count as damage and '
                             'runs the automatic Dressing fallback; "off" '
                             'skips the exact self-intersection classifier and '
                             'reports it as not measured. Explicit '
                             '--experimental-fallback-ftetwild / '
                             '--experimental-dressing still act on '
                             'self-intersections. Env: %s' % SI_MODE_ENV)
    parser.add_argument('--experimental-indirect-autorefine', action='store_true',
                        help='experimental prototype: exact arrangement-lite '
                             'self-intersection split via the rust/sutura-geom '
                             'extension (indirect predicates: broad phase + '
                             'exact triangle-triangle classifier + per-triangle '
                             '2D CDT + exact-rational welding). Resolves '
                             'proper intersections by subdivision instead of '
                             'face deletion. Adopted only when no worse on '
                             'holes+non-manifold than the stage-1 result '
                             '(same guard as --experimental-autorefine). NOT '
                             'the default behaviour, evaluation only')
    parser.add_argument('--experimental-edge-tiebreak', action='store_true',
                        help='experimental opt-in: use the 11-feature '
                             'classifier head (base features + the five strong '
                             'FAZ10 scan signals). NOT the default; the gain is '
                             'marginal (1 mesh on the 71-mesh labeled set).')
    parser.add_argument('--version', action='version', version='%(prog)s ' + VERSION)
    args = parser.parse_args()

    # --list-intensities: print the presets and user profiles, then exit.
    if args.list_intensities:
        print(triage.format_intensities())
        sys.exit(0)

    # --list-methods: print the method registry, then exit (no repair).
    if args.list_methods:
        if args.json:
            print(json.dumps(method_registry.methods_json(), ensure_ascii=False))
        else:
            print(method_registry.format_methods())
        sys.exit(0)

    # --intensity accepts a built-in preset OR a user profile; argparse cannot
    # know the profiles at parser-construction time, so validate here.
    _profiles = triage.load_profiles()
    if (args.intensity is not None and args.intensity not in triage.PRESETS
            and args.intensity not in _profiles):
        _valid = list(triage.INTENSITIES) + sorted(_profiles)
        parser.error("argument --intensity: invalid choice: %r (choose from %s)"
                     % (args.intensity, ', '.join(map(repr, _valid))))

    files = args.files
    out = args.output
    human = args.human and not args.json
    show_defects = args.defects
    show_diff = args.diff
    mode = args.mode
    dry_run = args.dry_run
    engine = resolve_classifier_engine(args.classifier_engine)
    si_mode = resolve_si_mode(args.si_mode)
    triage_spec = triage.resolve_intensity(args.intensity,
                                           user_profiles=_profiles)

    # --methods: tag the repair methods (validated against the registry).
    methods_sel = None
    if args.methods:
        parts = [p.strip() for p in args.methods.split(',') if p.strip()]
        if not parts:
            parser.error('argument --methods: empty list')
        nums = []
        for part in parts:
            m = method_registry.get_method(part)
            if m is None:
                parser.error('argument --methods: unknown method %r '
                             '(see --list-methods)' % part)
            nums.append(m.num)
        methods_sel = nums

    # --repeat-source / --repeat-target: the 3D points method 12 transplants
    # between. Validated as "X,Y,Z" and required together when 12 is tagged.
    def _parse_point(text, flag):
        parts = [p.strip() for p in text.split(',')]
        if len(parts) != 3:
            parser.error('argument %s: expected X,Y,Z' % flag)
        try:
            return tuple(float(p) for p in parts)
        except ValueError:
            parser.error('argument %s: coordinates must be numbers' % flag)

    repeat_source = (_parse_point(args.repeat_source, '--repeat-source')
                     if args.repeat_source else None)
    repeat_target = (_parse_point(args.repeat_target, '--repeat-target')
                     if args.repeat_target else None)
    if methods_sel is not None and 12 in methods_sel:
        if repeat_source is None or repeat_target is None:
            parser.error('--methods 12 requires --repeat-source X,Y,Z and '
                         '--repeat-target X,Y,Z')

    # --engines: tag specific external engines (names validated against the
    # configured engines below, after they are loaded).
    engines_sel = None
    if args.engines:
        parts = [p.strip() for p in args.engines.split(',') if p.strip()]
        if not parts:
            parser.error('argument --engines: empty list')
        engines_sel = parts

    # 'engines list|check' and 'ftetwild status|install|uninstall' are
    # subcommands, dispatched before any repair (like validate/export-history).
    if files and files[0] == 'engines':
        if out is not None or dry_run:
            print(json.dumps({'error': 'engines takes no -o/--dry-run'}))
            sys.exit(1)
        sys.exit(run_engines_cli(files[1:], human=human))
    if files and files[0] == 'ftetwild':
        if out is not None:
            print(json.dumps({'error': 'ftetwild takes no -o'}))
            sys.exit(1)
        sys.exit(run_ftetwild_cli(files[1:], dry_run=dry_run, yes=args.yes,
                                  human=human))

    # Repair budgets: validate the ranges, then 0 / unset both mean "no
    # budget" (disabled), matching the GUI's 0 = no limit convention.
    if args.max_geometry_change is not None and args.max_geometry_change < 0:
        print(json.dumps({'error': '--max-geometry-change must be >= 0'}))
        sys.exit(1)
    if args.max_risk is not None and not (0 <= args.max_risk <= 100):
        print(json.dumps({'error': '--max-risk must be between 0 and 100'}))
        sys.exit(1)
    max_geom_change = args.max_geometry_change or None
    max_risk = args.max_risk or None

    if len(files) > 1 and out is not None:
        print(json.dumps({'error': '-o cannot be used with multiple input files'}))
        sys.exit(1)

    if getattr(args, 'no_learning_triage', False):
        os.environ[history.LEARNING_ENV] = '0'

    if getattr(args, 'no_cache', False):
        try:
            from sutura_engine import chart as cache
            cache.set_cache_enabled(False)
        except Exception:
            pass

    if getattr(args, 'diagnostics', None):
        try:
            from sutura.diagnostics import run_diagnostics_cli
        except ImportError:
            from diagnostics import run_diagnostics_cli
        code = run_diagnostics_cli(args.diagnostics)
        sys.exit(code)

    if getattr(args, 'self_repair', False):
        try:
            from sutura.self_repair import run_self_repair_cli
        except ImportError:
            from self_repair import run_self_repair_cli
        code = run_self_repair_cli(force_yes=getattr(args, 'yes', False))
        sys.exit(code)

    if getattr(args, 'self_check', False):
        try:
            from sutura.self_check import run_self_check_cli
        except ImportError:
            from self_check import run_self_check_cli
        code = run_self_check_cli()
        sys.exit(code)

    # 'clear-cache' as the first positional argument clears ~/.cache/sutura/
    if files and files[0] == 'clear-cache':
        try:
            from sutura_engine import chart as cache
            freed = cache.clear_cache()
            if human:
                print('Cache cleared: %s freed' % cache._human_bytes(freed))
            else:
                print(json.dumps({'cleared': True, 'freed_bytes': freed, 'freed_human': cache._human_bytes(freed)}))
        except Exception as e:
            print(json.dumps({'error': 'clear-cache failed: %s' % e}))
            sys.exit(1)
        sys.exit(0)

    # 'clear-learning' as the first positional argument resets the local
    # learning-triage counts (the bounded ranking bonus) without touching the
    # usage history.
    if files and files[0] == 'clear-learning':
        if len(files) > 1 or out is not None or dry_run or human:
            print(json.dumps({'error': 'clear-learning takes no positional '
                                       'arguments'}))
            sys.exit(1)
        removed = history.clear_learning()
        print(json.dumps({'cleared': removed}))
        sys.exit(0)

    # 'export-history' as the first positional argument prints the anonymous
    # usage history (summary + full JSON array) instead of repairing.
    if files and files[0] == 'export-history':
        if len(files) > 1 or out is not None or human or dry_run:
            print(json.dumps({'error': 'export-history takes no positional '
                                       'arguments (flags: --last N, --clear, '
                                       '--summary-only)'}))
            sys.exit(1)
        history.export_history(last=args.last,
                               summary_only=args.summary_only,
                               clear=args.clear)
        sys.exit(0)

    # 'validate' as the first positional argument switches to validate-only
    # mode: analyze the mesh(es) without repairing or writing anything.
    if files and files[0] == 'validate':
        targets = files[1:]
        if not targets:
            print(json.dumps({'error': 'validate requires at least one input file'}))
            sys.exit(1)
        if out is not None:
            print(json.dumps({'error': '-o is not valid with validate'}))
            sys.exit(1)
        if dry_run:
            print(json.dumps({'error': '--dry-run is not valid with validate'}))
            sys.exit(1)
        if methods_sel is not None:
            print(json.dumps({'error': '--methods is not valid with validate'}))
            sys.exit(1)
        if engines_sel is not None:
            print(json.dumps({'error': '--engines is not valid with validate'}))
            sys.exit(1)
        results = [validate_file(f, engine, si_mode=si_mode) for f in targets]
        nerr = sum(1 for r in results if 'error' in r)
        if len(targets) == 1:
            result = results[0]
            if human:
                print(human_validate(result, show_defects=show_defects))
            else:
                print(json.dumps(result, ensure_ascii=False))
            sys.exit(0 if nerr == 0 else 1)
        if human:
            for result in results:
                print(human_validate(result, show_defects=show_defects))
                print()
        else:
            print(json.dumps({'files': results}, ensure_ascii=False))
        sys.exit(0 if nerr == 0 else 1)

    # --analyze: read-only per-object analysis + ranked method recommendations.
    if args.analyze:
        if out is not None:
            print(json.dumps({'error': '-o is not valid with --analyze'}))
            sys.exit(1)
        if dry_run:
            print(json.dumps({'error': '--analyze is not valid with --dry-run'}))
            sys.exit(1)
        if methods_sel is not None:
            print(json.dumps({'error': '--methods is not valid with --analyze'}))
            sys.exit(1)
        if engines_sel is not None:
            print(json.dumps({'error': '--engines is not valid with --analyze'}))
            sys.exit(1)
        if not files:
            print(json.dumps({'error': '--analyze requires at least one input file'}))
            sys.exit(1)
        results = [method_registry.analyze_report(
            f, engine, args.experimental_edge_tiebreak) for f in files]
        nerr = sum(1 for r in results if 'error' in r)
        if len(files) == 1:
            result = results[0]
            if human:
                print(method_registry.format_analyze_human(result))
            else:
                print(json.dumps(result, ensure_ascii=False))
            sys.exit(0 if nerr == 0 else 1)
        if human:
            for result in results:
                print(method_registry.format_analyze_human(result))
                print()
        else:
            print(json.dumps({'files': results}, ensure_ascii=False))
        sys.exit(0 if nerr == 0 else 1)

    # --dry-run: analyze and report the plan, write no output file at all.
    if dry_run:
        if out is not None:
            print(json.dumps({'error': '-o is not valid with --dry-run'}))
            sys.exit(1)
        if methods_sel is not None:
            print(json.dumps({'error': '--methods is not valid with --dry-run'}))
            sys.exit(1)
        if engines_sel is not None:
            print(json.dumps({'error': '--engines is not valid with --dry-run'}))
            sys.exit(1)
        results = [dry_run_file(f, mode=mode, profile=args.profile, engine=engine,
                                triage_spec=triage_spec, si_mode=si_mode,
                                flap=resolve_flap(no_flap=args.no_flap,
                                                  force=args.flap))
                   for f in files]
        nerr = sum(1 for r in results if 'error' in r)
        if len(files) == 1:
            result = results[0]
            if human:
                print(human_dry_run(result))
            else:
                print(json.dumps(result, ensure_ascii=False))
            sys.exit(0 if nerr == 0 else 1)
        if human:
            for result in results:
                print(human_dry_run(result))
                print()
        else:
            print(json.dumps({'files': results}, ensure_ascii=False))
        sys.exit(0 if nerr == 0 else 1)

    if args.ftetwild_optimize:
        # read by ftetwild_params() inside the fTetWild tier
        os.environ[FTETWILD_OPTIMIZE_ENV] = '1'
    # An explicit deep-repair / fTetWild flag always wins over the intensity
    # preset; otherwise the preset supplies the deep-repair tier defaults.
    if (args.deep_repair is not None or args.no_fallback_ftetwild
            or args.experimental_fallback_ftetwild):
        _dr_mode, _ft_arg = resolve_deep_repair_flags(
            args.deep_repair, args.no_fallback_ftetwild,
            args.experimental_fallback_ftetwild)
    else:
        _dr_mode = triage_spec.deep_repair
        _ft_arg = 'auto' if triage_spec.ftetwild_enabled else False
    # Graft (#13): CLI flag > env > 'auto' (tried by the deep-repair ladder
    # before fTetWild). --experimental-graft forces it outside the ladder.
    _graft_arg = resolve_graft(no_graft=args.no_graft,
                               force=args.experimental_graft)
    # Flap: on by default; --no-flap disables it, --flap forces it, SUTURA_FLAP
    # may set 0/1 and SUTURA_FLAP_DEFAULT overrides the built-in default.
    _flap_arg = resolve_flap(no_flap=args.no_flap, force=args.flap)
    # Tamponade (#17): the single default switch (TAMPONADE_DEFAULT_ENABLED /
    # SUTURA_TAMPONADE_DEFAULT) decides; --no-tamponade disables and
    # --tamponade forces. ON by default.
    _tamponade_arg = resolve_tamponade(no_tamponade=args.no_tamponade,
                                       force=args.tamponade)
    # Dressing (#16): the single default switch (DRESSING_DEFAULT_ENABLED /
    # SUTURA_DRESSING_DEFAULT) decides; --no-dressing disables and
    # --experimental-dressing forces.
    _dressing_arg = resolve_dressing(no_dressing=args.no_dressing,
                                     force=args.experimental_dressing)
    # --dressing-force-adopt can only act on a coat, so it implies enabling
    # Dressing even when --no-dressing / the default switch would disable it.
    if args.dressing_force_adopt:
        _dressing_arg = True
    _dressing_drain = resolve_dressing_drain(args.dressing_drain)
    _dressing_defects = resolve_dressing_defects(args.dressing_defects)
    _dressing_rmax_scale = resolve_dressing_rmax_scale(args.dressing_rmax_scale)
    _dressing_sigma_scale = resolve_dressing_sigma_scale(
        args.dressing_sigma_scale)
    # External engines are loaded ONCE for the whole batch; broken configs
    # only warn and never fail a repair.
    _engines, _engine_chain, _engine_warnings = load_engine_run()
    if engines_sel is not None:
        _unknown = [n for n in engines_sel if n not in _engines]
        if _unknown:
            parser.error('argument --engines: unknown engine(s): %s '
                         '(see `sutura engines list`)' % ', '.join(_unknown))
    results = [process_file(f, human, mode=mode, profile=args.profile,
                            no_history=args.no_history, engine=engine,
                            out=out, max_geom_change=max_geom_change,
                            max_risk=max_risk, force=args.force,
                            join_components=args.experimental_join_components,
                            autorefine=args.experimental_autorefine,
                            ftetwild=_ft_arg, deep_repair=_dr_mode,
                            graft=_graft_arg, flap=_flap_arg,
                            tamponade=_tamponade_arg,
                            dressing=_dressing_arg,
                            dressing_drain=_dressing_drain,
                            dressing_defects=_dressing_defects,
                            dressing_rmax_scale=_dressing_rmax_scale,
                            dressing_sigma_scale=_dressing_sigma_scale,
                            si_mode=si_mode,
                            dressing_force_adopt=args.dressing_force_adopt,
                            indirect_autorefine=args.experimental_indirect_autorefine,
                            extra_features=args.experimental_edge_tiebreak,
                            triage_spec=triage_spec, engines=_engines,
                            engine_chain=_engine_chain,
                            engine_warnings=_engine_warnings,
                            methods=methods_sel,
                            engine_filter=engines_sel,
                            repeat_source=repeat_source,
                            repeat_target=repeat_target,
                            wall_min_thickness=args.wall_min_thickness) for f in files]
    ok = sum(1 for _, c in results if c == 'watertight')
    warnings = sum(1 for _, c in results if c == 'warning')
    errors = sum(1 for _, c in results if c == 'error')

    # issue counts across the whole batch (e.g. volume_warning: 3)
    issue_counts = {}
    for r, _ in results:
        for code in r.get('issues', []):
            issue_counts[code] = issue_counts.get(code, 0) + 1

    if len(files) == 1:
        result, category = results[0]
        if human:
            print(human_report(result, show_defects=show_defects, show_diff=show_diff))
        else:
            print(json.dumps(result, ensure_ascii=False))
        sys.exit(0 if category != 'error' else 1)

    if human:
        for result, category in results:
            print(human_report(result, show_defects=show_defects, show_diff=show_diff))
            print()
        print('Summary: %d file(s), %d watertight, %d with warnings, %d failed.'
              % (len(files), ok, warnings, errors))
        for code, n in issue_counts.items():
            print('  - %s: %d dosya' % (issue_label(code), n))
    else:
        payload = {
            'files': [r for r, _ in results],
            'summary': {
                'total': len(files), 'ok': ok, 'warning': warnings, 'error': errors,
                'issue_counts': issue_counts,
            },
        }
        print(json.dumps(payload, ensure_ascii=False))
    sys.exit(0 if errors == 0 else 1)


if __name__ == '__main__':
    main()
