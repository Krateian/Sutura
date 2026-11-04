#!/usr/bin/env python3
"""Small-part / possible-debris report tests (report-only; no geometry change).

Checks the new ``sutura_engine.debris`` measurement (the Stage-1 ``<N faces``
removal list and census rule F for kept parts), and that both surface in the
JSON shape, ``classification.brief`` notes and ``human_report``.

Usage: <repair-venv>/bin/python tests/test_debris_report.py
"""
import json
import os
import sys

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, 'sutura'))

from sutura_engine import debris  # noqa: E402
import classification  # noqa: E402
from sutura_engine.pipeline.report import human_report  # noqa: E402


def _box(cx, cy, cz, s):
    h = s / 2.0
    v = [(cx - h, cy - h, cz - h), (cx + h, cy - h, cz - h),
         (cx + h, cy + h, cz - h), (cx - h, cy + h, cz - h),
         (cx - h, cy - h, cz + h), (cx + h, cy - h, cz + h),
         (cx + h, cy + h, cz + h), (cx - h, cy + h, cz + h)]
    f = [(0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7), (0, 1, 5), (0, 5, 4),
         (1, 2, 6), (1, 6, 5), (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7)]
    return v, f


def _mesh():
    """Main body + a small closed part (rule F) + a 4-face bit (<8 removal)."""
    v, t = [], []

    def add(vv, ff):
        b = len(v)
        v.extend(vv)
        t.extend(tuple(i + b for i in tri) for tri in ff)

    add(*_box(0, 0, 0, 10))            # main body
    add(*_box(20, 0, 0, 1.0))          # small closed cube -> possible debris
    b = len(v)
    v.extend([(50, 0, 0), (51, 0, 0), (50, 1, 0), (50, 0, 1)])
    t.extend([(b, b + 1, b + 2), (b, b + 1, b + 3),
              (b, b + 2, b + 3), (b + 1, b + 2, b + 3)])
    return np.array(v, dtype=np.float64), np.array(t, dtype=np.int64)


def test_removed_small_parts():
    v, t = _mesh()
    rem = debris.removed_small_parts(v, t, 8)
    assert rem is not None, rem
    assert rem['count'] == 1 and rem['faces_total'] == 4, rem
    assert rem['min_faces'] == 8 and rem['capped'] is False, rem
    p = rem['parts'][0]
    assert p['faces'] == 4, p
    assert p['size_mm'] > 0 and p['size_pct'] is not None, p
    assert isinstance(p['distance_mm'], float) and p['distance_mm'] > 0, p


def test_possible_debris_flags_small_closed_part():
    v, t = _mesh()
    pd = debris.possible_debris(v, t, min_faces=8)
    assert pd is not None and pd['count'] == 1, pd
    p = pd['parts'][0]
    assert p['faces'] == 12, p               # the small closed cube, not the tetra
    assert 0 < p['volume_pct'] < 0.5, p       # census rule F band
    assert isinstance(p['distance_mm'], float) and p['distance_mm'] > 0, p


def test_unreferenced_vertex_is_not_a_small_part():
    v, t = _mesh()
    v = np.vstack([v, [[99.0, 99.0, 99.0]]])  # isolated vertex, no face
    rem = debris.removed_small_parts(v, t, 8)
    assert rem is not None, rem
    assert rem['count'] == 1 and rem['faces_total'] == 4, rem


def test_single_component_is_silent():
    v, t = _box(0, 0, 0, 10)
    v, t = np.array(v, float), np.array(t, np.int64)
    assert debris.removed_small_parts(v, t, 8) is None
    assert debris.possible_debris(v, t) is None


def test_json_serializable():
    v, t = _mesh()
    payload = {'removed_small_parts': debris.removed_small_parts(v, t, 8),
               'possible_debris': debris.possible_debris(v, t)}
    json.dumps(payload)  # must not raise (no numpy scalars)


def _result():
    return {
        'input': '/tmp/in.stl', 'output': '/tmp/out_fixed.stl',
        'stage1': {
            'two_manifold': True, 'holes_remaining': 0, 'components': 3,
            'components_before': 3, 'holes_closed': 0,
            'removed_small_parts': {'count': 1, 'min_faces': 8,
                                    'faces_total': 4, 'capped': False,
                                    'parts': [{'faces': 4, 'size_mm': 1.7,
                                               'size_pct': 3.0,
                                               'distance_mm': 45.0}]},
            'possible_debris': {'count': 1, 'capped': False,
                                'parts': [{'faces': 12, 'volume_pct': 0.1,
                                           'size_pct': 3.0, 'distance_mm': 14.5}]},
        },
        'print_report': {'fixed': [], 'issues': [], 'advisories': [],
                         'geometry': {'components_before': 3,
                                      'components_after': 3}},
    }


def test_brief_note_and_human_render():
    b = classification.brief(_result())
    codes = {n['code'] for n in b['notes']}
    assert 'possible_debris' in codes, b['notes']
    text = human_report(_result())
    assert 'Small parts removed (< 8 faces): 1 component(s), 4 face(s)' in text, text
    assert '4 face(s), size' in text and 'from the main body' in text, text
    assert 'Suspicious separate part(s) kept: 1' in text, text
    assert 'nothing was removed' in text, text


def test_no_lines_when_nothing_flagged():
    r = _result()
    r['stage1']['removed_small_parts'] = None
    r['stage1']['possible_debris'] = None
    text = human_report(r)
    assert 'Small parts removed' not in text
    assert 'Suspicious separate part' not in text
    b = classification.brief(r)
    assert not any(n['code'] == 'possible_debris' for n in b['notes']), b['notes']


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('debris report tests passed')


if __name__ == '__main__':
    main()
