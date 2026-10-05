#!/usr/bin/env python3
"""Tamponade (#17) - interior hole plugging and self-intersection melting.

Tamponade closes open boundaries from the inside: the Rust core
(``sutura_geom.tamponade``) walks each boundary loop, builds a plug surface
that bridges the opening without touching the surrounding healthy triangles,
optionally melts residual self-intersections in thin layers (each layer is
rolled back when it does not reduce the crossing count) and micro-welds the
seam. Unlike the surface-based tiers it never deletes an input triangle: the
report's ``healthy_faces_kept`` counts the input faces carried through verbatim.

The tier is opt-in.  ``run_tamponade`` is a thin, never-raising wrapper around
the extension: an install without ``sutura_geom.tamponade`` (or with
``TAMPONADE_SUPPORTED`` absent) reports ``{'ran': False, 'reason':
'sutura_geom.tamponade unavailable'}`` and the caller keeps the mesh it had.

Pure API::

    run_tamponade(verts, tris, *, intensity=None, ml=None)
        -> (verts_out, tris_out, report)

No file is written.  The per-intensity wall-clock budgets follow the Triage
presets; ``quick`` disables the tier.
"""
from __future__ import annotations

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

_UNAVAILABLE_REASON = 'sutura_geom.tamponade unavailable'


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


def run_tamponade(verts, tris, *, intensity=None, ml=None):
    """Run the Rust Tamponade tier on ``verts``/``tris``.

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
              'melt_layers_tried': None, 'melt_layers_accepted': None,
              'melt_layers_rolled_back': None,
              'micro_welds_ok': None, 'micro_welds_failed': None,
              'healthy_faces_kept': None}
    try:
        v = np.asarray(verts, dtype=np.float64)
        t = np.asarray(tris, dtype=np.int64)
        report['input_faces'] = int(len(t))
        if len(t) == 0 or len(v) == 0:
            report['reason'] = 'empty mesh'
            return original_v, original_t, report

        budget = TAMPONADE_BUDGET_BY_INTENSITY.get(
            intensity, TAMPONADE_DEFAULT_BUDGET)
        report['budget_s'] = budget
        if budget is not None and budget <= 0:
            report['reason'] = ('intensity %r skips Tamponade (budget 0 s)'
                                % (intensity,))
            return original_v, original_t, report

        h_min = _median_edge_length(v, t)
        report['h_min'] = h_min
        out_v, out_t, info = module.tamponade(
            v, t, h_min=h_min, melt_si=True, max_nodes=None,
            time_budget_s=budget)
        info = dict(info or {})
        report['ran'] = True
        report['output_faces'] = int(len(out_t))
        report['seconds'] = info.get('seconds')
        for key in ('plugs', 'plug_faces', 'melt_layers_tried',
                    'melt_layers_accepted', 'melt_layers_rolled_back',
                    'micro_welds_ok', 'micro_welds_failed',
                    'healthy_faces_kept'):
            report[key] = info.get(key)
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
