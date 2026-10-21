# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""dressing tier (P1a split). Moved verbatim from sutura/repair.py; no logic change."""
import os

import numpy as np

import topology
from defects import detect as detect_defects
from sutura_engine import health as method_health
from sutura_engine.pipeline.p_weld import (
    _repair_welded_topology, weld_reload_equivalent,
)
from sutura_engine.pipeline.presets import (
    DRESSING_MAX_NORMAL_ANGLE_P95, DRESSING_MAX_PARTS_SLACK,
    DRESSING_MAX_VOLUME_DELTA,
)
from sutura_engine.pipeline.utils import _not_watertight_result


# CLI flags the Dressing suggestion points the user at: the plain opt-in, and
# the shape-gate override that keeps a gate-rejected (but watertight) coat.
DRESSING_SUGGEST_FLAG = '--experimental-dressing'


DRESSING_FORCE_ADOPT_FLAG = '--dressing-force-adopt'


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


def dressing_tier(v, t, input_v, input_t, after, stats, tmpdir, dressing=False,
                  intensity=None, drain=None, defects=None,
                  r_max_scale=None, sigma_scale=None, force_adopt=False,
                  ml=None):
    """Variable-viscosity coat tier for registry method 16 (Dressing).

    ``v``/``t`` is the result the ladder currently holds and
    ``input_v``/``input_t`` the original user surface this tier coats (like the
    closing / Graft / fTetWild tiers).  The candidate is adopted only
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
    v = np.asarray(v, dtype=np.float64)
    t = np.asarray(t, dtype=np.int64)
    input_v = np.asarray(input_v if input_v is not None else v, dtype=np.float64)
    input_t = np.asarray(input_t if input_t is not None else t, dtype=np.int64)
    if not dressing:
        return v, t, after
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
        return v, t, after
    with method_health.tier_guard('dressing', 'tier') as _g:
        in_v = input_v
        in_t = input_t
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
            cand_v, cand_t = _repair_welded_topology(cand_v, cand_t)
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
                vol_before = _dressing.signed_volume(v, t)
                parts_before = _dressing.count_components(t)
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
                after = topology.mesh_topology(cand_v, cand_t)
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
    if rec['adopted']:
        return (np.asarray(cand_v, dtype=np.float64),
                np.asarray(cand_t, dtype=np.int64), after)
    return v, t, after
