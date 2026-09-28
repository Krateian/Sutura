# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Backward-compatibility shim for sutura.object_analysis.

Re-exports all functionality from sutura_engine.diagnosis.
"""
from sutura_engine.diagnosis import (
    ObjectAnalysis,
    analyze_mesh,
    analyze_file,
    combine,
    to_dict,
    SI_COUNT_MAX_FACES,
    SI_SAMPLE_FACES,
    SI_SAMPLE_SEED,
    CLOSING_SIGNAL_MAX_FACES,
    REPETITION_MAX_FACES,
    _surface_area,
    _non_manifold_edge_count,
    _loop_spanned_area,
    _count_self_intersections,
    _component_count,
)

__all__ = [
    'ObjectAnalysis',
    'analyze_mesh',
    'analyze_file',
    'combine',
    'to_dict',
    'SI_COUNT_MAX_FACES',
    'SI_SAMPLE_FACES',
    'SI_SAMPLE_SEED',
    'CLOSING_SIGNAL_MAX_FACES',
    'REPETITION_MAX_FACES',
]
