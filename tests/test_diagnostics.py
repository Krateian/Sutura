#!/usr/bin/env python3
"""Tests for sutura/diagnostics.py (sutura --diagnostics out.zip)."""
import json
import os
import sys
import tempfile
import zipfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import diagnostics


def test_sanitize_value():
    home = '/Users/testuser'
    user = 'testuser'
    raw = {
        'recent_files': ['/Users/testuser/Documents/model.stl', '/tmp/other.obj'],
        'author': 'testuser',
        'count': 42,
        'nested': {'path': '/Users/testuser/.config/sutura'},
    }
    cleaned = diagnostics.sanitize_value(raw, home, user)
    assert cleaned['recent_files'][0] == '~/Documents/model.stl'
    assert cleaned['recent_files'][1] == '/tmp/other.obj'
    assert cleaned['author'] == '<user>'
    assert cleaned['count'] == 42
    assert cleaned['nested']['path'] == '~/.config/sutura'


def test_collect_system_info():
    info = diagnostics.collect_system_info()
    assert 'sutura_version' in info
    assert 'platform' in info
    assert 'python_version' in info
    assert 'sutura_geom' in info
    geom = info['sutura_geom']
    assert isinstance(geom['symbols'], list)


def test_collect_methods_availability():
    methods = diagnostics.collect_methods_availability()
    assert len(methods) == 18
    ids = {m['id'] for m in methods}
    assert 'quick_clean' in ids
    assert 'tamponade' in ids
    assert 'graft' in ids


def test_create_diagnostics_zip():
    with tempfile.TemporaryDirectory() as tmp:
        zip_path = os.path.join(tmp, 'test_diag.zip')
        res = diagnostics.create_diagnostics_zip(zip_path)
        assert res['ok'] is True
        assert os.path.isfile(zip_path)
        assert os.path.getsize(zip_path) > 0

        with zipfile.ZipFile(zip_path, 'r') as zf:
            names = set(zf.namelist())
            assert 'README.txt' in names
            assert 'system_info.json' in names
            assert 'methods_availability.json' in names
            assert 'config_sanitized.json' in names
            assert 'last_repair_history.json' in names

            sys_info = json.loads(zf.read('system_info.json'))
            assert sys_info['sutura_version'] == res['version']

            avail = json.loads(zf.read('methods_availability.json'))
            assert len(avail) == 18


def test_run_diagnostics_cli():
    with tempfile.TemporaryDirectory() as tmp:
        zip_path = os.path.join(tmp, 'cli_diag.zip')
        code = diagnostics.run_diagnostics_cli(zip_path)
        assert code == 0
        assert os.path.isfile(zip_path)


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('diagnostics tests passed')


if __name__ == '__main__':
    main()
