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
    _count_si, _hausdorff_rel, _obj_closed_result, _not_watertight_result,
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
from sutura_engine.pipeline.tiers.graft import graft_tier
from sutura_engine.pipeline.tiers.dressing import (
    DRESSING_SUGGEST_FLAG, DRESSING_FORCE_ADOPT_FLAG, _dressing_record,
    _dressing_gate_detail, _dressing_suggestions, _dressing_suggestion_text,
    dressing_tier,
)
from sutura_engine.pipeline.tiers.wall import wall_thicken_tier
from sutura_engine.pipeline.tiers.repeat import (
    REPEAT_MAX_HAUSDORFF_OUTSIDE, repeat_tier, _clean_and_orient,
)
from sutura_engine.pipeline.tiers.engines import (
    ENGINE_MESH_FORMATS, load_engine_run, engine_placement_order,
    _read_engine_mesh, _write_engine_mesh, run_engines,
    _record_engine_entries, run_after_stage2_engines,
)
from sutura_engine.pipeline.pipeline import (
    repair_mesh_from_arrays, deep_repair_ladder, _dressing_wanted,
    _dressing_damage, _deep_repair_offer,
)


VERSION = "0.9.1"


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
