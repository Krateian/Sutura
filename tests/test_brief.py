#!/usr/bin/env python3
"""Compact verdict/brief tests (C6)."""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import classification  # noqa: E402
from sutura_engine.pipeline.report import human_report  # noqa: E402


def _base(**kw):
    r = {
        'input': '/tmp/in.stl', 'output': '/tmp/out_fixed.stl',
        'stage1': {'two_manifold': True, 'holes_remaining': 0,
                   'components': 1, 'components_before': 1,
                   'holes_closed': 3, 'volume_before': 10.0,
                   'volume_after': 10.0},
        'stage2': {'ok': True},
        'print_report': {'fixed': [{'code': 'holes_closed', 'count': 3}],
                         'issues': [], 'advisories': [],
                         'geometry': {'components_before': 1,
                                      'components_after': 1,
                                      'deviation_max_pct': 0.4}},
    }
    r.update(kw)
    return r


def test_ready():
    b = classification.brief(_base())
    assert b['verdict'] == 'ready', b
    assert b['holes_closed'] == 3, b
    assert b['shape_change_pct'] == 0.4, b
    assert classification.VERDICT_LABELS['ready'] == 'Ready to print'
    text = human_report(_base())
    assert text.splitlines()[0] == 'Ready to print', text
    assert 'Holes closed: 3' in text, text


def test_notes_thin_wall_and_si():
    r = _base(stage2={'error': 'Stage 2 skipped: no bridge'})
    r['print_report']['advisories'] = [
        {'code': 'thin_wall', 'thin_ratio': 0.12, 'threshold_norm': 0.02,
         'min_mm': 0.35, 'estimated': False}]
    r['si_escalation'] = {'triggered': True, 'method': 'scaffold',
                          'outcome': 'adopted',
                          'reason': 'shape preserved'}
    b = classification.brief(r)
    assert b['verdict'] == 'notes', b
    codes = {n['code'] for n in b['notes']}
    assert {'stage2_skipped', 'thin_wall', 'si_escalation'} <= codes, codes
    text = human_report(r)
    assert 'Repaired with notes' in text, text
    assert 'thin walls' in text.lower(), text
    assert 'Overlapping surfaces were fixed with a stronger repair method.' in text, text


def test_failed_partial():
    r = _base(output=None)
    r.pop('stage2', None)
    r['stage1']['two_manifold'] = False
    r['stage1']['holes_remaining'] = 4
    b = classification.brief(r)
    assert b['verdict'] == 'failed', b
    text = human_report(r)
    assert 'Could not fully repair' in text, text
    assert 'hole(s) remain' in text, text


def test_orientation_and_parts():
    r = _base()
    r['print_report']['fixed'].append(
        {'code': 'orientation_corrected', 'count': 1})
    r['print_report']['geometry']['components_before'] = 3
    r['print_report']['geometry']['components_after'] = 1
    b = classification.brief(r)
    assert b['orientation_corrected'] is True, b
    assert (b['parts_before'], b['parts_after']) == (3, 1), b
    text = human_report(r)
    assert 'Orientation: corrected' in text, text
    assert 'Parts: 3 -> 1 (2 removed)' in text, text


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok ', name)
