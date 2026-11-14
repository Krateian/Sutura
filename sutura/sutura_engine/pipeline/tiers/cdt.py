# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: MIT OR Apache-2.0
"""CDT (#last-resort solid engine): exact constrained tetrahedralization.

Runs the Rust ``sutura_geom.cdt3d_solid`` core on the original input arrays.
It is selected by ``solid_engine='cdt'`` and sits after Coaptation and before
fTetWild.  The exact B5 certificate is OFF here; a candidate is adopted only
when it reloads strict-watertight, is one part, and passes the existing
adoption guard.  Failures and budget/timeout results are recorded and the
ladder keeps its current mesh so fTetWild can fall through.
"""
import json
import time

import numpy as np

import topology
from sutura_engine import health as method_health
from sutura_engine.pipeline.p_weld import weld_reload_equivalent
from sutura_engine.pipeline.tiers.splint import _count_defects
from sutura_engine.pipeline.adopt_guard import check_candidate, policy_for
from sutura_engine.pipeline.presets import CDT_MAX_TETS
from sutura_engine.pipeline.tiers.engine import (
    guard_params, tier_core, tier_engine,
)


def cdt_tier(v, t, input_v, input_t, after, stats, tmpdir, spec=None,
             wall_timeout_ms=None):
    """CDT tier entry point (Python by default, Rust when opted in)."""
    v = np.asarray(v, dtype=np.float64)
    t = np.asarray(t, dtype=np.int64)
    input_v = np.asarray(input_v if input_v is not None else v,
                         dtype=np.float64)
    input_t = np.asarray(input_t if input_t is not None else t, dtype=np.int64)
    core = tier_core()
    if tier_engine('cdt') != 'rust' or core is None:
        return _cdt_tier_python(
            v, t, input_v, input_t, after, stats, tmpdir, spec, wall_timeout_ms)
    return _cdt_tier_rust(
        v, t, input_v, input_t, after, stats, tmpdir, spec, wall_timeout_ms,
        core)


def _cdt_tier_rust(v, t, input_v, input_t, after, stats, tmpdir, spec,
                   wall_timeout_ms, core):
    """Rust-backed CDT tier (same cap, gates and report keys)."""
    stats = stats if stats is not None else {}
    cap = getattr(spec, 'ftetwild_max_faces', None) if spec is not None else None
    if wall_timeout_ms is None:
        wall_timeout_ms = int(
            float(getattr(spec, 'ftetwild_timeout', 180.0)) * 1000)
    drill = json.dumps({
        'cap': (int(cap) if cap is not None else None),
        'wall_timeout_ms': int(wall_timeout_ms),
        'max_tets': int(CDT_MAX_TETS),
        'input_faces': int(len(input_t)),
        'guard': guard_params('splint'),
    })
    payload = None
    with method_health.tier_guard('cdt', 'tier') as _g:
        payload = core.tier_cdt(
            np.ascontiguousarray(v, dtype=np.float64),
            np.ascontiguousarray(t, dtype=np.int64),
            np.ascontiguousarray(input_v, dtype=np.float64),
            np.ascontiguousarray(input_t, dtype=np.int64), drill)
    if _g.error is not None or payload is None:
        rec = {'tier': 'cdt', 'engine': 'cdt', 'ran': False,
               'adopted': False, 'reason': 'error: %s: %s'
               % (type(_g.error).__name__, _g.error)}
        stats['cdt'] = rec
        return v, t, after
    out_v, out_t, rec_json = payload
    rec = json.loads(rec_json)
    stats['cdt'] = rec
    if not rec.get('adopted'):
        return v, t, after
    out_v = np.asarray(out_v, dtype=np.float64)
    out_t = np.asarray(out_t, dtype=np.int64)
    return (out_v, out_t,
            topology.mesh_topology(np.asarray(out_v, dtype=np.float32),
                                   np.asarray(out_t, dtype=np.int32)))


