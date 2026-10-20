#!/usr/bin/env python3
"""Sutura repair CLI entry point and compatibility shim.

The implementation lives in the ``sutura_engine.pipeline`` package (P1a split):
presets, mesh I/O, Stage 1/2, P-WELD, the repair tiers, the central pipeline,
validation/report helpers and the CLI helpers. This module re-exports those
symbols (so ``import repair`` and the installed flat layout keep working),
defines the ``VERSION`` literal the release tooling parses, and carries the
``main`` entry point plus ``process_file`` (the batch driver). The input is
never overwritten; multi-object 3MF files are repaired per object.
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
   resolve_mode_params, resolve_si_mode, resolve_splint, resolve_tamponade,
   tuning_applied_for
)
from sutura_engine.pipeline.mesh_io import (
    write_obj, check_units, read_3mf_units, read_obj, stl_write_binary,
    should_extract_outer_shell, scan_bad_coordinates, _scan_bin_stl,
    obj_has_material_refs, parse_3mf_mesh_blocks, parse_3mf_meshes,
    build_mesh_block, rewrite_3mf_meshes,
    _3MF_MAX_ENTRY_BYTES, _read_zip_entry, load_meshes, save_mesh,
    _resolve_ftetwild_bridge, _resolve_indirect_bridge,
    FTETWILD_BRIDGE, INDIRECT_BRIDGE,
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
from sutura_engine.pipeline.tiers.ftetwild import (
    ftetwild_available, run_ftetwild, _ftetwild_manifold_postprocess,
    _ftetwild_candidate, _decimate_boundary,
    _ftetwild_decimate_and_postprocess, _ftetwild_boundary,
    _ftetwild_attempt, _ftetwild_tier, _FTETWILD_AVAILABLE,
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
from sutura_engine.pipeline.service import repair_file, repair_3mf
from sutura_engine.pipeline.validate import (
    validate_mesh_from_arrays, validate_file, dry_run_mesh_from_arrays,
    dry_run_file,
)
from sutura_engine.pipeline.report import (
    human_defects, human_report, human_validate, human_dry_run, budget_check,
    _budget_message, _geom_change_pct, _confirm_budget_save,
)
from sutura_engine.pipeline.cli import (
    run_engines_cli, run_ftetwild_cli, _confirm_cli,
)


VERSION = "0.9.1"


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
                 si_mode=None, dressing_force_adopt=False,
                 splint=False, splint_adaptive=None,
                 trace_path=None):
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
    from sutura_engine import trace
    trace.begin_trace(trace_path=trace_path, input_file=src,
                      command_args={'mode': mode, 'profile': profile, 'si_mode': si_mode})
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
            splint=splint, splint_adaptive=splint_adaptive,
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
    trace.finish_trace()
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
    parser.add_argument('--experimental-splint', action='store_true',
                        help='enable the Splint (#18) exact solid-rebuild tier: '
                             'the Rust proxy+Delaunay+cut rebuild runs on the '
                             'original input in the deep-repair ladder, after '
                             'Dressing and before fTetWild, and is adopted only '
                             'when it reloads strict-watertight. Off by default; '
                             'env: SUTURA_SPLINT / SUTURA_SPLINT_DEFAULT')
    parser.add_argument('--no-splint', action='store_true',
                        help='disable the Splint (#18) exact solid-rebuild tier '
                             '(it is off by default; this forces it off even '
                             'when SUTURA_SPLINT / the default switch enables it)')
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
    parser.add_argument('--trace', metavar='PATH',
                        default=os.environ.get('SUTURA_TRACE'),
                        help='record a structured step-by-step trace of every '
                             'pipeline stage to PATH (JSON format). Env: SUTURA_TRACE')
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

    # Required Rust extension: ONE central check before any mesh work.
    # Informational subcommands above (--list-*, clear-cache, clear-learning,
    # export-history, --diagnostics, --self-repair, --self-check) do not need
    # it; --self-check reports the failure itself. --help/--version are handled
    # by argparse before this point.
    _geom_ok, _geom_reason = method_health.check_required_geometry()
    if not _geom_ok:
        if human:
            print('error: %s' % _geom_reason)
        else:
            print(json.dumps({'error': _geom_reason}))
        sys.exit(1)

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
    # Splint (#18): the single default switch (SPLINT_DEFAULT_ENABLED /
    # SUTURA_SPLINT_DEFAULT, OFF) decides; --no-splint disables and
    # --experimental-splint forces.
    _splint_arg = resolve_splint(no_splint=args.no_splint,
                                 force=args.experimental_splint)
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
                            splint=_splint_arg,
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
                            wall_min_thickness=args.wall_min_thickness,
                            trace_path=args.trace) for f in files]
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
