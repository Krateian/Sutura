#!/usr/bin/env python3
"""Strict bridge-mode regression (P10 lead review R2).

``SUTURA_REQUIRE_RUST=1`` must turn a missing bridge into a hard error for
every transition engine that was asked for Rust, not just IO.
"""
import builtins
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

from sutura_engine import rust_bridge  # noqa: E402
from sutura_engine import debris, health  # noqa: E402
from sutura_engine.pipeline import mesh_io, p_weld, report, stage2  # noqa: E402
from sutura_engine.pipeline.tiers import engine as tier_engine  # noqa: E402


def _block_bridge():
    real = builtins.__import__

    def fake(name, *args, **kwargs):
        if name == 'sutura_core_py':
            raise ImportError('bridge blocked for the test')
        return real(name, *args, **kwargs)

    builtins.__import__ = fake
    return real


def test_require_rust_flags():
    old = os.environ.get('SUTURA_REQUIRE_RUST')
    try:
        for value, expected in (('1', True), ('true', True), ('yes', True),
                                ('on', True), ('0', False), ('', False),
                                ('no', False)):
            os.environ['SUTURA_REQUIRE_RUST'] = value
            assert rust_bridge.require_rust() is expected, value
    finally:
        if old is None:
            os.environ.pop('SUTURA_REQUIRE_RUST', None)
        else:
            os.environ['SUTURA_REQUIRE_RUST'] = old


def test_every_rust_core_honours_require_rust():
    keys = ('SUTURA_REQUIRE_RUST', 'SUTURA_REPORT_ENGINE',
            'SUTURA_STAGE_ENGINE', 'SUTURA_STAGE2_ENGINE',
            'SUTURA_PWELD_ENGINE', 'SUTURA_TIER_ENGINE', 'SUTURA_IO_ENGINE')
    saved = {k: os.environ.get(k) for k in keys}
    real = _block_bridge()
    try:
        os.environ['SUTURA_REQUIRE_RUST'] = '1'
        os.environ['SUTURA_REPORT_ENGINE'] = 'rust'
        os.environ['SUTURA_STAGE_ENGINE'] = 'rust'
        os.environ.pop('SUTURA_STAGE2_ENGINE', None)
        os.environ['SUTURA_PWELD_ENGINE'] = 'rust'
        os.environ['SUTURA_TIER_ENGINE'] = 'rust'
        os.environ['SUTURA_IO_ENGINE'] = 'rust'

        report._REPORT_CORE_TRIED = False
        report._REPORT_CORE = None
        health._REPORT_CORE_TRIED = False
        health._REPORT_CORE = None
        debris._REPORT_CORE_TRIED = False
        debris._REPORT_CORE = None
        p_weld._PWELD_CORE_TRIED = False
        p_weld._PWELD_CORE = None
        stage2._STAGE_CORE_TRIED = False
        stage2._STAGE_CORE = None
        tier_engine._CORE_TRIED = False
        tier_engine._CORE = None
        mesh_io._RUST_IO_STATE = 0
        mesh_io._RUST_IO_MOD = None

        checks = [
            ('report', report._rust_core),
            ('health', health._rust_core),
            ('debris', debris._rust_core),
            ('p_weld', p_weld._rust_core),
            ('stage2', stage2._rust_core),
            ('tier', tier_engine.tier_core),
            ('io', mesh_io._rust_io),
        ]
        for name, fn in checks:
            try:
                fn()
            except RuntimeError as exc:
                assert 'SUTURA_REQUIRE_RUST=1' in str(exc), (name, exc)
            else:
                raise AssertionError('%s did not fail in strict mode' % name)
    finally:
        builtins.__import__ = real
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


def main():
    test_require_rust_flags()
    test_every_rust_core_honours_require_rust()
    print('require-rust strict mode passed')


if __name__ == '__main__':
    main()
