# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Single-file and multi-object 3MF repair entry points (P1a split).

repair_file / repair_3mf: load, run the pipeline, run the tiers, write the
output. process_file (the batch driver) stays in the repair.py shim because it
uses the VERSION literal that the release tooling parses from that file.
Moved verbatim from sutura/repair.py; no logic change.
"""
import os
import time

import numpy as np

import history
import local_exact
import repair_score
import triage
from confidence import repair_confidence
from defects import detect as detect_defects
from sutura_engine import health as method_health
from sutura_engine import trace
from sutura_engine.pipeline.mesh_io import (
    aggregate_triangle_attributes, obj_has_material_refs,
    parse_3mf_mesh_blocks, read_3mf_units,
    rewrite_3mf_meshes, save_mesh, scan_bad_coordinates,
)
from sutura_engine.pipeline.p_weld import (
    enforce_reload_verdict, p_weld_final, reload_strict_holes_nm,
)
from sutura_engine.pipeline.pipeline import repair_mesh_from_arrays
from sutura_engine.pipeline.report import (
    aggregate_print_reports, build_print_report,
)
from sutura_engine.pipeline.stage1 import (
    _local_exact_budget, _should_run_local_exact,
)
from sutura_engine.pipeline.stage2 import maybe_run_stage2
from sutura_engine.pipeline.tiers.engines import run_after_stage2_engines
from sutura_engine.pipeline.tiers.flap import (
    _flap_final_needs_off, _flap_off_preferred, drop_flap_debris,
)
from sutura_engine.pipeline.tiers.tamponade import (
    _tamponade_final_needs_off, _tamponade_off_preferred,
)


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
                 si_mode=None, dressing_force_adopt=False,
                 splint=False, splint_adaptive=None):
    """Repair a single STL/OBJ/3MF file. Returns the report dict."""
    import pymeshlab as ml

    bad_coords = scan_bad_coordinates(src)
    if bad_coords:
        raise ValueError('input mesh contains %s' % bad_coords)

    ext = os.path.splitext(src)[1].lower()
    declared_unit = None
    mesh_block = None
    if ext == '.3mf':
        blocks = parse_3mf_mesh_blocks(src)
        if not blocks:
            raise ValueError('no mesh objects found in 3MF')
        if len(blocks) != 1:
            raise ValueError(
                'repair_file expects a single-object 3MF (%d mesh blocks found); '
                'use repair_3mf for multi-object files' % len(blocks))
        mesh_block = blocks[0]
        verts = np.asarray(mesh_block.verts, dtype=np.float32)
        tris = np.asarray(mesh_block.tris, dtype=np.int32)
        declared_unit = read_3mf_units(src).get(
            mesh_block.model_name, 'millimeter')
    else:
        from sutura_engine.pipeline.mesh_io import load_mesh

        verts, tris = load_mesh(src)
        verts = np.asarray(verts, dtype=np.float32)
        tris = np.asarray(tris, dtype=np.int32)

    if not trace.is_active() and os.environ.get('SUTURA_TRACE'):
        trace.begin_trace(trace_path=None, input_file=src,
                          command_args={'mode': mode, 'profile': profile, 'si_mode': si_mode})

    trace.set_context(object_id='main', attempt='primary')
    if trace.is_active():
        trace.record_stage('load', 'mesh_io.load_mesh', None, None, verts, tris,
                           gate={'status': 'ok', 'source': src})

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
            splint=splint, splint_adaptive=splint_adaptive,
            dressing_force_adopt=dressing_force_adopt)
        report['_fp'] = history.mesh_fingerprint(verts, tris)
        report['defects'] = detect_defects(verts, tris)
        if ext == '.obj':
            report['material_discarded'] = obj_has_material_refs(src)

        # Stage 2 applies to watertight results; shared helper keeps the single-
        # mesh path and per-object 3MF repair on the same code (same gate, same
        # OBJ round-trip, same explicit skip reporting).
        _s2_in_v, _s2_in_t = new_v, new_t
        new_v, new_t = maybe_run_stage2(report, new_v, new_t, tmpdir)
        if trace.is_active():
            trace.record_stage('stage2', 'stage2.maybe_run_stage2',
                               _s2_in_v, _s2_in_t, new_v, new_t,
                               gate=report.get('stage2', {'status': 'skipped'}))

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
            _pw_in_v, _pw_in_t = new_v, new_t
            new_v, new_t, _pwel = p_weld_final(ml, new_v, new_t)
            if _pwel is not None:
                report['p_weld'] = _pwel
            if trace.is_active():
                trace.record_stage('p_weld', 'p_weld.p_weld_final',
                                   _pw_in_v, _pw_in_t, new_v, new_t,
                                   gate=_pwel or {'status': 'noop'})
        else:
            if trace.is_active():
                trace.record_stage('p_weld', 'p_weld.p_weld_final',
                                   new_v, new_t, new_v, new_t,
                                   gate={'status': 'skipped', 'reason': 'not_stl'})

        # Localized exact self-union of residual SI clusters (experimental, OFF
        # by default). Runs on the post-Stage-2 + post-P-WELD mesh; a no-op that
        # returns the input arrays unchanged when disabled or when no cluster is
        # accepted.
        if _should_run_local_exact(report, triage_spec):
            _lse_spec = triage_spec or triage.resolve_intensity(None)
            new_v, new_t, _lse = local_exact.local_exact_self_union(
                new_v, new_t, time_budget=_local_exact_budget(_lse_spec))
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
        if trace.is_active():
            trace.record_stage('validate', 'service.enforce_reload_verdict',
                               new_v, new_t, new_v, new_t,
                               gate={'repaired_is_watertight': report.get('repaired_is_watertight')})
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
                trace.set_context(attempt='flap_off_safety_net')
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
                trace.set_context(attempt='tamponade_off_safety_net')
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

    report['print_report'] = build_print_report(
        report, in_arrays=(verts, tris), out_arrays=(new_v, new_t))

    report.setdefault('timings', {})
    report['timings']['file_total'] = round(time.perf_counter() - _t_file, 3)
    report['timings']['flap_off_rerun'] = bool(
        (report.get('flap') or {}).get('final_fallback', {}).get('ran'))
    report['timings']['tamponade_off_rerun'] = bool(
        (report.get('tamponade') or {}).get('final_fallback', {}).get('ran'))
    if ext == '.3mf':
        stats = rewrite_3mf_meshes(src, out, [(new_v, new_t)])
        report['triangle_attributes'] = stats[0]
    else:
        save_mesh(out, new_v, new_t)
    if trace.is_active():
        trace.finish_trace(final_verts=new_v, final_tris=new_t, final_report=report)
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
               si_mode=None, dressing_force_adopt=False,
               splint=False, splint_adaptive=None):
    """Repair every object mesh in a 3MF archive, preserving structure.

    Per-object Stage 2: any object that stage 1 closes (two-manifold with no
    remaining holes) is passed through the shared ``maybe_run_stage2`` helper,
    so a closed object gets a Manifold watertight rebuild exactly like a
    single-mesh file (the helper sets the per-object ``stage2`` report and
    returns the rebuilt arrays). Objects that remain open after stage 1 are
    written as stage-1 output, mirroring the single-mesh behaviour.
    """
    import pymeshlab as ml
    blocks = parse_3mf_mesh_blocks(src)
    if not blocks:
        return {'error': 'no mesh objects found in 3MF'}

    if not trace.is_active() and os.environ.get('SUTURA_TRACE'):
        trace.begin_trace(trace_path=None, input_file=src,
                          command_args={'mode': mode, 'profile': profile, 'si_mode': si_mode})

    units = read_3mf_units(src)
    reports = []
    updates = []
    cache = {}
    for i, block in enumerate(blocks):
        verts = np.asarray(block.verts, dtype=np.float32)
        tris = np.asarray(block.tris, dtype=np.int32)
        obj_name = getattr(block, 'model_name', None) or f"object_{i}"
        trace.set_context(object_id=obj_name, attempt='primary')
        if trace.is_active():
            trace.record_stage('load', '3mf.parse_3mf_mesh_blocks', None, None, verts, tris,
                               gate={'status': 'ok', 'object_name': obj_name})
        declared = units.get(block.model_name, 'millimeter')
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
                splint=splint, splint_adaptive=splint_adaptive,
                dressing_force_adopt=dressing_force_adopt)
            rep['defects'] = detect_defects(verts, tris)
            rep['_fp'] = history.mesh_fingerprint(verts, tris)
            # Per-object stage 2 first: closed objects get a watertight
            # rebuild (same helper/gate as the single-mesh path), and the
            # confidence/health/risk below then see the stage2 outcome.
            _s2_in_v, _s2_in_t = new_v, new_t
            new_v, new_t = maybe_run_stage2(rep, new_v, new_t, tmpdir)
            if trace.is_active():
                trace.record_stage('stage2', 'stage2.maybe_run_stage2',
                                   _s2_in_v, _s2_in_t, new_v, new_t,
                                   gate=rep.get('stage2', {'status': 'skipped'}))
            if engines:
                new_v, new_t = run_after_stage2_engines(
                    ml, rep, new_v, new_t, tmpdir, engines, engine_chain,
                    triage_spec or triage.resolve_intensity(None))
            if _should_run_local_exact(rep, triage_spec):
                _lse_spec = triage_spec or triage.resolve_intensity(None)
                new_v, new_t, _lse = local_exact.local_exact_self_union(
                    new_v, new_t,
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
            if trace.is_active():
                trace.record_stage('validate', 'service.enforce_reload_verdict',
                                   new_v, new_t, new_v, new_t,
                                   gate={'repaired_is_watertight': rep.get('repaired_is_watertight')})
            rep['print_report'] = build_print_report(
                rep, in_arrays=(verts, tris), out_arrays=(new_v, new_t))
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
        updates.append((new_v, new_t))

    stats = rewrite_3mf_meshes(src, out, updates)
    for rep, st in zip(reports, stats):
        rep['triangle_attributes'] = st

    agg = {'objects': len(blocks), 'object_reports': reports}
    agg['print_report'] = aggregate_print_reports(
        [r.get('print_report') for r in reports])
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
        _agg_attrs = aggregate_triangle_attributes(reports)
        if _agg_attrs is not None:
            agg['triangle_attributes'] = _agg_attrs
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
    if trace.is_active():
        trace.finish_trace()
    return agg
