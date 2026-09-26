#!/usr/bin/env python3
"""Triage user-profile regression tests.

Covers the named-profile layer on top of the intensity presets:

  1. storage round-trip and the atomic profiles.json write;
  2. a corrupt / wrong-schema / missing profiles file never crashes the
     resolver and falls back to the built-in presets;
  3. name rules (empty / reserved / case-insensitive duplicate) and the
     rename/duplicate helpers;
  4. resolution precedence CLI > SUTURA_INTENSITY > config > balanced when
     the sources name user profiles, and the Hausdorff floor clamp;
  5. the CLI ``--list-intensities`` and ``--intensity <profile>`` report;
  6. the offscreen GUI create / rename / duplicate / delete flow with a
     temporary HOME (run in a subprocess).

Usage: ~/.local/share/sutura/venv/bin/python tests/test_triage_profiles.py
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

import triage  # noqa: E402


def test_storage_round_trip_is_atomic():
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, 'nested', 'profiles.json')
        profiles = {'myfork': {
            'base': 'thorough',
            'overrides': {'ftetwild_timeout': 42.0, 'ftetwild_max_faces': None},
        }}
        triage.save_profiles(profiles, path)
        assert os.path.exists(path)
        assert triage.load_profiles(path) == profiles
        leftovers = [f for f in os.listdir(os.path.dirname(path))
                     if f.endswith('.tmp')]
        assert not leftovers, leftovers


def test_corrupt_file_never_crashes():
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, 'profiles.json')
        missing = os.path.join(d, 'none.json')
        cases = [
            '{ this is not json',
            json.dumps({'schema_version': 99, 'profiles': {'x': {}}}),
            json.dumps([1, 2, 3]),
            json.dumps({'schema_version': 1, 'profiles': 'nope'}),
        ]
        for blob in cases:
            with open(path, 'w') as f:
                f.write(blob)
            assert triage.load_profiles(path) == {}
            spec = triage.resolve_intensity(
                None, environ={}, config_path=missing, profiles_path=path)
            assert spec.name == 'balanced'
        # a missing file is simply empty
        assert triage.load_profiles(missing) == {}


def test_name_rules_and_helpers():
    profiles = {'MyFork': {'base': 'balanced', 'overrides': {}}}
    assert triage.name_error('balanced', profiles) == 'reserved'
    assert triage.name_error('BALANCED', profiles) == 'reserved'
    assert triage.name_error('myfork', profiles) == 'duplicate'
    assert triage.name_error('a new one', profiles) is None
    assert triage.name_error('   ', profiles) == 'empty'
    # rename respects the same rules and is case-insensitive
    assert triage.rename_profile(profiles, 'MyFork', 'balanced') == 'reserved'
    assert triage.rename_profile(profiles, 'MyFork', 'Other') is None
    assert 'Other' in profiles and 'MyFork' not in profiles
    # duplicate picks a free name
    new = triage.duplicate_profile(profiles, 'Other')
    assert new == 'Other copy' and new in profiles
    assert triage.unique_profile_name(profiles, 'Other') == 'Other 2'


def test_resolution_precedence_with_profiles():
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, 'profiles.json')
        cfg = os.path.join(d, 'config.json')
        triage.save_profiles(
            {'p1': {'base': 'thorough',
                    'overrides': {'ftetwild_timeout': 11.0}}}, path)
        # CLI profile wins over the env and the config
        spec = triage.resolve_intensity(
            'p1', environ={'SUTURA_INTENSITY': 'quick'},
            config_path=cfg, profiles_path=path)
        assert spec.name == 'p1' and spec.ftetwild_timeout == 11.0
        assert spec.base == 'thorough' and spec.overrides == {
            'ftetwild_timeout': 11.0}
        # the config may name a profile
        with open(cfg, 'w') as f:
            json.dump({'intensity': 'p1'}, f)
        assert triage.resolve_intensity(
            None, environ={}, config_path=cfg, profiles_path=path).name == 'p1'
        # a preset from the env beats a profile from the config
        assert triage.resolve_intensity(
            None, environ={'SUTURA_INTENSITY': 'quick'}, config_path=cfg,
            profiles_path=path).name == 'quick'
        # unknown everywhere -> balanced
        with open(cfg, 'w') as f:
            json.dump({'intensity': 'bogus'}, f)
        assert triage.resolve_intensity(
            None, environ={'SUTURA_INTENSITY': 'bogus'}, config_path=cfg,
            profiles_path=path).name == 'balanced'


def test_hausdorff_floor_enforced_in_resolver():
    low = {'base': 'balanced',
           'overrides': {'ftetwild_hausdorff_samples': 1000}}
    spec = triage.resolve_intensity(
        'low', environ={}, config_path='/nonexistent',
        user_profiles={'low': low})
    assert spec.ftetwild_hausdorff_samples == triage.HAUSDORFF_FLOOR
    # the applied override is reported clamped, not raw
    assert spec.overrides['ftetwild_hausdorff_samples'] == \
        triage.HAUSDORFF_FLOOR


def test_report_fields_and_listing():
    profiles = {'p': {'base': 'extreme',
                      'overrides': {'deep_repair': 'local'}}}
    spec = triage.resolve_intensity('p', environ={}, config_path='/nonexistent',
                                    user_profiles=profiles)
    fields = triage.triage_report_fields(spec)
    assert fields == {'triage_intensity': 'p',
                      'triage_profile_base': 'extreme',
                      'triage_overrides': {'deep_repair': 'local'}}
    # a built-in reports only the intensity
    assert triage.triage_report_fields(triage.PRESETS['balanced']) == {
        'triage_intensity': 'balanced'}
    rows = triage.list_intensities(profiles=profiles)
    kinds = [(r['name'], r['kind']) for r in rows]
    assert kinds[:4] == [('quick', 'preset'), ('balanced', 'preset'),
                         ('thorough', 'preset'), ('extreme', 'preset')]
    assert kinds[-1] == ('p', 'profile')
    text = triage.format_intensities(profiles=profiles)
    assert 'p' in text and 'base=extreme' in text and 'quick' in text


def test_cli_list_and_profile_round_trip():
    with tempfile.TemporaryDirectory() as home:
        cfg_dir = os.path.join(home, '.config', 'sutura')
        os.makedirs(cfg_dir)
        with open(os.path.join(cfg_dir, 'profiles.json'), 'w') as f:
            json.dump({'schema_version': 1, 'profiles': {'myfork': {
                'base': 'thorough',
                'overrides': {'ftetwild_timeout': 42.0,
                              'deep_repair': 'local'}}}}, f)
        env = dict(os.environ, HOME=home)
        repair = os.path.join(SUTURA, 'repair.py')

        listed = subprocess.run([sys.executable, repair, '--list-intensities'],
                                capture_output=True, text=True, env=env,
                                timeout=120)
        assert listed.returncode == 0, listed.stderr[-500:]
        assert 'myfork' in listed.stdout and 'base=thorough' in listed.stdout

        mesh = os.path.join(home, 'broken.stl')
        subprocess.run([sys.executable,
                        os.path.join(REPO, 'tests', 'make_broken_stl.py'),
                        mesh], capture_output=True, env=env, timeout=120)

        bad = subprocess.run([sys.executable, repair, '--intensity', 'nope',
                              mesh], capture_output=True, text=True, env=env,
                             timeout=120)
        assert bad.returncode != 0

        out = os.path.join(home, 'fixed.stl')
        run = subprocess.run(
            [sys.executable, repair, '--intensity', 'myfork',
             '--no-fallback-ftetwild', '--deep-repair', 'off',
             '-o', out, mesh],
            capture_output=True, text=True, env=env, timeout=300)
        report = json.loads(run.stdout.strip().splitlines()[-1])
        assert report['triage_intensity'] == 'myfork', report
        assert report['triage_profile_base'] == 'thorough', report
        assert report['triage_overrides']['ftetwild_timeout'] == 42.0, report


def test_gui_create_rename_delete_temp_home():
    code = (
        "import sys\n"
        "sys.path.insert(0, %r)\n"
        "from PySide6.QtWidgets import QApplication\n"
        "import gui, os, json\n"
        "app = QApplication([])\n"
        "w = gui.MainWindow()\n"
        "d = w._options_dialog\n"
        "assert w.intensity_combo.count() == 4, w.intensity_combo.count()\n"
        "d._ask_profile_name = lambda *a, **k: 'myfork'\n"
        "w._profile_new()\n"
        "assert 'myfork' in w._profiles\n"
        "path = os.path.expanduser('~/.config/sutura/profiles.json')\n"
        "assert os.path.exists(path), path\n"
        "data = json.load(open(path))\n"
        "assert data['schema_version'] == 1\n"
        "assert 'myfork' in data['profiles']\n"
        "assert w.intensity_combo.findData('myfork') >= 0\n"
        "d._ask_profile_name = lambda *a, **k: 'renamed'\n"
        "w._profile_rename()\n"
        "assert 'renamed' in w._profiles and 'myfork' not in w._profiles\n"
        "w._profile_duplicate()\n"
        "assert len(w._profiles) == 2, w._profiles\n"
        "d._confirm_delete = lambda n: True\n"
        "before = len(w._profiles)\n"
        "w._profile_delete()\n"
        "assert len(w._profiles) == before - 1\n"
        "assert json.load(open(path))['profiles'] == w._profiles\n"
        "# editing a built-in preset saves as a new profile with only the\n"
        "# changed fields as overrides\n"
        "w.intensity_combo.setCurrentIndex(\n"
        "    w.intensity_combo.findData('balanced'))\n"
        "w.profile_editor.spin_timeout.setValue(500.0)\n"
        "d._ask_profile_name = lambda *a, **k: 'edited'\n"
        "w._profile_save()\n"
        "assert w._profiles['edited'] == {'base': 'balanced', "
        "'overrides': {'ftetwild_timeout': 500.0}}, w._profiles['edited']\n"
        "w.close()\n"
        "print('GUI-OK')\n") % SUTURA
    with tempfile.TemporaryDirectory() as home:
        env = dict(os.environ, HOME=home, QT_QPA_PLATFORM='offscreen')
        r = subprocess.run([sys.executable, '-c', code], capture_output=True,
                           text=True, env=env, timeout=180)
    assert r.returncode == 0, (r.returncode, r.stderr[-1000:])
    assert 'GUI-OK' in r.stdout, r.stdout[-500:]


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('triage profile tests passed')


if __name__ == '__main__':
    main()
