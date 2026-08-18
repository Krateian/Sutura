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
}

# Stable summary keys returned by classify(); the GUI maps these to localized
# strings, the CLI (single-file) doesn't use the short summary.
SUMMARY_KEYS = ('watertight', 'stage2_skipped', 'stage2_error',
                'holes', 'partial', 'error')


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

    if data.get('error') and 'stage1' not in data:
        issues.append('malformed')
        return 'error', issues, 'error'

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
