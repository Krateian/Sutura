# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Rust-backed config engine (policy / intensity / learning).

Phase 5 moves the declarative repair config into ``sutura-core``
(``config::policy``, ``config::intensity``, ``config::learning``). This module
is the Python side of the transition: it exposes the same values through the
optional ``sutura_core_py`` bridge and falls back to the Python implementation
when the bridge is missing or ``SUTURA_CONFIG_ENGINE=python``.

Nothing here duplicates logic: the policy document crosses the boundary as the
exact ``json.dumps(..., sort_keys=True, separators=(',', ':'))`` text that both
sides hash, so ``policy_hash`` is comparable byte for byte.

Deliberately additive: ``sutura_engine.policy`` / ``triage`` / ``learning`` keep
their current implementation, so the Rust engine can be enabled per call site
while the branch merges with the tier work.
"""
import json
import os

from sutura_engine import rust_bridge


def available():
    """True when the Rust config engine can serve requests."""
    return rust_bridge.rust_config() is not None


def _python_policy():
    from sutura_engine import policy
    return policy


def policy_load(path=None):
    """Load the effective policy.

    Returns ``(document, block)``: ``document`` is the normalized policy dict
    (dotted keys expanded, JSON-compatible), ``block`` is the report's
    ``policy`` block (schema, hash, override, error, ignored keys).
    """
    mod = rust_bridge.rust_config()
    if mod is None:
        # The Python module binds its policy once at import; an explicit path
        # or a SUTURA_POLICY_USER change must be re-read here.
        policy = _python_policy()
        document = policy.load_policy(path)
        block = {
            'schema_version': int(document['policy']['schema_version']),
            'content_hash': policy.policy_hash(document),
            'user_override': bool(policy.USER_OVERRIDE),
            'policy_error': policy.POLICY_ERROR,
            'ignored_keys': list(policy.IGNORED_KEYS),
        }
        return document, block
    if path is not None:
        loaded = mod.config_policy_load(os.fspath(path))
    else:
        loaded = mod.config_policy_load()
    document = json.loads(loaded['json'])
    block = {
        'schema_version': loaded['schema_version'],
        'content_hash': loaded['content_hash'],
        'user_override': loaded['user_override'],
        'policy_error': loaded['policy_error'],
        'ignored_keys': list(loaded['ignored_keys']),
    }
    return document, block


def policy_hash(document=None):
    """Short content hash of a policy document (order-insensitive)."""
    mod = rust_bridge.rust_config()
    if document is None:
        document, _block = policy_load()
    if mod is None:
        return _python_policy().policy_hash(document)
    return mod.config_policy_hash(json.dumps(
        document, sort_keys=True, separators=(',', ':')))


def intensity_format():
    """The ``--list-intensities`` text (byte contract)."""
    mod = rust_bridge.rust_config()
    if mod is None:
        import triage
        return triage.format_intensities()
    return mod.config_intensity_format()


def intensity_resolve(cli_value=None, environ=None, config_path=None):
    """Resolve one intensity spec with the CLI > env > config priority."""
    mod = rust_bridge.rust_config()
    if mod is None:
        from sutura_engine import triage
        spec = triage.resolve_intensity(cli_value, environ, config_path)
        import dataclasses
        out = dataclasses.asdict(spec)
        out['dense_target_ladder'] = list(spec.dense_target_ladder)
        return out
    env = None if environ is None else dict(environ)
    path = None if config_path is None else os.fspath(config_path)
    return json.loads(mod.config_intensity_resolve(cli_value, env, path))


def learning_bonus(template, method_id, stats=None):
    """Bounded ranking bonus for one (template, method) bucket."""
    mod = rust_bridge.rust_config()
    if mod is None:
        from sutura_engine import learning
        return learning.learning_bonus(template, method_id, stats)
    payload = None if stats is None else json.dumps(stats)
    return mod.config_learning_bonus(template, method_id, payload)
