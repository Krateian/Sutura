# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Adapter for fTetWild / pytetwild (MPL-2.0).

Probes whether the optional fTetWild extra is installed.
"""


def is_ftetwild_available() -> bool:
    """Return True if fTetWild extra is installed and available."""
    try:
        from ftetwild_manager import is_installed
        return is_installed()
    except Exception:
        return False
