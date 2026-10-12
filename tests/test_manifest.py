#!/usr/bin/env python3
"""Tests for sutura/manifest.py (Release manifest generation and verification)."""
import json
import os
import shutil
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import manifest


def test_get_shipped_file_keys():
    keys = manifest.get_shipped_file_keys(REPO)
    assert 'repair.py' in keys
    assert 'diagnostics.py' in keys
    assert 'manifest.py' in keys
    assert 'LICENSE' in keys
    assert 'repair_score_config.json' in keys
    # Check that sutura_engine files are present
    engine_files = [k for k in keys if k.startswith('sutura_engine/')]
    assert len(engine_files) > 10
    assert 'sutura_engine/tamponade.py' in keys
    assert 'sutura_engine/methods/builtin/graft.py' in keys


def test_generate_manifest():
    man = manifest.generate_manifest(REPO)
    assert 'version' in man
    assert 'generated_at' in man
    assert 'file_count' in man
    assert man['file_count'] > 30
    assert 'files' in man
    assert 'repair.py' in man['files']
    assert len(man['files']['repair.py']) == 64  # sha256 hex string


def test_verify_manifest_clean():
    # Construct a mock APP_DIR containing a subset of manifest files
    with tempfile.TemporaryDirectory() as tmp:
        f1 = os.path.join(tmp, 'file1.py')
        f2 = os.path.join(tmp, 'sub', 'file2.py')
        os.makedirs(os.path.dirname(f2), exist_ok=True)
        with open(f1, 'w') as f:
            f.write('print("hello")\n')
        with open(f2, 'w') as f:
            f.write('print("world")\n')

        h1 = manifest.sha256_file(f1)
        h2 = manifest.sha256_file(f2)
        mock_man = {
            'version': '0.9.1',
            'files': {'file1.py': h1, 'sub/file2.py': h2}
        }

        ok, mismatches, missing = manifest.verify_manifest(tmp, mock_man)
        assert ok is True
        assert mismatches == []
        assert missing == []


def test_verify_manifest_detects_mismatch_and_missing():
    with tempfile.TemporaryDirectory() as tmp:
        f1 = os.path.join(tmp, 'file1.py')
        with open(f1, 'w') as f:
            f.write('original content\n')
        h1 = manifest.sha256_file(f1)

        mock_man = {
            'version': '0.9.1',
            'files': {
                'file1.py': h1,
                'file_missing.py': '0000000000000000000000000000000000000000000000000000000000000000',
            }
        }

        # Modify f1
        with open(f1, 'w') as f:
            f.write('tampered content\n')

        ok, mismatches, missing = manifest.verify_manifest(tmp, mock_man)
        assert ok is False
        assert mismatches == ['file1.py']
        assert missing == ['file_missing.py']


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('manifest tests passed')


if __name__ == '__main__':
    main()
