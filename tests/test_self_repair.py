#!/usr/bin/env python3
"""Offline unit tests for sutura/self_repair.py (sutura --self-repair).

Everything is offline: network is the only thing mocked
(``urllib.request.urlopen``).  The manifest is exercised through the real
``fetch_manifest`` code path (not ``_manifest_override``) wherever the fetch
itself is under test.

Usage: <venv>/bin/python tests/test_self_repair.py
"""
import hashlib
import json
import os
import re
import sys
import tempfile
import urllib.error
from types import SimpleNamespace
from unittest.mock import patch

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import manifest as manifest_mod  # noqa: E402
import self_repair  # noqa: E402


class MockHTTPResponse:
    def __init__(self, data: bytes):
        self._data = data
        self._pos = 0

    def read(self, n: int = -1):
        if n is None or n < 0:
            chunk = self._data[self._pos:]
            self._pos = len(self._data)
        else:
            chunk = self._data[self._pos:self._pos + n]
            self._pos += len(chunk)
        return chunk

    def __enter__(self):
        return self

    def __exit__(self, *args):
        pass


def _serve(mapping):
    """Return (urlopen_side_effect, seen_urls). mapping: list of
    (predicate(url)->bool, bytes)."""
    seen = []

    def _open(req, timeout=None):
        url = getattr(req, 'full_url', str(req))
        seen.append(url)
        for pred, data in mapping:
            if pred(url):
                return MockHTTPResponse(data)
        raise urllib.error.URLError('not served: ' + url)

    return _open, seen


def _healthy_geom():
    return patch.object(self_repair, 'check_extension_status',
                        return_value=(True, [], False, None))


# --- gates / version -------------------------------------------------------

def test_is_git_repository_protection():
    assert self_repair.is_git_repository(REPO) is True
    code = self_repair.run_self_repair(app_dir=REPO, _allow_git=False)
    assert code == 1


def test_version_mismatch_refused():
    with tempfile.TemporaryDirectory() as tmp:
        bad_manifest = {'version': '9.9.9', 'files': {}}
        code = self_repair.run_self_repair(
            app_dir=tmp, _manifest_override=bad_manifest, _allow_git=True)
        assert code == 1


def test_invalid_installed_version_refused():
    with tempfile.TemporaryDirectory() as tmp:
        with patch.object(self_repair, 'get_installed_version', return_value='v1.2'):
            code = self_repair.run_self_repair(app_dir=tmp, _allow_git=True)
        assert code == 1


# --- manifest validation (H1) ---------------------------------------------

def test_validate_manifest_unit():
    app = '/home/u/.local/share/sutura'
    good = {'version': '1.0.0', 'files': {
        'repair.py': 'x', 'sutura_engine/a.py': 'y', 'LICENSE': 'z', 'open.sh': 'w'}}
    assert manifest_mod.validate_manifest(good, app) == []
    for bad_key in ('../evil.py', '/etc/passwd', 'a/../../b.py', 'a\\b.py',
                    'C:/x.py', '', 'a//b.py'):
        m = {'version': '1.0.0', 'files': {bad_key: 'x'}}
        assert manifest_mod.validate_manifest(m, app), bad_key
    too_many = {'files': {'f%d.py' % i: 'x' for i in range(1001)}}
    assert any('too many' in r for _k, r in manifest_mod.validate_manifest(too_many, app))


def test_traversal_and_absolute_keys_refuse_before_io():
    with tempfile.TemporaryDirectory() as tmp:
        for key in ('../evil.py', '/tmp/evil_absolute.py'):
            mock_man = {'version': self_repair.get_installed_version(),
                        'files': {key: hashlib.sha256(b'x').hexdigest()}}
            with _healthy_geom():
                code = self_repair.run_self_repair(
                    app_dir=tmp, _manifest_override=mock_man, _allow_git=True)
            assert code == 1, key
        # nothing was written next to the install
        assert not os.path.exists(os.path.join(os.path.dirname(tmp), 'evil.py'))


# --- fetch path (M2) -------------------------------------------------------

def test_fetch_manifest_uses_pinned_version_url():
    ver = self_repair.get_installed_version()
    body = json.dumps({'version': ver, 'files': {}}).encode()
    opener, seen = _serve([(lambda u: u.endswith('manifest.json'), body)])
    with patch('urllib.request.urlopen', side_effect=opener):
        man = self_repair.fetch_manifest(ver)
    assert man['version'] == ver
    assert seen[0] == ('https://github.com/Krateian/Sutura/releases/download/'
                       'v%s/manifest.json' % ver)


