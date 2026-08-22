#!/usr/bin/env python3
"""Repair-mode (--mode) regression tests for sutura/repair.py.

Checks that:
  1. Every fixed mode (low/medium/aggressive/extreme) produces a valid report
     carrying its own repair_mode, and that the default (no --mode) is 'auto'.
  2. --mode auto is byte-identical to running without --mode: the same
     detected_type / detected_confidence / tuning_applied / category.
  3. An invalid --mode value is rejected by argparse (non-zero exit).

Uses the repo's own repair.py under the venv (pymeshlab), not the installed
CLI, so this tracks the code under test.
Usage: ~/.local/share/sutura/venv/bin/python tests/test_repair_mode.py
"""
import json
import os
import struct
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPAIR_PY = os.path.join(REPO, 'sutura', 'repair.py')

MODES = ('low', 'medium', 'auto', 'aggressive', 'extreme')


def _write_stl(path, verts, tris):
    with open(path, 'wb') as f:
        f.write(b'repair-mode'.ljust(80, b'\0'))
        f.write(struct.pack('<I', len(tris)))
        for a, b, c in tris:
            f.write(struct.pack('<3f', 0, 0, 0))
            f.write(struct.pack('<3f', *verts[a]))
            f.write(struct.pack('<3f', *verts[b]))
            f.write(struct.pack('<3f', *verts[c]))
            f.write(struct.pack('<H', 0))


def _sphere_stl(path):
    """A smooth icosphere with a patch of faces removed (a genuine hole) -
    large enough that even 'extreme' (mincomponentsize=20) does not delete the
    whole object as debris, and clean enough that repair yields a typed
    result rather than a degenerate mess."""
    import numpy as np
    import trimesh
    m = trimesh.creation.icosphere(subdivisions=2)
    verts = np.asarray(m.vertices, dtype=np.float32)
    tris = np.asarray(m.faces, dtype=np.int64)
    drop = np.array([verts[t, 1].mean() > 0.85 for t in tris])
    _write_stl(path, verts, tris[~drop])
    return int((~drop).sum())


def _run(args):
    r = subprocess.run([sys.executable, REPAIR_PY] + args,
                       capture_output=True, text=True, timeout=600)
    return r


def _json(r):
    return json.loads(r.stdout.strip().splitlines()[-1])


def test_each_mode_reports_repair_mode(tmp):
    path = os.path.join(tmp, 'sphere.stl')
    _sphere_stl(path)
    for mode in MODES:
        r = _run(['--mode', mode, path])
        d = _json(r)
        assert r.returncode == 0, (mode, r.stderr)
        assert 'error' not in d, (mode, d.get('error'))
        assert d.get('repair_mode') == mode, (mode, d.get('repair_mode'))


def test_default_mode_is_auto(tmp):
    path = os.path.join(tmp, 'sphere.stl')
    _sphere_stl(path)
    d = _json(_run([path]))
    assert d.get('repair_mode') == 'auto', d.get('repair_mode')


def test_auto_identical_to_no_flag(tmp):
    path = os.path.join(tmp, 'sphere.stl')
    _sphere_stl(path)
    default = _json(_run([path]))
    auto = _json(_run(['--mode', 'auto', path]))
    for key in ('repair_mode', 'detected_type', 'detected_confidence',
                'tuning_applied', 'category'):
        assert default.get(key) == auto.get(key), (key, default.get(key), auto.get(key))
    # the auto path still runs the classifier + gate (Aşama 3): the type is
    # one of the three and tuning_applied is a plain bool
    assert auto.get('detected_type') in ('mechanical', 'organic', 'unknown'), auto
    assert isinstance(auto.get('tuning_applied'), bool), auto


def test_invalid_mode_rejected(tmp):
    path = os.path.join(tmp, 'sphere.stl')
    _sphere_stl(path)
    r = _run(['--mode', 'bogus', path])
    assert r.returncode != 0, r.stdout
    assert 'invalid choice' in r.stderr, r.stderr


def test_human_report_shows_mode(tmp):
    path = os.path.join(tmp, 'sphere.stl')
    _sphere_stl(path)
    r = _run(['--mode', 'aggressive', '--human', path])
    assert r.returncode == 0, r.stderr
    assert 'Mode  : aggressive' in r.stdout, r.stdout


def main():
    with tempfile.TemporaryDirectory(prefix='sutura-mode-') as tmp:
        for name, fn in sorted(globals().items()):
            if name.startswith('test_') and callable(fn):
                fn(tmp)
                print('ok  %s' % name)
    print('repair_mode tests passed')


if __name__ == '__main__':
    main()