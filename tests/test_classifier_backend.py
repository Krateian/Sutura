#!/usr/bin/env python3
# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
"""Tests for the Rust classifier backend (`SUTURA_CLASSIFIER_BACKEND`).

The backend must be a drop-in for `diagnose.classify_with_engine`:
  * default is `python`; `rust` selects `sutura_core_py.classify_mesh`,
  * both backends return the same type/confidence/metrics,
  * the opt-in extra-feature head (not ported) reports itself unavailable and
    raises under `SUTURA_REQUIRE_RUST=1` instead of silently returning the
    6-feature verdict.

Usage: python3 tests/test_classifier_backend.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, 'sutura'))
sys.path.insert(0, os.path.join(REPO, 'tests'))

import numpy as np  # noqa: E402

from sutura_engine import classifier_rust, rust_bridge  # noqa: E402
from sutura_engine.diagnosis import classify_with_engine  # noqa: E402
from make_classifier_set import iter_meshes  # noqa: E402


def _backend(name):
    os.environ['SUTURA_CLASSIFIER_BACKEND'] = name
    rust_bridge.reset()


def test_default_backend_is_python():
    saved = os.environ.pop('SUTURA_CLASSIFIER_BACKEND', None)
    try:
        rust_bridge.reset()
        assert classifier_rust.selected() == 'python'
    finally:
        if saved is not None:
            os.environ['SUTURA_CLASSIFIER_BACKEND'] = saved


def test_backends_agree():
    available = []
    for m in iter_meshes():
        _backend('python')
        py = [classify_with_engine(m['verts'], m['tris'], engine)[0]
              for engine in ('classic', 'experimental')]
        if not classifier_rust.available():
            return  # no Rust bridge in this environment: nothing to compare
        _backend('rust')
        rs = [classify_with_engine(m['verts'], m['tris'], engine)[0]
              for engine in ('classic', 'experimental')]
        for want, got in zip(py, rs):
            assert got['type'] == want['type'], (m['name'], got, want)
            assert got['confidence'] == want['confidence'], (m['name'], got,
                                                             want)
            assert got['metrics'] == want['metrics'], (m['name'], got, want)
            assert got['feature_engine'] == 'rust'
        available.append(m['name'])
    assert available, 'no mesh compared'


def test_extra_features_is_not_silently_downgraded():
    _backend('rust')
    if not classifier_rust.available():
        return
    assert not classifier_rust.available(extra_features=True)
    os.environ['SUTURA_REQUIRE_RUST'] = '1'
    try:
        rust_bridge.reset()
        try:
            classifier_rust.available(extra_features=True)
        except RuntimeError as exc:
            assert 'extra_features' in str(exc), exc
            return
        raise AssertionError('strict mode accepted extra_features')
    finally:
        os.environ.pop('SUTURA_REQUIRE_RUST', None)
        rust_bridge.reset()
        os.environ['SUTURA_CLASSIFIER_BACKEND'] = 'python'


def test_result_shape_is_validated():
    _backend('python')
    verts = np.zeros((0, 3), dtype=np.float64)
    tris = np.zeros((0, 3), dtype=np.int64)
    result, used = classify_with_engine(verts, tris, 'experimental')
    assert result['type'] in ('mechanical', 'organic', 'unknown'), result
    assert used in ('classic', 'experimental'), used
