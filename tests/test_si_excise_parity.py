#!/usr/bin/env python3
"""Python-vs-Rust SI excise/re-cap report parity (P10 lead review).

The private corpus paths are passed through ``SUTURA_SI_PARITY_MESHES``
(``:``-separated) so no benchmark name/path is stored in this file.
"""
import os
import sys

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

from sutura_engine.pipeline.stage1 import si_excise_recap  # noqa: E402


def _meshes():
    raw = os.environ.get('SUTURA_SI_PARITY_MESHES', '').strip()
    return [p for p in raw.split(':') if p]


def _strip_timing(value):
    if isinstance(value, dict):
        return {
            k: _strip_timing(v)
            for k, v in value.items()
            if k != 'seconds' and not k.endswith('_seconds')
        }
    if isinstance(value, list):
        return [_strip_timing(v) for v in value]
    return value


def _run(engine, v, t, guard, mode):
    old_engine = os.environ.get('SUTURA_STAGE1_ENGINE')
    old_mode = os.environ.get('SUTURA_SI_EXCISE_GUARD_MODE')
    try:
        if engine == 'python':
            os.environ.pop('SUTURA_STAGE1_ENGINE', None)
        else:
            os.environ['SUTURA_STAGE1_ENGINE'] = 'rust'
        os.environ['SUTURA_SI_EXCISE_GUARD_MODE'] = mode
        return si_excise_recap(v, t, time_budget=5.0, shape_guard=guard)
    finally:
        if old_engine is None:
            os.environ.pop('SUTURA_STAGE1_ENGINE', None)
        else:
            os.environ['SUTURA_STAGE1_ENGINE'] = old_engine
        if old_mode is None:
            os.environ.pop('SUTURA_SI_EXCISE_GUARD_MODE', None)
        else:
            os.environ['SUTURA_SI_EXCISE_GUARD_MODE'] = old_mode


def test_si_excise_parity():
    meshes = _meshes()
    if not meshes:
        import pytest
        pytest.skip('set SUTURA_SI_PARITY_MESHES to run the corpus parity test')
    try:
        import sutura_core_py  # noqa: F401
        import sutura_geom  # noqa: F401
    except Exception as exc:  # noqa: BLE001
        import pytest
        pytest.skip('bridge/geometry unavailable: %s' % exc)

    from sutura_core_py import load_stl
    for path in meshes:
        v, t = load_stl(path)
        v = np.asarray(v, dtype=np.float64)
        t = np.asarray(t, dtype=np.int64)
        for guard in (True, False):
            for mode in ('local', 'full', 'parity'):
                pv, pt, pr = _run('python', v, t, guard, mode)
                rv, rt, rr = _run('rust', v, t, guard, mode)
                label = '%s guard=%s mode=%s' % (
                    os.path.basename(path), guard, mode)
                assert np.array_equal(pv, rv), label + ' verts differ'
                assert np.array_equal(pt, rt), label + ' tris differ'
                assert _strip_timing(pr) == _strip_timing(rr), (
                    label + ' report differs\npy=%r\nrs=%r'
                    % (_strip_timing(pr), _strip_timing(rr)))
