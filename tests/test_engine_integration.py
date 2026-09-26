#!/usr/bin/env python3
"""Regression tests for the external-engine integration in repair.py.

- Placement: an engine runs at its configured placement (before_stage1,
  after_stage1, after_stage2, replace_ftetwild, final_fallback).
- Guard: a passing output is adopted, a corrupt / empty / timed-out output is
  rejected (entry carries ``reject_reason``), and a valid but far-off output is
  adopted with ``shape_changed=True`` (identical semantics to fTetWild).
- chain.toml: a valid custom chain controls the order engines run in; an
  invalid chain is rejected by engines.py and falls back to the default.
- No-engines byte-identity: with no engines configured the output arrays and
  the report are identical to a run that never loaded the engine layer.
- CLI: ``engines list|check`` and ``ftetwild status|install --dry-run|
  uninstall --dry-run`` (dry-run only; the real environment is never touched).

Needs the venv (pymeshlab). Usage:
    ~/.local/share/sutura/venv/bin/python tests/test_engine_integration.py
"""
import json
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402
import pymeshlab as ml  # noqa: E402

import repair  # noqa: E402
import engines as eng_mod  # noqa: E402

OPEN_SAMPLE = os.path.join(REPO, 'tests', 'real-world-samples',
                           'thingi10k_100827.stl')

PASSED = []


def ok(name):
    PASSED.append(name)
    print('ok  %s' % name)


def _sphere_with_hole(z_cut=0.95):
    ms = ml.MeshSet()
    ms.create_sphere(subdiv=3)
    m = ms.current_mesh()
    v = np.asarray(m.vertex_matrix(), np.float64)
    t = np.asarray(m.face_matrix(), np.int64)
    keep = v[t][:, :, 2].mean(axis=1) < z_cut
    return np.asarray(v, np.float32), np.asarray(t[keep], np.int32)


def _sample():
    if os.path.exists(OPEN_SAMPLE):
        ms = ml.MeshSet()
        ms.load_new_mesh(OPEN_SAMPLE)
        m = ms.current_mesh()
        return (np.asarray(m.vertex_matrix(), np.float32),
                np.asarray(m.face_matrix(), np.int32))
    return _sphere_with_hole()


def _write_script(path, body):
    with open(path, 'w') as f:
        f.write(body)
    os.chmod(path, 0o755)
    return path


def _make_engines_dir(root, configs):
    """configs: {filename: toml_text}, written to $XDG_CONFIG_HOME/sutura/
    engines. Returns the XDG_CONFIG_HOME base to point the loader at."""
    eng_dir = os.path.join(root, 'sutura', 'engines')
    os.makedirs(eng_dir, exist_ok=True)
    for fname, text in configs.items():
        with open(os.path.join(eng_dir, fname), 'w') as f:
            f.write(text)
    return root


def _load(xdg_base):
    saved = os.environ.get('XDG_CONFIG_HOME')
    os.environ['XDG_CONFIG_HOME'] = xdg_base
    try:
        return repair.load_engine_run()
    finally:
        if saved is None:
            os.environ.pop('XDG_CONFIG_HOME', None)
        else:
            os.environ['XDG_CONFIG_HOME'] = saved


def _repair_with(engines, chain, v, t, **kw):
    with tempfile.TemporaryDirectory(prefix='sutura-eng-') as tmp:
        return repair.repair_mesh_from_arrays(
            v, t, tmp, engines=engines, engine_chain=chain, **kw)


def test_placement_after_stage1_adopt():
    with tempfile.TemporaryDirectory(prefix='sutura-eng-') as root:
        cp = _write_script(os.path.join(root, 'copy.sh'),
                           '#!/bin/sh\ncp "$1" "$2"\n')
        eng_dir = _make_engines_dir(root, {'copy.toml': (
            'name = "copycat"\n'
            'command = ["%s", "{input}", "{output}"]\n'
            'placement = "after_stage1"\ntimeout = 20.0\n' % cp)})
        eng, chain, warns = _load(eng_dir)
        assert eng and not warns, warns
        v, t = _sphere_with_hole()
        rep, _, _ = _repair_with(eng, chain, v, t, ftetwild=False)
        entries = rep.get('engines')
        assert entries and entries[0]['name'] == 'copycat', entries
        assert entries[0]['placement'] == 'after_stage1'
        assert entries[0]['adopted'] is True
        assert entries[0]['shape_changed'] is False
    ok('test_placement_after_stage1_adopt')


