# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Adapter for fTetWild / pytetwild (MPL-2.0).

Wraps the fTetWild tetrahedralization fallback tier and runtime manager.
"""
from typing import Optional, Tuple


def is_ftetwild_available() -> bool:
    """Return True if fTetWild extra is installed and available."""
    try:
        from ftetwild_manager import is_installed
        return is_installed()
    except Exception:
        return False


def get_ftetwild_status() -> dict:
    """Return current installation status and disk size of fTetWild."""
    try:
        import ftetwild_manager
        return ftetwild_manager.status()
    except Exception as e:
        return {'installed': False, 'error': str(e)}
