# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Backward-compatibility shim for ``sutura.shell_wrap``.

The real implementation lives in :mod:`sutura_engine.graft` (method #13,
"Graft").  This module re-exports it so the historical
``from shell_wrap import shell_wrap`` import path keeps working.
"""
from sutura_engine.graft import (  # noqa: F401
    DISPLAY_NAME,
    DISPLAY_NAME_FULL,
    METHOD_NUMBER,
    _cap_boundary_loops,
    _detail_loss,
    _hybrid_close,
    _isotropic_remesh,
    _project_envelope,
    _si_count,
    _stitch_local,
    _split_long_edges,
    shell_wrap,
)

__all__ = [
    'METHOD_NUMBER',
    'DISPLAY_NAME',
    'DISPLAY_NAME_FULL',
    'shell_wrap',
    '_cap_boundary_loops',
    '_detail_loss',
    '_hybrid_close',
    '_isotropic_remesh',
    '_project_envelope',
    '_si_count',
    '_stitch_local',
    '_split_long_edges',
]
