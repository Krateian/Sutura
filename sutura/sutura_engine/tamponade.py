#!/usr/bin/env python3
"""Tamponade (#17) - interior hole plugging and self-intersection melting.

Tamponade closes open boundaries from the inside: the Rust core
(``sutura_geom.tamponade``) walks each boundary loop, fits a smooth reference
surface ("line 2") through the intact ring around it, trims any
rim/tail vertex that overshoots past that level, and builds a plug that follows
the reference -- a concave dent and a convex bump alike (a saddle / noisy ring
has no reference and falls back to the rim-only bridge). It optionally melts
residual self-intersections in thin layers (each layer is rolled back when it
does not reduce the crossing count) and micro-welds the seam. Unlike the
surface-based tiers it never deletes an input triangle: the report's
``healthy_faces_kept`` counts the input faces carried through verbatim, and
``reference_used`` / ``reference_fallback`` / ``trimmed_vertices`` /
``patch_vertices`` describe the reference-surface path.

The tier is on by default (disable with ``--no-tamponade`` /
``SUTURA_TAMPONADE=0``).  ``run_tamponade`` is a thin, never-raising wrapper
around the extension: an install without ``sutura_geom.tamponade`` (or with
``TAMPONADE_SUPPORTED`` absent) reports ``{'ran': False, 'reason':
'sutura_geom.tamponade unavailable'}`` and the caller keeps the mesh it had.

Pure API::

    run_tamponade(verts, tris, *, intensity=None, ml=None)
        -> (verts_out, tris_out, report)

No file is written.  The per-intensity wall-clock budgets follow the Triage
presets; ``quick`` disables the tier.
"""
from __future__ import annotations

import json
import os

import numpy as np
import sutura_geom

# Deterministic work budget (cut-grid cells) per Triage intensity preset.
# ``quick`` is 0 and skips the tier entirely; the others match the deep-repair
# ladder's ordering.
TAMPONADE_WORK_BY_INTENSITY = {
    'quick': 0,
    'balanced': 2_000_000,
    'thorough': 8_000_000,
    'extreme': 25_000_000,
}
# Budget used when no intensity preset is supplied (library/balanced default).
TAMPONADE_DEFAULT_WORK = 2_000_000
# Deterministic work budget for the Tamponade PREPASS block, in cut-grid cells.
# The Rust core charges each loop's `throat_cells` before its min-cut, so the
# stop point is a pure function of the input and the preset.  ``quick`` skips
# the prepass entirely.  The former seconds-based cap (15/40/60 s minus an 8 s
# SI reserve) made the seal's loop set depend on machine load.
TAMPONADE_PREPASS_WORK_BY_INTENSITY = {
    'quick': 0,
    'balanced': 2_000_000,
    'thorough': 8_000_000,
    'extreme': 25_000_000,
}
TAMPONADE_PREPASS_DEFAULT_WORK = 2_000_000

# The inside-out SI melt is gated by intensity: it runs only at the two
# heavier presets (or when explicitly forced with SUTURA_TAMPONADE_MELT=1),
# because slicers ignore self-intersections and the melt is not worth its
# per-layer cost at quick/balanced.
MELT_INTENSITIES = ('thorough', 'extreme')

_UNAVAILABLE_REASON = 'sutura_geom.tamponade unavailable'


def resolve_melt_si(intensity, environ=None):
    """Whether the SI melt should run for ``intensity``.

    Precedence: ``SUTURA_TAMPONADE_MELT`` (``1`` forces on, anything else
    forces off) > the intensity preset (melt only at ``thorough``/``extreme``).
    """
    env = (environ if environ is not None else os.environ).get(
        'SUTURA_TAMPONADE_MELT')
    if env is not None:
        return str(env).strip() == '1'
    return intensity in MELT_INTENSITIES


def resolve_tamponade_prepass_budget(intensity, environ=None):
    """Deterministic work cap (cut-grid cells) for the Tamponade PREPASS block.

    ``SUTURA_TAMPONADE_PREPASS_BUDGET`` overrides the per-intensity preset and
    is interpreted in work units (the former seconds meaning is retired); an
    unparseable value is ignored.  A value <= 0 makes the prepass skip itself;
    ``quick`` is 0 by preset.  The cap covers the seal; the local SI guard runs
    after the seal and is bounded by its own subset work.
    """
    env = (environ if environ is not None else os.environ).get(
        'SUTURA_TAMPONADE_PREPASS_BUDGET')
    if env is not None:
        try:
            return int(float(env))
        except (TypeError, ValueError):
            pass
    return TAMPONADE_PREPASS_WORK_BY_INTENSITY.get(
        intensity, TAMPONADE_PREPASS_DEFAULT_WORK)


def _tamponade_module():
    """Return the ``sutura_geom`` module when it exposes the Tamponade API.

    ``None`` when the extension does not export ``tamponade`` or does not
    advertise ``TAMPONADE_SUPPORTED`` (an older build)."""
    if not getattr(sutura_geom, 'TAMPONADE_SUPPORTED', False):
        return None
    if not hasattr(sutura_geom, 'tamponade'):
        return None
    return sutura_geom


