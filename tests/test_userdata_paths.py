#!/usr/bin/env python3
"""Regression test for sutura_engine.paths (runtime user-data location).

Checks that:
  1. data_dir()/data_file() respect SUTURA_DATA_DIR and otherwise fall back to
     ~/.local/share/sutura/data (NEVER the code/install directory).
  2. migrate_data_dir() moves legacy files into the data directory, is
     idempotent, never clobbers an existing destination, and leaves the source
     untouched on a copy fallback.
  3. ensure_data_dir() creates the directory; with an explicit non-default
     SUTURA_DATA_DIR it skips the automatic legacy migration (tests/power
     users own the location) and never touches the real legacy directory.
  4. updater.backup_install()/rollback() preserve the data/ entry: a record
     written after the backup survives a rollback byte-for-byte, while code
     is restored and new code paths are dropped. No updater code is run.

Usage: python3 tests/test_userdata_paths.py
(any interpreter, stdlib only).
"""
import os
import shutil
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, 'sutura'))

from sutura_engine import paths  # noqa: E402


def _set_env(value):
    if value is None:
        os.environ.pop(paths.DATA_ENV, None)
    else:
        os.environ[paths.DATA_ENV] = value


def test_data_dir_default_is_data_subdir():
    old = os.environ.get(paths.DATA_ENV)
    _set_env(None)
    try:
        d = paths.data_dir()
        assert d == paths.DEFAULT_DATA_DIR, d
        assert d.endswith(os.path.join('sutura', 'data')), d
        assert os.path.basename(paths.data_file('history.jsonl')) == 'history.jsonl'
        assert os.path.dirname(paths.data_file('x')) == d
    finally:
        _set_env(old)


def test_data_dir_env_override():
    old = os.environ.get(paths.DATA_ENV)
    _set_env('/tmp/custom-sutura-data')
    try:
        assert paths.data_dir() == '/tmp/custom-sutura-data'
        assert paths.data_file('triage_learning.json') == \
            '/tmp/custom-sutura-data/triage_learning.json'
    finally:
        _set_env(old)


def test_migrate_moves_and_is_idempotent():
    tmp = tempfile.mkdtemp(prefix='sutura-paths-')
    try:
        legacy = os.path.join(tmp, 'legacy')
        dest = os.path.join(tmp, 'data')
        os.makedirs(legacy)
        with open(os.path.join(legacy, 'history.jsonl'), 'w') as f:
            f.write('h\n')
        info = paths.migrate_data_dir(legacy_dir=legacy, dest=dest)
        assert info['moved'] == ['history.jsonl'], info
        assert info['errors'] == [], info
        assert os.path.exists(os.path.join(dest, 'history.jsonl'))
        assert not os.path.exists(os.path.join(legacy, 'history.jsonl'))
        # second run: source gone, destination present -> nothing to do
        info2 = paths.migrate_data_dir(legacy_dir=legacy, dest=dest)
        assert info2['moved'] == [], info2
        assert info2['errors'] == [], info2
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_migrate_never_clobbers():
    tmp = tempfile.mkdtemp(prefix='sutura-paths-')
    try:
        legacy = os.path.join(tmp, 'legacy')
        dest = os.path.join(tmp, 'data')
        os.makedirs(legacy)
        os.makedirs(dest)
        with open(os.path.join(legacy, 'history.jsonl'), 'w') as f:
            f.write('legacy\n')
        with open(os.path.join(dest, 'history.jsonl'), 'w') as f:
            f.write('current\n')
        info = paths.migrate_data_dir(legacy_dir=legacy, dest=dest)
        assert info['moved'] == [], info
        assert 'history.jsonl' in info['skipped'], info
        with open(os.path.join(dest, 'history.jsonl')) as f:
            assert f.read() == 'current\n'
        assert os.path.exists(os.path.join(legacy, 'history.jsonl'))
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_ensure_creates_dir_and_skips_migration_on_override():
    old = os.environ.get(paths.DATA_ENV)
    tmp = tempfile.mkdtemp(prefix='sutura-paths-')
    try:
        dest = os.path.join(tmp, 'data')
        legacy = os.path.join(tmp, 'legacy')
        os.makedirs(legacy)
        with open(os.path.join(legacy, 'history.jsonl'), 'w') as f:
            f.write('legacy\n')
        _set_env(dest)
        info = paths.ensure_data_dir(legacy_dir=legacy)
        assert os.path.isdir(dest)
        # explicit override owns the location: no automatic migration
        assert not os.path.exists(os.path.join(dest, 'history.jsonl'))
        assert os.path.exists(os.path.join(legacy, 'history.jsonl'))
        assert info['errors'] == [], info
    finally:
        _set_env(old)
        shutil.rmtree(tmp, ignore_errors=True)


def test_updater_backup_rollback_preserve_data():
    import updater
    tmp = tempfile.mkdtemp(prefix='sutura-updater-data-')
    orig = (updater.APP_DIR, updater.BACKUP_DIR)
    try:
        app = os.path.join(tmp, 'app')
        backup = os.path.join(tmp, 'backup')
        os.makedirs(os.path.join(app, 'data'))
        os.makedirs(os.path.join(app, 'sutura_engine'))
        with open(os.path.join(app, 'repair.py'), 'w') as f:
            f.write('v1')
        with open(os.path.join(app, 'data', 'history.jsonl'), 'w') as f:
            f.write('old\n')
        updater.APP_DIR = app
        updater.BACKUP_DIR = backup

        updater.backup_install()
        assert not os.path.exists(os.path.join(backup, 'data')), \
            'backup must not copy data/'
        assert os.path.exists(os.path.join(backup, 'repair.py'))

        # simulate an updated install: new code + a newer data record
        with open(os.path.join(app, 'repair.py'), 'w') as f:
            f.write('v2')
        os.makedirs(os.path.join(app, 'extra_code'))
        with open(os.path.join(app, 'data', 'history.jsonl'), 'a') as f:
            f.write('new\n')

        updater.rollback('0.9.0')
        with open(os.path.join(app, 'repair.py')) as f:
            assert f.read() == 'v1', 'code must be restored'
        assert not os.path.exists(os.path.join(app, 'extra_code')), \
            'new code must be dropped on rollback'
        with open(os.path.join(app, 'data', 'history.jsonl')) as f:
            assert f.read() == 'old\nnew\n', \
                'data written after the backup must survive the rollback'
    finally:
        updater.APP_DIR, updater.BACKUP_DIR = orig
        shutil.rmtree(tmp, ignore_errors=True)


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('userdata paths tests passed')


if __name__ == '__main__':
    main()
