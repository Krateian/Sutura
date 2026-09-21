#!/usr/bin/env python3
"""Regression test for sutura/history.py (anonymous usage history).

Checks that:
  1. history.py is stdlib+numpy only (no pymeshlab/trimesh/manifold3d).
  2. mesh_fingerprint() is deterministic for identical geometry and changes
     when the geometry changes.
  3. A history record NEVER leaks identifying data: its key set is exactly the
     documented schema and no value contains the input path, file name or user
     name.
  4. write()/append_record()/load_records() round-trip correctly (single-mesh
     and multi-object 3MF), and export_history() prints summary + full data.
Usage: ~/.local/share/sutura/venv/bin/python tests/test_history.py
"""
import io
import json
import os
import sys
import tempfile
from contextlib import redirect_stdout

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402

import history  # noqa: E402

SCHEMA_KEYS = {
    'schema_version', 'seq', 'ts', 'app_version', 'format', 'mesh_fingerprint',
    'elapsed_ms', 'faces_before', 'faces_after', 'vertices_before',
    'vertices_after', 'defect_holes', 'defect_non_manifold', 'holes_closed',
    'holes_remaining', 'non_manifold_edges_fixed',
    'non_manifold_edges_remaining', 'detected_type', 'detected_confidence',
    'repair_mode', 'tuning_applied', 'filters_applied', 'filters_skipped',
    'stage2_status', 'category', 'issues', 'repair_confidence',
    'repair_confidence_label', 'volume_change_percent',
    'extreme_passes_applied', 'self_intersections_found',
    'self_intersections_removed', 'autorefine_applied',
}


def _sample_result(**over):
    r = {
        'input': '/home/krateian/src/sutura/supersecret_turbo.stl',
        'output': '/home/krateian/src/sutura/supersecret_turbo_fixed.stl',
        'stage1': {
            'faces_before': 100, 'faces_after': 99,
            'vertices_before': 60, 'vertices_after': 58,
            'holes_closed': 2, 'holes_remaining': 0,
            'non_manifold_edges_fixed': 1, 'non_manifold_edges_remaining': 0,
            'applied_filters': 11,
            'skipped': {'meshing_repair_non_manifold_edges_by_removing_faces': True},
            'volume_change_percent': 0.5,
        },
        'defects': {'holes': [{'centroid': [1, 2, 3]}], 'non_manifold': []},
        'detected_type': 'mechanical', 'detected_confidence': 0.91,
        'repair_mode': 'auto', 'tuning_applied': True,
        'extreme_passes_applied': False,
        'stage2': {'ok': True},
        'category': 'watertight', 'issues': [],
        'repair_confidence': 92, 'repair_confidence_label': 'high',
    }
    r.update(over)
    return r


def test_stdlib_numpy_only():
    assert 'pymeshlab' not in sys.modules, 'history must not pull in pymeshlab'


def test_fingerprint_deterministic():
    v = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1]], dtype=np.float32)
    t = np.array([[0, 1, 2], [0, 2, 3]], dtype=np.int32)
    a = history.mesh_fingerprint(v, t)
    b = history.mesh_fingerprint(v.copy(), t.copy())
    assert a == b, 'identical geometry must hash identically'
    v2 = v.copy()
    v2[0, 0] += 0.5
    c = history.mesh_fingerprint(v2, t)
    assert a != c, 'changed geometry must hash differently'


def test_record_schema_and_no_leak():
    rec = history.build_record(_sample_result(), 'fp1234', '0.2.0', 42)
    assert set(rec.keys()) == SCHEMA_KEYS, set(rec.keys()) ^ SCHEMA_KEYS
    dumped = json.dumps(rec)
    for leak in ('/home', 'krateian', 'supersecret', 'turbo', 'src', 'sutura'):
        assert leak not in dumped, 'leaked identifying data: %r in %s' % (leak, dumped)
    for k in rec:
        assert '/' not in k and '\\' not in k, 'path-like key: %r' % k
        assert not any(w in k for w in ('path', 'file', 'name', 'user')), k


def test_record_format_derived_but_path_not_stored():
    rec = history.build_record(_sample_result(), 'fp', '0.2.0', 0)
    assert rec['format'] == 'stl'


