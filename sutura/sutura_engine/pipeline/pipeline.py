# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Central repair pipeline: repair_mesh_from_arrays and the deep-repair ladder
(P1a split). Moved verbatim from sutura/repair.py; no logic change.

Internal geometry arrays stay float64 through the pipeline. Downcasting to
float32 happens only when writing STL (or another float32 output format);
never coerce a Stage-1 or tier result to float32 at an array boundary, because
it changes exact duplicate/weld decisions and cascades into the tiers.
"""
import os
import tempfile
import time

import numpy as np

import topology
import triage
from sutura_engine import health as method_health
from sutura_engine import trace
from sutura_engine.pipeline.mesh_io import check_units, should_extract_outer_shell
from sutura_engine.pipeline.order import (
    FINAL_TIER_PRECEDENCE, ftetwild_supersedes_graft,
)
from sutura_engine.pipeline.p_weld import reload_strict_holes_nm
from sutura_engine.pipeline.presets import (
    ExtremeRemovedAllError, cad_likeness, classify_with_engine,
    estimate_deep_repair_time, resolve_mode_params, resolve_si_mode,
)
from sutura_engine.pipeline.stage1 import (
    _si_excise_budget, apply_chain, delete_fallback_chain,
    extreme_extra_passes, join_small_components, si_excise_enabled,
    si_excise_recap, stage1_chain, stage1_cleanup_chain,
)
from sutura_engine.pipeline.stage2 import _split_pinched_vertices
from sutura_engine.pipeline.tiers import ftetwild as _ftetwild_mod
from sutura_engine.pipeline.tiers.closing import closing_ladder
from sutura_engine.pipeline.tiers.dressing import dressing_tier
from sutura_engine.pipeline.tiers.engines import (
    _record_engine_entries, engine_placement_order, run_engines,
)
from sutura_engine.pipeline.tiers.flap import _flap_step
from sutura_engine.pipeline.tiers.ftetwild import _ftetwild_tier
from sutura_engine.pipeline.tiers.graft import graft_tier
from sutura_engine.pipeline.tiers.local_remesh import _local_remesh_tier
from sutura_engine.pipeline.tiers.repeat import repeat_tier
from sutura_engine.pipeline.tiers.splint import splint_tier
from sutura_engine.pipeline.tiers.tamponade import tamponade_tier
from sutura_engine.pipeline.tiers.wall import wall_thicken_tier
from sutura_engine.pipeline.utils import (
    _count_vertex_components, boundary_loop_stats, surface_area,
)


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
                             si_mode=None, dressing_force_adopt=False,
                             splint=False, splint_adaptive=None):
    """Repair one mesh given as numpy arrays. Returns (report, verts, tris).

    Internal arrays stay float64; the final verts/tris are float32 for output
    (the same rule as the module docstring).

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
    ``'poisson'`` (native implicit reconstruction, legacy alias; ``sutura/closing.py``) or
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
    # The narrowest shared library entry: every repair path (CLI, GUI, methods,
    # direct API) funnels through here, so a missing/stale sutura_geom cannot
    # silently degrade when the function is called as a library.
    method_health.require_geometry()
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
            _base_topo = topology.mesh_topology(v, t)
            _cv, _ct, _adopted = run_engines(
                engines, engine_chain, 'before_stage1', tmpdir, v, t,
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
            # Guard A (P0): an input that is ALREADY reload-equivalent watertight
            # and multi-part needs no outer-shell union. extract_outer_shell ->
            # outer_shell_union ends in largest_component(), which keeps only the
            # largest part and deletes the rest (thingi10k_248395: 4 parts,
            # 13264 -> 6944 faces, -58% volume). The reload-equivalent metric is
            # the right separator: 248395 reloads as (holes, nm) == (0, 0),
            # whereas a mesh that genuinely needs the union (Rubik-style touching
            # cubies, reloaded as non-manifold) does not match. This gate only
            # guards the fallback, not the P-a/P-b/P-c prepass (which is safe and
            # load-bearing for other samples).
            _in_rholes, _in_rnm = reload_strict_holes_nm(verts, tris)
            if _in_rholes == 0 and _in_rnm == 0:
                stats['outer_shell_skipped'] = 'healthy_multipart_input'
            else:
                with method_health.tier_guard('outer_shell', 'tier') as _go:
                    import repeat_repair as _rr
                    _shell_v, _shell_t = _rr.extract_outer_shell(verts, tris)
                    if len(_shell_t) > 0:
                        # Guard B: the union may only improve the reload topology,
                        # never worsen it. Keeps the useful case (55772: input
                        # reloads (3, 12), union closes to (0, 0)) while making
                        # the fallback unable to degrade a damaged input.
                        _out_rholes, _out_rnm = reload_strict_holes_nm(
                            _shell_v, _shell_t)
                        if _out_rholes <= _in_rholes and _out_rnm <= _in_rnm:
                            verts = np.asarray(_shell_v, dtype=np.float32)
                            tris = np.asarray(_shell_t, dtype=np.int32)
                            v, t = verts, tris
                            stats['outer_shell_extracted'] = True
                        else:
                            stats['outer_shell_rejected'] = 'topology_regressed'
            # on error the guard records it and the input is kept unchanged

    before = topology.mesh_topology(v, t)
    before_geom = topology.mesh_geometric_measures(v, t)
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
        _tp_in_v, _tp_in_t = v, t
        _tp_v, _tp_t, _tp_a, _tp_s = apply_chain(
            v, t, stage1_cleanup_chain())
        _tafter = topology.mesh_topology(_tp_v, _tp_t)
        _tp_v, _tp_t, _tafter = tamponade_tier(
            _tp_v, _tp_t, v, t, _tafter, stats, tmpdir, tamponade=True,
            intensity=(getattr(triage_spec, 'base', None)
                       or getattr(triage_spec, 'name', None)),
            spec=triage_spec, si_mode=si_mode, prepass=True)
        if (stats.get('tamponade') or {}).get('adopted'):
            v = np.asarray(_tp_v, dtype=np.float32)
            t = np.asarray(_tp_t, dtype=np.int32)
            verts, tris = v, t
        stats['timings']['prepass'] = round(time.perf_counter() - _t_pre, 3)
        if trace.is_active():
            trace.record_stage('tamponade_prepass', 'tiers.tamponade.tamponade_tier',
                               _tp_in_v, _tp_in_t, v, t,
                               gate=stats.get('tamponade', {}), elapsed=stats['timings']['prepass'])
    else:
        if trace.is_active():
            trace.record_stage('tamponade_prepass', 'tiers.tamponade.tamponade_tier',
                               v, t, v, t,
                               gate={'status': 'skipped', 'reason': 'disabled'})

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
        _t_flap = time.perf_counter()
        _fl_in_v, _fl_in_t = v, t
        _fafter = topology.mesh_topology(v, t)
        _fv, _ft, _fafter = _flap_step(v, t, v, t, _fafter, stats, None)
        if (stats.get('flap') or {}).get('adopted'):
            v = np.asarray(_fv, dtype=np.float32)
            t = np.asarray(_ft, dtype=np.int32)
            verts, tris = v, t
        if trace.is_active():
            trace.record_stage('flap_prepass', 'tiers.flap._flap_step',
                               _fl_in_v, _fl_in_t, v, t,
                               gate=stats.get('flap', {}),
                               elapsed=round(time.perf_counter() - _t_flap, 4))
    else:
        if trace.is_active():
            trace.record_stage('flap_prepass', 'tiers.flap._flap_step',
                               v, t, v, t,
                               gate={'status': 'skipped', 'reason': 'disabled'})

    _t_s1 = time.perf_counter()
    _s1_in_v, _s1_in_t = v, t

    cur_v, cur_t = np.asarray(v, np.float64), np.asarray(t, np.int64)
    cur_v, cur_t, applied, skipped = apply_chain(
        cur_v, cur_t, stage1_chain(**_p, join_components=join_components))
    after = topology.mesh_topology(cur_v, cur_t)
    if trace.is_active():
        trace.record_stage('stage1_chain', 'stage1.apply_chain',
                           _s1_in_v, _s1_in_t, cur_v, cur_t,
                           gate={'status': 'applied', 'filters': applied,
                                 'skipped': len(skipped) if hasattr(skipped, '__len__') else skipped},
                           elapsed=round(time.perf_counter() - _t_s1, 4))

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
                ar_v, ar_t, _a, _s = apply_chain(
                    ar_v, ar_t, stage1_chain(**_p, join_components=join_components))
                cand_after = topology.mesh_topology(ar_v, ar_t)
                cand_holes = boundary_loop_stats(ar_v, ar_t)[0]
                cand_nm = cand_after.get('non_two_manifold_edges', 0)
                base_holes = boundary_loop_stats(cur_v, cur_t)[0]
                base_nm = after.get('non_two_manifold_edges', 0)
                adopted = bool(cand_holes <= base_holes and cand_nm <= base_nm)
                ar_rep['adopted'] = adopted
                if adopted:
                    cur_v, cur_t = ar_v, ar_t
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
                    ia_v, ia_t, _a, _s = apply_chain(
                        ia_v, ia_t, stage1_chain(**_p, join_components=join_components))
                    cand_after = topology.mesh_topology(ia_v, ia_t)
                    cand_holes = boundary_loop_stats(ia_v, ia_t)[0]
                    cand_nm = cand_after.get('non_two_manifold_edges', 0)
                    base_holes = boundary_loop_stats(cur_v, cur_t)[0]
                    base_nm = after.get('non_two_manifold_edges', 0)
                    adopted = bool(cand_holes <= base_holes and cand_nm <= base_nm)
                    ia_rep['adopted'] = adopted
                    if adopted:
                        cur_v, cur_t = ia_v, ia_t
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
        _t_fb = time.perf_counter()
        _fb_in_v, _fb_in_t = cur_v, cur_t
        fb_v, fb_t, fapplied, fskipped = apply_chain(
            cur_v, cur_t, delete_fallback_chain(**_p, join_components=join_components))
        fafter = topology.mesh_topology(fb_v, fb_t)
        _adopted = (fafter.get('non_two_manifold_edges', 0) + fafter.get('non_two_manifold_vertices', 0)
                < after.get('non_two_manifold_edges', 0) + after.get('non_two_manifold_vertices', 0))
        if _adopted:
            cur_v, cur_t, after, applied, skipped = fb_v, fb_t, fafter, fapplied, fskipped
        if trace.is_active():
            trace.record_stage('stage1_delete_fallback', 'stage1.delete_fallback_chain',
                               _fb_in_v, _fb_in_t, cur_v, cur_t,
                               gate={'status': 'adopted' if _adopted else 'rejected'},
                               elapsed=round(time.perf_counter() - _t_fb, 4))
    else:
        if trace.is_active():
            trace.record_stage('stage1_delete_fallback', 'stage1.delete_fallback_chain',
                               cur_v, cur_t, cur_v, cur_t,
                               gate={'status': 'skipped', 'reason': 'clean_manifold'})

    # --experimental-join-components prototype: instead of deleting small
    # components (the mincomponentsize filter was dropped from the chain
    # above), move them onto the nearest larger component and re-close.
    stats['experimental_join_components'] = False
    if join_components:
        cur_v, cur_t, moved, remaining = join_small_components(
            cur_v, cur_t, _p['mincomponentsize'], _p['maxholesize'])
        stats['experimental_join_components'] = {'moved': moved,
                                                 'remaining_small': remaining}
        after = topology.mesh_topology(cur_v, cur_t)

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
        _t_si = time.perf_counter()
        _si_in_v, _si_in_t = cur_v, cur_t
        cur_v, cur_t, _si_rec = si_excise_recap(
            cur_v, cur_t, maxholesize=_p['maxholesize'],
            time_budget=_si_excise_budget(triage_spec))
        # Only report when a round was actually accepted (self-intersections
        # strictly reduced, holes/nm not worse); a mesh the excision cannot
        # improve -- including any 0-SI mesh -- keeps the pre-existing report
        # and output byte-identical.
        if _si_rec.get('applied'):
            stats['si_excise'] = _si_rec
            after = topology.mesh_topology(cur_v, cur_t)
        if trace.is_active():
            trace.record_stage('si_excise', 'stage1.si_excise_recap',
                               _si_in_v, _si_in_t, cur_v, cur_t,
                               gate=_si_rec, elapsed=round(time.perf_counter() - _t_si, 4))
    else:
        if trace.is_active():
            trace.record_stage('si_excise', 'stage1.si_excise_recap',
                               cur_v, cur_t, cur_v, cur_t,
                               gate={'status': 'skipped', 'reason': 'disabled_or_not_full'})

    # Extreme-only extra passes (Stage C): self-intersection cleanup + one more
    # run of the main chain to close the holes / drop the debris the removal
    # exposed. ONLY for mode == 'extreme'; every other mode is untouched.
    # meshing_isotropic_explicit_remeshing (full remesh) is deliberately out of
    # scope: it can unpredictably change topology.
    stats['extreme_passes_applied'] = False
    if mode == 'extreme':
        applied_extra, si_found, si_removed, applied2, skipped2, cur_v, cur_t = \
            extreme_extra_passes(cur_v, cur_t, _p)
        stats['extreme_passes_applied'] = applied_extra
        applied += applied2
        if skipped2:
            skipped.update(skipped2)
        if si_found:
            stats['self_intersections_found'] = si_found
            stats['self_intersections_removed'] = si_removed
        after = topology.mesh_topology(cur_v, cur_t)
        if after.get('faces_number', 0) == 0:
            raise ExtremeRemovedAllError(
                'Extreme mode removed all geometry (small connected component '
                'below the size threshold); try a less aggressive mode (e.g. Auto)')

    # Downstream tiers are array-native: the stage-1 result is carried as
    # ``(cur_v, cur_t)`` (float64/int64), with no MeshSet boundary object.
    cur_v = np.asarray(cur_v, dtype=np.float64)
    cur_t = np.asarray(cur_t, dtype=np.int64)

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
            _cur_v = np.asarray(cur_v, dtype=np.float32)
            _cur_t = np.asarray(cur_t, dtype=np.int32)
            _cv, _ct, _adopted = run_engines(
                engines, engine_chain, 'after_stage1', tmpdir, _cur_v, _cur_t,
                boundary_loop_stats(_cur_v, _cur_t)[0],
                after.get('non_two_manifold_edges', 0), triage_spec, _entries)
            if _adopted:
                cur_v = np.asarray(_cv, dtype=np.float64)
                cur_t = np.asarray(_ct, dtype=np.int64)
                after = topology.mesh_topology(cur_v, cur_t)
        _record_engine_entries(stats, _entries)

    # Registry methods 8/9/10 (P-INT): standalone closing / proxy-template
    # modules. They run on the ORIGINAL input arrays, before the deep-repair
    # ladder, and adopt only a strict-watertight, no-worse candidate (see
    # ``closing_ladder``).
    stats['timings']['stage1'] = round(time.perf_counter() - _t_start, 3)
    _t_tiers = time.perf_counter()
    _t_cl = time.perf_counter()
    _cl_in_v, _cl_in_t = ((np.asarray(cur_v, dtype=np.float32),
                           np.asarray(cur_t, dtype=np.int32))
                          if trace.is_active() else (None, None))
    _cv = np.asarray(cur_v, dtype=np.float32)
    _ct = np.asarray(cur_t, dtype=np.int32)
    _cv, _ct, after = closing_ladder(_cv, _ct, v, t, after, stats, tmpdir,
                                     closing=closing,
                                     proxy_template=proxy_template)
    cur_v = np.asarray(_cv, dtype=np.float64)
    cur_t = np.asarray(_ct, dtype=np.int64)
    if trace.is_active():
        trace.record_stage('closing_ladder', 'tiers.closing.closing_ladder',
                           _cl_in_v, _cl_in_t,
                           np.asarray(cur_v, dtype=np.float32),
                           np.asarray(cur_t, dtype=np.int32),
                           gate={'closing': closing, 'proxy_template': proxy_template},
                           elapsed=round(time.perf_counter() - _t_cl, 4))

    # Registry method 17 (Tamponade): interior hole plug + SI melt. When the
    # tier is ON it normally runs as a pre-pass (before Flap / Stage 1, see
    # above) so its reference plug wins over the generic cap; this post-chain
    # call is then skipped. It is kept for the OFF path and as a second chance
    # when the prepass ran but did not adopt (e.g. no reference found there).
    # Adopts only when the reload-honest holes+non-manifold strictly falls
    # without raising parts or losing healthy faces.
    if not (stats.get('tamponade') or {}).get('adopted'):
        _t_tp = time.perf_counter()
        _tp_in_v = _tp_in_t = None
        if trace.is_active():
            _tp_in_v = np.asarray(cur_v, dtype=np.float32)
            _tp_in_t = np.asarray(cur_t, dtype=np.int32)
        _cv = np.asarray(cur_v, dtype=np.float32)
        _ct = np.asarray(cur_t, dtype=np.int32)
        _cv, _ct, after = tamponade_tier(
            _cv, _ct, v, t, after, stats, tmpdir, tamponade=(tamponade is True),
            intensity=(getattr(triage_spec, 'base', None)
                       or getattr(triage_spec, 'name', None)),
            spec=triage_spec, si_mode=si_mode)
        cur_v = np.asarray(_cv, dtype=np.float64)
        cur_t = np.asarray(_ct, dtype=np.int64)
        if trace.is_active():
            trace.record_stage('tamponade_post', 'tiers.tamponade.tamponade_tier',
                               _tp_in_v, _tp_in_t,
                               np.asarray(cur_v, dtype=np.float32),
                               np.asarray(cur_t, dtype=np.int32),
                               gate=stats.get('tamponade', {}),
                               elapsed=round(time.perf_counter() - _t_tp, 4))
    else:
        if trace.is_active():
            _cur_v = np.asarray(cur_v, dtype=np.float32)
            _cur_t = np.asarray(cur_t, dtype=np.int32)
            trace.record_stage('tamponade_post', 'tiers.tamponade.tamponade_tier',
                               _cur_v, _cur_t, _cur_v, _cur_t,
                               gate={'status': 'skipped', 'reason': 'adopted_in_prepass'})

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
    _t_gr = time.perf_counter()
    _gr_cur_v = np.asarray(cur_v, dtype=np.float32)
    _gr_cur_t = np.asarray(cur_t, dtype=np.int32)
    _g_v, _g_t, after = graft_tier(
        np.asarray(_gr_cur_v, dtype=np.float64),
        np.asarray(_gr_cur_t, dtype=np.int64), v, t, after, stats, tmpdir,
        graft=(graft is True) and not _tamp_watertight,
        intensity=(getattr(triage_spec, 'base', None)
                   or getattr(triage_spec, 'name', None)),
        spec=triage_spec)
    cur_v = np.asarray(_g_v, dtype=np.float64)
    cur_t = np.asarray(_g_t, dtype=np.int64)
    if trace.is_active():
        trace.record_stage('graft', 'tiers.graft.graft_tier',
                           _gr_cur_v, _gr_cur_t,
                           np.asarray(cur_v, dtype=np.float32),
                           np.asarray(cur_t, dtype=np.int32),
                           gate=stats.get('graft', {'status': 'disabled'}),
                           elapsed=round(time.perf_counter() - _t_gr, 4))

    _cv = np.asarray(cur_v, dtype=np.float32)
    _ct = np.asarray(cur_t, dtype=np.int32)
    _cv, _ct, after = deep_repair_ladder(_cv, _ct, v, t, after, stats, tmpdir,
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
                                   splint=(splint if not _tamp_watertight
                                           else False),
                                   splint_adaptive=splint_adaptive,
                                   dressing_force_adopt=dressing_force_adopt)
    cur_v = np.asarray(_cv, dtype=np.float64)
    cur_t = np.asarray(_ct, dtype=np.int64)

    # Registry methods 11/12 (P-REP): repeated-element transplant, after the
    # whole stage-1 chain (the transplant needs a watertight M).
    cur_v, cur_t, after = repeat_tier(cur_v, cur_t, after, stats, mode=repeat,
                                      source_point=repeat_source,
                                      target_point=repeat_target)

    # Registry method 15 (P-WALL): opt-in thin-wall thicken-to-min. Runs on the
    # ORIGINAL input arrays (like the closing tier) and adopts only a
    # strict-watertight, no-worse candidate; never automatic.
    if wall_thicken:
        cur_v, cur_t, after = wall_thicken_tier(cur_v, cur_t, after, stats, v, t,
                                                min_thickness=wall_min_thickness)
    stats['timings']['deep_repair'] = round(time.perf_counter() - _t_tiers, 3)

    # A closed result with no non-manifold edge can still have pinched
    # ("bowtie") vertices, e.g. when the final close_holes fan-fills a hole at
    # a vertex shared by two boundary loops; stage 2 then never runs and a
    # strictly watertight mesh is reported as a warning (thingi10k_145065).
    _t_ps = time.perf_counter()
    _ps_in_v = _ps_in_t = None
    if trace.is_active():
        _ps_in_v = np.asarray(cur_v, dtype=np.float32)
        _ps_in_t = np.asarray(cur_t, dtype=np.int32)
    cur_v, cur_t, after, stats['pinched_vertices_split'] = _split_pinched_vertices(
        cur_v, cur_t, after)
    if trace.is_active():
        trace.record_stage('pinch_split', 'stage2._split_pinched_vertices',
                           _ps_in_v, _ps_in_t,
                           np.asarray(cur_v, dtype=np.float32),
                           np.asarray(cur_t, dtype=np.int32),
                           gate={'split_count': stats.get('pinched_vertices_split', 0)},
                           elapsed=round(time.perf_counter() - _t_ps, 4))

    # final_fallback engines run once, on the ORIGINAL input, after every
    # built-in tier (including replace_ftetwild) and after the pinch split.
    if engines:
        _entries = []
        if engine_placement_order(engines, engine_chain, 'final_fallback'):
            _cur_v = np.asarray(cur_v, dtype=np.float32)
            _cur_t = np.asarray(cur_t, dtype=np.int32)
            _cv, _ct, _adopted = run_engines(
                engines, engine_chain, 'final_fallback', tmpdir, v, t,
                boundary_loop_stats(_cur_v, _cur_t)[0],
                after.get('non_two_manifold_edges', 0), triage_spec, _entries)
            if _adopted:
                cur_v = np.asarray(_cv, dtype=np.float64)
                cur_t = np.asarray(_ct, dtype=np.int64)
                after = topology.mesh_topology(cur_v, cur_t)
        _record_engine_entries(stats, _entries)

    holes_after = boundary_loop_stats(cur_v, cur_t)[0]
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

    cur_v = np.asarray(cur_v, dtype=np.float32)
    cur_t = np.asarray(cur_t, dtype=np.int32)
    geom = topology.mesh_geometric_measures(cur_v, cur_t)
    if geom.get('mesh_volume', 0) < 0:
        cur_t = np.asarray(topology.invert_face_orientation(cur_t),
                           dtype=np.int32)
        geom = topology.mesh_geometric_measures(cur_v, cur_t)
        if geom.get('mesh_volume', 0) < 0:
            cur_t = np.asarray(topology.invert_face_orientation(cur_t),
                               dtype=np.int32)
            geom = topology.mesh_geometric_measures(cur_v, cur_t)
    after_volume = geom.get('mesh_volume', 0)

    new_verts = cur_v
    new_tris = cur_t
    fin = topology.mesh_topology(new_verts, new_tris)
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
    # health metric). The count is measured from the final arrays with the
    # exact Rust classifier.
    #
    # Assumption (documented, not silent): stage 2 (Manifold) only runs on a
    # watertight closed mesh and REBUILDS the watertight solid, so its output
    # is assumed free of self-intersections by construction -- therefore stage
    # 2's output is NOT re-measured here. When stage 2 runs and writes the
    # final mesh, this value is a conservative stage-1-based upper bound on
    # the health signal, which is fine for scoring.
    # Under --si-mode off the policy is 'not measured': skip the count entirely
    # and report None.
    if si_mode == 'off':
        stats['stage1']['self_intersections_remaining'] = None
    else:
        _, count = topology.self_intersecting_faces(new_verts, new_tris)
        stats['stage1']['self_intersections_remaining'] = (
            None if int(count) < 0 else int(count))
    stats['method_errors'] = method_health.current().to_dicts()
    return stats, new_verts, new_tris


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


def _dressing_damage(cur_v, cur_t, after, measure_si=True):
    """``(holes, non_manifold, exact_si)`` of the current result.

    ``exact_si`` is ``None`` when the classifier cannot measure it (empty/too
    large mesh, or a build without the Rust extension) or when ``measure_si``
    is False (``--si-mode off``).  Used to decide whether the Dressing fallback
    should run on the result the ladder currently holds (after Graft), so under
    the default 'report' mode an SI-only residual is reported but not escalated.
    """
    holes = boundary_loop_stats(cur_v, cur_t)[0]
    nm = int(after.get('non_two_manifold_edges', 0))
    si = None
    if measure_si:
        try:
            from sutura_engine import dressing as _d
            si = _d.si_face_count(
                np.asarray(cur_v, dtype=np.float64),
                np.asarray(cur_t, dtype=np.int64))
        except Exception:  # noqa: BLE001 - unknown SI never blocks a repair
            si = None
    return int(holes), nm, si


def deep_repair_ladder(cur_v, cur_t, input_v, input_t, after, stats, tmpdir,
                       mode=None, ftetwild=False, spec=None, engines=None,
                       engine_chain=None, graft=False, dressing=False,
                       dressing_drain=None, dressing_defects=None,
                       dressing_rmax_scale=None, dressing_sigma_scale=None,
                       si_mode='report', splint=False, splint_adaptive=None,
                       dressing_force_adopt=False):
    """Single entry point of the deep-repair ladder, called after the stage-1
    chain. ``cur_v``/``cur_t`` is the result the ladder currently holds and
    ``input_v``/``input_t`` the original user surface. ``mode`` is one of
    DEEP_REPAIR_MODES, or None for the library default (no deep-repair report;
    ``ftetwild`` alone decides, as before the ladder existed). 'local' runs the
    local re-mesh tier; 'full' and None run the fTetWild tier (``ftetwild``:
    'auto'/True/False, unchanged semantics); 'off' runs nothing. ``spec`` is the
    resolved intensity spec (defaults to Balanced); it supplies the tier's
    timeout, size cap, decimation ladder and Hausdorff sample count. Records
    ``stats['deep_repair']`` (mode, tiers run, per-tier reports); the
    ``available`` offer is filled in by ``_deep_repair_offer`` after the final
    stage-1 measurement. Returns ``(cur_v, cur_t, after)``."""
    cur_v = np.asarray(cur_v, dtype=np.float64)
    cur_t = np.asarray(cur_t, dtype=np.int64)
    if spec is None:
        spec = triage.PRESETS[triage.DEFAULT_INTENSITY]
    tiers_run = []
    # The attempted tiers follow the canonical order in
    # sutura_engine.pipeline.order (DEEP_TIER_ORDER = 'local', 'graft',
    # 'dressing', 'ftetwild'). This function expresses it procedurally -- local
    # first, then the full-mode Graft -> Dressing -> fTetWild chain; the order
    # and its tie-breaks are pinned by tests/test_pipeline_order.py.
    cad = None
    dmg = None
    cur_holes = boundary_loop_stats(cur_v, cur_t)[0]
    cur_nm = int(after.get('non_two_manifold_edges', 0))
    if mode == 'local':
        if cur_holes > 0 or cur_nm > 0:
            _lv = np.asarray(cur_v, dtype=np.float32)
            _lt = np.asarray(cur_t, dtype=np.int32)
            _lv, _lt, after, local_rep = _local_remesh_tier(
                _lv, _lt, input_v, input_t, after)
            cur_v = np.asarray(_lv, dtype=np.float64)
            cur_t = np.asarray(_lt, dtype=np.int64)
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
                engines, engine_chain, 'replace_ftetwild', tmpdir, input_v,
                input_t, cur_holes, cur_nm, spec, _entries)
            _record_engine_entries(stats, _entries)
            if _adopted:
                cur_v = np.asarray(_cv, dtype=np.float64)
                cur_t = np.asarray(_ct, dtype=np.int64)
                after = topology.mesh_topology(cur_v, cur_t)
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
            cad = cad_likeness(input_v, input_t)
            graft_v = graft_t = graft_after = None
            graft_dev = None
            graft_ok = False
            graft_cov_failed = False
            # Only run the last-resort Graft when stage 1 still leaves holes or
            # non-manifold edges (the same trigger fTetWild's 'auto' uses).  A
            # closed stage-1 result is already the final mesh: Graft must not
            # run on it, or already-watertight repairs would change output and
            # pay an unnecessary tier.
            if graft in (True, 'auto') and (cur_holes > 0 or cur_nm > 0):
                _g_cur_v = np.asarray(cur_v, dtype=np.float32)
                _g_cur_t = np.asarray(cur_t, dtype=np.int32)
                _g_v, _g_t, _g_after = graft_tier(
                    np.asarray(_g_cur_v, dtype=np.float64),
                    np.asarray(_g_cur_t, dtype=np.int64), input_v, input_t,
                    after, stats, tmpdir, graft=True,
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
                    _fid = _g.get('fidelity_ok')
                    _cov = _g.get('coverage_ok')
                    _fid_failed = (_fid is not None and not bool(_fid))
                    _cov_failed = (_cov is not None and not bool(_cov))
                    if not _fid_failed and not _cov_failed:
                        cur_v = np.asarray(_g_v, dtype=np.float64)
                        cur_t = np.asarray(_g_t, dtype=np.int64)
                        after = _g_after
                        graft_ok = True
                        tiers_run.append('graft')
                    else:
                        graft_v = np.asarray(_g_v, dtype=np.float64)
                        graft_t = np.asarray(_g_t, dtype=np.int64)
                        graft_after = _g_after
                        graft_dev = _g.get('hausdorff_healthy')
                        graft_cov_failed = _cov_failed

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
                _dh, _dnm, _dsi = _dressing_damage(cur_v, cur_t, after,
                                                   measure_si=(si_mode != 'off'))
                dmg = {'holes': _dh, 'non_manifold': _dnm,
                       'self_intersections': _dsi}
            if _dressing_wanted(dressing, _dh, _dnm, _dsi, si_mode):
                _t_dr = time.perf_counter()
                _dr_cur_v = np.asarray(cur_v, dtype=np.float32)
                _dr_cur_t = np.asarray(cur_t, dtype=np.int32)
                _dr_in_v, _dr_in_t = ((_dr_cur_v, _dr_cur_t)
                                      if trace.is_active() else (None, None))
                _d_v, _d_t, _d_after = dressing_tier(
                    np.asarray(_dr_cur_v, dtype=np.float64),
                    np.asarray(_dr_cur_t, dtype=np.int64), input_v, input_t,
                    after, stats, tmpdir, dressing=True,
                    intensity=(getattr(spec, 'base', None)
                               or getattr(spec, 'name', None)),
                    drain=dressing_drain, defects=dressing_defects,
                    r_max_scale=dressing_rmax_scale,
                    sigma_scale=dressing_sigma_scale,
                    force_adopt=dressing_force_adopt)
                if (stats.get('dressing') or {}).get('adopted'):
                    cur_v = np.asarray(_d_v, dtype=np.float64)
                    cur_t = np.asarray(_d_t, dtype=np.int64)
                    after = _d_after
                    dressing_ok = True
                    tiers_run.append('dressing')
                if trace.is_active():
                    trace.record_stage('dressing', 'tiers.dressing.dressing_tier',
                                       _dr_in_v, _dr_in_t,
                                       np.asarray(cur_v, dtype=np.float32),
                                       np.asarray(cur_t, dtype=np.int32),
                                       gate=stats.get('dressing', {}),
                                       elapsed=round(time.perf_counter() - _t_dr, 4))
            else:
                if trace.is_active():
                    _cur_v = np.asarray(cur_v, dtype=np.float32)
                    _cur_t = np.asarray(cur_t, dtype=np.int32)
                    trace.record_stage('dressing', 'tiers.dressing.dressing_tier',
                                       _cur_v, _cur_t, _cur_v, _cur_t,
                                       gate={'status': 'skipped', 'reason': 'not_wanted'})

            # Splint (#18) sits AFTER Dressing and BEFORE fTetWild: an exact
            # solid rebuild that closes the mesh supersedes fTetWild.  It runs
            # only when the result the ladder now holds (after Graft/Dressing)
            # still has holes or non-manifold edges, and only when neither Graft
            # nor Dressing already produced the final mesh.
            splint_ok = False
            if splint in (True, 'auto') and not dressing_ok and not graft_ok:
                _cur_v = np.asarray(cur_v, dtype=np.float32)
                _cur_t = np.asarray(cur_t, dtype=np.int32)
                _sh = boundary_loop_stats(_cur_v, _cur_t)[0]
                _snm = int(after.get('non_two_manifold_edges', 0))
                if _sh > 0 or _snm > 0:
                    _t_sp = time.perf_counter()
                    _sp_in_v, _sp_in_t = ((_cur_v, _cur_t)
                                          if trace.is_active() else (None, None))
                    _s_v, _s_t, _s_after = splint_tier(
                        np.asarray(_cur_v, dtype=np.float64),
                        np.asarray(_cur_t, dtype=np.int64), input_v, input_t,
                        after, stats, tmpdir, splint=True,
                        intensity=(getattr(spec, 'base', None)
                                   or getattr(spec, 'name', None)),
                        spec=spec, adaptive=splint_adaptive)
                    if (stats.get('splint') or {}).get('adopted'):
                        cur_v = np.asarray(_s_v, dtype=np.float64)
                        cur_t = np.asarray(_s_t, dtype=np.int64)
                        after = _s_after
                        splint_ok = True
                        tiers_run.append('splint')
                    if trace.is_active():
                        trace.record_stage('splint', 'tiers.splint.splint_tier',
                                           _sp_in_v, _sp_in_t,
                                           np.asarray(cur_v, dtype=np.float32),
                                           np.asarray(cur_t, dtype=np.int32),
                                           gate=stats.get('splint', {}),
                                           elapsed=round(time.perf_counter() - _t_sp, 4))
                else:
                    if trace.is_active():
                        trace.record_stage('splint', 'tiers.splint.splint_tier',
                                           _cur_v, _cur_t, _cur_v, _cur_t,
                                           gate={'status': 'skipped', 'reason': 'mesh_already_closed'})
            else:
                if trace.is_active():
                    _cur_v = np.asarray(cur_v, dtype=np.float32)
                    _cur_t = np.asarray(cur_t, dtype=np.int32)
                    trace.record_stage('splint', 'tiers.splint.splint_tier',
                                       _cur_v, _cur_t, _cur_v, _cur_t,
                                       gate={'status': 'skipped', 'reason': 'not_wanted'})

            if dressing_ok:
                stats['experimental_ftetwild'] = False
                if graft_v is not None:
                    stats['graft']['adopted'] = False
                    stats['graft']['reason'] = 'superseded by Dressing'
                if trace.is_active():
                    _cur_v = np.asarray(cur_v, dtype=np.float32)
                    _cur_t = np.asarray(cur_t, dtype=np.int32)
                    trace.record_stage('ftetwild', 'pipeline._ftetwild_tier',
                                       _cur_v, _cur_t, _cur_v, _cur_t,
                                       gate={'status': 'skipped', 'reason': 'superseded_by_dressing'})
            elif splint_ok:
                stats['experimental_ftetwild'] = False
                if graft_v is not None:
                    stats['graft']['adopted'] = False
                    stats['graft']['reason'] = 'superseded by Splint'
                if trace.is_active():
                    _cur_v = np.asarray(cur_v, dtype=np.float32)
                    _cur_t = np.asarray(cur_t, dtype=np.int32)
                    trace.record_stage('ftetwild', 'pipeline._ftetwild_tier',
                                       _cur_v, _cur_t, _cur_v, _cur_t,
                                       gate={'status': 'skipped', 'reason': 'superseded_by_splint'})
            elif graft_ok:
                stats['experimental_ftetwild'] = False
                if trace.is_active():
                    _cur_v = np.asarray(cur_v, dtype=np.float32)
                    _cur_t = np.asarray(cur_t, dtype=np.int32)
                    trace.record_stage('ftetwild', 'pipeline._ftetwild_tier',
                                       _cur_v, _cur_t, _cur_v, _cur_t,
                                       gate={'status': 'skipped', 'reason': 'superseded_by_graft'})
            else:
                _t_ft = time.perf_counter()
                _ft_cur_v = np.asarray(cur_v, dtype=np.float32)
                _ft_cur_t = np.asarray(cur_t, dtype=np.int32)
                _ft_in_v, _ft_in_t = ((_ft_cur_v, _ft_cur_t)
                                      if trace.is_active() else (None, None))
                _ft_cur_v, _ft_cur_t, after = _ftetwild_tier(
                    np.asarray(_ft_cur_v, dtype=np.float64),
                    np.asarray(_ft_cur_t, dtype=np.int64), input_v, input_t,
                    after, stats, tmpdir, ftetwild, spec=spec)
                cur_v = np.asarray(_ft_cur_v, dtype=np.float64)
                cur_t = np.asarray(_ft_cur_t, dtype=np.int64)
                if (stats.get('experimental_ftetwild') or {}).get('ran'):
                    tiers_run.append('ftetwild')
                if trace.is_active():
                    trace.record_stage('ftetwild', 'pipeline._ftetwild_tier',
                                       _ft_in_v, _ft_in_t,
                                       np.asarray(cur_v, dtype=np.float32),
                                       np.asarray(cur_t, dtype=np.int32),
                                       gate=stats.get('experimental_ftetwild', {}),
                                       elapsed=round(time.perf_counter() - _t_ft, 4))
                if graft_v is not None:
                    ft_dev = (stats.get('experimental_ftetwild')
                              or {}).get('hausdorff_rel')
                    ft_adopted = bool((stats.get('experimental_ftetwild')
                                       or {}).get('adopted'))
                    if (not ft_adopted
                            or ftetwild_supersedes_graft(ft_dev, graft_dev,
                                                         graft_cov_failed)):
                        stats['graft']['adopted'] = False
                        stats['graft']['reason'] = (
                            'superseded by fTetWild (input coverage below '
                            'threshold)' if graft_cov_failed else
                            'superseded by fTetWild (lower healthy deviation)')
                        tiers_run.append('graft')
                    else:
                        cur_v, cur_t, after = graft_v, graft_t, graft_after
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
            _t_dr = time.perf_counter()
            _dr_cur_v = np.asarray(cur_v, dtype=np.float32)
            _dr_cur_t = np.asarray(cur_t, dtype=np.int32)
            _dr_in_v, _dr_in_t = ((_dr_cur_v, _dr_cur_t)
                                  if trace.is_active() else (None, None))
            _d_v, _d_t, _d_after = dressing_tier(
                np.asarray(_dr_cur_v, dtype=np.float64),
                np.asarray(_dr_cur_t, dtype=np.int64), input_v, input_t,
                after, stats, tmpdir, dressing=True,
                intensity=(getattr(spec, 'base', None)
                           or getattr(spec, 'name', None)),
                drain=dressing_drain, defects=dressing_defects,
                r_max_scale=dressing_rmax_scale,
                sigma_scale=dressing_sigma_scale,
                force_adopt=dressing_force_adopt)
            if (stats.get('dressing') or {}).get('adopted'):
                cur_v = np.asarray(_d_v, dtype=np.float64)
                cur_t = np.asarray(_d_t, dtype=np.int64)
                after = _d_after
                tiers_run.append('dressing')
            if trace.is_active():
                trace.record_stage('dressing', 'tiers.dressing.dressing_tier',
                                   _dr_in_v, _dr_in_t,
                                   np.asarray(cur_v, dtype=np.float32),
                                   np.asarray(cur_t, dtype=np.int32),
                                   gate=stats.get('dressing', {}),
                                   elapsed=round(time.perf_counter() - _t_dr, 4))
        else:
            if trace.is_active():
                _cur_v = np.asarray(cur_v, dtype=np.float32)
                _cur_t = np.asarray(cur_t, dtype=np.int32)
                trace.record_stage('dressing', 'tiers.dressing.dressing_tier',
                                   _cur_v, _cur_t, _cur_v, _cur_t,
                                   gate={'status': 'skipped', 'reason': 'not_wanted'})
        stats['experimental_ftetwild'] = False
        if trace.is_active():
            _cur_v = np.asarray(cur_v, dtype=np.float32)
            _cur_t = np.asarray(cur_t, dtype=np.int32)
            trace.record_stage('ftetwild', 'pipeline._ftetwild_tier',
                               _cur_v, _cur_t, _cur_v, _cur_t,
                               gate={'status': 'skipped', 'reason': f'mode_{mode}'})
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
            'splint': stats.get('splint') or None,
            'cad_likeness': cad,
            'dressing_damage': dmg,
            'available': None,
        }
    return cur_v, cur_t, after


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
    _adopted_reports = {
        'ftetwild': dr.get('ftetwild'),
        'graft': stats.get('graft'),
        'dressing': stats.get('dressing'),
        'splint': stats.get('splint'),
        'local': dr.get('local'),
    }
    for _tier in FINAL_TIER_PRECEDENCE:
        if (_adopted_reports.get(_tier) or {}).get('adopted'):
            final = _tier
            break
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