def _cdt_tier_python(v, t, input_v, input_t, after, stats, tmpdir, spec=None,
                     wall_timeout_ms=None):
    """Try the CDT exact solid engine.  Never raises; keeps ``v``/``t`` when
    the candidate is unavailable, invalid, over budget or rejected."""
    rec = {
        'tier': 'cdt', 'engine': 'cdt', 'ran': False, 'adopted': False,
        'watertight': False, 'candidate_watertight': False,
        'seconds': None, 'wall_seconds': None,
        'input_faces': int(len(input_t)),
        'output_faces': None, 'output_vertices': None, 'output_parts': None,
        'holes': None, 'non_manifold': None, 'parts': None,
        'bbox_ok': None, 'hausdorff': None,
        'verify_coverage': False, 'coverage_certified': False,
        'reject_reason': None, 'reason': None, 'wall_timeout_ms': None,
        'max_tets': CDT_MAX_TETS, 'budget_limited_by': None,
    }
    cap = getattr(spec, 'ftetwild_max_faces', None) if spec is not None else None
    if cap is not None and len(input_t) > int(cap):
        rec['reject_reason'] = 'too_large'
        rec['reason'] = ('input has %d faces, above the CDT cap of %d; '
                         'the ladder result was kept' % (len(input_t), int(cap)))
        stats['cdt'] = rec
        return v, t, after
    if wall_timeout_ms is None:
        wall_timeout_ms = int(float(getattr(spec, 'ftetwild_timeout', 180.0)) * 1000)
    rec['wall_timeout_ms'] = int(wall_timeout_ms)
    with method_health.tier_guard('cdt', 'tier') as _g:
        try:
            import sutura_geom as _sg
            core = getattr(_sg, 'cdt3d_solid', None)
        except Exception:  # noqa: BLE001
            core = None
        if core is None:
            rec['reject_reason'] = 'unavailable'
            rec['reason'] = ('cdt3d_solid is unavailable in this sutura_geom '
                             'build; the ladder result was kept')
            stats['cdt'] = rec
            return v, t, after
        t0 = time.perf_counter()
        try:
            cand_v, cand_t, srec = core(
                np.asarray(input_v, dtype=np.float64),
                np.asarray(input_t, dtype=np.int32),
                opts={
                    'verify_coverage': False,
                    'max_tets': int(CDT_MAX_TETS),
                    'wall_timeout_ms': int(wall_timeout_ms),
                    'label_mode': 'gwn_threshold',
                })
        except BaseException as e:  # pyo3 panic is a BaseException
            rec['reject_reason'] = 'error'
            rec['reason'] = 'error: %s: %s' % (type(e).__name__, e)
            rec['wall_seconds'] = time.perf_counter() - t0
            stats['cdt'] = rec
            return v, t, after
        rec['ran'] = True
        rec['wall_seconds'] = time.perf_counter() - t0
        rec['seconds'] = srec.get('seconds', rec['wall_seconds'])
        for key in ('output_faces', 'cap_faces', 'cap_area', 'parts', 'holes',
                    'nonmanifold', 'bbox_ok', 'hausdorff', 'valid', 'tets',
                    'constraint_cut_faces', 'free_cut_faces', 'inside_cells',
                    'orientation_used', 'orientation_components',
                    'orientation_flipped', 'w_hist', 'coverage_certified',
                    'verify_coverage', 'max_tets', 'wall_timeout_ms', 'hash'):
            if key in srec:
                rec[key] = srec.get(key)
        rec['output_parts'] = srec.get('parts')
        rec['output_vertices'] = int(len(cand_v))
        rec['output_faces'] = int(len(cand_t))
        cand_v = np.asarray(cand_v)
        cand_t = np.asarray(cand_t)
        if len(cand_t):
            cand_v, cand_t = weld_reload_equivalent(cand_v, cand_t)
            cand_holes, cand_nm = _count_defects(cand_v, cand_t)
            rec['holes'] = int(cand_holes)
            rec['non_manifold'] = int(cand_nm)
            rec['parts'] = int(srec.get('parts') or 0)
            rec['faces'] = int(len(cand_t))
            if cand_holes == 0 and cand_nm == 0 and rec['parts'] == 1:
                guard = check_candidate(
                    len(input_t), len(input_v), len(cand_t), len(cand_v),
                    slack_units=0, base_arrays=(v, t),
                    candidate_arrays=(cand_v, cand_t), measure_si=True,
                    policy=policy_for('splint'))
                for key, value in guard.fields.items():
                    if key in rec:
                        rec[key] = value
                if not guard.ok:
                    rec['reject_reason'] = 'guard'
                    rec['reason'] = 'rejected: %s' % guard.reason
                else:
                    after = topology.mesh_topology(
                        np.asarray(cand_v, dtype=np.float32),
                        np.asarray(cand_t, dtype=np.int32))
                    rec['adopted'] = True
                    rec['candidate_watertight'] = True
                    rec['reason'] = 'strict-watertight one-part CDT candidate'
            else:
                rec['reject_reason'] = 'not_watertight'
                rec['reason'] = ('candidate not strict-watertight one-part '
                                 '(parts=%s holes=%d non-manifold=%d)'
                                 % (rec['parts'], cand_holes, cand_nm))
        else:
            rec['reject_reason'] = 'empty'
            rec['reason'] = 'cdt3d_solid returned no geometry'
    if _g.error is not None:
        rec['reject_reason'] = rec.get('reject_reason') or 'error'
        rec['reason'] = 'error: %s: %s' % (type(_g.error).__name__, _g.error)
    stats['cdt'] = rec
    if rec['adopted']:
        return (np.asarray(cand_v, dtype=np.float64),
                np.asarray(cand_t, dtype=np.int64), after)
    return v, t, after
