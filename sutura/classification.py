"""Batch repair summary - single source of truth for result classification.

stdlib-only by design (no numpy, no pymeshlab, no manifold3d): the CLI runs
under the PyMeshLab venv while the GUI shells out to it, so both sides can
import this module regardless of which interpreter is running. Keep it free
of third-party imports so it stays importable everywhere.

The module stays language-neutral: classify() returns stable machine-readable
codes/keys. The CLI (English, not localized) formats them via ISSUE_LABELS;
the GUI localizes them through its own EN/TR dictionary.
"""
import re

# Machine-readable issue codes, with a human-readable English label for the
# CLI's --human summary (the GUI localizes these separately).
ISSUE_LABELS = {
    'volume_warning': 'Volume change',
    'stage2_skipped': 'Stage 2 skipped',
    'stage2_error': 'Stage 2 error',
    'partial': 'Partial repair (holes remaining)',
    'malformed': 'Malformed input',
    'error': 'Error',
    'budget_exceeded': 'Repair budget exceeded',
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
    watertight_s1 = bool(s1.get('two_manifold')) and s1.get('holes_remaining', 0) == 0

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


def is_stage2_skipped(data):
    """True if the report says stage 2 was skipped (used for the summary)."""
    s2 = data.get('stage2')
    return bool(
        s2 and isinstance(s2.get('error'), str)
        and re.match(r'^Stage 2 skipped', s2.get('error', '')))
