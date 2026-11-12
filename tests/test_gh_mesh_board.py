#!/usr/bin/env python3
"""Board aggregation tests for scripts/gh_mesh_board.py.

Pins the verdict semantics the CI board shares with the local one:
TRADE-SI is its own non-failing verdict with an SI-delta column, the visible
two-sided values are shown next to the *_all_* ones, and WORSE/FAIL/ERROR/
CHANGED/MISSING/shard errors all fail --check.
"""
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, '..', 'scripts', 'gh_mesh_board.py')


def _load():
    spec = importlib.util.spec_from_file_location('gh_mesh_board', SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)
    return mod


B = mod = _load()

SHAPE = {
    'reload_holes': 0, 'reload_non_manifold_edges': 0, 'components': 1,
    'closed_shells': 1, 'orca_number_of_parts': 1, 'orca_manifold': 'yes',
    'volume_error_rel': 0.0, 'two_sided_max_mm': 0.1, 'two_sided_p95_mm': 0.02,
    'two_sided_max_all_mm': 0.4, 'two_sided_p95_all_mm': 0.08,
    'i2o_max_all_mm': 0.4, 'i2o_p95_all_mm': 0.08,
    'leak_max_mm': 0.0, 'leak_count': 0, 'si_output': 0,
}


def _row(name, verdict, base=None, cand=None, reason='why'):
    return {'id': name, 'verdict': verdict, 'reason': reason,
            'baseline': dict(SHAPE if base is None else base),
            'candidate': dict(SHAPE if cand is None else cand),
            'missing': []}


def _run(rows, extra=None):
    d = tempfile.mkdtemp()
    payload = {'set': 'samples40', 'shard': 0, 'shards': 1,
               'mode': 'single_side', 'rc': 0, 'meshes': len(rows),
               'rows': rows, 'error': None}
    json.dump(payload, open(os.path.join(d, 'a.json'), 'w'))
    if extra:
        json.dump(extra, open(os.path.join(d, 'b.json'), 'w'))
    return d


def _board(d, check=False):
    cmd = [sys.executable, SCRIPT, d] + (['--check'] if check else [])
    p = subprocess.run(cmd, capture_output=True, text=True)
    return p.returncode, p.stdout


def test_trade_si_is_non_failing_and_has_a_delta_column():
    rc, out = _board(_run([_row('m1', 'TRADE-SI', cand=dict(SHAPE, si_output=6))]),
                     check=True)
    assert rc == 0, out
    assert 'SI trade' in out, out
    assert '| +6 |' in out, out
    assert '0/6' in out, out


def test_trade_si_is_not_in_the_failing_set():
    assert 'TRADE-SI' not in B.FAILING


def test_worse_fails():
    rc, out = _board(_run([_row('m1', 'WORSE',
                                cand=dict(SHAPE, reload_holes=3))]), check=True)
    assert rc == 1, out


def test_changed_fails():
    rc, _out = _board(_run([_row('m1', 'CHANGED')]), check=True)
    assert rc == 1


def test_shard_error_fails_even_with_good_rows():
    rc, _out = _board(_run([_row('m1', 'SAME')],
                           extra={'error': {'error_type': 'X', 'rc': 1}}),
                      check=True)
    assert rc == 1


def test_visible_and_all_face_columns_are_both_shown():
    rc, out = _board(_run([_row('m1', 'SAME')]))
    assert rc == 0
    assert '2-sided max b/c' in out and '2-sided max all b/c' in out, out
    assert '0.1000/0.1000' in out, out
    assert '0.4000/0.4000' in out, out


def test_missing_metric_renders_dash_not_a_crash():
    rc, out = _board(_run([_row('m1', 'SAME', cand={'reload_holes': 0})]))
    assert rc == 0, out
    assert '0/-' in out, out


def _board_plan(d, plan, check=True):
    pfile = os.path.join(d, 'plan.json')
    json.dump(plan, open(pfile, 'w'))
    cmd = [sys.executable, SCRIPT, '--plan', pfile, d]
    if check:
        cmd.insert(2, '--check')
    p = subprocess.run(cmd, capture_output=True, text=True)
    return p.returncode, p.stdout


def test_a_planned_shard_without_a_summary_fails():
    # aggregate runs with if: always(), so a dead shard leaves no JSON; its
    # meshes must fail the board rather than silently disappear.
    d = _run([_row('m1', 'SAME')])
    plan = {'include': [
        {'set': 'samples40', 'mode': 'single_side', 'shard': 0, 'shards': 2},
        {'set': 'samples40', 'mode': 'single_side', 'shard': 1, 'shards': 2}]}
    rc, out = _board_plan(d, plan)
    assert rc == 1, out
    assert 'no summary' in out, out


def test_every_planned_shard_present_passes():
    d = _run([_row('m1', 'SAME')])
    plan = {'include': [
        {'set': 'samples40', 'mode': 'single_side', 'shard': 0, 'shards': 1}]}
    rc, out = _board_plan(d, plan)
    assert rc == 0, out


def test_fewer_rows_than_meshes_fails():
    d = _run([_row('m1', 'SAME')])
    doc_path = os.path.join(d, 'a.json')
    doc = json.load(open(doc_path))
    doc['meshes'] = 3
    json.dump(doc, open(doc_path, 'w'))
    plan = {'include': [
        {'set': 'samples40', 'mode': 'single_side', 'shard': 0, 'shards': 1}]}
    rc, out = _board_plan(d, plan)
    assert rc == 1, out
    assert 'meshes but' in out, out


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('gh_mesh_board tests passed')
