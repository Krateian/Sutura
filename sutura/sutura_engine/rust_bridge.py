# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Strict-mode helper for the optional ``sutura_core_py`` bridge.

``SUTURA_REQUIRE_RUST=1`` turns a silent Python fallback into a hard error.
The transition engines keep their own opt-out envs; this flag only decides
what happens when the Rust engine was requested but the bridge is missing.
"""
import os

_TRUTHY = ('1', 'true', 'yes', 'on')


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
