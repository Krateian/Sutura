#!/usr/bin/env python3
"""Unit tests for learning triage (local (template, method) success/time bonus).

Uses a temporary SUTURA_DATA_DIR so the real usage history is never touched. No
repair runs: record_learning is fed synthetic methods_tried entries and the
ranking bonus is checked on an ObjectAnalysis.

Usage: <venv>/bin/python tests/test_learning_triage.py
(any interpreter with numpy).
"""
import os
import sys
import json
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

_TMP = tempfile.mkdtemp(prefix='sutura-learning-')
os.environ['SUTURA_DATA_DIR'] = _TMP
os.environ.pop('SUTURA_LEARNING_TRIAGE', None)

import history  # noqa: E402
import sutura_engine.learning as learning_mod  # noqa: E402

CV = learning_mod.CLASSIFIER_VERSION


def _write_raw(data):
    """Write a learning file directly, then drop the in-process mtime cache so
    the next read sees it regardless of filesystem mtime resolution."""
    os.makedirs(os.path.dirname(history.LEARNING_PATH), exist_ok=True)
    with open(history.LEARNING_PATH, 'w', encoding='utf-8') as f:
        json.dump(data, f)
    learning_mod._LEARNING_CACHE['mtime'] = None
    learning_mod._LEARNING_CACHE['stats'] = None


def _read_raw():
    with open(history.LEARNING_PATH, encoding='utf-8') as f:
        return json.load(f)


def _legacy_file(method='local_mend', n=8, ok=8, ms=1600):
    return {'schema_version': 1, 'templates': {'mechanical': {
        method: {'n': n, 'ok': ok, 'total_ms': ms},
        'join': {'n': 3, 'ok': 1, 'total_ms': 900}}}}


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


def test_legacy_bucket_grandfathered():
    # A schema-1 file with bare (unstamped) buckets: every count is read and
    # counted unchanged, and the next record touches only the recorded bucket.
    history.clear_learning()
    _write_raw(_legacy_file())
    stats = history.triage_stats()
    assert stats['mechanical']['local_mend']['n'] == 8, stats
    assert stats['mechanical']['join']['n'] == 3, stats
    bonus_legacy = history.learning_bonus('mechanical', 'local_mend')
    assert 0.0 < bonus_legacy <= history.LEARNING_MAX_BONUS, bonus_legacy
    history.record_learning(_result([
        _tried('local_mend', 'mechanical', 'accepted', 100)]))
    raw = _read_raw()
    assert raw['schema_version'] == 2, raw
    touched = raw['templates']['mechanical']['local_mend']
    assert touched['cv'] == CV, touched
    assert touched['n'] == 9 and touched['ok'] == 9, touched
    assert touched['total_ms'] == 1700, touched
    # an untouched legacy bucket survives, still bare and still counted
    untouched = raw['templates']['mechanical']['join']
    assert untouched == {'n': 3, 'ok': 1, 'total_ms': 900}, untouched
    assert history.triage_stats()['mechanical']['join']['n'] == 3


def test_missing_version_counted():
    history.clear_learning()
    _write_raw(_legacy_file())
    assert history.learning_bonus('mechanical', 'local_mend') > 0.0


def test_different_known_version_ignored():
    history.clear_learning()
    _write_raw({'schema_version': 1, 'templates': {'mechanical': {
        'local_mend': {'n': 9, 'ok': 9, 'total_ms': 900, 'cv': '0'}}}})
    assert history.triage_stats() == {}
    assert history.learning_bonus('mechanical', 'local_mend') == 0.0
    history.record_learning(_result([
        _tried('local_mend', 'mechanical', 'accepted', 100)]))
    methods = _read_raw()['templates']['mechanical']
    assert methods['local_mend@0']['n'] == 9, methods
    assert methods['local_mend@0']['ok'] == 9, methods
    assert methods['local_mend@0']['cv'] == '0', methods
    assert methods['local_mend']['n'] == 1, methods
    assert methods['local_mend']['cv'] == CV, methods
    # the current bucket is counted; the preserved stale one is not
    assert history.triage_stats()['mechanical']['local_mend']['n'] == 1


def test_current_version_counted():
    history.clear_learning()
    _write_raw({'schema_version': 2, 'templates': {'mechanical': {
        'local_mend': {'n': 8, 'ok': 8, 'total_ms': 800, 'cv': CV}}}})
    assert history.learning_bonus('mechanical', 'local_mend') > 0.0


