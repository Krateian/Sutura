# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Tests for the Rust-backed config engine (policy / intensity / learning).

The bridge is optional: with ``SUTURA_CONFIG_ENGINE=python`` or without the
``sutura_core_py`` wheel these tests exercise the Python path and the Rust
comparisons are skipped, so the suite stays green on a default install.

Runs without numpy:
    python3 tests/test_config_rust.py
"""
import json
import os
import shutil
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

from sutura_engine import config_rust  # noqa: E402
from sutura_engine import policy as policy_mod  # noqa: E402


def _tmpdir():
    return tempfile.mkdtemp(prefix='sutura-config-')


def _write_user_policy(directory, table_text):
    path = os.path.join(directory, 'policy.toml')
    with open(path, 'w') as f:
        f.write(table_text)
    return path


CASES = {
    'order_swap': ('083522b13ed79e27', '[order]\n'
                   'deep_tier_order = ["graft", "local", "dressing", '
                   '"splint", "coaptation", "ftetwild"]\n'),
    'tier_tweak': ('8637b5438a95c9d4', '[tiers.flap]\ndefault_work = 50000000\n'
                   '\n[ranking]\ntop_n = 4\n'),
    'locked_gate': ('abb52584b7c6eee4', '[gates.adoption]\nface_ratio = 99.0\n'
                     '\n[si_escalation]\nmethod_num = 2\n'
                     '\n[auto]\ngenerative_max_hausdorff_rel = 0.5\n'),
}


def _load_with(user_path):
    saved = os.environ.get('SUTURA_POLICY_USER')
    os.environ['SUTURA_POLICY_USER'] = user_path
    try:
        # The Python engine binds its policy at import; re-read the module
        # state so the fallback path sees this file too.
        from sutura_engine import policy as pm
        pm._POLICY = pm.load_policy()
        pm.SCHEMA_VERSION = int(pm._POLICY['policy']['schema_version'])
        pm.CONTENT_HASH = pm.policy_hash(pm._POLICY)
        return config_rust.policy_load()
    finally:
        if saved is None:
            os.environ.pop('SUTURA_POLICY_USER', None)
        else:
            os.environ['SUTURA_POLICY_USER'] = saved


def test_default_hash_both_engines():
    """The packaged default hashes to the same value through both engines."""
    document, block = config_rust.policy_load()
    assert block['schema_version'] == 1, block
    assert block['content_hash'] == policy_mod.policy_hash(document), block
    assert block['content_hash'] == policy_mod.CONTENT_HASH, block
    if config_rust.available():
        rust_hash = config_rust.policy_hash(document)
        assert rust_hash == block['content_hash'], (rust_hash, block)


def test_user_override_hashes_match_the_python_reference():
    """Every override fixture keeps its captured hash in both engines."""
    directory = _tmpdir()
    try:
        for name, (expected, text) in CASES.items():
            path = _write_user_policy(directory, text)
            document, block = _load_with(path)
            assert block['content_hash'] == expected, (name, block)
            assert block['user_override'] is True, (name, block)
            if name == 'locked_gate':
                assert sorted(block['ignored_keys']) == [
                    'auto.generative_max_hausdorff_rel',
                    'gates.adoption',
                    'si_escalation',
                ], block
            else:
                assert block['ignored_keys'] == [], (name, block)
            if config_rust.available():
                assert config_rust.policy_hash(document) == expected, name
            os.remove(path)
    finally:
        shutil.rmtree(directory, ignore_errors=True)


def test_malformed_user_policy_falls_back():
    directory = _tmpdir()
    try:
        path = _write_user_policy(directory, '[order\nnever closed = 1\n')
        _document, block = _load_with(path)
        assert block['content_hash'] == policy_mod.CONTENT_HASH, block
        assert block['user_override'] is True, block
        assert block['policy_error'], block
    finally:
        shutil.rmtree(directory, ignore_errors=True)


def test_list_intensities_matches_the_python_text():
    """`--list-intensities` stays byte-identical to the Python formatter."""
    import triage
    expected = triage.format_intensities()
    got = config_rust.intensity_format()
    assert got == expected, (got, expected)


def test_intensity_priority_cli_env_config():
    import triage
    directory = _tmpdir()
    try:
        config_path = os.path.join(directory, 'config.json')
        with open(config_path, 'w') as f:
            json.dump({'intensity': 'thorough'}, f)
        env = {'SUTURA_INTENSITY': 'quick'}
        for cli, expected in [(('extreme', env), 'extreme'),
                               ((None, env), 'quick'),
                               ((None, {}), 'thorough'),
                               (('not-a-preset', env), 'quick')]:
            spec = config_rust.intensity_resolve(
                cli[0], cli[1], config_path)
            assert spec['name'] == expected, (cli, spec)
            reference = triage.resolve_intensity(cli[0], cli[1], config_path)
            assert spec['ftetwild_timeout'] == reference.ftetwild_timeout, spec
            assert spec['deep_repair'] == reference.deep_repair, spec
    finally:
        shutil.rmtree(directory, ignore_errors=True)


def test_learning_bonus_matches_the_python_value():
    from sutura_engine import learning
    stats = {
        'mechanical': {'1': {'n': 6, 'ok': 5, 'total_ms': 600}},
        'organic': {'1': {'n': 8, 'ok': 6, 'total_ms': 400}},
    }
    for template, method in (('mechanical', '1'), ('organic', '1'),
                             ('mechanical', '2'), ('nowhere', '1')):
        expected = learning.learning_bonus(template, method, stats)
        got = config_rust.learning_bonus(template, method, stats)
        assert abs(got - expected) <= 1e-9, (template, method, got, expected)


def test_opt_out_runs_the_python_engine():
    saved = os.environ.get('SUTURA_CONFIG_ENGINE')
    os.environ['SUTURA_CONFIG_ENGINE'] = 'python'
    try:
        assert config_rust.available() is False
        document, block = config_rust.policy_load()
        assert block['content_hash'] == policy_mod.CONTENT_HASH, block
        assert config_rust.intensity_format() is not None
    finally:
        if saved is None:
            os.environ.pop('SUTURA_CONFIG_ENGINE', None)
        else:
            os.environ['SUTURA_CONFIG_ENGINE'] = saved


if __name__ == '__main__':
    failures = 0
    for name, fn in sorted(globals().items()):
        if not name.startswith('test_') or not callable(fn):
            continue
        try:
            fn()
            print('ok  %s' % name)
        except AssertionError as exc:
            failures += 1
            print('FAIL %s: %s' % (name, exc))
    print('config tests %s' % ('passed' if not failures else 'failed'))
    sys.exit(1 if failures else 0)
