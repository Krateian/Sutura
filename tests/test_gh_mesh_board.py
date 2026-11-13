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


def _strict_row(name, verdict, strict_b, strict_c, set_name='bench115'):
    holes_b = 0 if strict_b else 1
    holes_c = 0 if strict_c else 1
    b = dict(SHAPE, reload_holes=holes_b)
    c = dict(SHAPE, reload_holes=holes_c)
    return {'id': name, 'verdict': verdict, 'reason': 'why',
            'baseline': b, 'candidate': c, 'missing': [],
            'set': set_name, 'mode': 'bench115',
            'strict_watertight': {'baseline': strict_b, 'candidate': strict_c}}


def test_strict_watertight_counts_are_printed_per_set():
    rows = [_strict_row('m1', 'SAME', True, True),
            _strict_row('m2', 'SAME', True, False),
            _strict_row('m3', 'SAME', False, False)]
    rc, out = _board(_run(rows))
    assert rc == 0, out
    assert 'StrictWT b/c' in out, out
    assert 'baseline 2/3, candidate 1/3' in out, out
    assert 'September 2026' in out, out


def test_strict_counts_are_not_gated():
    # A strict-watertight regression is a measurement, not a verdict: the row
    # verdict drives the exit code, never the count.
    rows = [_strict_row('m1', 'SAME', True, False)]
    rc, out = _board(_run(rows))
    assert rc == 0, out
    assert 'candidate 0/1' in out, out


def test_strict_recomputed_when_shard_json_predates_the_field():
    row = _strict_row('m1', 'SAME', True, False)
    del row['strict_watertight']
    d = _run([row])
    rc, out = _board(d)
    assert rc == 0, out
    assert 'yes/no' in out, out


def test_skipped_nondet_is_reported_in_the_strict_block():
    rows = [_strict_row('m1', 'SKIPPED-NONDET', True, True)]
    rc, out = _board(_run(rows))
    assert rc == 0, out
    assert 'SKIPPED-NONDET 1' in out, out


def test_same_id_in_two_sets_renders_twice():
    a = _strict_row('artec_metal-nut', 'SAME', True, True, set_name='bench115')
    b = _strict_row('artec_metal-nut', 'SAME', True, True, set_name='samples40')
    rc, out = _board(_run([a, b]))
    assert rc == 0, out
    assert out.count('artec_metal-nut') == 2, out


CAP_NOTE = 'skipped (face cap)'


def _cap_row(name, verdict='SAME', b_note=CAP_NOTE, c_note=CAP_NOTE,
             set_name='bench115'):
    b = dict(SHAPE, si_output=None, si_output_note=b_note)
    c = dict(SHAPE, si_output=None, si_output_note=c_note)
    return {'id': name, 'verdict': verdict, 'reason': 'why',
            'baseline': b, 'candidate': c, 'missing': [],
            'set': set_name, 'mode': 'bench115',
            'strict_watertight': {'baseline': True, 'candidate': True}}


def test_face_cap_rows_get_their_own_count_line():
    rows = [_cap_row('m1'), _cap_row('m2'), _cap_row('m3')]
    rc, out = _board(_run(rows))
    assert rc == 0, out
    assert 'SI n/a (face cap)' in out, out
    assert 'bench115     3 row(s) judged without SI, face cap' in out, out


def test_face_cap_count_is_per_set():
    rows = [_cap_row('m1', set_name='bench115'),
            _cap_row('m2', set_name='derived264')]
    rc, out = _board(_run(rows))
    assert rc == 0, out
    assert 'bench115' in out and 'derived264' in out, out
    assert '1 row(s) judged without SI' in out, out


def test_face_cap_line_absent_when_no_capped_rows():
    rc, out = _board(_run([_row('m1', 'SAME')]))
    assert rc == 0, out
    assert 'judged without SI' not in out, out


def test_face_cap_rows_are_not_failures():
    # Judged without SI, but SAME/BETTER/WORSE semantics still rule the exit.
    rc, out = _board(_run([_cap_row('m1', 'SAME')]), check=True)
    assert rc == 0, out
    rc, out = _board(_run([_cap_row('m1', 'WORSE')]), check=True)
    assert rc == 1, out


def test_one_sided_cap_is_not_counted_as_a_skip():
    rows = [_cap_row('m1', b_note=CAP_NOTE, c_note=None)]
    rc, out = _board(_run(rows))
    assert rc == 0, out
    assert 'judged without SI' not in out, out


INTERIOR_NOTE = 'interior walls not computed (large mesh)'


def _timeout_row(name, set_name='bench115'):
    b = dict(SHAPE, si_output=None)
    c = dict(SHAPE, si_output=None)
    return {'id': name, 'verdict': 'TIMEOUT', 'reason': 'measurement exceeded 900s',
            'baseline': b, 'candidate': c, 'missing': [],
            'set': set_name, 'mode': 'bench115',
            'strict_watertight': None}


def test_timeout_is_a_failure_and_has_its_own_line():
    rc, out = _board(_run([_timeout_row('m1')]), check=True)
    assert rc == 1, out
    assert 'TIMEOUT' in out, out
    assert '1 row(s) timed out' in out, out


def test_timeout_counts_per_set():
    rows = [_timeout_row('m1', 'bench115'), _timeout_row('m2', 'derived264')]
    rc, out = _board(_run(rows), check=True)
    assert rc == 1, out
    assert 'bench115' in out and 'derived264' in out, out


def test_no_timeout_line_when_none_timed_out():
    rc, out = _board(_run([_row('m1', 'SAME')]))
    assert rc == 0, out
    assert 'timed out' not in out, out


def test_interior_walls_note_gets_its_own_line():
    b = dict(SHAPE, buried_note=INTERIOR_NOTE)
    c = dict(SHAPE, buried_note=INTERIOR_NOTE)
    row = {'id': 'big', 'verdict': 'SAME', 'reason': 'ok', 'baseline': b,
           'candidate': c, 'missing': [], 'set': 'bench115', 'mode': 'bench115',
           'strict_watertight': {'baseline': True, 'candidate': True}}
    rc, out = _board(_run([row]))
    assert rc == 0, out
    assert INTERIOR_NOTE in out, out
    assert '1 row(s) use all-face distances' in out, out


def test_interior_walls_line_absent_below_the_gate():
    rc, out = _board(_run([_row('m1', 'SAME')]))
    assert rc == 0, out
    assert 'use all-face distances' not in out, out


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('gh_mesh_board tests passed')
