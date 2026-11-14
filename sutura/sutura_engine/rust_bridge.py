# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Strict-mode helper for the optional ``sutura_core_py`` bridge.

``SUTURA_REQUIRE_RUST=1`` turns a silent Python fallback into a hard error.
The transition engines keep their own opt-out envs; this flag only decides
what happens when the Rust engine was requested but the bridge is missing.
"""
import os
import sys

_TRUTHY = ('1', 'true', 'yes', 'on')
_LOADED = {}
_TRIED = set()


def require_rust():
    """Whether ``SUTURA_REQUIRE_RUST`` is enabled."""
    return os.environ.get('SUTURA_REQUIRE_RUST', '').strip().lower() in _TRUTHY


def unavailable(message, exc=None):
    """Raise in strict mode; otherwise return ``None`` for the fallback path."""
    if require_rust():
        text = 'SUTURA_REQUIRE_RUST=1 but ' + message
        if exc is not None:
            raise RuntimeError(text) from exc
        raise RuntimeError(text)
    return None


def _engine(env_names, default):
    for name in env_names:
        if name in os.environ:
            return os.environ[name].strip().lower()
    return default


def load(name, env_names, default='rust'):
    """Load ``sutura_core_py`` for one transition engine, with caching.

    ``env_names`` are checked in priority order (the first set env wins).
    Strict mode turns a missing bridge into the same error as `unavailable`.
    """
    if _engine(env_names, default) != 'rust':
        return None
    if name in _TRIED:
        mod = _LOADED.get(name)
        if mod is None:
            return unavailable('sutura_core_py is unavailable')
        return mod
    _TRIED.add(name)
    try:
        import sutura_core_py as mod
    except Exception as exc:  # noqa: BLE001 - environment dependent
        _LOADED[name] = None
        return unavailable('sutura_core_py is unavailable: %s' % exc, exc)
    _LOADED[name] = mod
    return mod


def reset():
    """Clear the loader cache (tests / process reconfiguration)."""
    _LOADED.clear()
    _TRIED.clear()


def json_arg(data):
    """JSON-encode a report argument; fail clearly on unknown objects."""
    import json
    def encode(o):
        if hasattr(o, 'item'):
            try:
                return o.item()
            except Exception as exc:  # multi-element numpy arrays, etc.
                raise TypeError(
                    'not JSON serializable: %s (%s)'
                    % (type(o).__name__, exc)) from exc
        raise TypeError('not JSON serializable: %s' % (type(o).__name__,))
    return json.dumps(data, ensure_ascii=False, default=encode)


# --- config bridge (policy / intensity / learning) ---------------------------
#
# Mirrors the mesh_io precedent: Rust is the default engine and
# ``SUTURA_CONFIG_ENGINE=python`` is the opt-out. The policy document crosses
# as the exact JSON text both sides hash.

CONFIG_ENGINE_ENV = 'SUTURA_CONFIG_ENGINE'


def rust_config():
    """The bridge module when the Rust config engine is usable, else ``None``.

    Reuses the shared loader, so ``SUTURA_REQUIRE_RUST=1`` behaves exactly like
    every other transition engine.
    """
    mod = load('config', (CONFIG_ENGINE_ENV,), 'rust')
    if mod is None:
        return None
    if not all(hasattr(mod, name) for name in (
            'config_policy_load', 'config_intensity_resolve',
            'config_learning_bonus', 'config_intensity_format')):
        return unavailable('sutura_core_py has no config bindings')
    return mod


def classifier_features(verts, tris):
    """Rust classifier feature kernel, or ``(None, 'numpy')``.

    The kernel lives in ``sutura_geom`` (the geometry extension), not in the
    ``sutura_core_py`` bridge. A missing extension falls back to NumPy;
    ``SUTURA_REQUIRE_RUST=1`` turns that fallback into an error, and an
    exception raised by a present kernel is propagated, never swallowed.
    """
    requested = os.environ.get('SUTURA_CLASSIFIER_BACKEND', 'rust').strip().lower()
    if requested != 'rust':
        return None, 'numpy'
    try:
        import sutura_geom as mod
    except Exception as exc:  # noqa: BLE001 - environment dependent
        unavailable('sutura_geom is unavailable: %s' % exc, exc)
        return None, 'numpy'
    if not hasattr(mod, 'classifier_features'):
        unavailable('sutura_geom has no classifier_features binding')
        return None, 'numpy'
    return mod.classifier_features(verts, tris), 'rust'
