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


def _run(args, env=None):
    r = subprocess.run([sys.executable, REPAIR_PY] + args,
                       capture_output=True, text=True, timeout=600, env=env)
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


# --- Triage Engine intensity (--intensity) --------------------------------

INTENSITIES = ('quick', 'balanced', 'thorough', 'extreme')


def _clean_env(tmp):
    """A run environment with no ambient SUTURA_INTENSITY and a private HOME
    (no config.json), so the intensity resolves to the explicit flag only."""
    env = dict(os.environ, HOME=tmp)
    env.pop('SUTURA_INTENSITY', None)
    return env


def test_each_intensity_reports_itself(tmp):
    path = os.path.join(tmp, 'sphere.stl')
    _sphere_stl(path)
    env = _clean_env(tmp)
    for intensity in INTENSITIES:
        r = _run(['--intensity', intensity, path], env=env)
        d = _json(r)
        assert r.returncode == 0, (intensity, r.stderr)
        assert d.get('triage_intensity') == intensity, (intensity, d)


def test_default_intensity_is_balanced(tmp):
    path = os.path.join(tmp, 'sphere.stl')
    _sphere_stl(path)
    d = _json(_run([path], env=_clean_env(tmp)))
    assert d.get('triage_intensity') == 'balanced', d.get('triage_intensity')
    # balanced is byte-identical to passing the flag explicitly
    explicit = _json(_run(['--intensity', 'balanced', path], env=_clean_env(tmp)))
    for key in ('triage_intensity', 'repair_mode', 'detected_type',
                'detected_confidence', 'tuning_applied', 'category'):
        assert d.get(key) == explicit.get(key), (key, d.get(key), explicit.get(key))


def test_intensity_env_and_cli_precedence(tmp):
    path = os.path.join(tmp, 'sphere.stl')
    _sphere_stl(path)
    env = _clean_env(tmp)
    env['SUTURA_INTENSITY'] = 'thorough'
    assert _json(_run([path], env=env)).get('triage_intensity') == 'thorough'
    # the CLI flag wins over the env var
    assert _json(_run(['--intensity', 'quick', path], env=env)
                 ).get('triage_intensity') == 'quick'


def test_invalid_intensity_rejected(tmp):
    path = os.path.join(tmp, 'sphere.stl')
    _sphere_stl(path)
    r = _run(['--intensity', 'bogus', path], env=_clean_env(tmp))
    assert r.returncode != 0, r.stdout
    assert 'invalid choice' in r.stderr, r.stderr


def test_human_report_shows_intensity(tmp):
    path = os.path.join(tmp, 'sphere.stl')
    _sphere_stl(path)
    r = _run(['--intensity', 'thorough', '--human', path], env=_clean_env(tmp))
    assert r.returncode == 0, r.stderr
    assert 'Intensity: thorough' in r.stdout, r.stdout


def _tiny_cube_stl(path):
    """A cube with a whole face removed - 10 triangles (below extreme's
    mincomponentsize=20), so extreme mode deletes the entire mesh as debris
    while every other mode repairs it normally."""
    import numpy as np
    v = np.array([
        [1, 0, 0], [1, 0, 1], [1, 1, 0], [1, 1, 1],
        [0, 0, 0], [0, 0, 1], [0, 1, 0], [0, 1, 1],
    ], dtype=np.float32)
    quads = [
        (0, 1, 3, 2), (4, 6, 7, 5), (0, 2, 6, 4),
        (1, 5, 7, 3), (0, 4, 5, 1), (2, 3, 7, 6),
    ]
    tris = []
    for a, b, c, d in quads:
        tris += [(a, b, c), (a, c, d)]
    tris = [t for i, t in enumerate(tris) if i not in (0, 1)]  # remove the +X face
    _write_stl(path, v, tris)
    return len(tris)


def test_extreme_removed_object_is_distinct_from_malformed(tmp):
    path = os.path.join(tmp, 'tiny.stl')
    _tiny_cube_stl(path)
    # extreme: mincomponentsize=20 deletes the 10-face mesh -> the dedicated
    # 'extreme_removed_object' issue, NOT the generic malformed/error.
    r = _run(['--mode', 'extreme', path])
    d = _json(r)
    assert r.returncode == 1, r.stdout
    assert d.get('category') == 'error', d
    assert 'extreme_removed_object' in d.get('issues', []), d
    assert 'Extreme mode removed all geometry' in d.get('error', ''), d
    assert 'malformed' not in d.get('issues', []), d
    # --human shows the same clear message
    rh = _run(['--mode', 'extreme', '--human', path])
    assert rh.returncode == 1, rh.stdout
    assert 'Extreme mode removed all geometry' in rh.stdout, rh.stdout
    # a less aggressive mode repairs the same mesh normally (no error)
    for mode in ('low', 'medium', 'auto', 'aggressive'):
        rm = _run(['--mode', mode, path])
        dm = _json(rm)
        assert rm.returncode == 0, (mode, rm.stderr)
        assert 'error' not in dm, (mode, dm.get('error'))


def main():
    with tempfile.TemporaryDirectory(prefix='sutura-mode-') as tmp:
        for name, fn in sorted(globals().items()):
            if name.startswith('test_') and callable(fn):
                fn(tmp)
                print('ok  %s' % name)
    print('repair_mode tests passed')


if __name__ == '__main__':
    main()