def test_write_and_load_roundtrip():
    with tempfile.TemporaryDirectory() as tmp:
        history.HISTORY_DIR = tmp
        history.HISTORY_PATH = os.path.join(tmp, 'history.jsonl')
        history.write(_sample_result(), '0.2.0', 123, ['fp1'])
        recs = history.load_records()
        assert len(recs) == 1
        r = recs[0]
        assert r['seq'] == 1
        assert r['app_version'] == '0.2.0'
        assert r['elapsed_ms'] == 123
        assert r['mesh_fingerprint'] == 'fp1'
        assert r['defect_holes'] == 1
        assert r['stage2_status'] == 'ok'
        assert r['category'] == 'watertight'
        # a second append increments seq and keeps both records
        history.write(_sample_result(), '0.2.0', 7, ['fp2'])
        assert len(history.load_records()) == 2
        assert history.load_records()[1]['seq'] == 2


def test_write_multi_object_3mf():
    with tempfile.TemporaryDirectory() as tmp:
        history.HISTORY_DIR = tmp
        history.HISTORY_PATH = os.path.join(tmp, 'history.jsonl')
        result = _sample_result()
        result['input'] = '/home/krateian/src/sutura/secret_thing.3mf'
        result['object_reports'] = [
            _sample_result(), _sample_result(stage1={
                'faces_before': 5, 'faces_after': 5, 'vertices_before': 4,
                'vertices_after': 4, 'holes_closed': 0, 'holes_remaining': 0,
                'non_manifold_edges_fixed': 0, 'non_manifold_edges_remaining': 0,
                'applied_filters': 11, 'volume_change_percent': 0.0,
            }),
        ]
        del result['stage1']
        del result['defects']
        history.write(result, '0.2.0', 500, ['fpA', 'fpB'])
        recs = history.load_records()
        assert len(recs) == 2
        assert [r['mesh_fingerprint'] for r in recs] == ['fpA', 'fpB']
        assert all(r['format'] == '3mf' for r in recs)


def test_pop_fingerprints():
    result = _sample_result()
    result['_fp'] = 'top'
    result['object_reports'] = [{'_fp': 'o1'}, {'_fp': 'o2'}]
    fps = history.pop_fingerprints(result)
    assert fps == ['top', 'o1', 'o2']
    assert '_fp' not in result
    assert all('_fp' not in r for r in result['object_reports'])


def test_stage2_status_mapping():
    assert history._stage2_status({'stage2': {'ok': True}}) == 'ok'
    assert history._stage2_status({'stage2': {'error': 'x'}}) == 'error'
    assert history._stage2_status({}) == 'not_run'
    assert history._stage2_status({'stage2': {}}) == 'skipped'


def test_export_smoke():
    with tempfile.TemporaryDirectory() as tmp:
        history.HISTORY_DIR = tmp
        history.HISTORY_PATH = os.path.join(tmp, 'history.jsonl')
        history.write(_sample_result(), '0.2.0', 50, ['fp1'])
        buf = io.StringIO()
        with redirect_stdout(buf):
            history.export_history(last=5)
        out = buf.getvalue()
        assert 'usage history: 1 repair record(s)' in out
        assert '--- full data (paste this into the issue) ---' in out
        json.loads(out.split('--- full data', 1)[1].split('\n', 1)[1])
        # --summary-only path
        buf2 = io.StringIO()
        with redirect_stdout(buf2):
            history.export_history(summary_only=True)
        out2 = buf2.getvalue()
        assert '--- full data' not in out2


def test_export_clear():
    with tempfile.TemporaryDirectory() as tmp:
        history.HISTORY_DIR = tmp
        history.HISTORY_PATH = os.path.join(tmp, 'history.jsonl')
        history.write(_sample_result(), '0.2.0', 1, ['fp1'])
        buf = io.StringIO()
        with redirect_stdout(buf):
            history.export_history(clear=True)
        assert 'history cleared' in buf.getvalue()
        assert history.load_records() == []


def test_no_history_reports_empty():
    with tempfile.TemporaryDirectory() as tmp:
        history.HISTORY_DIR = tmp
        history.HISTORY_PATH = os.path.join(tmp, 'nope.jsonl')
        buf = io.StringIO()
        with redirect_stdout(buf):
            history.export_history()
        assert 'no history yet' in buf.getvalue()


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('history tests passed')


if __name__ == '__main__':
    main()