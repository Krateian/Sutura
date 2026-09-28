# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Backward-compatibility shim for sutura.templates.

Re-exports all functionality from sutura_engine.analysis.
"""
from sutura_engine.analysis import (
    Template,
    ObjectProfile,
    TEMPLATES,
    SCORE_TEMPLATES,
    by_id,
    register_template,
    all_templates,
    OPEN_AREA_SCAN,
    OPEN_AREA_RELIEF,
    LARGE_LOOP_RATIO,
    MANY_LOOPS,
    SI_MODERATE,
    SI_HEAVY,
)

__all__ = [
    'Template',
    'ObjectProfile',
    'TEMPLATES',
    'SCORE_TEMPLATES',
    'by_id',
    'register_template',
    'all_templates',
    'OPEN_AREA_SCAN',
    'OPEN_AREA_RELIEF',
    'LARGE_LOOP_RATIO',
    'MANY_LOOPS',
    'SI_MODERATE',
    'SI_HEAVY',
]
