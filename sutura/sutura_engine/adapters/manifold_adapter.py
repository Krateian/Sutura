# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Adapter for Manifold / manifold3d (Apache-2.0).

Probes whether manifold3d can be imported in-process or reached via the
subprocess bridge (manifold_bridge.py across Python runtimes).
"""
import os


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
