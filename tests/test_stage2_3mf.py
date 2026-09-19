#!/usr/bin/env python3
"""Per-object Stage 2 regression for multi-object 3MF files.

Checks that:
  1. Every object in a multi-object 3MF gets its own per-object report, and
     an object that stage 1 closes gets a per-object stage 2 report (the
     manifold3d watertight rebuild).
  2. When stage 2 can run in this environment (bridge + manifold3d), every
     closed object reports `stage2.ok` and the aggregates
     `objects_watertight` / `objects_stage2_ok` agree.
  3. When stage 2 cannot run, closed objects still carry the honest
     per-object "Stage 2 skipped" report — the per-object mechanics are
     exercised either way.
  4. Byte-identical objects each get their own per-object report (the
     geometry cache reuses the repair but still reports per object).
  5. The rebuilt mesh round-trips (output is a valid zip).

Uses the repo's own repair.py under the sutura venv (pymeshlab + manifold3d).
Usage: ~/.local/share/sutura/venv/bin/python tests/test_stage2_3mf.py
       (macOS: /opt/homebrew/.../envs/sutura-env/bin/python tests/test_stage2_3mf.py)
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

import repair  # noqa: E402

REPAIR_PY = os.path.join(SUTURA, 'repair.py')
MAKE_3MF = os.path.join(REPO, 'tests', 'make_layered_multiobject_3mf.py')


def _run(args):
    r = subprocess.run([sys.executable, REPAIR_PY] + args,
                       capture_output=True, text=True, timeout=600)
    return r


def _json(r):
    return json.loads(r.stdout.strip().splitlines()[-1])


def _make_layered(path):
    subprocess.run([sys.executable, MAKE_3MF, path],
                   check=True, stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL)


# --- inline 2-object 3MF (two byte-identical closed cubes) ----------------

def _box_vt():
    v = [(-1, -1, -1), (1, -1, -1), (1, 1, -1), (-1, 1, -1),
         (-1, -1, 1), (1, -1, 1), (1, 1, 1), (-1, 1, 1)]
    t = [(0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7), (0, 1, 5), (0, 5, 4),
         (1, 2, 6), (1, 6, 5), (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7)]
    return v, t


def _make_identical_2obj(path):
    """Two byte-identical closed cubes as two SEPARATE .model files (like a
    slicer's per-object layout) so the multi-object path is taken."""
    v, t = _box_vt()
    vs = ''.join('<vertex x="%.7g" y="%.7g" z="%.7g"/>' % p for p in v)
    ts = ''.join('<triangle v1="%d" v2="%d" v3="%d"/>' % tr for tr in t)
    model = ('<?xml version="1.0"?>'
             '<model unit="millimeter" '
             'xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">'
             '<resources><object id="%d" type="model"><mesh><vertices>%s</vertices>'
             '<triangles>%s</triangles></mesh></object></resources></model>')
    with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED) as z:
        z.writestr('3D/Objects/o1.model', model % (1, vs, ts))
        z.writestr('3D/Objects/o2.model', model % (3, vs, ts))


# --- tests -----------------------------------------------------------------

def test_layered_objects_each_get_stage2(tmp):
    """After the layered fix, both layered objects + the clean cube close;
    each gets a per-object stage2 report and the aggregates agree."""
    path = os.path.join(tmp, 'layered.3mf')
    _make_layered(path)
    r = _run([path])
    assert r.returncode == 0, (r.stdout, r.stderr)
    d = _json(r)
    assert d.get('category') in ('watertight', 'warning'), d
    assert len(d.get('object_reports', [])) == 3, d
    assert d.get('objects_watertight') is not None, d
    assert d.get('objects_stage2_ok') is not None, d
    assert d.get('objects_watertight') == d.get('objects_stage2_ok'), d
    for rep in d['object_reports']:
        assert 'stage2' in rep, rep  # per-object stage2 report key exists
    # every closed object reports a stage2 outcome (ok or an honest skip)
    for i, rep in enumerate(d['object_reports']):
        s1 = rep.get('stage1', {})
        s2 = rep.get('stage2') or {}
        assert s1.get('two_manifold'), (i, s1)
        assert s1.get('holes_remaining', 0) == 0, (i, s1)
        assert 'error' in s2 or 'ok' in s2, (i, s2)
    # when stage 2 can actually run, every object is confirmed watertight
    if any((r.get('stage2') or {}).get('ok') for r in d['object_reports']):
        assert all((r.get('stage2') or {}).get('ok')
                   for r in d['object_reports']), d['object_reports']
        assert d['objects_watertight'] == 3, d
        assert d['category'] == 'watertight', d
    else:
        # stage 2 unavailable -> closed objects carry the honest skip
        for rep in d['object_reports']:
            s2 = rep.get('stage2') or {}
            assert str(s2.get('error', '')).startswith('Stage 2 skipped'), rep
    fixed = path[:-4] + '_fixed.3mf'
    assert os.path.exists(fixed)
    assert zipfile.ZipFile(fixed).testzip() is None


def test_identical_objects_each_reported_with_stage2(tmp):
    """Byte-identical objects: the geometry cache reuses the repair, but each
    object still gets its own per-object report including stage2."""
    path = os.path.join(tmp, 'identical.3mf')
    _make_identical_2obj(path)
    r = _run([path])
    assert r.returncode == 0, (r.stdout, r.stderr)
    d = _json(r)
    reports = d.get('object_reports', [])
    assert len(reports) == 2, d
    assert d.get('objects_watertight') == 2, d
    for rep in reports:
        assert rep.get('stage1', {}).get('two_manifold'), rep
        s2 = rep.get('stage2') or {}
        assert 'error' in s2 or 'ok' in s2, rep
    if any((r.get('stage2') or {}).get('ok') for r in reports):
        assert all((r.get('stage2') or {}).get('ok') for r in reports), reports
    assert os.path.exists(path[:-4] + '_fixed.3mf')


def main():
    with tempfile.TemporaryDirectory(prefix='sutura-stage2-3mf-') as tmp:
        failed = 0
        for name, fn in sorted(globals().items()):
            if name.startswith('test_') and callable(fn):
                try:
                    fn(tmp)
                    print('ok  %s' % name)
                except AssertionError as e:
                    failed += 1
                    print('FAIL %s: %s' % (name, e))
        if failed:
            return 1
    print('stage2-3mf tests passed')
    return 0


if __name__ == '__main__':
    sys.exit(main())