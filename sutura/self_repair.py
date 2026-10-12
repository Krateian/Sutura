# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Self-repair mechanism for Sutura.

Compares installed shipped files against the official release manifest,
prompts the user, downloads verified replacements of the SAME version over
HTTPS, backs up existing files on a strict allow-list, replaces them, and
rolls back on verification failure.

Never touches user data, history, configs, or the surrounding environment.

Safety model (P0 review fixes):

* H1 -- the whole manifest is validated before any filesystem I/O; one unsafe
  key (absolute, ``..``, backslash, or resolving outside ``app_dir``) refuses
  the entire manifest.
* H2 -- no write ever follows a symlink: the target and every parent below
  ``app_dir`` are lstat-checked, and files are written to a temp file in the
  same directory then moved into place with ``os.replace``.  A symlinked
  ``app_dir`` root is allowed (users legitimately relocate the data dir).
* H3 -- every verified payload is staged to a temp file before any replace;
  any exception during replacement restores the backup and exits 1 without a
  traceback.  A leftover backup from an interrupted run is recovered from a
  journal (only when every backup hash matches) or the run refuses cleanly.
* L1 -- the installed version must match ``^\\d+\\.\\d+\\.\\d+$`` before it is
  used in a URL.
* L3 -- a manifest is capped at 1000 files and each download at 10 MB.
* M1 -- platform gates fail closed; the macOS bundle is detected from the
  install path, not ``sys.executable``.
* M3 -- when only the Rust extension is broken and it lives in Sutura's own
  venv, the matching wheel is reinstalled (SHA-256 verified against the
  release ``sutura_geom-SHA256SUMS`` before pip runs); otherwise the run
  reports "extension not repaired" and exits 1.