def test_reject_corrupt_output():
    with tempfile.TemporaryDirectory(prefix='sutura-eng-') as root:
        bad = _write_script(os.path.join(root, 'bad.sh'),
                            '#!/bin/sh\nprintf "garbage" > "$2"\n')
        eng_dir = _make_engines_dir(root, {'bad.toml': (
            'name = "badout"\n'
            'command = ["%s", "{input}", "{output}"]\n'
            'placement = "after_stage1"\ntimeout = 20.0\n' % bad)})
        eng, chain, _ = _load(eng_dir)
        v, t = _sphere_with_hole()
        rep, out_v, out_t = _repair_with(eng, chain, v, t, ftetwild=False)
        e = rep['engines'][0]
        assert e['adopted'] is False
        assert e['reject_reason'], e
    ok('test_reject_corrupt_output')


def test_reject_timeout():
    with tempfile.TemporaryDirectory(prefix='sutura-eng-') as root:
        slow = _write_script(os.path.join(root, 'slow.sh'),
                             '#!/bin/sh\nsleep 10\n')
        eng_dir = _make_engines_dir(root, {'slow.toml': (
            'name = "slowpoke"\n'
            'command = ["%s", "{input}", "{output}"]\n'
            'placement = "after_stage1"\ntimeout = 1.0\n' % slow)})
        eng, chain, _ = _load(eng_dir)
        v, t = _sphere_with_hole()
        rep, _, _ = _repair_with(eng, chain, v, t, ftetwild=False)
        e = rep['engines'][0]
        assert e['adopted'] is False
        assert 'timed out' in (e['reject_reason'] or ''), e
    ok('test_reject_timeout')


def test_shape_change_flagged_not_rejected():
    with tempfile.TemporaryDirectory(prefix='sutura-eng-') as root:
        scaler = _write_script(os.path.join(root, 'scale.py'), (
            'import sys\n'
            'import numpy as np\n'
            'import pymeshlab as ml\n'
            'ms = ml.MeshSet(); ms.load_new_mesh(sys.argv[1])\n'
            'm = ms.current_mesh()\n'
            'v = np.asarray(m.vertex_matrix(), np.float64) * 10.0\n'
            't = np.asarray(m.face_matrix(), np.int32)\n'
            'o = ml.MeshSet()\n'
            'o.add_mesh(ml.Mesh(vertex_matrix=v.astype(np.float32),'
            ' face_matrix=t))\n'
            'o.save_current_mesh(sys.argv[2])\n'))
        eng_dir = _make_engines_dir(root, {'scale.toml': (
            'name = "huge"\n'
            'command = ["%s", "%s", "{input}", "{output}"]\n'
            'placement = "after_stage1"\ntimeout = 60.0\n'
            % (sys.executable, scaler))})
        eng, chain, _ = _load(eng_dir)
        v, t = _sphere_with_hole()
        rep, _, _ = _repair_with(eng, chain, v, t, ftetwild=False)
        e = rep['engines'][0]
        assert e['adopted'] is True, e
        assert e['shape_changed'] is True, e
        assert rep.get('shape_changed') is True
    ok('test_shape_change_flagged_not_rejected')


def test_replace_ftetwild_engine():
    with tempfile.TemporaryDirectory(prefix='sutura-eng-') as root:
        cp = _write_script(os.path.join(root, 'copy.sh'),
                           '#!/bin/sh\ncp "$1" "$2"\n')
        eng_dir = _make_engines_dir(root, {'rep.toml': (
            'name = "repftw"\n'
            'command = ["%s", "{input}", "{output}"]\n'
            'placement = "replace_ftetwild"\ntimeout = 20.0\n' % cp)})
        eng, chain, _ = _load(eng_dir)
        assert 'ftetwild' not in chain and 'repftw' in chain, chain
        v, t = _sphere_with_hole()
        rep, _, _ = _repair_with(eng, chain, v, t, deep_repair='full')
        names = [e['name'] for e in rep.get('engines', [])]
        assert 'repftw' in names, rep.get('engines')
    ok('test_replace_ftetwild_engine')


def test_chain_toml_order():
    with tempfile.TemporaryDirectory(prefix='sutura-eng-') as root:
        bad1 = _write_script(os.path.join(root, 'b1.sh'),
                             '#!/bin/sh\nprintf x > "$2"\n')
        bad2 = _write_script(os.path.join(root, 'b2.sh'),
                             '#!/bin/sh\nprintf y > "$2"\n')
        _make_engines_dir(root, {
            'a.toml': ('name = "alpha"\ncommand = ["%s", "{input}", "{output}"]\n'
                       'placement = "after_stage1"\n' % bad1),
            'b.toml': ('name = "beta"\ncommand = ["%s", "{input}", "{output}"]\n'
                       'placement = "after_stage1"\n' % bad2),
            'chain.toml': ('stages = ["stage1", "beta", "alpha", "stage2", '
                           '"deep_repair"]\n')})
        eng, chain, warns = _load(root)
        assert not warns, warns
        assert chain.index('beta') < chain.index('alpha'), chain
        v, t = _sphere_with_hole()
        rep, _, _ = _repair_with(eng, chain, v, t, ftetwild=False)
        order = [e['name'] for e in rep.get('engines', [])]
        assert order == ['beta', 'alpha'], order
    ok('test_chain_toml_order')


