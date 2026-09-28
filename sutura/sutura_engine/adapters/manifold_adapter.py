# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Adapter for Manifold / manifold3d (Apache-2.0).

Wraps Stage 2 solid rebuilding, boolean CSG operations, and the subprocess bridges:
  - In-process manifold3d execution when available
  - Subprocess bridge (manifold_bridge.py / csg_bridge.py) across Python runtimes
"""
import os
import subprocess
import sys
from typing import Any, List, Optional, Tuple
import numpy as np


def is_manifold_available() -> bool:
    """Return True if manifold3d can be imported in-process or reached via bridge."""
    try:
        import manifold3d  # noqa: F401
        return True
    except (ImportError, Exception):
        pass
    # Check bridge
    sutura_dir = os.environ.get('SUTURA_DIR', os.path.expanduser('~/.local/share/sutura'))
    bridge = os.path.join(sutura_dir, 'manifold_bridge.py')
    venv311 = os.path.join(sutura_dir, 'venv311', 'bin', 'python')
    return os.path.isfile(bridge) and os.path.isfile(venv311)


def get_bridge_paths() -> Tuple[str, str]:
    """Return (bridge_script, venv311_python)."""
    sutura_dir = os.environ.get('SUTURA_DIR', os.path.expanduser('~/.local/share/sutura'))
    bridge = os.path.join(sutura_dir, 'manifold_bridge.py')
    venv311 = os.path.join(sutura_dir, 'venv311', 'bin', 'python')
    if not os.path.isfile(bridge):
        # Look relative to current file
        here = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
        cand_bridge = os.path.join(here, 'manifold_bridge.py')
        if os.path.isfile(cand_bridge):
            bridge = cand_bridge
    return bridge, venv311
