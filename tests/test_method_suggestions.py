#!/usr/bin/env python3
"""D13: per-object suggested methods are additive and informational."""
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

from sutura_engine import methods  # noqa: E402
from sutura_engine.diagnosis import ObjectAnalysis  # noqa: E402


def test_top_three_and_estimate():
    a = ObjectAnalysis(faces=1000, vertices=600, boundary_loops=3,
                       non_manifold_edges=4, self_intersections=0,
                       mesh_type='mechanical', type_confidence=0.9)
    s = methods.suggest_methods(a)
    assert len(s) == 3, s
    assert all(0 <= x['confidence'] <= 95 for x in s), s
    assert all(x['confidence_label'] in ('high', 'medium', 'low') for x in s), s
    assert all(x['basis'] == 'rank+core150-c26ed2e' for x in s), s
    assert all(x['estimate'] is True for x in s), s
    assert all(x['num'] not in (15, 16, 17) for x in s), s
    assert all(x['reason_key'].startswith('sugg_') for x in s), s


def test_si_and_repeated_prior_are_used():
    si = ObjectAnalysis(faces=1000, vertices=600, self_intersections=8,
                        mesh_type='mechanical', type_confidence=0.9)
    s = methods.suggest_methods(si)
    assert len(s) == 3 and all(x['estimate'] for x in s), s
    rep = ObjectAnalysis(faces=1000, vertices=600, repetition_score=0.8,
                         mesh_type='mechanical', type_confidence=0.9)
    r = methods.suggest_methods(rep)
    assert len(r) == 3 and all(x['estimate'] for x in r), r


def test_tie_is_never_high():
    orig = methods.rank_methods
    R = methods.Recommendation
    fake = [R(3, 'full_mend', 'Full Mend', 0.99, 'x', None, 'x', ()),
            R(10, 'scaffold', 'Scaffold', 0.99, 'x', None, 'x', ()),
            R(11, 'transplant', 'Transplant', 0.80, 'x', None, 'x', ())]
    methods.rank_methods = lambda *_a, **_k: fake
    try:
        s = methods.suggest_methods(ObjectAnalysis(faces=10, vertices=8))
    finally:
        methods.rank_methods = orig
    assert [x['confidence'] for x in s] == [95, 95, 80], s
    assert [x['confidence_label'] for x in s] == ['medium', 'medium', 'medium'], s


def test_join_requires_body_overlap():
    orig = methods.rank_methods
    R = methods.Recommendation
    fake = [R(4, 'join', 'Join', 0.99, 'x', None, 'x', ()),
            R(3, 'full_mend', 'Full Mend', 0.90, 'x', None, 'x', ()),
            R(1, 'quick_clean', 'Quick Clean', 0.80, 'x', None, 'x', ())]
    methods.rank_methods = lambda *_a, **_k: fake
    try:
        no = methods.suggest_methods(ObjectAnalysis(
            faces=10, vertices=8,
            diagnosis_v2={'self_intersections': {'inter_body': 0},
                          'parts': {'overlap_edges': []}}))
        yes = methods.suggest_methods(ObjectAnalysis(
            faces=10, vertices=8,
            diagnosis_v2={'self_intersections': {'inter_body': 3},
                          'parts': {'overlap_edges': []}}))
    finally:
        methods.rank_methods = orig
    assert 4 not in [x['num'] for x in no], no
    assert [x['num'] for x in yes][:1] == [4], yes


def test_analyze_multi_object_has_per_object_suggestions():
    tmp = tempfile.mkdtemp(prefix='sutura-suggest-')
    src = os.path.join(tmp, 'layered.3mf')
    subprocess.run([sys.executable,
                    os.path.join(REPO, 'tests', 'make_layered_multiobject_3mf.py'),
                    src], check=True, stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL, timeout=120)
    d = methods.analyze_report(src)
    assert 'error' not in d, d
    assert d.get('objects', 0) >= 2, d
    assert len(d.get('object_analyses') or []) == d['objects'], d
    for obj in d['object_analyses']:
        assert len(obj.get('suggestions') or []) == 3, obj
    assert len(d.get('recommendations') or []) <= 6, d
    assert len(d.get('suggestions') or []) == 3, d
    assert d.get('suggestion_basis') == 'rank+core150-c26ed2e', d


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok ', name)
