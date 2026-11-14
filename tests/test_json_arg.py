#!/usr/bin/env python3
"""JSON-argument fallback regression (P10 lead review C3).

Unknown objects must fail clearly; silently stringifying them hides a real
report-shape bug.
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402

from sutura_engine import rust_bridge  # noqa: E402


def test_json_arg_accepts_scalars_and_lists():
    assert rust_bridge.json_arg({'a': np.float32(1.5), 'b': [1, 2]}) == \
        '{"a": 1.5, "b": [1, 2]}'


def test_json_arg_rejects_unknown_objects():
    try:
        rust_bridge.json_arg({'a': object()})
    except TypeError as exc:
        assert 'not JSON serializable' in str(exc)
    else:
        raise AssertionError('unknown object must fail clearly')


def test_json_arg_rejects_multi_element_arrays():
    try:
        rust_bridge.json_arg({'a': np.array([1.0, 2.0])})
    except TypeError as exc:
        assert 'not JSON serializable' in str(exc)
    else:
        raise AssertionError('multi-element array must fail clearly')


if __name__ == '__main__':
    test_json_arg_accepts_scalars_and_lists()
    test_json_arg_rejects_unknown_objects()
    test_json_arg_rejects_multi_element_arrays()
    print('json_arg strictness passed')