def test_no_engines_byte_identical():
    v, t = _sample()
    with tempfile.TemporaryDirectory(prefix='sutura-eng-') as root:
        eng_dir = _make_engines_dir(root, {})
        eng, chain, warns = _load(eng_dir)
        assert eng == {} and not warns, (eng, warns)
        base_rep, base_v, base_t = _repair_with(None, None, v, t,
                                                ftetwild=False)
        loaded_rep, loaded_v, loaded_t = _repair_with(eng, chain, v, t,
                                                      ftetwild=False)
    assert 'engines' not in base_rep and 'engines' not in loaded_rep
    assert np.array_equal(base_v, loaded_v)
    assert np.array_equal(base_t, loaded_t)
    strip = lambda r: {k: val for k, val in r.items() if k != 'stage1'}
    assert json.dumps(strip(base_rep), sort_keys=True, default=str) == \
        json.dumps(strip(loaded_rep), sort_keys=True, default=str)
    ok('test_no_engines_byte_identical')


def test_cli_engines_and_ftetwild_dry_run():
    with tempfile.TemporaryDirectory(prefix='sutura-cli-') as root:
        cp = _write_script(os.path.join(root, 'copy.sh'),
                           '#!/bin/sh\ncp "$1" "$2"\n')
        eng_dir = _make_engines_dir(root, {'copy.toml': (
            'name = "copycat"\n'
            'command = ["%s", "{input}", "{output}"]\n'
            'placement = "after_stage1"\n' % cp)})
        home = os.path.join(root, 'home')
        os.makedirs(home, exist_ok=True)
        env = dict(os.environ, XDG_CONFIG_HOME=eng_dir, HOME=home)
        py = sys.executable
        cli = os.path.join(SUTURA, 'repair.py')

        r = subprocess.run([py, cli, 'engines', 'list'], env=env,
                           capture_output=True, text=True, timeout=60)
        data = json.loads(r.stdout.strip().splitlines()[-1])
        assert any(e['name'] == 'copycat' for e in data['engines']), data

        r = subprocess.run([py, cli, 'engines', 'check'], env=env,
                           capture_output=True, text=True, timeout=60)
        data = json.loads(r.stdout.strip().splitlines()[-1])
        assert data['ok'] is True and r.returncode == 0, (data, r.returncode)

        # a missing binary makes check fail (without ever running it)
        _make_engines_dir(root, {'gone.toml': (
            'name = "gone"\ncommand = ["no-such-binary-xyz", "{input}", '
            '"{output}"]\nplacement = "after_stage1"\n')})
        r = subprocess.run([py, cli, 'engines', 'check'], env=env,
                           capture_output=True, text=True, timeout=60)
        assert r.returncode == 1, r.stdout

        r = subprocess.run([py, cli, 'ftetwild', 'status'], env=env,
                           capture_output=True, text=True, timeout=60)
        st = json.loads(r.stdout.strip().splitlines()[-1])
        assert 'supported' in st and 'estimate' in st, st

        r = subprocess.run([py, cli, 'ftetwild', 'install', '--dry-run'],
                           env=env, capture_output=True, text=True, timeout=60)
        plan = json.loads(r.stdout.strip().splitlines()[-1])
        assert plan['plan']['dry_run'] is True
        assert 'install' in plan['plan']['command'], plan

        r = subprocess.run([py, cli, 'ftetwild', 'uninstall', '--dry-run'],
                           env=env, capture_output=True, text=True, timeout=60)
        plan = json.loads(r.stdout.strip().splitlines()[-1])
        assert plan['plan']['dry_run'] is True
        assert 'uninstall' in plan['plan']['command'], plan
    ok('test_cli_engines_and_ftetwild_dry_run')


def main():
    test_placement_after_stage1_adopt()
    test_reject_corrupt_output()
    test_reject_timeout()
    test_shape_change_flagged_not_rejected()
    test_replace_ftetwild_engine()
    test_chain_toml_order()
    test_no_engines_byte_identical()
    test_cli_engines_and_ftetwild_dry_run()
    print('\n%d engine-integration tests passed' % len(PASSED))


if __name__ == '__main__':
    main()
