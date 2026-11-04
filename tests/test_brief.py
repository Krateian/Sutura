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
    # ONE headline verdict: a failed repair reads as the repair_verdict wording.
    assert 'Not fully repaired' in text, text
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


def test_headline_folds_fidelity():
    # Fidelity measured -> headline is the repair_verdict wording.
    r = _base()
    r['repair_verdict_code'] = 'preserved'
    r['repair_verdict_areas'] = 0
    assert classification.headline_verdict(r)['code'] == 'preserved'
    text = human_report(r)
    assert text.splitlines()[0] == 'Repaired, shape preserved', text

    r2 = _base()
    r2['repair_verdict_code'] = 'changed'
    r2['repair_verdict_areas'] = 2
    assert classification.headline_verdict(r2) == {'code': 'changed', 'areas': 2}

    # A failed brief overrides a "preserved" Fidelity.
    r3 = _base(output=None)
    r3.pop('stage2', None)
    r3['stage1']['two_manifold'] = False
    r3['stage1']['holes_remaining'] = 3
    r3['repair_verdict_code'] = 'preserved'
    assert classification.headline_verdict(r3)['code'] == 'not_repaired'
    assert human_report(r3).splitlines()[0] == 'Not fully repaired'

    # No Fidelity -> the honest brief verdict is the headline (no fake shape claim).
    assert classification.headline_verdict(_base())['code'] == 'ready'
    assert human_report(_base()).splitlines()[0] == 'Ready to print'


def test_attribute_loss_warns():
    r = _base()
    r['triangle_attributes'] = {'original_attributed': 10, 'kept': 6,
                                'dropped': 4, 'output_triangles': 34}
    b = classification.brief(r)
    assert b['verdict'] == 'notes', b
    assert any(n['code'] == 'attribute_loss' for n in b['notes']), b['notes']
    text = human_report(r)
    assert 'WARNING:' in text, text
    assert 'kept 6' in text and 'dropped 4' in text, text
    assert 'new without attributes 28' in text, text  # 34 output - 6 kept


def test_attribute_new_only_is_info_not_warning():
    r = _base()
    r['triangle_attributes'] = {'original_attributed': 10, 'kept': 10,
                                'dropped': 0, 'output_triangles': 30}
    b = classification.brief(r)
    # only new triangles lack attributes: information, not a warning/note
    assert b['verdict'] == 'ready', b
    assert not any(n['code'] == 'attribute_loss' for n in b['notes']), b['notes']
    text = human_report(r)
    assert 'Note:' in text and 'new without attributes 20' in text, text
    assert 'WARNING:' not in text, text


def test_attribute_silent_without_attributes():
    r = _base()
    r['triangle_attributes'] = {'original_attributed': 0, 'kept': 0,
                                'dropped': 0, 'output_triangles': 30}
    assert classification.attribute_stats(r) is None
    assert 'without attributes' not in human_report(r)


def test_attribute_multi_object_per_object_lines():
    r = _base()
    r['objects'] = 2
    r['triangle_attributes'] = {'original_attributed': 10, 'kept': 6,
                                'dropped': 4, 'output_triangles': 40}
    r['object_reports'] = [
        {'model': '3D/Objects/a.model',
         'triangle_attributes': {'original_attributed': 6, 'kept': 6,
                                 'dropped': 0, 'output_triangles': 20}},
        {'model': '3D/Objects/b.model',
         'triangle_attributes': {'original_attributed': 4, 'kept': 0,
                                 'dropped': 4, 'output_triangles': 20}},
    ]
    stats = classification.attribute_stats(r)
    assert stats['dropped'] == 4 and stats['kept'] == 6, stats
    assert stats['new_without'] == 34, stats  # (20-6) + (20-0)
    text = human_report(r)
    assert '3D/Objects/a.model: kept 6, dropped 0' in text, text
    assert '3D/Objects/b.model: kept 0, dropped 4' in text, text


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok ', name)