def test_fetch_manifest_rejects_invalid_version():
    try:
        self_repair.fetch_manifest('not-a-version')
    except self_repair.SelfRepairError:
        return
    raise AssertionError('invalid version must be refused')


def test_full_flow_fetches_manifest_and_repairs():
    ver = self_repair.get_installed_version()
    with tempfile.TemporaryDirectory() as tmp:
        with open(os.path.join(tmp, 'file1.py'), 'wb') as f:
            f.write(b'corrupted\n')
        good = b'print("clean")\n'
        h = hashlib.sha256(good).hexdigest()
        body = json.dumps({'version': ver, 'files': {'file1.py': h}}).encode()
        opener, seen = _serve([
            (lambda u: u.endswith('manifest.json'), body),
            (lambda u: u.endswith('/sutura/file1.py'), good),
        ])
        with patch('urllib.request.urlopen', side_effect=opener), _healthy_geom():
            code = self_repair.run_self_repair(
                app_dir=tmp, force_yes=True, _allow_git=True)
        assert code == 0
        with open(os.path.join(tmp, 'file1.py'), 'rb') as f:
            assert f.read() == good
        assert any(u.endswith('manifest.json') for u in seen)


# --- symlinks (H2) ---------------------------------------------------------

def test_symlink_target_refused():
    with tempfile.TemporaryDirectory() as tmp:
        outside = os.path.join(tmp, 'outside.txt')
        with open(outside, 'wb') as f:
            f.write(b'user owned\n')
        app = os.path.join(tmp, 'app')
        os.makedirs(app)
        os.symlink(outside, os.path.join(app, 'file1.py'))
        good = b'print("clean")\n'
        h = hashlib.sha256(good).hexdigest()
        mock_man = {'version': self_repair.get_installed_version(),
                    'files': {'file1.py': h}}
        opener, _ = _serve([(lambda u: u.endswith('/sutura/file1.py'), good)])
        with patch('urllib.request.urlopen', side_effect=opener), _healthy_geom():
            code = self_repair.run_self_repair(
                app_dir=app, force_yes=True, _manifest_override=mock_man, _allow_git=True)
        assert code == 1
        assert os.path.islink(os.path.join(app, 'file1.py'))
        with open(outside, 'rb') as f:
            assert f.read() == b'user owned\n'


def test_symlinked_app_dir_root_allowed():
    with tempfile.TemporaryDirectory() as tmp:
        real = os.path.join(tmp, 'real')
        os.makedirs(real)
        link = os.path.join(tmp, 'link')
        os.symlink(real, link)
        good = b'print("clean")\n'
        with open(os.path.join(real, 'file1.py'), 'wb') as f:
            f.write(good)
        mock_man = {'version': self_repair.get_installed_version(),
                    'files': {'file1.py': hashlib.sha256(good).hexdigest()}}
        with _healthy_geom():
            code = self_repair.run_self_repair(
                app_dir=link, force_yes=True, _manifest_override=mock_man, _allow_git=True)
        assert code == 0


# --- atomic replace + rollback (H3) ---------------------------------------

def test_mid_replace_failure_rolls_back():
    ver = self_repair.get_installed_version()
    with tempfile.TemporaryDirectory() as tmp:
        originals = {}
        for name in ('a.py', 'b.py'):
            data = ('original %s\n' % name).encode()
            originals[name] = data
            with open(os.path.join(tmp, name), 'wb') as f:
                f.write(data)
        good = {'a.py': b'good a\n', 'b.py': b'good b\n'}
        mock_man = {'version': ver, 'files': {n: hashlib.sha256(c).hexdigest()
                                              for n, c in good.items()}}
        opener, _ = _serve([(lambda u, n=n: u.endswith('/sutura/' + n), c)
                            for n, c in good.items()])
        real_replace = os.replace
        calls = {'n': 0}

        def flaky(src, dst):
            calls['n'] += 1
            if calls['n'] == 3:  # journal=1, a.py=2, b.py=3
                raise OSError('injected replace failure')
            return real_replace(src, dst)

        with patch('urllib.request.urlopen', side_effect=opener), _healthy_geom(), \
                patch('os.replace', side_effect=flaky):
            code = self_repair.run_self_repair(
                app_dir=tmp, force_yes=True, _manifest_override=mock_man, _allow_git=True)
        assert code == 1
        for name, data in originals.items():
            with open(os.path.join(tmp, name), 'rb') as f:
                assert f.read() == data, name
        assert not os.path.isdir(self_repair._backup_dir(tmp))
        leftovers = [n for n in os.listdir(tmp) if self_repair.TMP_SUFFIX in n]
        assert leftovers == [], leftovers


