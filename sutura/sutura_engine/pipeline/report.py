# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Human-readable reports and interactive budget prompts (P1a split).
Moved verbatim from sutura/repair.py; no logic change."""
import sys
import time

import numpy as np

from sutura_engine import health as method_health
from sutura_engine.pipeline.tiers.dressing import _dressing_suggestion_text

# --- Print-impact report ---------------------------------------------------
#
# A per-defect decomposition of an already-built repair report: what was FIXED,
# what is LEFT and how each remaining condition affects an FDM print, plus the
# geometry-change numbers (volume/surface/deviation). It is assembled here (the
# existing report home) and never re-measures the defects themselves: every
# count comes from ``stage1`` / ``stage2`` / ``p_weld`` / the tier reports. The
# single top-line "is it good" verdict stays repair_score's Status; this layer
# only explains the individual conditions behind it.
#
# Severity language (three levels, stable codes; the GUI localizes them):
SEVERITY_BREAKS = 'breaks'          # breaks the print
SEVERITY_MAY = 'may_affect'         # may affect the print
SEVERITY_COSMETIC = 'cosmetic'      # cosmetic, slicers ignore it
_SEVERITY_RANK = {SEVERITY_BREAKS: 0, SEVERITY_MAY: 1, SEVERITY_COSMETIC: 2}

# Deviation gate: above this face count the per-repair point-to-mesh deviation
# is not measured; the report records it under ``not_assessed`` to keep the
# repair fast on huge scans. Measured with the Rust ``sutura_geom`` BVH
# (``closest_points``), sampling the result surface toward the input.
DEVIATION_MAX_FACES = 150_000
DEVIATION_SAMPLES = 20_000

# Heuristic thresholds (NOT read from the mesh literature or the repo; they are
# engineering estimates, flagged ``heuristic`` in the issue detail so the UI can
# label them as estimates). Volume change alone is repo-grounded: the existing
# volume_warning fires above 15 % (pipeline.stage1), reused here as the breaks
# level.
_VOLUME_MAY_PCT = 5.0
_VOLUME_BREAKS_PCT = 15.0        # matches the existing volume_warning threshold
_DEVIATION_MAY_FRAC = 0.01       # 1 % of the bounding-box diagonal
_DEVIATION_BREAKS_FRAC = 0.05    # 5 % of the bounding-box diagonal


def _bbox_diagonal(verts):
    """Bounding-box space diagonal of an (N, 3) vertex array, or 0.0."""
    if verts is None:
        return 0.0
    v = np.asarray(verts, dtype=np.float64)
    if v.size == 0:
        return 0.0
    lo = v.min(axis=0)
    hi = v.max(axis=0)
    return float(np.linalg.norm(hi - lo))


def _deviation_pct(report, in_arrays, out_arrays):
    """Result-to-input deviation as a % of the input bbox diagonal, or None.

    Reuses a tier's already-computed per-vertex deviation when present (Graft's
    ``per_vertex_deviation`` or Dressing's ``hausdorff_rel_max``); otherwise
    measures a fresh sampled point-to-mesh distance with the Rust BVH
    (``sutura_geom.closest_points``, result surface -> input surface). Returns
    None past the face gate (``DEVIATION_MAX_FACES``) so the caller can report
    it as ``not_assessed`` rather than pay the cost on a huge mesh.
    """
    in_v = _as_f64(in_arrays[0]) if in_arrays else None
    in_t = _as_i32(in_arrays[1]) if in_arrays else None
    diag = _bbox_diagonal(in_v)
    if diag > 0:
        g = report.get('graft') or {}
        pvd = g.get('per_vertex_deviation')
        if pvd is None:
            pvd = report.get('per_vertex_deviation')
        if pvd is not None:
            d = np.abs(np.asarray(pvd, dtype=np.float64))
            if d.size:
                return {'max_pct': round(float(d.max()) / diag * 100, 4),
                        'mean_pct': round(float(d.mean()) / diag * 100, 4),
                        'samples': int(d.size), 'source': 'graft'}
        dr = report.get('dressing') or {}
        hr = dr.get('hausdorff_rel_max')
        if isinstance(hr, (int, float)) and not isinstance(hr, bool):
            return {'max_pct': round(float(hr) * 100, 4),
                    'mean_pct': None, 'samples': None, 'source': 'dressing'}
    out_v = _as_f64(out_arrays[0]) if out_arrays else None
    out_t = _as_i32(out_arrays[1]) if out_arrays else None
    if in_v is None or in_t is None or out_v is None or out_t is None or diag <= 0:
        return None
    if out_t.shape[0] > DEVIATION_MAX_FACES or in_t.shape[0] > DEVIATION_MAX_FACES:
        return None
    try:
        import sutura_geom
        if out_v.shape[0] > DEVIATION_SAMPLES:
            idx = np.linspace(0, out_v.shape[0] - 1,
                              DEVIATION_SAMPLES).astype(np.int64)
            pts = out_v[idx]
        else:
            pts = out_v
        _q, d, _f = sutura_geom.closest_points(in_v, in_t, pts)
        d = np.asarray(d, dtype=np.float64)
        if d.size == 0:
            return None
        return {'max_pct': round(float(d.max()) / diag * 100, 4),
                'mean_pct': round(float(d.mean()) / diag * 100, 4),
                'samples': int(d.size), 'source': 'measured'}
    except Exception:  # noqa: BLE001 - deviation is informational, never fatal
        return None


def _as_f64(a):
    return None if a is None else np.asarray(a, dtype=np.float64)


def _as_i32(a):
    return None if a is None else np.asarray(a, dtype=np.int32)


def _volume_issue(vol):
    """Severity-coded issue for a |volume change| above the heuristic bands."""
    if not isinstance(vol, (int, float)) or isinstance(vol, bool):
        return None
    a = abs(float(vol))
    if a > _VOLUME_BREAKS_PCT:
        sev = SEVERITY_BREAKS
    elif a > _VOLUME_MAY_PCT:
        sev = SEVERITY_MAY
    else:
        return None
    return {'code': 'volume_change', 'severity': sev, 'count': None,
            'message_key': 'pr_volume_change',
            'detail': {'pct': round(float(vol), 2), 'heuristic': True}}


def _deviation_issue(dev_pct):
    """Severity-coded issue for a |deviation| above the heuristic bands."""
    if not isinstance(dev_pct, (int, float)) or isinstance(dev_pct, bool):
        return None
    a = abs(float(dev_pct))
    if a > _DEVIATION_BREAKS_FRAC * 100:
        sev = SEVERITY_BREAKS
    elif a > _DEVIATION_MAY_FRAC * 100:
        sev = SEVERITY_MAY
    else:
        return None
    return {'code': 'shape_deviation', 'severity': sev, 'count': None,
            'message_key': 'pr_shape_deviation',
            'detail': {'pct': round(float(dev_pct), 3), 'heuristic': True}}


def build_print_report(report, in_arrays=None, out_arrays=None):
    """Per-defect print-impact decomposition of a finished repair report.

    Returns ``{'issues': [...], 'fixed': [...], 'geometry': {...},
    'not_assessed': [...]}`` or None when there is no stage-1 result to
    describe (hard error, missing report). Pure mapping over already-computed
    fields; the only optional measurement is the sampled deviation in
    ``_deviation_pct`` (Rust BVH, gated by ``DEVIATION_MAX_FACES``).
    """
    s1 = report.get('stage1')
    if not isinstance(s1, dict):
        return None

    issues = []
    fixed = []

    holes_rem = _int(s1.get('holes_remaining'))
    if holes_rem > 0:
        issues.append({'code': 'holes_remaining', 'severity': SEVERITY_BREAKS,
                       'count': holes_rem, 'message_key': 'pr_holes_remaining',
                       'detail': {'two_manifold': bool(s1.get('two_manifold'))}})
    nm_rem = _int(s1.get('non_manifold_edges_remaining'))
    if nm_rem > 0:
        issues.append({'code': 'non_manifold_remaining',
                       'severity': SEVERITY_BREAKS, 'count': nm_rem,
                       'message_key': 'pr_non_manifold_remaining'})
    si = s1.get('self_intersections_remaining')
    if isinstance(si, (int, float)) and not isinstance(si, bool) and si > 0:
        issues.append({'code': 'self_intersections_remaining',
                       'severity': SEVERITY_COSMETIC, 'count': int(si),
                       'message_key': 'pr_self_intersections_remaining',
                       'detail': {'note': 'even_odd_double_wall_caveat'}})

    comp = s1.get('components')
    comp_before = s1.get('components_before')
    if _int(comp) > 1:
        issues.append({'code': 'multiple_shells', 'severity': SEVERITY_MAY,
                       'count': _int(comp), 'message_key': 'pr_multiple_shells'})
    removed_shells = max(_int(comp_before) - _int(comp), 0)
    if removed_shells > 0:
        fixed.append({'code': 'shells_removed', 'count': removed_shells})

    hc = _int(s1.get('holes_closed'))
    if hc > 0:
        fixed.append({'code': 'holes_closed', 'count': hc})
    nmf = _int(s1.get('non_manifold_edges_fixed'))
    if nmf > 0:
        fixed.append({'code': 'non_manifold_fixed', 'count': nmf})
    fr = _int(s1.get('faces_removed'))
    if fr > 0:
        fixed.append({'code': 'faces_removed', 'count': fr})
    vb = s1.get('volume_before')
    va = s1.get('volume_after')
    if (isinstance(vb, (int, float)) and not isinstance(vb, bool)
            and isinstance(va, (int, float)) and not isinstance(va, bool)
            and vb < 0 and va > 0):
        fixed.append({'code': 'orientation_corrected', 'count': 1})

    vol = s1.get('volume_change_percent')
    vi = _volume_issue(vol)
    if vi:
        issues.append(vi)

    not_assessed = []
    dev = _deviation_pct(report, in_arrays, out_arrays)
    if dev is None:
        not_assessed.append('deviation')
    else:
        di = _deviation_issue(dev.get('max_pct'))
        if di:
            issues.append(di)

    issues.sort(key=lambda it: _SEVERITY_RANK.get(it.get('severity'), 9))
    geometry = {
        'volume_change_pct': vol,
        'surface_area_change_pct': s1.get('surface_area_change_percent'),
        'deviation_max_pct': None if dev is None else dev.get('max_pct'),
        'deviation_mean_pct': None if dev is None else dev.get('mean_pct'),
        'deviation_source': None if dev is None else dev.get('source'),
        'faces_before': s1.get('faces_before'),
        'faces_after': s1.get('faces_after'),
        'components_before': comp_before,
        'components_after': comp,
    }
    return {'issues': issues, 'fixed': fixed, 'geometry': geometry,
            'not_assessed': not_assessed}


def aggregate_print_reports(prs):
    """Merge per-object print reports into one file-level report.

    The worst object drives the issue severities; counts are summed and the
    geometry block takes the object with the largest absolute volume change
    (a single representative worst case). Returns None when empty.
    """
    prs = [p for p in prs if p]
    if not prs:
        return None
    issues = {}
    fixed = {}
    not_assessed = set()
    for p in prs:
        for it in p.get('issues', []):
            cur = issues.get(it['code'])
            if cur is None:
                issues[it['code']] = dict(it)
            else:
                cur['count'] = (cur.get('count') or 0) + (it.get('count') or 0)
                if _SEVERITY_RANK.get(it.get('severity'), 9) < \
                        _SEVERITY_RANK.get(cur.get('severity'), 9):
                    cur['severity'] = it['severity']
        for f in p.get('fixed', []):
            fixed[f['code']] = fixed.get(f['code'], 0) + (f.get('count') or 0)
        not_assessed.update(p.get('not_assessed', []))
    worst = max(prs, key=lambda p: abs(
        (p.get('geometry') or {}).get('volume_change_pct') or 0.0))
    return {
        'issues': sorted(issues.values(),
                         key=lambda it: _SEVERITY_RANK.get(it.get('severity'), 9)),
        'fixed': [{'code': k, 'count': v} for k, v in sorted(fixed.items())],
        'geometry': worst.get('geometry'),
        'not_assessed': sorted(not_assessed),
    }


def _int(v):
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        return 0
    try:
        return int(v)
    except (TypeError, ValueError):
        return 0


def human_print_report(pr):
    """Render the print-impact block for --human mode (English labels)."""
    if not pr:
        return None
    lines = ['', 'Print impact:']
    fixed = pr.get('fixed') or []
    if fixed:
        labels = {
            'holes_closed': 'hole(s) closed',
            'non_manifold_fixed': 'non-manifold edge(s) fixed',
            'faces_removed': 'bad face(s) removed',
            'shells_removed': 'loose shell(s) removed',
            'orientation_corrected': 'orientation corrected',
        }
        lines.append('  Fixed : ' + ', '.join(
            '%d %s' % (f.get('count'), labels.get(f['code'], f['code']))
            for f in fixed))
    else:
        lines.append('  Fixed : nothing (input was already sound)')
    issues = pr.get('issues') or []
    _sev = {SEVERITY_BREAKS: 'breaks the print',
            SEVERITY_MAY: 'may affect the print',
            SEVERITY_COSMETIC: 'cosmetic, slicers ignore it'}
    if issues:
        parts = []
        for it in issues:
            n = it.get('count')
            name = it['code'].replace('_', ' ')
            if n is not None:
                parts.append('%d %s [%s]' % (n, name, _sev.get(it.get('severity'), '?')))
            else:
                parts.append('%s [%s]' % (name, _sev.get(it.get('severity'), '?')))
        lines.append('  Remaining: ' + ', '.join(parts))
    else:
        lines.append('  Remaining: none')
    g = pr.get('geometry') or {}
    seg = []
    if g.get('volume_change_pct') is not None:
        seg.append('volume %s%%' % g['volume_change_pct'])
    if g.get('deviation_max_pct') is not None:
        seg.append('deviation max %s%% of size' % g['deviation_max_pct'])
    if seg:
        lines.append('  Geometry: ' + ', '.join(seg))
    na = pr.get('not_assessed') or []
    if na:
        lines.append('  Not assessed: ' + ', '.join(na))
    return '\n'.join(lines)


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
    pr = r.get('print_report')
    if pr:
        block = human_print_report(pr)
        if block:
            lines.append(block)
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
            pp = ' + Manifold post-process' if ft_r.get('manifold_postprocessed') else ''
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
    sp_r = r.get('splint')
    if sp_r and sp_r.get('ran'):
        if sp_r.get('adopted'):
            state = ' adopted'
        elif sp_r.get('aborted'):
            state = ' NOT adopted (stopped for time; ladder result kept)'
        elif sp_r.get('skipped_budget'):
            state = ' skipped (%s)' % (sp_r.get('reason') or 'budget')
        else:
            state = ' NOT adopted (%s)' % (sp_r.get('reason') or '?')
        lines.append('  Splint (exact rebuild)  : %s faces in %.2fs, '
                     'res=%s, adaptive=%s, parts=%s, %s'
                     % (sp_r.get('output_faces') or sp_r.get('faces', 0),
                        sp_r.get('seconds', 0), sp_r.get('res'),
                        sp_r.get('adaptive'), sp_r.get('output_parts'),
                        state))
    cp_r = r.get('coaptation')
    if cp_r and cp_r.get('ran'):
        if cp_r.get('adopted'):
            state = ' adopted'
        else:
            state = ' NOT adopted (%s)' % (cp_r.get('reject_reason')
                                           or cp_r.get('reason') or '?')
        _c_cand = cp_r.get('candidate') or {}
        lines.append('  Coaptation (seam union) : %s bodies -> %s faces, '
                     'si %s -> %s, parts %s -> %s, %s'
                     % (cp_r.get('bodies_after_weld', cp_r.get('bodies_in')),
                        cp_r.get('output_faces') or _c_cand.get('faces', 0),
                        (cp_r.get('baseline') or {}).get('si'),
                        _c_cand.get('si'),
                        (cp_r.get('baseline') or {}).get('parts'),
                        _c_cand.get('parts'), state))
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
