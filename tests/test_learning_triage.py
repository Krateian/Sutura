#!/usr/bin/env python3
"""Unit tests for learning triage (local (template, method) success/time bonus).

Uses a temporary SUTURA_DIR so the real usage history is never touched. No
repair runs: record_learning is fed synthetic methods_tried entries and the
ranking bonus is checked on an ObjectAnalysis.

Usage: <venv>/bin/python tests/test_learning_triage.py
(any interpreter with numpy).
"""
import os
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

_TMP = tempfile.mkdtemp(prefix='sutura-learning-')
os.environ['SUTURA_DIR'] = _TMP
os.environ.pop('SUTURA_LEARNING_TRIAGE', None)

import history  # noqa: E402


def _result(tried):
    return {'methods_tried': tried, 'method_used': {'num': 2,
                                                    'id': 'local_mend',
                                                    'source': 'tagged'}}


def _tried(method_id, template, outcome, ms):
    return {'num': None, 'id': method_id, 'outcome': outcome,
            'template': template, 'elapsed_ms': ms}


def test_no_data_is_neutral():
    history.clear_learning()
    assert history.triage_stats() == {}
    assert history.learning_bonus('mechanical', 'local_mend') == 0.0


def test_record_and_bonus():
    history.clear_learning()
    for _ in range(8):
        history.record_learning(_result([
            _tried('quick_clean', 'mechanical', 'rejected', 500),
            _tried('local_mend', 'mechanical', 'accepted', 200),
        ]))
    stats = history.triage_stats()
    assert stats['mechanical']['local_mend']['n'] == 8, stats
    assert stats['mechanical']['local_mend']['ok'] == 8, stats
    bonus = history.learning_bonus('mechanical', 'local_mend')
    assert 0.0 < bonus <= history.LEARNING_MAX_BONUS, bonus
    # a template/method with no data stays neutral
    assert history.learning_bonus('organic', 'local_mend') == 0.0
    assert history.learning_bonus('mechanical', 'join') == 0.0
    # the always-failing sibling gets no bonus
    assert history.learning_bonus('mechanical', 'quick_clean') == 0.0


def test_reset():
    history.clear_learning()
    history.record_learning(_result([
        _tried('local_mend', 'mechanical', 'accepted', 100)]))
    assert history.triage_stats(), 'expected recorded data'
    assert history.clear_learning() is True
    assert history.triage_stats() == {}
    # clearing again is a no-op, not an error
    assert history.clear_learning() is False


def test_env_disable():
    os.environ['SUTURA_LEARNING_TRIAGE'] = '0'
    try:
        assert history.learning_enabled() is False
    finally:
        os.environ.pop('SUTURA_LEARNING_TRIAGE', None)
    assert history.learning_enabled() is True


def test_ranking_bonus_applied():
    import methods
    from object_analysis import ObjectAnalysis as A
    history.clear_learning()
    a = A(mesh_type='mechanical', type_confidence=0.9,
          boundary_loops=2, non_manifold_edges=1, open_area_ratio=0.1)
    base = {r.num: r.score for r in methods.rank_methods(a)}
    assert base.get(2, 0.0) > 0.0, base
    # a strong local success record for (mechanical, local_mend) lifts its score
    for _ in range(10):
        history.record_learning(_result([
            _tried('local_mend', 'mechanical', 'accepted', 100)]))
    lifted = {r.num: r.score for r in methods.rank_methods(a)}
    assert lifted.get(2, 0.0) >= base.get(2, 0.0), (base, lifted)
    assert lifted.get(2, 0.0) <= 1.0, lifted
    assert lifted.get(2, 0.0) > base.get(2, 0.0), (base, lifted)


def test_build_record_fields():
    rep = _result([_tried('local_mend', 'mechanical', 'accepted', 100)])
    rec = history.build_record(rep, 'fp', '0.6.1', 123)
    assert rec['method_used_id'] == 'local_mend', rec
    assert rec['methods_tried'][0]['template'] == 'mechanical', rec
    assert rec['methods_tried'][0]['elapsed_ms'] == 100, rec


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('learning triage tests passed')


if __name__ == '__main__':
    main()
