#!/usr/bin/env python3
"""Repair-budget regression tests for sutura/repair.py.

Checks that:
  1. budget_check() reuses the existing geometry-change / risk metrics,
     returns None with no budget set (no behaviour change), and correctly
     flags geometry-change / risk overruns, including the worst-object rule
     for multi-object 3MF reports.
  2. On a non-TTY run (stdin is not a terminal) an exceeded budget DECLINES
     the save: no output file, exit 1, and the explicit top-level
     'status': 'budget_declined' field (distinct from a generic error).
  3. --force saves anyway (exit 0, file written, budget block still present).
  4. Within budget: saved normally and the numbers are reported
     (budget block with within_budget True).
  5. Invalid budget values are rejected.
  6. --human shows the Budget line / the declined ERROR.

Uses the repo's own repair.py under the sutura venv (pymeshlab).
Usage: ~/.local/share/sutura/venv/bin/python tests/test_budget.py
       (macOS: /opt/homebrew/.../envs/sutura-env/bin/python tests/test_budget.py)
"""
import json
import os
import subprocess
import sys
import tempfile
import zipfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402
import repair  # noqa: E402

REPAIR_PY = os.path.join(SUTURA, 'repair.py')


def _run(args):
    # stdin=DEVNULL: the child must never see a TTY, so an exceeded budget
    # deterministically declines instead of blocking on a prompt.
    r = subprocess.run([sys.executable, REPAIR_PY] + args,
                       capture_output=True, text=True, timeout=600,
                       stdin=subprocess.DEVNULL)
    return r


def _json(r):
    return json.loads(r.stdout.strip().splitlines()[-1])


# --- budget_check unit tests ------------------------------------------------

def _single_report(repair_risk=15, **s1):
    base = {
        'stage1': {'volume_change_percent': 3.0,
                   'surface_area_change_percent': -1.5,
                   'faces_before': 1000, 'faces_after': 1020,
                   'vertices_before': 600, 'vertices_after': 610,
                   'components_before': 1, 'components': 1},
        'repair_risk': repair_risk,
    }
    base['stage1'].update(s1)
    return base


def test_no_budget_returns_none():
    r = repair.budget_check(_single_report())
    assert r is None, r
    r = repair.budget_check(_single_report(), None, None)
    assert r is None, r


def test_within_budget():
    r = repair.budget_check(_single_report(), 10, 30)
    assert r is not None
    assert r['within_budget'] is True
    assert r['geometry_change_pct'] == 3.0  # max(|3|, |-1.5|)
    assert r['risk_score'] == 15
    assert r['exceeded'] == [], r


def test_geometry_change_uses_worst_of_volume_surface():
    r = repair.budget_check(_single_report(volume_change_percent=2.0,
                                           surface_area_change_percent=-8.0),
                            5, 30)
    assert r['within_budget'] is False
    assert r['geometry_change_pct'] == 8.0
    assert r['exceeded'][0]['metric'] == 'geometry_change'
    assert r['exceeded'][0]['value'] == 8.0


def test_risk_overrun():
    r = repair.budget_check(_single_report(), 100, 10)
    assert r['within_budget'] is False
    assert r['exceeded'][0]['metric'] == 'risk'
    assert r['exceeded'][0]['value'] == 15


def test_geometry_only_budget():
    r = repair.budget_check(_single_report(), 1, None)
    assert r['within_budget'] is False
    assert len(r['exceeded']) == 1
    assert r['exceeded'][0]['metric'] == 'geometry_change'


def test_risk_only_budget():
    r = repair.budget_check(_single_report(), None, 5)
    assert r['within_budget'] is False
    assert r['exceeded'][0]['metric'] == 'risk'


def test_missing_metrics_no_false_positive():
    # volume/surface are 0.0 on a no-change repair -> within
    r = repair.budget_check(_single_report(repair_risk=0,
                                           volume_change_percent=0.0,
                                           surface_area_change_percent=0.0),
                            1, 1)
    assert r['within_budget'] is True, r
    # missing risk entirely -> geometry still checked, risk just absent
    rep = _single_report(repair_risk=None)
    r = repair.budget_check(rep, 1, 1)
    assert r['risk_score'] is None, r
    assert r['within_budget'] is False, r