# --- interrupted-backup recovery (H3) -------------------------------------

def _make_leftover_backup(app, good, journal_sha):
    bdir = self_repair._backup_dir(app)
    os.makedirs(bdir, exist_ok=True)
    with open(os.path.join(bdir, 'file1.py'), 'wb') as f:
        f.write(good)
    with open(os.path.join(bdir, self_repair.JOURNAL_NAME), 'w') as f:
        json.dump({'schema': self_repair.JOURNAL_SCHEMA,
                   'version': self_repair.get_installed_version(),
                   'entries': {'file1.py': {'existed': True,
                                            'backup_sha256': journal_sha}}}, f)
    return bdir


def test_interrupted_backup_restored_then_proceeds():
    ver = self_repair.get_installed_version()
    with tempfile.TemporaryDirectory() as tmp:
        good = b'good\n'
        with open(os.path.join(tmp, 'file1.py'), 'wb') as f:
            f.write(b'partial write\n')
        bdir = _make_leftover_backup(tmp, good, hashlib.sha256(good).hexdigest())
        mock_man = {'version': ver, 'files': {'file1.py': hashlib.sha256(good).hexdigest()}}
        with _healthy_geom():
            code = self_repair.run_self_repair(
                app_dir=tmp, force_yes=True, _manifest_override=mock_man, _allow_git=True)
        assert code == 0
        with open(os.path.join(tmp, 'file1.py'), 'rb') as f:
            assert f.read() == good
        assert not os.path.isdir(bdir)


def test_interrupted_backup_hash_mismatch_refuses():
    ver = self_repair.get_installed_version()
    with tempfile.TemporaryDirectory() as tmp:
        with open(os.path.join(tmp, 'file1.py'), 'wb') as f:
            f.write(b'partial\n')
        bdir = _make_leftover_backup(tmp, b'good\n', hashlib.sha256(b'DIFFERENT').hexdigest())
        mock_man = {'version': ver, 'files': {'file1.py': hashlib.sha256(b'good\n').hexdigest()}}
        with _healthy_geom():
            code = self_repair.run_self_repair(
                app_dir=tmp, force_yes=True, _manifest_override=mock_man, _allow_git=True)
        assert code == 1
        with open(os.path.join(tmp, 'file1.py'), 'rb') as f:
            assert f.read() == b'partial\n'
        assert os.path.isdir(bdir)  # left in place


def test_leftover_backup_without_journal_refuses():
    with tempfile.TemporaryDirectory() as tmp:
        bdir = self_repair._backup_dir(tmp)
        os.makedirs(bdir, exist_ok=True)
        with open(os.path.join(bdir, 'file1.py'), 'wb') as f:
            f.write(b'x')
        mock_man = {'version': self_repair.get_installed_version(), 'files': {}}
        with _healthy_geom():
            code = self_repair.run_self_repair(
                app_dir=tmp, force_yes=True, _manifest_override=mock_man, _allow_git=True)
        assert code == 1
        assert os.path.isdir(bdir)


# --- extension-only path (M3) ---------------------------------------------

def test_extension_only_unrepairable_exits_1():
    with tempfile.TemporaryDirectory() as tmp:
        mock_man = {'version': self_repair.get_installed_version(), 'files': {}}
        with patch.object(self_repair, 'check_extension_status',
                          return_value=(False, ['tamponade'], False, None)):
            code = self_repair.run_self_repair(
                app_dir=tmp, force_yes=True, _manifest_override=mock_man, _allow_git=True)
        assert code == 1


