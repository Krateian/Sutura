# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Release manifest generator and verifier for Sutura shipped files.

Defines the single source of truth for the application files allow-list
and produces/verifies SHA-256 hashes for all shipped Python modules
and engine packages.
"""
from datetime import datetime, timezone
import hashlib
import json
import os
import sys
from typing import Any, Dict, List, Optional, Tuple

_HERE = os.path.dirname(os.path.abspath(__file__))
_REPO = os.path.dirname(_HERE)

# L3 hard cap: a manifest may never declare more than this many files. It is
# a policy ceiling, not a real count (real releases ship well under 100).
MAX_MANIFEST_FILES = 1000


def get_shipped_file_keys(repo_or_sutura_dir: str) -> List[str]:
    """Return sorted list of manifest-relative paths for all shipped files.

    Derives the list from:
      - APP_MODULES in updater.py
      - COPY_EXTRA_MODULES ('__init__.py')
      - LINUX_EXTRA_FILES ('open.sh')
      - repair_score_config.json
      - LICENSE
      - Recursive scan of sutura_engine/ for an explicit allow-list of
        extensions: .py modules and .toml data (policy/default_policy.toml).
        A filesystem walk with a fixed allow-list keeps untracked build
        artifacts (a built .so, logs, editor backups) out of the manifest.
        __pycache__, .pyc and dotfiles are skipped.
    """
    try:
        from sutura import updater
        app_mods = list(updater.APP_MODULES)
        extra_mods = list(updater.COPY_EXTRA_MODULES)
        linux_extras = list(updater.LINUX_EXTRA_FILES)
    except ImportError:
        try:
            import updater
            app_mods = list(updater.APP_MODULES)
            extra_mods = list(updater.COPY_EXTRA_MODULES)
            linux_extras = list(updater.LINUX_EXTRA_FILES)
        except ImportError:
            app_mods = []
            extra_mods = ['__init__.py']
            linux_extras = ['open.sh']

    keys = set(app_mods + extra_mods + linux_extras)
    keys.add('repair_score_config.json')
    keys.add('LICENSE')

    # Scan sutura_engine tree
    # Check if repo_or_sutura_dir points to repo or sutura directory
    engine_candidates = [
        os.path.join(repo_or_sutura_dir, 'sutura', 'sutura_engine'),
        os.path.join(repo_or_sutura_dir, 'sutura_engine'),
    ]
    engine_dir = None
    for cand in engine_candidates:
        if os.path.isdir(cand):
            engine_dir = cand
            break

    if engine_dir:
        for root, dirs, files in os.walk(engine_dir):
            dirs[:] = [d for d in dirs if d != '__pycache__' and not d.startswith('.')]
            for f in files:
                # Explicit allow-list, not "every file": the scan sees the
                # build machine's filesystem, so a built .so, a log or an
                # editor backup must never enter the release manifest. The only
                # shipped non-.py file is the policy TOML.
                if (f.endswith('.py') or f.endswith('.toml')) and not f.startswith('.'):
                    full_p = os.path.join(root, f)
                    rel_to_engine = os.path.relpath(full_p, os.path.dirname(engine_dir))
                    # Normalise to 'sutura_engine/...'
                    if rel_to_engine.startswith('sutura' + os.sep):
                        rel_to_engine = rel_to_engine[len('sutura' + os.sep):]
                    keys.add(rel_to_engine.replace('\\', '/'))

    return sorted(keys)


def sha256_file(path: str) -> str:
    """Compute sha256 hex digest of a file on disk."""
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        while chunk := f.read(65536):
            h.update(chunk)
    return h.hexdigest()


def generate_manifest(repo_root: Optional[str] = None) -> Dict[str, Any]:
    """Generate the release manifest dictionary from a repository root."""
    if repo_root is None:
        repo_root = _REPO
    if not os.path.isfile(os.path.join(repo_root, 'requirements.txt')):
        # Perhaps we were passed the sutura/ folder
        if os.path.isfile(os.path.join(os.path.dirname(repo_root), 'requirements.txt')):
            repo_root = os.path.dirname(repo_root)

    try:
        from sutura.repair import VERSION
    except ImportError:
        try:
            from repair import VERSION
        except ImportError:
            VERSION = 'unknown'

    keys = get_shipped_file_keys(repo_root)
    file_hashes: Dict[str, str] = {}

    for k in keys:
        if k == 'LICENSE':
            cand = os.path.join(repo_root, 'LICENSE')
        else:
            cand = os.path.join(repo_root, 'sutura', k)
            if not os.path.isfile(cand):
                cand = os.path.join(repo_root, k)
        if os.path.isfile(cand):
            file_hashes[k] = sha256_file(cand)

    manifest = {
        'version': VERSION,
        'generated_at': datetime.now(timezone.utc).isoformat(),
        'file_count': len(file_hashes),
        'files': file_hashes,
    }
    return manifest


def verify_manifest(app_dir: str, manifest: Dict[str, Any]) -> Tuple[bool, List[str], List[str]]:
    """Compare files in app_dir against manifest.

    Returns:
      (all_match, list_of_mismatching_files, list_of_missing_files)
    """
    files = manifest.get('files', {})
    mismatches = []
    missing = []

    for rel_path, expected_hash in files.items():
        disk_path = os.path.join(app_dir, rel_path)
        if not os.path.isfile(disk_path):
            missing.append(rel_path)
            continue
        try:
            actual_hash = sha256_file(disk_path)
            if actual_hash != expected_hash:
                mismatches.append(rel_path)
        except Exception:
            mismatches.append(rel_path)

    all_match = (len(mismatches) == 0 and len(missing) == 0)
    return all_match, sorted(mismatches), sorted(missing)


def _bad_key_reason(key, app_dir: str) -> Optional[str]:
    """Return a rejection reason for a manifest key, or None when it is safe.

    A key is safe only when it is a plain POSIX relative path made of ordinary
    segments that stays inside ``app_dir`` after normalisation. Absolute keys,
    ``..`` segments, backslashes, drive letters and empty segments are all
    rejected (H1): act on the manifest only when every key is safe.
    """
    if not isinstance(key, str):
        return 'key is not a string'
    if not key or key in ('.', '..'):
        return 'empty or dot key'
    if '\\' in key:
        return 'backslash in key'
    if key.startswith('/'):
        return 'absolute key'
    if len(key) >= 2 and key[1] == ':':
        return 'drive-letter key'
    parts = key.split('/')
    if any(p == '..' for p in parts):
        return "'..' path segment"
    if any(p == '' for p in parts):
        return 'empty path segment'
    root = os.path.normpath(app_dir)
    target = os.path.normpath(os.path.join(root, key))
    if target != root and not target.startswith(root + os.sep):
        return 'resolves outside app_dir'
    return None


def validate_manifest(manifest: Dict[str, Any], app_dir: str) -> List[Tuple[str, str]]:
    """Validate the WHOLE manifest before any filesystem I/O (H1).

    Returns a list of ``(key, reason)`` for every unsafe entry; an empty list
    means the manifest is safe to act on. The caller must refuse the entire
    manifest when anything is returned.
    """
    files = manifest.get('files')
    if not isinstance(files, dict):
        return [('<files>', 'manifest has no files mapping')]
    bad: List[Tuple[str, str]] = []
    if len(files) > MAX_MANIFEST_FILES:
        bad.append(('<files>', 'too many files (%d > %d)'
                    % (len(files), MAX_MANIFEST_FILES)))
    for key in files:
        reason = _bad_key_reason(key, app_dir)
        if reason:
            bad.append((key, reason))
    return bad


def main():
    import argparse
    parser = argparse.ArgumentParser(description="Sutura Release Manifest Tool")
    parser.add_argument('--repo', default=None, help='Repository root path')
    parser.add_argument('--out', default=None, help='Output path for manifest.json')
    parser.add_argument('--verify', default=None, help='Verify APP_DIR against manifest')
    parser.add_argument('--manifest', default=None, help='Manifest file to verify against')
    args = parser.parse_args()

    if args.verify:
        if not args.manifest:
            print("error: --verify requires --manifest <path>", file=sys.stderr)
            sys.exit(1)
        with open(args.manifest, 'r', encoding='utf-8') as f:
            man = json.load(f)
        ok, mismatches, missing = verify_manifest(args.verify, man)
        if ok:
            print("Manifest verification: OK (%d files checked)" % len(man.get('files', {})))
            sys.exit(0)
        else:
            print("Manifest verification: FAILED")
            if missing:
                print("Missing files (%d):" % len(missing))
                for f in missing:
                    print("  missing:", f)
            if mismatches:
                print("Corrupted / modified files (%d):" % len(mismatches))
                for f in mismatches:
                    print("  mismatch:", f)
            sys.exit(1)

    # Generate manifest
    man = generate_manifest(args.repo)
    raw = json.dumps(man, indent=2)
    if args.out:
        with open(args.out, 'w', encoding='utf-8') as f:
            f.write(raw)
        print("Manifest written to %s (%d files, version %s)" % (args.out, man['file_count'], man['version']))
    else:
        print(raw)


if __name__ == '__main__':
    main()