def test_multi_object_worst_drives():
    rep = {'object_reports': [
        {'stage1': {'volume_change_percent': 2.0, 'surface_area_change_percent': 1.0},
         'repair_risk': 10},
        {'stage1': {'volume_change_percent': -30.0, 'surface_area_change_percent': -5.0},
         'repair_risk': 55},
        {'stage1': {'volume_change_percent': 0.0, 'surface_area_change_percent': 0.0},
         'repair_risk': 3},
    ]}
    r = repair.budget_check(rep, 25, 40)
    assert r['geometry_change_pct'] == 30.0, r   # worst object
    assert r['risk_score'] == 55, r
    assert r['within_budget'] is False
    assert len(r['exceeded']) == 2, r


def test_multi_object_within():
    rep = {'object_reports': [
        {'stage1': {'volume_change_percent': 2.0, 'surface_area_change_percent': 1.0},
         'repair_risk': 10},
        {'stage1': {'volume_change_percent': -3.0, 'surface_area_change_percent': -5.0},
         'repair_risk': 20},
    ]}
    r = repair.budget_check(rep, 25, 40)
    assert r['within_budget'] is True, r


# --- CLI-level tests -------------------------------------------------------

def _make_broken_cube(path, size=40):
    import trimesh
    m = trimesh.creation.box(extents=[size, size, size])
    m.update_faces(np.arange(len(m.faces) - 1))
    with open(path, 'wb') as f:
        f.write(b'sutura-budget'.ljust(80, b'\0'))
        f.write(len(m.faces).to_bytes(4, 'little'))
        for face in m.faces:
            f.write(b'\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00')
            for idx in face:
                f.write(np.asarray(m.vertices[idx], dtype=np.float32).tobytes())
            f.write(b'\x00\x00')
    assert os.path.getsize(path) == 84 + len(m.faces) * 50


def _fixed(path):
    return path[:-4] + '_fixed.stl'


def _test_path(tmp, name):
    """Unique input path per test (each test shares one tmp dir)."""
    return os.path.join(tmp, name + '.stl')


def test_declined_non_tty(tmp):
    path = _test_path(tmp, 'declined')
    _make_broken_cube(path)
    r = _run([path, '--max-geometry-change', '0.001'])
    assert r.returncode == 1, r.stdout
    d = _json(r)
    assert d.get('status') == 'budget_declined', d
    assert d.get('category') == 'error', d
    assert 'budget_exceeded' in d.get('issues', []), d
    assert d.get('budget', {}).get('within_budget') is False, d
    assert not os.path.exists(_fixed(path)), \
        'a declined save must not write the output file'


def test_force_saves_anyway(tmp):
    path = _test_path(tmp, 'force')
    _make_broken_cube(path)
    r = _run([path, '--max-geometry-change', '0.001', '--force'])
    assert r.returncode == 0, (r.stdout, r.stderr)
    d = _json(r)
    assert d.get('status') != 'budget_declined', d
    assert d.get('category') == 'watertight', d
    b = d.get('budget', {})
    assert b.get('within_budget') is False, b   # forced, but still reported
    assert os.path.exists(_fixed(path))


def test_risk_budget_declined(tmp):
    path = _test_path(tmp, 'risk')
    _make_broken_cube(path)
    r = _run([path, '--max-risk', '1'])
    assert r.returncode == 1, r.stdout
    d = _json(r)
    assert d.get('status') == 'budget_declined', d
    assert d.get('budget', {}).get('exceeded', [{}])[0].get('metric') == 'risk', d
    assert not os.path.exists(_fixed(path))


def test_within_budget_saves_and_reports(tmp):
    path = _test_path(tmp, 'within')
    _make_broken_cube(path)
    r = _run([path, '--max-geometry-change', '50', '--max-risk', '50'])
    assert r.returncode == 0, (r.stdout, r.stderr)
    d = _json(r)
    assert d.get('status') != 'budget_declined', d
    b = d.get('budget', {})
    assert b.get('within_budget') is True, b
    assert b.get('geometry_change_pct') is not None, b
    assert b.get('risk_score') is not None, b
    assert os.path.exists(_fixed(path))


def test_no_budget_no_budget_key(tmp):
    path = _test_path(tmp, 'nobudget')
    _make_broken_cube(path)
    r = _run([path])
    assert r.returncode == 0, r.stdout
    d = _json(r)
    assert 'budget' not in d, d      # default behaviour unchanged
    assert os.path.exists(_fixed(path))


