"""Batch repair summary - single source of truth for result classification.

stdlib-only by design (no numpy, no pymeshlab, no Manifold): the CLI runs
under the PyMeshLab venv while the GUI shells out to it, so both sides can
import this module regardless of which interpreter is running. Keep it free
of third-party imports so it stays importable everywhere.

The module stays language-neutral: classify() returns stable machine-readable
codes/keys. The CLI (English, not localized) formats them via ISSUE_LABELS;
the GUI localizes them through its own EN/TR dictionary.
"""

# Machine-readable issue codes, with a human-readable English label for the
# CLI's --human summary (the GUI localizes these separately).
ISSUE_LABELS = {
    'volume_warning': 'Volume change',
    'stage2_skipped': 'Stage 2 skipped',
    'stage2_error': 'Stage 2 error',
    'partial': 'Partial repair (holes remaining)',
    'reload_non_manifold_vertices': 'Saved mesh reloads with non-manifold vertices',
    'malformed': 'Malformed input',
    'error': 'Error',
    'budget_exceeded': 'Repair budget exceeded',
    'shape_changed': 'Shape changed by the fTetWild fallback',
    'graft_detail_loss': 'Graft smoothed fine surface detail (see warnings)',
    'dressing_detail_loss': 'Dressing coat deviates from the input surface (see warnings)',
}

# Stable summary keys returned by classify(); the GUI maps these to localized
# strings, the CLI (single-file) doesn't use the short summary.
SUMMARY_KEYS = ('watertight', 'stage2_skipped', 'stage2_error',
                'holes', 'partial', 'error', 'budget_declined')


def _obj_closed(r):
    """True when one object's stage-1 output is a closed manifold."""
    s1 = r.get('stage1', {})
    return bool(s1.get('two_manifold')) and s1.get('holes_remaining', 0) == 0


def _classify_objects(reports, issues):
    """Classify a multi-object 3MF result considering EVERY object.

    The whole file is only 'watertight' when every object is stage-1 closed
    AND confirmed by stage 2. If all objects close but stage 2 is missing/
    skipped/errored on some, it is the stage2 warning. If any object remains
    open, the file is a partial repair. A volume_warning from any object is
    surfaced too."""
    n = len(reports)
    if any(r.get('shape_changed') for r in reports):
        issues.append('shape_changed')
    if any(isinstance(r.get('graft'), dict) and r['graft'].get('adopted')
           and r['graft'].get('warnings') for r in reports):
        issues.append('graft_detail_loss')
    if any(isinstance(r.get('dressing'), dict) and r['dressing'].get('adopted')
           and r['dressing'].get('warnings') for r in reports):
        issues.append('dressing_detail_loss')
    closed = [_obj_closed(r) for r in reports]
    all_closed = n > 0 and all(closed)

    outcomes = []
    for r in reports:
        s2 = r.get('stage2') or {}
        if 'error' not in s2:
            outcomes.append('none')
        elif isinstance(s2.get('error'), str) and s2['error'].startswith('Stage 2 skipped'):
            outcomes.append('skipped')
        else:
            outcomes.append('error')
    all_s2_ok = n > 0 and all(out == 'none' for out in outcomes) \
        and all(bool(r.get('stage2', {}).get('ok')) for r in reports)
    any_error = 'error' in outcomes
    any_skipped = 'skipped' in outcomes

    if all_closed and all_s2_ok:
        return 'watertight', issues, 'watertight'
    if all_closed:
        code = 'stage2_error' if any_error else 'stage2_skipped'
        issues.append(code)
        return 'warning', issues, code
    issues.append('partial')
    if any(r.get('stage1', {}).get('volume_warning') for r in reports):
        issues.append('volume_warning')
    return 'warning', issues, 'partial'