def test_no_top_level_version_key():
    history.clear_learning()
    history.record_learning(_result([
        _tried('local_mend', 'mechanical', 'accepted', 100)]))
    raw = _read_raw()
    assert 'classifier_version' not in raw, raw
    assert raw['templates']['mechanical']['local_mend']['cv'] == CV, raw


def test_record_stamps_new_bucket():
    history.clear_learning()
    history.record_learning(_result([
        _tried('quick_clean', 'mechanical', 'rejected', 50)]))
    raw = _read_raw()
    assert raw['schema_version'] == 2, raw
    assert raw['templates']['mechanical']['quick_clean']['cv'] == CV, raw


def test_learning_bonus_rejects_stale_stats():
    # The gate must live in learning_bonus too, not only triage_stats.
    stale = {'mechanical': {'local_mend': {
        'n': 9, 'ok': 9, 'total_ms': 0, 'cv': '0'}}}
    assert history.learning_bonus('mechanical', 'local_mend',
                                  stats=stale) == 0.0


def test_newer_version_preserved_and_ignored():
    # A bucket from a NEWER classifier (downgrade) is still a different known
    # version: preserved on disk and ignored (never incremented into).
    history.clear_learning()
    _write_raw({'schema_version': 2, 'templates': {'mechanical': {
        'local_mend': {'n': 5, 'ok': 5, 'total_ms': 500, 'cv': '2'}}}})
    assert history.triage_stats() == {}
    assert history.learning_bonus('mechanical', 'local_mend') == 0.0
    history.record_learning(_result([
        _tried('local_mend', 'mechanical', 'accepted', 100)]))
    methods = _read_raw()['templates']['mechanical']
    assert methods['local_mend@2']['n'] == 5, methods
    assert methods['local_mend']['n'] == 1, methods
    assert methods['local_mend']['cv'] == CV, methods


def test_relocation_merges_on_collision():
    # A stale bucket relocating onto an existing method@cv key merges counts
    # instead of overwriting it.
    history.clear_learning()
    _write_raw({'schema_version': 2, 'templates': {'mechanical': {
        'local_mend': {'n': 3, 'ok': 3, 'total_ms': 300, 'cv': '0'},
        'local_mend@0': {'n': 4, 'ok': 4, 'total_ms': 400, 'cv': '0'}}}})
    history.record_learning(_result([
        _tried('local_mend', 'mechanical', 'accepted', 100)]))
    methods = _read_raw()['templates']['mechanical']
    assert methods['local_mend@0']['n'] == 7, methods
    assert methods['local_mend@0']['ok'] == 7, methods
    assert methods['local_mend@0']['total_ms'] == 700, methods
    assert methods['local_mend']['n'] == 1, methods
    assert methods['local_mend']['cv'] == CV, methods


def test_median_excludes_stale_buckets():
    # Stale buckets must not skew the speed median that damps current buckets.
    history.clear_learning()
    _write_raw({'schema_version': 2, 'templates': {'mechanical': {
        'local_mend': {'n': 8, 'ok': 8, 'total_ms': 800000, 'cv': CV},
        'join': {'n': 8, 'ok': 8, 'total_ms': 800, 'cv': '0'},
        'graft': {'n': 8, 'ok': 8, 'total_ms': 800, 'cv': '0'}}}})
    got = history.learning_bonus('mechanical', 'local_mend')
    only = history.learning_bonus('mechanical', 'local_mend', stats={
        'mechanical': {'local_mend': {
            'n': 8, 'ok': 8, 'total_ms': 800000}}})
    assert got == only, (got, only)


def test_import_is_not_fatal():
    import subprocess
    env = dict(os.environ)
    env['SUTURA_DATA_DIR'] = _TMP
    env['PYTHONPATH'] = os.pathsep.join([REPO, SUTURA])
    r = subprocess.run(
        [sys.executable, '-c', 'import history; print(history.LEARNING_PATH)'],
        cwd=REPO, env=env, capture_output=True, text=True)
    assert r.returncode == 0, r.stderr


def test_clear_still_removes_file():
    history.clear_learning()
    history.record_learning(_result([
        _tried('local_mend', 'mechanical', 'accepted', 100)]))
    assert os.path.exists(history.LEARNING_PATH)
    assert history.clear_learning() is True
    assert not os.path.exists(history.LEARNING_PATH)
    assert history.clear_learning() is False


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('learning triage tests passed')


if __name__ == '__main__':
    main()
