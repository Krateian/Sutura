#!/usr/bin/env python3
"""Tamponade (#17) - interior hole plugging and self-intersection melting.

Tamponade closes open boundaries from the inside: the Rust core
(``sutura_geom.tamponade``) walks each boundary loop, fits a smooth reference
surface (Arif's "line 2") through the intact ring around it, trims any
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

# Wall-clock budget (seconds) per Triage intensity preset. ``quick`` is 0 and
# skips the tier entirely; the others match the deep-repair ladder's ordering.
TAMPONADE_BUDGET_BY_INTENSITY = {
    'quick': 0.0,
    'balanced': 60.0,
    'thorough': 180.0,
    'extreme': 600.0,
}
# Budget used when no intensity preset is supplied (library/balanced default).
TAMPONADE_DEFAULT_BUDGET = 60.0
# Wall-clock cap (seconds) for the Tamponade PREPASS block as a whole (the
# hygiene chain + the seal + the local self-intersection guard).  The Rust tier
# otherwise spends its full per-intensity budget (60 s at balanced) even when
# its output is rejected downstream, which dominated the ornate-frame prepass;
# this bounds the whole block so a large scan cannot stall the pipeline.  A
# candidate not verified within the cap is dropped (the caller falls back to the
# OFF path).  ``quick`` skips the prepass entirely.
TAMPONADE_PREPASS_BUDGET_BY_INTENSITY = {
    'quick': 0.0,
    'balanced': 15.0,
    'thorough': 40.0,
    'extreme': 60.0,
}
TAMPONADE_PREPASS_DEFAULT_BUDGET = 15.0
# Seconds held back from the prepass cap for the local self-intersection guard
# and the weld/reload measurement that run after the Rust tier.  Measured on
# ornate-frame (396k faces): the subset guard costs ~6 s, weld+reload ~0.3 s, so
# the reserve leaves enough room for both while the seal still gets the bulk of
# the cap.
TAMPONADE_PREPASS_SI_RESERVE_S = 8.0
# Below this many seconds left, the prepass is skipped rather than started.
TAMPONADE_PREPASS_MIN_S = 1.0
# The per-loop wall-clock allowance lives in the Rust core: each boundary loop
# gets `base + per-vertex * rim_size` seconds, and the whole run is capped by the
# Triage preset budget above. The allowance scales with the HOLES, not the face
# count, so a single genuine hole on a large mesh is not cut short (a 30-vertex
# rim gets ~3 s). An install without the per-loop allowance degrades gracefully:
# the preset budget is still the hard cap.
TAMPONADE_LOOP_BUDGET_BASE_S = 0.30
TAMPONADE_LOOP_BUDGET_PER_VERTEX_S = 0.10

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
    """Wall-clock cap (seconds) for the whole Tamponade PREPASS block.

    ``SUTURA_TAMPONADE_PREPASS_BUDGET`` (float seconds) overrides the per-
    intensity preset; an unparseable value is ignored.  A value <= 0 makes the
    prepass skip itself; ``quick`` is 0 by preset.  The cap covers the seal plus
    the local self-intersection guard, so a large scan cannot stall the pipeline
    waiting on a candidate that will be rejected anyway.
    """
    env = (environ if environ is not None else os.environ).get(
        'SUTURA_TAMPONADE_PREPASS_BUDGET')
    if env is not None:
        try:
            return float(env)
        except (TypeError, ValueError):
            pass
    return TAMPONADE_PREPASS_BUDGET_BY_INTENSITY.get(
        intensity, TAMPONADE_PREPASS_DEFAULT_BUDGET)


def _tamponade_module():
    """Return the ``sutura_geom`` module when it exposes the Tamponade API.

    ``None`` when the extension is missing, does not export ``tamponade`` or
    does not advertise ``TAMPONADE_SUPPORTED`` (an older build)."""
    try:
        import sutura_geom  # type: ignore
    except Exception:  # noqa: BLE001 - no extension is a normal case
        return None
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


def run_tamponade(verts, tris, *, intensity=None, ml=None, time_budget_s=None):
    """Run the Rust Tamponade tier on ``verts``/``tris``.

    ``time_budget_s`` overrides the per-intensity budget (the prepass passes
    the seconds left in its own cap so the seal cannot overrun it).

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
              'intensity': intensity, 'budget_s': None, 'h_min': None,
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

        budget = TAMPONADE_BUDGET_BY_INTENSITY.get(
            intensity, TAMPONADE_DEFAULT_BUDGET)
        if time_budget_s is not None:
            budget = float(time_budget_s)
        report['budget_s'] = budget
        if budget is not None and budget <= 0:
            report['reason'] = ('intensity %r skips Tamponade (budget 0 s)'
                                % (intensity,))
            return original_v, original_t, report

        h_min = _median_edge_length(v, t)
        report['h_min'] = h_min
        melt_si = resolve_melt_si(intensity)
        report['melt_enabled'] = melt_si
        out_v, out_t, info = module.tamponade(
            v, t, h_min=h_min, melt_si=melt_si, max_nodes=None,
            time_budget_s=budget)
        info = dict(info or {})
        report['ran'] = True
        report['output_faces'] = int(len(out_t))
        report['seconds'] = info.get('seconds')
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
