# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
"""Rust classifier backend (`SUTURA_CLASSIFIER_BACKEND=rust`).

Runs the whole classifier (classic or v2 head, RANSAC features and metrics) in
`sutura-core`, so the Python path and the Rust CLI cannot drift apart. The
NumPy engine stays the default; this module is only reached when the backend
env selects it.

Unlike the feature kernel in `mesh_classifier_v2`, this path computes the
metrics themselves in Rust. The v2 head with `extra_features=True` is not
ported yet; it is reported as unavailable so the caller can fall back instead
of silently returning different numbers.
"""
from sutura_engine import rust_bridge

BACKEND_ENV = 'SUTURA_CLASSIFIER_BACKEND'
BACKENDS = ('python', 'rust')


def selected():
    """The configured backend (`python` unless overridden)."""
    import os
    value = os.environ.get(BACKEND_ENV, 'python').strip().lower()
    return value if value in BACKENDS else 'python'


def _module():
    return rust_bridge.load('classifier', (BACKEND_ENV,), 'python')


def available(extra_features=False):
    """Whether the Rust backend can serve this request."""
    if selected() != 'rust':
        return False
    mod = _module()
    return mod is not None and hasattr(mod, 'classify_mesh')


def classify_mesh(verts, tris, engine='experimental', extra_features=False):
    """Return `(result, used_engine)` exactly like `diagnose.classify_with_engine`."""
    import numpy as np
    mod = _module()
    if mod is None:
        return rust_bridge.unavailable('sutura_core_py is unavailable')
    v = np.ascontiguousarray(np.asarray(verts, dtype=np.float64))
    t = np.ascontiguousarray(np.asarray(tris, dtype=np.int64))
    try:
        result = mod.classify_mesh(v, t, 'classic' if engine != 'experimental'
                                   else 'experimental', extra_features)
    except TypeError:
        if extra_features:
            raise
        result = mod.classify_mesh(v, t, 'classic' if engine != 'experimental'
                                   else 'experimental')
    if not isinstance(result, dict):
        raise ValueError('Rust classifier result is not a dict')
    if result.get('type') not in ('mechanical', 'organic', 'unknown'):
        raise ValueError(f"unexpected classifier type: {result.get('type')!r}")
    return result, ('classic' if engine != 'experimental' else 'experimental')
