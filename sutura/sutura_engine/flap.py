"""Flap: surface-based hole filling (Liepa-style surgical flap).

A surgical "flap" covers a defect and continues the neighbouring tissue: every
boundary loop is triangulated with a minimum-area dynamic program, refined to
the surrounding edge length, and faired with a discrete biharmonic
(thin-plate) solve whose boundary condition carries the position AND
slope/curvature of the surrounding original surface (a C1 ghost ring built
from each rim vertex's original 1-ring step).

No voxels are used and the original triangles are kept verbatim: every output
face is either an original input face or a flap face built from the hole's own
boundary vertices plus refined interior points.

The implementation is the Rust core (``sutura_geom.flap_fill`` in
``rust/sutura-geom/src/flap``). The pure-Python oracle that used to live here
has been removed: ``sutura_geom`` is a hard requirement of the runtime, so a
missing extension or binding raises :class:`FlapUnavailableError` instead of
silently degrading. The public entry point is :func:`flap_fill`; it is wired
into the Stage-1 chain behind the off-by-default Flap switch.
"""
from __future__ import annotations

import numpy as np


class FlapUnavailableError(RuntimeError):
    """The required ``sutura_geom.flap_fill`` Rust binding is not available."""


# ---------------------------------------------------------------------------
# Rust core selection
# ---------------------------------------------------------------------------
# ``None`` means "not probed yet"; ``False`` means "probed and absent". The
# lookup never raises so callers can report availability, but ``flap_fill``
# turns an absent core into the named error below.
_RUST_FLAP = None


def _load_rust_flap():
    """Return ``sutura_geom.flap_fill`` when the extension provides it, else None.

    The result is cached. The lookup never raises; an absent/incompatible
    extension is reported as ``None`` (never a numpy fallback).
    """
    global _RUST_FLAP
    if _RUST_FLAP is None:
        try:
            import sutura_geom as _sg
            fn = getattr(_sg, 'flap_fill', None)
            _RUST_FLAP = fn if callable(fn) else False
        except Exception:  # noqa: BLE001 - report absence, do not crash
            _RUST_FLAP = False
    return _RUST_FLAP or None


def flap_fill(verts, tris, *, refine=True, fair=True, max_loop=1200,
              max_faces=400000, collect_quality=False,
              drop_lone_tris=True, weld_cracks=True, split_nm=True,
              separate_stl=True, bridge_open_chains=False, weld_max_frac=0.02,
              orient="reverse", avoid_coplanar=True, coplanar_frac=0.05,
              coplanar_cos=0.99, sliver_cleanup=False, time_budget=None):
    """Fill boundary loops with the Rust flap core.

    ``time_budget`` (seconds) bounds the Rust per-loop candidate selector so the
    alternative search stops at its preset budget and leaves the remaining loops
    to Stage-1.

    The Rust binding is mandatory. When it is absent (extension not installed,
    or installed without the flap binding) this raises
    :class:`FlapUnavailableError`, a named error the central ``tier_guard``
    records as a failure rather than a skip. The returned ``(verts, tris,
    report)`` is the Rust result unchanged.
    """
    rfn = _load_rust_flap()
    if rfn is None:
        raise FlapUnavailableError(
            'sutura_geom.flap_fill is required but the Rust binding is not '
            'available; reinstall Sutura or rebuild the sutura_geom extension')
    rv, rt, rrep = rfn(
        np.ascontiguousarray(verts, dtype=np.float64),
        np.ascontiguousarray(tris, dtype=np.int64),
        refine=refine, fair=fair, max_loop=max_loop,
        max_faces=max_faces, collect_quality=collect_quality,
        drop_lone_tris=drop_lone_tris, weld_cracks=weld_cracks,
        split_nm=split_nm, separate_stl=separate_stl,
        bridge_open_chains=bridge_open_chains,
        weld_max_frac=weld_max_frac, orient=orient,
        avoid_coplanar=avoid_coplanar, coplanar_frac=coplanar_frac,
        coplanar_cos=coplanar_cos, sliver_cleanup=sliver_cleanup,
        time_budget=time_budget)
    return (np.asarray(rv, dtype=np.float64),
            np.asarray(rt, dtype=np.int64), dict(rrep))
