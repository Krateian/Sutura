#!/usr/bin/env python3
"""Offline unit tests for sutura/self_repair.py (sutura --self-repair)."""
import hashlib
import io
import json
import os
import shutil
import sys
import tempfile
import urllib.error
from unittest.mock import patch

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import self_repair


class MockHTTPResponse:
    def __init__(self, data: bytes):
        self._data = data

    def read(self):
        return self._data

    def __enter__(self):
        return self

    def __exit__(self, *args):
        pass


def test_is_git_repository_protection():
    assert self_repair.is_git_repository(REPO) is True
    # Without allow_git=True, it refuses
    code = self_repair.run_self_repair(app_dir=REPO, allow_git=False)
    assert code == 1


def test_version_mismatch_refused():
    with tempfile.TemporaryDirectory() as tmp:
        bad_manifest = {
            'version': '9.9.9',
            'files': {}
        }
        code = self_repair.run_self_repair(
            app_dir=tmp,
            manifest_override=bad_manifest,
            allow_git=True,
        )
        assert code == 1


def test_clean_installation_verified():
    with tempfile.TemporaryDirectory() as tmp:
        f1 = os.path.join(tmp, 'file1.py')
        content = b'print("clean")\n'
        with open(f1, 'wb') as f:
            f.write(content)
        h1 = hashlib.sha256(content).hexdigest()

        ver = self_repair.get_installed_version()
        mock_man = {
            'version': ver,
            'files': {'file1.py': h1}
        }
        code = self_repair.run_self_repair(
            app_dir=tmp,
            manifest_override=mock_man,
            allow_git=True,
        )
        assert code == 0


def test_offline_mock_download_and_repair():
    with tempfile.TemporaryDirectory() as tmp:
        # Create corrupted file1.py
        f1 = os.path.join(tmp, 'file1.py')
        with open(f1, 'wb') as f:
            f.write(b'corrupted content\n')

        correct_content1 = b'print("fixed 1")\n'
        correct_content2 = b'print("fixed 2")\n'
        h1 = hashlib.sha256(correct_content1).hexdigest()
        h2 = hashlib.sha256(correct_content2).hexdigest()

        ver = self_repair.get_installed_version()
        mock_man = {
            'version': ver,
            'files': {
                'file1.py': h1,
                'sub/file2.py': h2,  # missing file
            }
        }

        # Mock urllib.request.urlopen returning correct bytes
        def mock_urlopen(req, timeout=15):
            url = req.full_url if hasattr(req, 'full_url') else str(req)
            if 'file1.py' in url:
                return MockHTTPResponse(correct_content1)
            elif 'file2.py' in url:
                return MockHTTPResponse(correct_content2)
            raise urllib.error.URLError('not found')

        with patch('urllib.request.urlopen', side_effect=mock_urlopen):
            code = self_repair.run_self_repair(
                app_dir=tmp,
                force_yes=True,
                manifest_override=mock_man,
                allow_git=True,
            )
            assert code == 0

        # Verify files were repaired on disk
        with open(f1, 'rb') as f:
            assert f.read() == correct_content1
        with open(os.path.join(tmp, 'sub', 'file2.py'), 'rb') as f:
            assert f.read() == correct_content2


def test_tampered_download_aborts_without_modifying_files():
    with tempfile.TemporaryDirectory() as tmp:
        f1 = os.path.join(tmp, 'file1.py')
        original_bytes = b'original corrupted\n'
        with open(f1, 'wb') as f:
            f.write(original_bytes)

        expected_hash = hashlib.sha256(b'expected clean').hexdigest()
        ver = self_repair.get_installed_version()
        mock_man = {
            'version': ver,
            'files': {'file1.py': expected_hash}
        }

        # Mock returns tampered bytes that don't match expected_hash
        def mock_urlopen(req, timeout=15):
            return MockHTTPResponse(b'tampered payload from mitm')

        with patch('urllib.request.urlopen', side_effect=mock_urlopen):
            code = self_repair.run_self_repair(
                app_dir=tmp,
                force_yes=True,
                manifest_override=mock_man,
                allow_git=True,
            )
            assert code == 1

        # File must remain untouched
        with open(f1, 'rb') as f:
            assert f.read() == original_bytes


def test_allow_list_leaves_user_data_and_history_untouched():
    with tempfile.TemporaryDirectory() as tmp:
        # User files outside manifest
        hist_path = os.path.join(tmp, 'history.jsonl')
        with open(hist_path, 'w') as f:
            f.write('{"seq": 100, "user_data": true}\n')

        data_dir = os.path.join(tmp, 'data')
        os.makedirs(data_dir, exist_ok=True)
        data_file = os.path.join(data_dir, 'user_mesh.stl')
        with open(data_file, 'wb') as f:
            f.write(b'user mesh data')

        # Corrupted manifest file
        f1 = os.path.join(tmp, 'file1.py')
        with open(f1, 'wb') as f:
            f.write(b'bad')
        good_bytes = b'good'
        h1 = hashlib.sha256(good_bytes).hexdigest()

        ver = self_repair.get_installed_version()
        mock_man = {
            'version': ver,
            'files': {'file1.py': h1}
        }

        with patch('urllib.request.urlopen', return_value=MockHTTPResponse(good_bytes)):
            code = self_repair.run_self_repair(
                app_dir=tmp,
                force_yes=True,
                manifest_override=mock_man,
                allow_git=True,
            )
            assert code == 0

        # User data must be 100% identical and untouched
        with open(hist_path, 'r') as f:
            assert f.read() == '{"seq": 100, "user_data": true}\n'
        with open(data_file, 'rb') as f:
            assert f.read() == b'user mesh data'
        with open(f1, 'rb') as f:
            assert f.read() == good_bytes


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('self-repair tests passed')


if __name__ == '__main__':
    main()