def _median_edge_length(verts, tris):
    """Median triangle edge length (the default Tamponade feature size)."""
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    if len(t) == 0 or len(v) == 0:
        return 0.0
    a = v[t[:, 0]]
    b = v[t[:, 1]]
    c = v[t[:, 2]]
    edges = np.concatenate([
        np.linalg.norm(a - b, axis=1),
        np.linalg.norm(b - c, axis=1),
        np.linalg.norm(c - a, axis=1),
    ])
    return float(np.median(edges)) if len(edges) else 0.0


def run_tamponade(verts, tris, *, intensity=None, ml=None, work_budget=None):
    """Run the Rust Tamponade tier on ``verts``/``tris``.

    ``work_budget`` overrides the per-intensity deterministic work budget (the
    prepass passes its own cut-grid-cell cap so the seal cannot overrun it).

    Returns ``(verts_out, tris_out, report)``.  The function never raises: on
    any failure (extension absent, a Rust panic surfaced as a ``BaseException``
    by PyO3, malformed arrays) it returns the input arrays unchanged and a
    ``report`` whose ``ran`` is False and whose ``reason`` explains the skip.
    ``ml`` is accepted for API symmetry with the other tiers but is unused by
    this pure-Rust path.
    """
    original_v = np.asarray(verts)
    original_t = np.asarray(tris)
    unavailable = {'ran': False, 'reason': _UNAVAILABLE_REASON}

    module = _tamponade_module()
    if module is None:
        return original_v, original_t, dict(unavailable)

    report = {'ran': False, 'adopted': False, 'reason': None,
              'intensity': intensity, 'work_budget': None, 'work_units': None,
              'h_min': None,
              'input_faces': None, 'output_faces': None, 'seconds': None,
              'plugs': None, 'plug_faces': None,
              'plugs_rejected': None, 'plugs_inconclusive': None,
              'plugs_rejected_size': None, 'plugs_rejected_dev': None,
              'melt_layers_tried': None, 'melt_layers_accepted': None,
              'melt_layers_rolled_back': None,
              'micro_welds_ok': None, 'micro_welds_failed': None,
              'excised_faces': None,
              'healthy_faces_kept': None,
              'reference_used': None, 'reference_fallback': None,
              'reference_skirt': None,
              'stent_used': None, 'stent_rejected': None,
              'stent_subplugs': None,
              'plug_metrics': None,
              'trimmed_vertices': None, 'patch_vertices': None,
              'plugs_rolled_back': None, 'plugs_leaking': None,
              'plugs_double_wall': None, 'plugs_floating': None,
              'components_before': None, 'components_after': None,
              'closed_shells_before': None, 'closed_shells_after': None}
    try:
        v = np.asarray(verts, dtype=np.float64)
        t = np.asarray(tris, dtype=np.int64)
        report['input_faces'] = int(len(t))
        if len(t) == 0 or len(v) == 0:
            report['reason'] = 'empty mesh'
            return original_v, original_t, report

        budget = TAMPONADE_WORK_BY_INTENSITY.get(
            intensity, TAMPONADE_DEFAULT_WORK)
        if work_budget is not None:
            budget = int(work_budget)
        report['work_budget'] = budget
        if budget is not None and budget <= 0:
            report['reason'] = ('intensity %r skips Tamponade (work budget 0)'
                                % (intensity,))
            return original_v, original_t, report

        h_min = _median_edge_length(v, t)
        report['h_min'] = h_min
        melt_si = resolve_melt_si(intensity)
        report['melt_enabled'] = melt_si
        out_v, out_t, info = module.tamponade(
            v, t, h_min=h_min, melt_si=melt_si, max_nodes=None,
            work_budget=budget)
        info = dict(info or {})
        report['ran'] = True
        report['output_faces'] = int(len(out_t))
        report['seconds'] = info.get('seconds')
        report['work_units'] = info.get('work_units')
        for key in ('plugs', 'plug_faces', 'plugs_rejected',
                    'plugs_inconclusive', 'plugs_rejected_size',
                    'plugs_rejected_dev', 'melt_layers_tried',
                    'melt_layers_accepted', 'melt_layers_rolled_back',
                    'micro_welds_ok', 'micro_welds_failed',
                    'excised_faces', 'healthy_faces_kept',
                    'reference_used', 'reference_fallback',
                    'reference_skirt',
                    'stent_used', 'stent_rejected', 'stent_subplugs',
                    'trimmed_vertices', 'patch_vertices',
                    'plugs_rolled_back', 'plugs_leaking',
                    'plugs_double_wall', 'plugs_floating',
                    'components_before', 'components_after',
                    'closed_shells_before', 'closed_shells_after'):
            report[key] = info.get(key)
        raw_metrics = info.get('plug_metrics')
        if raw_metrics:
            try:
                report['plug_metrics'] = json.loads(raw_metrics)
            except (ValueError, TypeError):
                report['plug_metrics'] = None
        if info.get('reason'):
            report['reason'] = info['reason']
        if len(out_t) == 0:
            report['reason'] = 'tamponade returned no geometry'
            return original_v, original_t, report
        return (np.asarray(out_v), np.asarray(out_t), report)
    except BaseException as e:  # noqa: BLE001 - a tier never crashes a repair
        if isinstance(e, (KeyboardInterrupt, SystemExit)):
            raise
        report['reason'] = 'error: %s: %s' % (type(e).__name__, e)
        return original_v, original_t, report