def classify(data):
    """Classify one repair result dict into (category, issues, summary_key).

    category is one of:
      'watertight' - stage 2 ran and confirmed a closed manifold solid
      'warning'    - repaired but with a caveat (volume change, stage 2
                     skipped/errored, or holes remaining)
      'error'      - hard failure (malformed input, exception, ...)
    issues is an ordered list of machine-readable issue codes.
    summary_key is a stable key from SUMMARY_KEYS; see summary_args() for the
    dynamic argument (e.g. the hole count) the GUI uses to format it.
    """
    issues = []

    # Extreme mode with a small mesh deleted every face (mincomponentsize=20):
    # this is the intended-but-aggressive extreme behaviour, NOT malformed
    # input, so it is reported distinctly (same 'error' category, different
    # issue code + clear message). Checked before the generic error branch.
    if data.get('extreme_removed_object'):
        issues.append('extreme_removed_object')
        return 'error', issues, 'extreme_removed_object'

    # A declined save (repair budget exceeded, user did not confirm): the
    # repair itself ran but no output was written. Carries the explicit
    # top-level 'status': 'budget_declined' marker (set by process_file) so
    # callers can distinguish it from a generic hard failure and offer a
    # --force re-run.
    if data.get('status') == 'budget_declined':
        issues.append('budget_exceeded')
        return 'error', issues, 'budget_declined'

    if data.get('error') and 'stage1' not in data:
        issues.append('malformed')
        return 'error', issues, 'error'

    # Multi-object 3MF: the verdict comes from ALL objects, not just object-0
    # (object-0's stage1 is still kept top-level for backward compatibility).
    reports = data.get('object_reports')
    if reports:
        return _classify_objects(reports, issues)

    s1 = data.get('stage1', {})
    watertight_s1 = (bool(s1.get('two_manifold'))
                     and s1.get('holes_remaining', 0) == 0
                     and not s1.get('reload_non_manifold_vertices'))
    if s1.get('reload_non_manifold_vertices'):
        issues.append('reload_non_manifold_vertices')

    # An adopted fTetWild result that moved far from the input (Hausdorff
    # above repair.FTETWILD_MAX_HAUSDORFF_REL): an issue of its own, it does
    # not change the category (a closed, stage-2-confirmed result stays
    # 'watertight').
    if data.get('shape_changed'):
        issues.append('shape_changed')

    # Graft (#13) detail-loss: the shell wrap is a geometry-inventing tier; a
    # detail_loss/fidelity warning does not change the category (a closed,
    # stage-2-confirmed result stays 'watertight') but is surfaced so the CLI
    # and GUI can show the EN/TR warning message.
    _graft = data.get('graft')
    if (isinstance(_graft, dict) and _graft.get('adopted')
            and _graft.get('warnings')):
        issues.append('graft_detail_loss')

    # Dressing (#16) fidelity/time warning: same rule as Graft, it never
    # changes the category.
    _dressing = data.get('dressing')
    if (isinstance(_dressing, dict) and _dressing.get('adopted')
            and _dressing.get('warnings')):
        issues.append('dressing_detail_loss')

    # Stage 2 outcome: present-and-ok, explicitly skipped, or errored.
    s2 = data.get('stage2')
    s2_skipped = bool(
        s2 and 'error' in s2 and isinstance(s2.get('error'), str)
        and s2['error'].startswith('Stage 2 skipped'))
    s2_error = bool(s2 and 'error' in s2 and not s2_skipped)
    s2_ok = bool(s2 and 'ok' in s2 and not s2_error and not s2_skipped)

    # "Watertight" is only claimed once stage 2 actually ran and validated
    # the closed mesh. A mesh that stage 1 closed but stage 2 never confirmed
    # (missing bridge, in-process failure e.g. macOS, or explicitly skipped)
    # is a warning, not watertight.
    if watertight_s1 and s2_ok:
        return 'watertight', issues, 'watertight'

    # stage 1 closed it but stage 2 did not confirm the solid (skipped,
    # errored, or never ran - e.g. missing bridge, macOS in-process failure).
    # Output is still written via stage 1, so this is a warning, not a hard
    # error; it is never reported as watertight.
    if watertight_s1 and (s2_skipped or s2_error or not s2):
        code = 'stage2_error' if s2_error else 'stage2_skipped'
        issues.append(code)
        return 'warning', issues, code

    # Stage 1 left it open or with problems.
    if s1.get('volume_warning'):
        issues.append('volume_warning')
    if not watertight_s1:
        issues.append('partial')
        if s1.get('two_manifold') and s1.get('holes_remaining', 0) > 0:
            return 'warning', issues, 'holes'
        if not s1.get('two_manifold'):
            return 'warning', issues, 'partial'
    return 'warning', issues, 'partial'


def summary_args(data):
    """Dynamic argument(s) for formatting the summary_key (e.g. hole count).

    Returns a tuple to pass into the localized string template.
    """
    if data.get('stage1', {}).get('two_manifold'):
        return (data.get('stage1', {}).get('holes_remaining', 0),)
    return ()


def issue_label(code):
    """English human-readable label for an issue code (CLI --human)."""
    return ISSUE_LABELS.get(code, code)