"""
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from typing import Any, Dict, List, Optional, Tuple

APP_DIR = os.path.expanduser('~/.local/share/sutura')
BACKUP_DIR = os.path.expanduser('~/.local/share/sutura.self_repair_backup')
MANIFEST_URL = 'https://github.com/Krateian/Sutura/releases/download/v{version}/manifest.json'
RAW_FILE_URL = 'https://raw.githubusercontent.com/Krateian/Sutura/v{version}/{repo_path}'
SHA256SUMS_URL = 'https://github.com/Krateian/Sutura/releases/download/v{version}/sutura_geom-SHA256SUMS'
WHEEL_URL = 'https://github.com/Krateian/Sutura/releases/download/v{version}/{wheel_name}'

REQUIRED_GEOM_SYMBOLS = (
    'tamponade',
    'morph_close',
    'dressing_coat',
    'arrangement_lite',
)

MANIFEST_VERSION_RE = re.compile(r'^\d+\.\d+\.\d+$')

MAX_MANIFEST_BYTES = 2 * 1024 * 1024      # L3: manifest JSON ceiling
MAX_FILE_BYTES = 10 * 1024 * 1024         # L3: shipped-file download ceiling
MAX_SUMS_BYTES = 1 * 1024 * 1024          # SHA256SUMS ceiling
MAX_WHEEL_BYTES = 200 * 1024 * 1024       # sutura_geom wheel ceiling

JOURNAL_NAME = '_journal.json'
JOURNAL_SCHEMA = 1
TMP_SUFFIX = '.sutura-tmp'


class SelfRepairError(Exception):
    """Raised when self-repair cannot proceed or fails."""


def _get_manifest_mod():
    try:
        from sutura import manifest as manifest_mod
    except ImportError:
        import manifest as manifest_mod
    return manifest_mod


def _backup_dir(app_dir: str) -> str:
    """Backup directory beside the install (same path as BACKUP_DIR for the
    default install). Derived from app_dir so tests never touch the real one."""
    return os.path.abspath(app_dir) + '.self_repair_backup'


def get_installed_version() -> str:
    try:
        from sutura.repair import VERSION
        return VERSION
    except ImportError:
        try:
            from repair import VERSION
            return VERSION
        except ImportError:
            return 'unknown'


def is_git_repository(path: str) -> bool:
    """Check if the given directory is inside a git working tree."""
    cand = path
    while cand and cand != os.path.dirname(cand):
        if os.path.isdir(os.path.join(cand, '.git')) or os.path.isfile(os.path.join(cand, '.git')):
            return True
        cand = os.path.dirname(cand)
    return False


def is_macos_bundle(app_dir: Optional[str] = None) -> bool:
    """True when the install directory itself is inside a macOS .app bundle.

    Detected from the *install path*, not ``sys.executable`` (M1): the
    released self-contained .app bundles the tools under ``*.app/Contents``,
    while the script installer keeps files at ``~/.local/share/sutura``
    (outside any bundle, so repairing them is safe).
    """
    p = os.path.abspath(app_dir or APP_DIR).replace('\\', '/') + '/'
    return '.app/Contents/' in p


def _read_capped(resp, max_bytes: int) -> bytes:
    """Read a response fully, refusing anything larger than ``max_bytes`` (L3)."""
    chunks = []
    total = 0
    while True:
        chunk = resp.read(65536)
        if not chunk:
            break
        total += len(chunk)
        if total > max_bytes:
            raise SelfRepairError('download exceeds %d bytes' % max_bytes)
        chunks.append(chunk)
    return b''.join(chunks)


def _download_bytes(url: str, version: str, max_bytes: int, timeout: int = 30) -> bytes:
    req = urllib.request.Request(url, headers={'User-Agent': f'Sutura-SelfRepair/{version}'})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return _read_capped(resp, max_bytes)
    except urllib.error.URLError as e:
        raise SelfRepairError(f"Failed to download {url}: {e}")


def fetch_manifest(version: str, timeout: int = 15) -> Dict[str, Any]:
    if not MANIFEST_VERSION_RE.match(version or ''):
        raise SelfRepairError(f"refusing to fetch a manifest for invalid version {version!r}")
    url = MANIFEST_URL.format(version=version)
    try:
        raw = _download_bytes(url, version, MAX_MANIFEST_BYTES, timeout=timeout)
        return json.loads(raw.decode('utf-8'))
    except SelfRepairError:
        raise
    except Exception as e:
        raise SelfRepairError(f"Error parsing manifest from {url}: {e}")


def check_extension_status(app_dir: str) -> Tuple[bool, List[str], bool, Optional[str]]:
    """Check sutura_geom health.

    Returns:
      (is_ok, missing_symbols, is_sutura_venv, geom_file_path)
    """
    try:
        import sutura_geom
        geom_path = getattr(sutura_geom, '__file__', None)
    except Exception:
        return False, list(REQUIRED_GEOM_SYMBOLS), False, None

    missing = [sym for sym in REQUIRED_GEOM_SYMBOLS if not hasattr(sutura_geom, sym)]
    is_ok = (len(missing) == 0)

    # Check whether extension is located inside Sutura's own Linux venv
    venv_dir = os.path.join(app_dir, 'venv')
    is_sutura_venv = False
    if geom_path:
        try:
            if os.path.commonpath([os.path.abspath(geom_path),
                                   os.path.abspath(venv_dir)]) == os.path.abspath(venv_dir):
                is_sutura_venv = True
        except ValueError:
            is_sutura_venv = False

    return is_ok, missing, is_sutura_venv, geom_path


def _resolve_target(app_dir: str, rel: str) -> str:
    """Return the contained absolute path for a validated key or raise (H1)."""
    root = os.path.normpath(app_dir)
    target = os.path.normpath(os.path.join(root, rel))
    if target != root and not target.startswith(root + os.sep):
        raise SelfRepairError(f"path escapes app_dir: {rel!r}")
    return target


def _assert_no_symlink(app_dir: str, rel: str) -> None:
    """Refuse to act through a symlink at the target or any parent below the
    install root (H2).  The app_dir root itself may be a symlink (D2)."""
    root = os.path.normpath(app_dir)
    cur = root
    for part in rel.split('/'):
        cur = os.path.join(cur, part)
        if os.path.islink(cur):
            raise SelfRepairError(f"refusing to write through symlink: {cur}")


def _atomic_write(target: str, data: bytes) -> None:
    """Write via a temp file in the SAME directory then ``os.replace`` (H2/H3)."""
    d = os.path.dirname(target)
    os.makedirs(d, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix=os.path.basename(target) + TMP_SUFFIX + '.', dir=d)
    try:
        with os.fdopen(fd, 'wb') as f:
            f.write(data)
        os.replace(tmp, target)
    except BaseException:
        try:
            os.remove(tmp)
        except OSError:
            pass
        raise


def download_verified_file(version: str, rel_path: str, expected_hash: str, timeout: int = 15) -> bytes:
    repo_path = 'LICENSE' if rel_path == 'LICENSE' else f'sutura/{rel_path}'
    url = RAW_FILE_URL.format(version=version, repo_path=repo_path)
    content = _download_bytes(url, version, MAX_FILE_BYTES, timeout=timeout)

    actual_hash = hashlib.sha256(content).hexdigest()
    if actual_hash != expected_hash:
        raise SelfRepairError(
            f"Hash mismatch for downloaded file {rel_path}!\n"
            f"  Expected: {expected_hash}\n"
            f"  Got:      {actual_hash}"
        )
    return content


# --- H3: interrupted-run recovery -----------------------------------------

def recover_interrupted_backup(app_dir: str, backup_dir: str) -> str:
    """Restore a leftover backup from an interrupted run, or refuse cleanly.

    Returns ``'none'`` (no backup), ``'restored'`` or ``'refused'``.  Per D3 a
    file is restored only when its backup exists and its SHA-256 matches the
    journal; any mismatch, missing backup or unreadable/absent journal makes the
    whole recovery refuse, leaving everything in place.
    """
    if not os.path.isdir(backup_dir):
        return 'none'
    manifest_mod = _get_manifest_mod()
    jpath = os.path.join(backup_dir, JOURNAL_NAME)
    if not os.path.isfile(jpath):
        print(f"error: leftover self-repair backup at {backup_dir} has no journal.", file=sys.stderr)
        print("Refusing to continue (nothing was modified). Inspect and remove it manually.", file=sys.stderr)
        return 'refused'
    try:
        with open(jpath, 'r', encoding='utf-8') as f:
            journal = json.load(f)
    except Exception as e:
        print(f"error: cannot read self-repair journal ({e}).", file=sys.stderr)
        print("Refusing to continue (nothing was modified).", file=sys.stderr)
        return 'refused'
    if journal.get('schema') != JOURNAL_SCHEMA or not isinstance(journal.get('entries'), dict):
        print("error: self-repair journal is malformed.", file=sys.stderr)
        print("Refusing to continue (nothing was modified).", file=sys.stderr)
        return 'refused'

    entries = journal['entries']
    to_restore = []
    for rel, meta in entries.items():
        if manifest_mod._bad_key_reason(rel, app_dir) is not None:
            print(f"error: journal key {rel!r} is unsafe.", file=sys.stderr)
            print("Refusing to continue (nothing was modified).", file=sys.stderr)
            return 'refused'
        if not isinstance(meta, dict) or not meta.get('existed'):
            continue
        bkp = os.path.join(backup_dir, rel)
        if not os.path.isfile(bkp):
            print(f"error: journal lists a backup for {rel!r} but it is missing.", file=sys.stderr)
            print("Refusing to continue (nothing was modified).", file=sys.stderr)
            return 'refused'
        if manifest_mod.sha256_file(bkp) != meta.get('backup_sha256'):
            print(f"error: backup for {rel!r} does not match the journal hash.", file=sys.stderr)
            print("Refusing to continue (nothing was modified).", file=sys.stderr)
            return 'refused'
        to_restore.append(rel)

    for rel in to_restore:
        try:
            target = _resolve_target(app_dir, rel)
            _assert_no_symlink(app_dir, rel)
            os.makedirs(os.path.dirname(target), exist_ok=True)
            shutil.copy2(os.path.join(backup_dir, rel), target)
        except Exception as e:
            print(f"error: failed to restore {rel!r}: {e}", file=sys.stderr)
            return 'refused'
    shutil.rmtree(backup_dir, ignore_errors=True)
    print(f"Recovered from an interrupted self-repair (restored {len(to_restore)} file(s)).")
    return 'restored'


def _rollback(app_dir: str, backup_map: Dict[str, str], missing: List[str]) -> None:
    for rel, bkp in backup_map.items():
        try:
            shutil.copy2(bkp, _resolve_target(app_dir, rel))
        except Exception:
            pass
    for rel in missing:
        try:
            target = _resolve_target(app_dir, rel)
            if os.path.isfile(target):
                os.remove(target)
        except Exception:
            pass


# --- M3: sutura_geom wheel reinstall --------------------------------------

def _wheel_pattern() -> Optional['re.Pattern']:
    sysname, machine = platform.system(), platform.machine().lower()
    if sysname == 'Linux' and machine in ('x86_64', 'amd64'):
        return re.compile(r'manylinux[^ ]*x86_64\.whl$')
    if sysname == 'Linux' and machine in ('aarch64', 'arm64'):
        return re.compile(r'manylinux[^ ]*aarch64\.whl$')
    if sysname == 'Darwin':
        return re.compile(r'macosx[^ ]*universal2\.whl$')
    return None


def _venv_pythons(app_dir: str) -> List[str]:
    """Sutura's own venv interpreters (D1: venv and venv311 when present)."""
    out = []
    for sub in ('venv', 'venv311'):
        py = os.path.join(app_dir, sub, 'bin', 'python')
        if os.path.isfile(py):
            out.append(py)
    return out


