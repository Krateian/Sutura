# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Adapter for PyMeshLab / VCG Library (Visual Computing Lab - ISTI - CNR).

Probes whether pymeshlab is importable in the current environment. PyMeshLab is
dual-licensed under GPL; this module imports it lazily so the rest of Sutura is
free of it.
"""


def is_pymeshlab_available() -> bool:
    """Return True if pymeshlab can be imported in the current Python environment."""
    try:
        import pymeshlab  # noqa: F401
        return True
    except (ImportError, Exception):
        return False