def test_extension_wheel_reinstall_verified_then_pip():
    ver = self_repair.get_installed_version()
    wheel = b'fake wheel bytes'
    wname = 'sutura_geom-0.9.1-cp311-abi3-manylinux_2_17_x86_64.whl'
    sums = ('%s  %s\n' % (hashlib.sha256(wheel).hexdigest(), wname)).encode()
    opener, _ = _serve([(lambda u: u.endswith('SHA256SUMS'), sums),
                        (lambda u: u.endswith(wname), wheel)])
    run_mock = SimpleNamespace(returncode=0, stderr='', stdout='')
    with patch.object(self_repair, '_venv_pythons', return_value=['/x/venv/bin/python']), \
            patch.object(self_repair, '_wheel_pattern',
                         return_value=re.compile(r'manylinux[^ ]*x86_64\.whl$')), \
            patch.object(self_repair, '_symbols_ok', return_value=True), \
            patch('subprocess.run', return_value=run_mock) as run, \
            patch('urllib.request.urlopen', side_effect=opener):
        assert self_repair.install_geom_wheel(ver, '/x') is True
    assert run.called
    args = run.call_args[0][0]
    assert 'pip' in args and '--force-reinstall' in args


def test_extension_wheel_hash_mismatch_never_runs_pip():
    ver = self_repair.get_installed_version()
    wname = 'sutura_geom-0.9.1-cp311-abi3-manylinux_2_17_x86_64.whl'
    sums = ('%s  %s\n' % ('0' * 64, wname)).encode()  # wrong hash
    opener, _ = _serve([(lambda u: u.endswith('SHA256SUMS'), sums),
                        (lambda u: u.endswith(wname), b'wheel')])
    with patch.object(self_repair, '_venv_pythons', return_value=['/x/venv/bin/python']), \
            patch.object(self_repair, '_wheel_pattern',
                         return_value=re.compile(r'manylinux[^ ]*x86_64\.whl$')), \
            patch('subprocess.run') as run, \
            patch('urllib.request.urlopen', side_effect=opener):
        assert self_repair.install_geom_wheel(ver, '/x') is False
    assert not run.called


def test_run_level_extension_orchestration():
    ver = self_repair.get_installed_version()
    mock_man = {'version': ver, 'files': {}}
    with tempfile.TemporaryDirectory() as tmp:
        with patch.object(self_repair, 'check_extension_status',
                          return_value=(False, ['tamponade'], True, '/x/venv/g.so')), \
                patch.object(self_repair, 'install_geom_wheel', return_value=True):
            assert self_repair.run_self_repair(
                app_dir=tmp, force_yes=True, _manifest_override=mock_man, _allow_git=True) == 0
        with patch.object(self_repair, 'check_extension_status',
                          return_value=(False, ['tamponade'], True, '/x/venv/g.so')), \
                patch.object(self_repair, 'install_geom_wheel', return_value=False):
            assert self_repair.run_self_repair(
                app_dir=tmp, force_yes=True, _manifest_override=mock_man, _allow_git=True) == 1


# --- allow-list ------------------------------------------------------------

def test_allow_list_leaves_user_data_and_history_untouched():
    with tempfile.TemporaryDirectory() as tmp:
        hist_path = os.path.join(tmp, 'history.jsonl')
        with open(hist_path, 'w') as f:
            f.write('{"seq": 100, "user_data": true}\n')
        data_dir = os.path.join(tmp, 'data')
        os.makedirs(data_dir, exist_ok=True)
        data_file = os.path.join(data_dir, 'user_mesh.stl')
        with open(data_file, 'wb') as f:
            f.write(b'user mesh data')

        with open(os.path.join(tmp, 'file1.py'), 'wb') as f:
            f.write(b'bad')
        good = b'good'
        h = hashlib.sha256(good).hexdigest()
        mock_man = {'version': self_repair.get_installed_version(), 'files': {'file1.py': h}}
        opener, _ = _serve([(lambda u: u.endswith('/sutura/file1.py'), good)])
        with patch('urllib.request.urlopen', side_effect=opener), _healthy_geom():
            code = self_repair.run_self_repair(
                app_dir=tmp, force_yes=True, _manifest_override=mock_man, _allow_git=True)
        assert code == 0
        with open(hist_path) as f:
            assert f.read() == '{"seq": 100, "user_data": true}\n'
        with open(data_file, 'rb') as f:
            assert f.read() == b'user mesh data'
        with open(os.path.join(tmp, 'file1.py'), 'rb') as f:
            assert f.read() == good


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('self-repair tests passed')


if __name__ == '__main__':
    main()