def _symbols_ok(python: str) -> bool:
    check = 'import sutura_geom as g\n' + '\n'.join(
        'assert hasattr(g, %r)' % s for s in REQUIRED_GEOM_SYMBOLS)
    try:
        r = subprocess.run([python, '-c', check], capture_output=True, text=True, timeout=120)
    except Exception:
        return False
    return r.returncode == 0


def install_geom_wheel(version: str, app_dir: str) -> bool:
    """Reinstall sutura_geom into Sutura's own venv(s), SHA-256 verified.

    Returns True only when pip ran on every venv AND a fresh import in each
    venv exposes all required symbols.  Never runs pip on unverified content.
    """
    pythons = _venv_pythons(app_dir)
    pattern = _wheel_pattern()
    if not pythons or pattern is None:
        return False

    sums = _download_bytes(SHA256SUMS_URL.format(version=version), version, MAX_SUMS_BYTES)
    wheel_name = wheel_hash = None
    for line in sums.decode('utf-8', 'replace').splitlines():
        parts = line.split()
        if len(parts) >= 2:
            name = parts[1].lstrip('*')
            if pattern.search(name):
                wheel_name, wheel_hash = name, parts[0]
                break
    if not wheel_name:
        return False

    data = _download_bytes(WHEEL_URL.format(version=version, wheel_name=wheel_name),
                           version, MAX_WHEEL_BYTES)
    if hashlib.sha256(data).hexdigest() != wheel_hash:
        print(f"error: SHA-256 mismatch for {wheel_name}; discarding download.", file=sys.stderr)
        return False

    fd, wheel_path = tempfile.mkstemp(prefix='sutura_geom-', suffix='.whl')
    try:
        with os.fdopen(fd, 'wb') as f:
            f.write(data)
        for py in pythons:
            r = subprocess.run([py, '-m', 'pip', 'install', '--quiet', '--no-deps',
                                '--force-reinstall', wheel_path],
                               capture_output=True, text=True, timeout=600)
            if r.returncode != 0:
                print(f"error: pip install failed for {py}: {r.stderr.strip()[:400]}", file=sys.stderr)
                return False
        return all(_symbols_ok(py) for py in pythons)
    except Exception as e:
        print(f"error: wheel reinstall failed: {e}", file=sys.stderr)
        return False
    finally:
        try:
            os.remove(wheel_path)
        except OSError:
            pass