# Human verdicts: the machine category stays unchanged; this maps it to the
# one line a normal user needs. The GUI localizes these keys.
VERDICT_LABELS = {
    'ready': 'Ready to print',
    'notes': 'Repaired with notes',
    'failed': 'Could not fully repair',
}


def _num(v, default=None):
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        return default
    return v


def _fixed_counts(data):
    out = {}
    for f in ((data.get('print_report') or {}).get('fixed') or []):
        try:
            out[f['code']] = int(f.get('count') or 0)
        except (KeyError, TypeError, ValueError):
            continue
    return out


def _code_value(data, code):
    for f in ((data.get('print_report') or {}).get('fixed') or []):
        if f.get('code') == code:
            try:
                return int(f.get('count') or 0)
            except (TypeError, ValueError):
                return 0
    return 0


def _brief_notes(data, issues, pr):
    """Machine-readable notes (code + args) for the compact report."""
    notes = []
    seen = set()

    def add(code, **args):
        if code in seen:
            return
        seen.add(code)
        notes.append({'code': code, 'args': args})

    for code in issues:
        if code in ('stage2_skipped', 'stage2_error', 'shape_changed',
                    'volume_warning', 'graft_detail_loss',
                    'dressing_detail_loss', 'reload_non_manifold_vertices'):
            add(code)
    if data.get('unit_warning'):
        add('unit_warning')
    if data.get('material_discarded'):
        add('material_discarded')
    b = data.get('budget')
    if isinstance(b, dict) and b.get('within_budget') is False:
        add('budget')
    for a in (pr.get('advisories') or []):
        if a.get('code') == 'thin_wall':
            add('thin_wall', ratio=a.get('thin_ratio'),
                threshold=a.get('threshold_norm'),
                min_mm=a.get('min_mm'), estimated=bool(a.get('estimated')))
    esc = data.get('si_escalation')
    if isinstance(esc, dict) and esc.get('triggered'):
        add('si_escalation', outcome=esc.get('outcome'),
            reason=esc.get('reason'), method=esc.get('method'))
    return notes


def brief(data):
    """Compact user-facing summary for one repair result.

    Returns a dict with a stable ``verdict`` key, four plain counts and a
    machine-readable note list. It never changes ``classify()`` or the JSON
    category; the GUI and the CLI only format this dict.
    """
    category, issues, summary = classify(data)
    pr = data.get('print_report') or {}
    s1 = data.get('stage1') or {}
    geom = pr.get('geometry') or {}

    holes = _code_value(data, 'holes_closed')
    if not holes:
        holes = int(_num(s1.get('holes_closed'), 0) or 0)
    orientation = bool(_code_value(data, 'orientation_corrected'))
    if not orientation:
        vb = _num(s1.get('volume_before'))
        va = _num(s1.get('volume_after'))
        orientation = bool(vb is not None and va is not None and vb < 0 < va)
    parts_before = _num(geom.get('components_before'))
    if parts_before is None:
        parts_before = _num(s1.get('components_before'))
    parts_after = _num(geom.get('components_after'))
    if parts_after is None:
        parts_after = _num(s1.get('components'))
    shape = _num(geom.get('deviation_max_pct'))
    shape_source = 'deviation' if shape is not None else None
    if shape is None:
        shape = _num(geom.get('volume_change_pct'))
        if shape is not None:
            shape_source = 'volume'

    notes = _brief_notes(data, issues, pr)
    out = data.get('output')
    failed = (category == 'error' or summary in ('partial', 'holes')
              or not out)
    if failed:
        holes_left = 0
        for it in (pr.get('issues') or []):
            if it.get('code') == 'holes_remaining':
                holes_left = int(it.get('count') or 0)
                break
        if not holes_left:
            holes_left = int(_num(s1.get('holes_remaining'), 0) or 0)
        if holes_left > 0:
            notes.append({'code': 'holes_remaining',
                          'args': {'count': holes_left}})
        elif summary in ('partial', 'holes') or (category != 'error' and not out):
            notes.append({'code': 'not_closed', 'args': {}})
        verdict = 'failed'
    elif category == 'watertight' and not issues and not notes:
        verdict = 'ready'
    else:
        verdict = 'notes'
    return {
        'verdict': verdict,
        'holes_closed': holes,
        'orientation_corrected': orientation,
        'parts_before': None if parts_before is None else int(parts_before),
        'parts_after': None if parts_after is None else int(parts_after),
        'shape_change_pct': shape,
        'shape_source': shape_source,
        'notes': notes,
        'error': data.get('error'),
    }
