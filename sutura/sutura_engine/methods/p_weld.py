# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""P-WELD reload-equivalent welding tier.

Provides float32-safe vertex welding and reload-equivalent watertightness
checks to guarantee that files saved and reloaded by third-party slicers
honour the watertight claim.
"""
from sutura_engine.core import (
    weld_reload_equivalent,
    p_weld_final_pass,
    reload_strict_holes_nm,
    enforce_reload_verdict,
    is_strict_watertight,
)

__all__ = [
    'weld_reload_equivalent',
    'p_weld_final_pass',
    'reload_strict_holes_nm',
    'enforce_reload_verdict',
    'is_strict_watertight',
]