def run_self_repair(
    app_dir: Optional[str] = None,
    force_yes: bool = False,
    _manifest_override: Optional[Dict[str, Any]] = None,
    _allow_git: bool = False,
) -> int:
    """Execute the full self-repair flow.

    ``_manifest_override`` and ``_allow_git`` are PRIVATE test hooks (M1); the
    CLI never passes them.
    """
    if app_dir is None:
        app_dir = APP_DIR
    app_dir = os.path.abspath(app_dir)

    # 1. Platform & Environment Guards -- fail closed (M1)
    try:
        from sutura import updater
    except ImportError:
        try:
            import updater
        except Exception as e:
            print(f"error: cannot load updater to check the platform: {e}", file=sys.stderr)
            return 1
    try:
        appimage = updater.is_appimage()
    except Exception as e:
        print(f"error: could not determine AppImage status: {e}", file=sys.stderr)
        return 1
    if appimage:
        print("Notice: AppImage installation detected.")
        print("In-place self-repair is not supported on read-only AppImage filesystems.")
        print("To repair or reinstall, download the latest release from:")
        print("  https://github.com/Krateian/Sutura/releases")
        return 0

    if is_macos_bundle(app_dir):
        print("Notice: macOS application bundle detected.")
        print("In-place modification would invalidate code signature.")
        print("To repair, re-download the .dmg package from:")
        print("  https://github.com/Krateian/Sutura/releases")
        return 0

    if not _allow_git and is_git_repository(app_dir):
        print(f"error: {app_dir} is inside a git repository.", file=sys.stderr)
        print("Self-repair is designed for deployed installations (~/.local/share/sutura).", file=sys.stderr)
        print("To restore files in a development checkout, use 'git checkout'.", file=sys.stderr)
        return 1

    version = get_installed_version()
    if not MANIFEST_VERSION_RE.match(version):  # L1
        print(f"error: invalid installed version {version!r}; refusing.", file=sys.stderr)
        return 1
    print(f"Sutura Self-Repair (version {version})")
    print(f"Checking installation at {app_dir}...")

    manifest_mod = _get_manifest_mod()

    # 2. Fetch & Validate Manifest
    try:
        if _manifest_override is not None:
            manifest = _manifest_override
        else:
            manifest = fetch_manifest(version)
    except SelfRepairError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1

    man_version = manifest.get('version')
    if man_version != version:
        print(
            f"error: manifest version (v{man_version}) does not match installed version (v{version}).\n"
            "Self-repair only repairs files matching the currently installed version.",
            file=sys.stderr,
        )
        return 1

    # 2b. Validate the WHOLE manifest before ANY filesystem I/O (H1)
    bad = manifest_mod.validate_manifest(manifest, app_dir)
    if bad:
        print("error: manifest refused before any change (unsafe entry):", file=sys.stderr)
        for key, reason in bad:
            print(f"    {key!r}: {reason}", file=sys.stderr)
        return 1

    # 3. Recover a leftover backup from an interrupted run (H3)
    backup_dir = _backup_dir(app_dir)
    if recover_interrupted_backup(app_dir, backup_dir) == 'refused':
        return 1

    # 4. Verify shipped files + extension
    ok, mismatches, missing = manifest_mod.verify_manifest(app_dir, manifest)
    geom_ok, geom_missing, is_sutura_venv, geom_path = check_extension_status(app_dir)

    if ok and geom_ok:
        print(f"All {len(manifest.get('files', {}))} shipped application files verified. Installation is intact.")
        return 0

    # 5. Report defects found
    print("\nIntegrity audit found mismatches:")
    if missing:
        print(f"  Missing files ({len(missing)}):")
        for f in missing:
            print(f"    - {f}")
    if mismatches:
        print(f"  Corrupted/modified files ({len(mismatches)}):")
        for f in mismatches:
            print(f"    - {f}")
    if not geom_ok:
        print(f"  sutura_geom extension issue: missing symbol(s) {', '.join(geom_missing)}")
        if not is_sutura_venv:
            if os.environ.get('CONDA_PREFIX'):
                print("  Note: Running under Conda. Auto-reinstall is disabled to prevent environment conflict.")
            else:
                print("  Note: Running in a non-venv/dev environment; the extension cannot be auto-reinstalled.")

    files_to_repair = mismatches + missing
    geom_needs_repair = (not geom_ok) and is_sutura_venv

    # 5b. Nothing to do -- but a broken extension that cannot be repaired is a
    # failure, never a silent success with 0 files (M3).
    if not files_to_repair and not geom_needs_repair:
        if not geom_ok:
            print("\nerror: extension not repaired (not in Sutura's own venv).", file=sys.stderr)
            return 1
        print("\nNo auto-repairable files need changes.")
        return 0

    # 6. User consent
    if not force_yes:
        extra = " and the Rust extension" if geom_needs_repair else ""
        prompt_msg = f"\nProceed to download and repair {len(files_to_repair)} file(s){extra}? [y/N]: "
        try:
            choice = input(prompt_msg).strip().lower()
        except (KeyboardInterrupt, EOFError):
            print("\nCancelled.")
            return 1
        if choice not in ('y', 'yes'):
            print("Self-repair cancelled by user.")
            return 0

    # 7. Backup (allow-listed files only) + journal
    backup_map: Dict[str, str] = {}
    if files_to_repair:
        if os.path.exists(backup_dir):
            shutil.rmtree(backup_dir, ignore_errors=True)
        os.makedirs(backup_dir, exist_ok=True)
        print(f"Creating backup of modified files in {backup_dir}...")
        entries: Dict[str, Any] = {}
        try:
            for rel in mismatches:
                src = _resolve_target(app_dir, rel)
                _assert_no_symlink(app_dir, rel)
                if os.path.isfile(src):
                    dst = os.path.join(backup_dir, rel)
                    os.makedirs(os.path.dirname(dst), exist_ok=True)
                    shutil.copy2(src, dst)
                    backup_map[rel] = dst
                    entries[rel] = {'existed': True,
                                    'backup_sha256': manifest_mod.sha256_file(dst)}
            for rel in missing:
                entries[rel] = {'existed': False}
        except SelfRepairError as e:
            print(f"error: refusing to modify files: {e}", file=sys.stderr)
            shutil.rmtree(backup_dir, ignore_errors=True)
            return 1
        _atomic_write(os.path.join(backup_dir, JOURNAL_NAME),
                      json.dumps({'schema': JOURNAL_SCHEMA, 'version': version,
                                  'entries': entries}, indent=2).encode('utf-8'))

    # 8. Download and verify files in memory first
    downloaded: Dict[str, bytes] = {}
    if files_to_repair:
        print("Downloading verified replacements from GitHub...")
        try:
            for rel in files_to_repair:
                exp_hash = manifest['files'][rel]
                downloaded[rel] = download_verified_file(version, rel, exp_hash)
        except SelfRepairError as e:
            print(f"error during download: {e}", file=sys.stderr)
            print("Aborting. No installed files were modified.")
            shutil.rmtree(backup_dir, ignore_errors=True)
            return 1

    # 9. Stage every payload, then atomically replace (H3)
    staged: Dict[str, str] = {}
    if downloaded:
        print(f"Applying repairs to {len(downloaded)} file(s)...")
        try:
            for rel, data in downloaded.items():
                target = _resolve_target(app_dir, rel)
                _assert_no_symlink(app_dir, rel)
                os.makedirs(os.path.dirname(target), exist_ok=True)
                fd, tmp = tempfile.mkstemp(
                    prefix=os.path.basename(target) + TMP_SUFFIX + '.',
                    dir=os.path.dirname(target))
                with os.fdopen(fd, 'wb') as f:
                    f.write(data)
                staged[rel] = tmp
        except Exception as e:
            _cleanup_temps(staged)
            _rollback(app_dir, backup_map, missing)
            shutil.rmtree(backup_dir, ignore_errors=True)
            print(f"error: could not stage repairs ({e}); rolled back.", file=sys.stderr)
            return 1

        try:
            for rel, tmp in staged.items():
                os.replace(tmp, _resolve_target(app_dir, rel))
            staged = {}
        except Exception as e:
            _cleanup_temps(staged)
            _rollback(app_dir, backup_map, missing)
            shutil.rmtree(backup_dir, ignore_errors=True)
            print(f"error: replacement failed ({e}); rolled back.", file=sys.stderr)
            return 1

    # 10. Post-repair verification
    if downloaded:
        post_ok, post_mismatches, _post_missing = manifest_mod.verify_manifest(app_dir, manifest)
        if not post_ok:
            print("error: post-repair manifest verification failed! Rolling back...", file=sys.stderr)
            _rollback(app_dir, backup_map, missing)
            shutil.rmtree(backup_dir, ignore_errors=True)
            return 1
        shutil.rmtree(backup_dir, ignore_errors=True)

    # 11. Rust extension reinstall (M3)
    geom_fixed = False
    if geom_needs_repair:
        print("Reinstalling sutura_geom from the verified release wheel...")
        if not install_geom_wheel(version, app_dir):
            print("error: extension not repaired.", file=sys.stderr)
            return 1
        geom_fixed = True

    # A broken extension that could not be reinstalled is always a failure,
    # even when shipped files were repaired (M3: never report success).
    if not geom_ok and not geom_fixed:
        print("error: extension not repaired.", file=sys.stderr)
        return 1

    parts = []
    if downloaded:
        parts.append(f"{len(downloaded)} file(s)")
    if geom_fixed:
        parts.append("sutura_geom")
    print(f"Self-repair successfully restored {', '.join(parts) if parts else 'nothing'}.")
    return 0


def _cleanup_temps(staged: Dict[str, str]) -> None:
    for tmp in staged.values():
        try:
            os.remove(tmp)
        except OSError:
            pass


def run_self_repair_cli(force_yes: bool = False) -> int:
    return run_self_repair(app_dir=APP_DIR, force_yes=force_yes)


if __name__ == '__main__':
    yes = '--yes' in sys.argv or '-y' in sys.argv
    sys.exit(run_self_repair_cli(force_yes=yes))