def test_human_budget_lines(tmp):
    path = _test_path(tmp, 'human')
    _make_broken_cube(path)
    r = _run([path, '--max-geometry-change', '50', '--human'])
    assert r.returncode == 0, r.stdout
    assert 'Budget:' in r.stdout and 'within budget' in r.stdout, r.stdout
    r2 = _run([path, '--max-geometry-change', '0.001', '--human'])
    assert r2.returncode == 1, r2.stdout
    assert 'ERROR: Save declined' in r2.stdout, r2.stdout
    assert 'EXCEEDED' in r2.stdout, r2.stdout


def test_invalid_budget_values_rejected(tmp):
    path = _test_path(tmp, 'invalid')
    _make_broken_cube(path)
    for args in (['--max-geometry-change', '-1'],
                 ['--max-risk', '200'],
                 ['--max-risk', '-5']):
        r = _run([path] + args)
        assert r.returncode == 1, (args, r.stdout)
        d = _json(r)
        assert 'error' in d, (args, d)


# --- multi-object 3MF budget case -----------------------------------------

_3MF_TMPL = ('<?xml version="1.0"?>'
             '<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">'
             '<resources><object id="%d" type="model"><mesh><vertices>%s</vertices>'
             '<triangles>%s</triangles></mesh></object></resources></model>')


def _make_broken_2obj_3mf(path, size=40):
    """Two-object 3MF (separate .model files, slicer-style): a broken cube
    (missing top face) + a clean cube. The broken cube's repair changes
    geometry, so a tiny geometry budget trips."""
    def _vt(open_face=False):
        s = size / 2.0
        v = [(-s, -s, -s), (s, -s, -s), (s, s, -s), (-s, s, -s),
             (-s, -s, s), (s, -s, s), (s, s, s), (-s, s, s)]
        t = [(0, 2, 1), (0, 3, 2), (0, 1, 5), (0, 5, 4), (1, 2, 6), (1, 6, 5),
             (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7)]
        if not open_face:
            t += [(4, 5, 6), (4, 6, 7)]
        return v, t

    v1, t1 = _vt(open_face=True)
    v2, t2 = _vt(open_face=False)
    with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED) as z:
        for oid, (v, t) in enumerate(((v1, t1), (v2, t2)), start=1):
            vs = ''.join('<vertex x="%.7g" y="%.7g" z="%.7g"/>' % p for p in v)
            ts = ''.join('<triangle v1="%d" v2="%d" v3="%d"/>' % tr for tr in t)
            z.writestr('3D/Objects/o%d.model' % oid,
                       _3MF_TMPL % (oid, vs, ts))


def test_multi_object_3mf_budget_declined(tmp):
    """Multi-object 3MF: the worst object drives the budget; exceeded -> the
    whole file's save is declined (no output) with status budget_declined."""
    path = os.path.join(tmp, 'multi_budget.3mf')
    _make_broken_2obj_3mf(path)
    r = _run([path, '--max-geometry-change', '0.001'])
    assert r.returncode == 1, (r.stdout, r.stderr)
    d = _json(r)
    assert d.get('status') == 'budget_declined', d
    assert d.get('category') == 'error', d
    b = d.get('budget', {})
    assert b.get('within_budget') is False, b
    assert b.get('geometry_change_pct') is not None, b
    assert not os.path.exists(path[:-4] + '_fixed.3mf'), \
        'a declined 3MF save must not write the output file'
    # --force saves the multi-object file
    r2 = _run([path, '--max-geometry-change', '0.001', '--force'])
    assert r2.returncode == 0, (r2.stdout, r2.stderr)
    d2 = _json(r2)
    assert d2.get('status') != 'budget_declined', d2
    assert d2.get('budget', {}).get('within_budget') is False, d2
    assert os.path.exists(path[:-4] + '_fixed.3mf')


def main():
    with tempfile.TemporaryDirectory(prefix='sutura-budget-') as tmp:
        failed = 0
        for name, fn in sorted(globals().items()):
            if name.startswith('test_') and callable(fn):
                try:
                    if fn.__code__.co_argcount == 1:
                        fn(tmp)
                    else:
                        fn()
                    print('ok  %s' % name)
                except AssertionError as e:
                    failed += 1
                    print('FAIL %s: %s' % (name, e))
        if failed:
            print('%d/%d budget tests failed' % (failed, len([
                n for n in globals() if n.startswith('test_')])))
            return 1
    print('budget tests passed')
    return 0


if __name__ == '__main__':
    sys.exit(main())