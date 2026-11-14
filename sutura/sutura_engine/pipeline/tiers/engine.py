# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Tier engine selection for the P10 phase-4 port.

The Python tier modules stay the default.  ``SUTURA_TIER_ENGINE=rust`` (or a
per-tier ``SUTURA_<TIER>_ENGINE=rust``) makes a tier delegate its decision
logic to the ``sutura_core_py`` bridge.  A missing bridge falls back to Python
silently, matching the Phase 1-3 engine contract.
"""
import os

_CORE = None
_CORE_TRIED = False


def tier_engine(name):
    """Return ``'rust'`` or ``'python'`` for a tier slug."""
    key = 'SUTURA_%s_ENGINE' % str(name).upper().replace('-', '_')
    value = os.environ.get(key, os.environ.get('SUTURA_TIER_ENGINE', 'python'))
    return str(value).strip().lower()


def tier_core():
    """Return the ``sutura_core_py`` module, or ``None`` when unavailable."""
    global _CORE, _CORE_TRIED
    if _CORE_TRIED:
        return _CORE
    _CORE_TRIED = True
    try:
        import sutura_core_py as mod
        _CORE = mod
    except Exception:  # noqa: BLE001 - a missing bridge is a normal fallback
        _CORE = None
    return _CORE


def guard_params(tier):
    """Serialize the resolved adoption guard for the Rust tier."""
    from sutura_engine.pipeline.adopt_guard import policy_for
    p = policy_for(tier)
    return {
        'face_ratio': float(p.face_ratio),
        'vertex_ratio': float(p.vertex_ratio),
        'min_slack': int(p.min_slack),
        'loop_face_slack': int(p.loop_face_slack),
        'loop_vertex_slack': int(p.loop_vertex_slack),
        'si_policy': str(p.si_policy),
        'si_rel_tol': float(p.si_rel_tol),
        'si_abs_tol': int(p.si_abs_tol),
        'face_abs_floor': int(p.face_abs_floor),
        'vertex_abs_floor': int(p.vertex_abs_floor),
        'allow_growth_if_improved': bool(p.allow_growth_if_improved),
    }
