#!/usr/bin/env python3
"""B0: surface_metrics.measure must accept a precomputed interior-wall mask.

verify_repair_shape computes the buried mask for two_sided_distance and then
used to let surface_metrics.measure recompute it -- a second full per-face
winding pass on every mesh. The fix passes it in; this test pins that the
injected mask is USED (not silently ignored, and not recomputed).

Needs numpy + sutura_geom.
"""
import importlib.util
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.join(HERE, '..')
SCRIPTS = os.path.join(REPO, '.claude', 'skills', 'sutura-verify', 'scripts')
sys.path.insert(0, SCRIPTS)
sys.path.insert(0, os.path.join(REPO, 'sutura'))


def _load(name):
    spec = importlib.util.spec_from_file_location(
        name, os.path.join(SCRIPTS, name + '.py'))
    m = importlib.util.module_from_spec(spec)
    sys.modules[name] = m
    spec.loader.exec_module(m)
    return m


sm = _load('surface_metrics')


def _cube():
    v = np.array([[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
                  [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]], np.float64)
    t = np.array([[0, 1, 2], [0, 2, 3], [4, 6, 5], [4, 7, 6],
                  [0, 4, 5], [0, 5, 1], [1, 5, 6], [1, 6, 2],
                  [2, 6, 7], [2, 7, 3], [3, 7, 4], [3, 4, 0]], np.int64)
    return v, t


def test_measure_accepts_a_precomputed_buried_pair():
    v, t = _cube()
    calls = {'n': 0}
    real = sm.input_buried_face_mask

    def counting(*a, **k):
        calls['n'] += 1
        return real(*a, **k)

    sm.input_buried_face_mask = counting
    try:
        mask = np.zeros(len(t), dtype=bool)
        mask[0] = True
        info = {'buried_input_available': True, 'buried_input_faces': 1,
                'buried_input_area_mm2': 0.5, 'buried_input_debris_faces': 0,
                'buried_input_degenerate_faces': 0,
                'buried_input_epsilon_mm': 0.0, 'buried_input_note': None}
        out = sm.measure(v, t, v, t, buried=(mask, info), budget='sweep')
        assert calls['n'] == 0, 'measure recomputed the mask despite buried='
        assert out['buried_input_faces'] == 1, out.get('buried_input_faces')
        assert out['buried_input_available'] is True
    finally:
        sm.input_buried_face_mask = real


def test_measure_still_computes_when_not_supplied():
    v, t = _cube()
    out = sm.measure(v, t, v, t, budget='sweep')
    assert 'buried_input_available' in out
    assert out['buried_input_faces'] == 0, out.get('buried_input_faces')


def test_injected_mask_changes_the_recall_denominator():
    # A mask that marks every face must be honoured, not ignored: the recall
    # denominators use it, so an all-buried mask cannot equal the empty one.
    v, t = _cube()
    empty = sm.measure(v, t, v, t, budget='sweep')
    full_mask = np.ones(len(t), dtype=bool)
    info = {'buried_input_available': True, 'buried_input_faces': len(t),
            'buried_input_area_mm2': 6.0, 'buried_input_debris_faces': 0,
            'buried_input_degenerate_faces': 0,
            'buried_input_epsilon_mm': 0.0, 'buried_input_note': None}
    full = sm.measure(v, t, v, t, buried=(full_mask, info), budget='sweep')
    assert full['buried_input_faces'] == len(t)
    assert full != empty, 'the injected mask had no effect'


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('buried-mask dedupe (B0) tests passed')
