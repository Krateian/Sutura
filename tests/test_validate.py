#!/usr/bin/env python3
"""validate + --dry-run regression tests for sutura/repair.py.

Checks that:
  1. `validate` analyzes clean/broken/multi-object-3MF meshes without
     repairing them, reports the documented JSON fields and exits 0.
  2. `validate` NEVER modifies the input file (bytes unchanged).
  3. `--dry-run` reports the would-do plan (mode/thresholds/tuning, found
     holes/debris, stage 2 availability) and writes NO output file at all
     (no _fixed file, no extra files in the directory).
  4. Malformed input still returns a JSON error and exits 1.

Uses the repo's own repair.py under the venv (pymeshlab), not the installed
CLI, so this tracks the code under test.
Usage: ~/.local/share/sutura/venv/bin/python tests/test_validate.py
"""
import hashlib
import json
import os
import struct
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPAIR_PY = os.path.join(REPO, 'sutura', 'repair.py')


def _run(args):
    r = subprocess.run([sys.executable, REPAIR_PY] + args,
                       capture_output=True, text=True, timeout=600)
    return r


def _json(r):
    return json.loads(r.stdout.strip().splitlines()[-1])


def _sha(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        h.update(f.read())
    return h.hexdigest()


def _make_broken(path):
    subprocess.run([sys.executable,
                    os.path.join(REPO, 'tests', 'make_broken_stl.py'), path],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def _make_sphere(path):
    import numpy as np
    import trimesh
    m = trimesh.creation.icosphere(subdivisions=2)
    verts = np.asarray(m.vertices, dtype=np.float32)
    tris = np.asarray(m.faces, dtype=np.int64)
    with open(path, 'wb') as f:
        f.write(b'sutura-validate'.ljust(80, b'\0'))
        f.write(struct.pack('<I', len(tris)))
        for a, b, c in tris:
            f.write(struct.pack('<3f', 0, 0, 0))
            f.write(struct.pack('<3f', *verts[a]))
            f.write(struct.pack('<3f', *verts[b]))
            f.write(struct.pack('<3f', *verts[c]))
            f.write(struct.pack('<H', 0))


def _make_multi_3mf(path):
    subprocess.run([sys.executable,
                    os.path.join(REPO, 'tests', 'make_layered_multiobject_3mf.py'),
                    path], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def test_validate_clean_sphere(tmp):
    path = os.path.join(tmp, 'sphere.stl')
    _make_sphere(path)
    r = _run(['validate', path])
    assert r.returncode == 0, r.stderr
    d = _json(r)
    assert 'error' not in d, d.get('error')
    assert d.get('input') == path, d
    v = d.get('validation', {})
    assert v.get('watertight') is True, v
    assert v.get('holes') == [], v
    assert v.get('non_manifold') == [], v
    assert v.get('self_intersections') == 0, v
    assert v.get('signed_volume') > 0, v  # consistent outward winding
    assert v.get('orientation') == 'consistent', v
    assert d.get('detected_type') in ('mechanical', 'organic', 'unknown'), d
    assert isinstance(d.get('detected_confidence'), (int, float)), d


def test_validate_broken_mesh(tmp):
    path = os.path.join(tmp, 'broken.stl')
    _make_broken(path)
    r = _run(['validate', path])
    assert r.returncode == 0, r.stderr  # analysis of a broken mesh is a success
    d = _json(r)
    v = d.get('validation', {})
    assert v.get('watertight') is False, v
    assert len(v.get('holes', [])) >= 1, v
    assert v.get('faces', 0) > 0, v
    assert 'self_intersections' in v, v
    assert 'connected_components' in v, v
    assert 'surface_area' in v, v


def test_validate_multi_object_3mf(tmp):
    path = os.path.join(tmp, 'multi.3mf')
    _make_multi_3mf(path)
    r = _run(['validate', path])
    assert r.returncode == 0, r.stderr
    d = _json(r)
    assert 'error' not in d, d.get('error')
    assert d.get('objects') == 2, d
    assert len(d.get('object_reports', [])) == 2, d
    for rep in d['object_reports']:
        assert 'validation' in rep, rep
        assert 'model' in rep, rep


def test_validate_does_not_modify_input(tmp):
    path = os.path.join(tmp, 'broken.stl')
    _make_broken(path)
    before = _sha(path)
    r = _run(['validate', path])
    assert r.returncode == 0, r.stderr
    assert _sha(path) == before, 'validate must not modify the input file'
    assert not os.path.exists(os.path.join(tmp, 'broken_fixed.stl')), \
        'validate must not produce a _fixed file'


def test_dry_run_writes_nothing(tmp):
    path = os.path.join(tmp, 'broken.stl')
    _make_broken(path)
    listing_before = sorted(os.listdir(tmp))
    r = _run(['--dry-run', path])
    assert r.returncode == 0, r.stderr
    assert sorted(os.listdir(tmp)) == listing_before, \
        '--dry-run must not create any file'
    assert not os.path.exists(os.path.join(tmp, 'broken_fixed.stl')), \
        '--dry-run must not produce a _fixed file'


def test_dry_run_reports_plan(tmp):
    path = os.path.join(tmp, 'broken.stl')
    _make_broken(path)
    r = _run(['--dry-run', path])
    assert r.returncode == 0, r.stderr
    d = _json(r)
    assert 'error' not in d, d.get('error')
    assert d.get('repair_mode') == 'auto', d
    assert d.get('detected_type') in ('mechanical', 'organic', 'unknown'), d
    assert isinstance(d.get('tuning_applied'), bool), d
    pa = d.get('would_apply', {})
    assert 'mincomponentsize' in pa and 'maxholesize' in pa, d
    assert d.get('holes_found', 0) >= 1, d
    assert 'debris_faces_removable' in d, d
    assert isinstance(d.get('stage2_bridge_available'), bool), d
    assert not os.path.exists(os.path.join(tmp, 'broken_fixed.stl'))


def test_malformed_input_exit_1(tmp):
    path = os.path.join(tmp, 'garbage.stl')
    with open(path, 'wb') as f:
        f.write(os.urandom(200))
    for args in (['validate', path], ['--dry-run', path]):
        r = _run(args)
        assert r.returncode == 1, (args, r.stdout, r.stderr)
        d = _json(r)
        assert 'error' in d, (args, d)


def main():
    with tempfile.TemporaryDirectory(prefix='sutura-validate-') as tmp:
        for name, fn in sorted(globals().items()):
            if name.startswith('test_') and callable(fn):
                fn(tmp)
                print('ok  %s' % name)
    print('validate tests passed')


if __name__ == '__main__':
    main()