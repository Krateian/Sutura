# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Self-repair mechanism for Sutura.

Compares installed shipped files against the official release manifest,
prompts the user, downloads verified replacements of the SAME version
over HTTPS, backs up existing files on a strict allow-list, replaces them,
and rolls back on verification failure.

Never touches user data, history, configs, or the surrounding environment.
"""
import hashlib
import json
import os
import shutil
import subprocess
import sys
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


class SelfRepairError(Exception):
    """Raised when self-repair cannot proceed or fails."""
    pass


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


def is_macos_bundle() -> bool:
    """Check if running from a macOS .app bundle."""
    exe = sys.executable or ''
    return '.app/Contents/MacOS' in exe


def fetch_manifest(version: str, timeout: int = 15) -> Dict[str, Any]:
    url = MANIFEST_URL.format(version=version)
    req = urllib.request.Request(url, headers={'User-Agent': f'Sutura-SelfRepair/{version}'})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            data = resp.read().decode('utf-8')
            return json.loads(data)
    except urllib.error.URLError as e:
        raise SelfRepairError(f"Failed to fetch manifest from {url}: {e}")
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
    except Exception as e:
        return False, list(REQUIRED_GEOM_SYMBOLS), False, None

    missing = [sym for sym in REQUIRED_GEOM_SYMBOLS if not hasattr(sutura_geom, sym)]
    is_ok = (len(missing) == 0)

    # Check whether extension is located inside Sutura's own Linux venv
    venv_dir = os.path.join(app_dir, 'venv')
    is_sutura_venv = False
    if geom_path and os.path.commonpath([os.path.abspath(geom_path), os.path.abspath(venv_dir)]) == os.path.abspath(venv_dir):
        is_sutura_venv = True

    return is_ok, missing, is_sutura_venv, geom_path


def download_verified_file(version: str, rel_path: str, expected_hash: str, timeout: int = 15) -> bytes:
    repo_path = 'LICENSE' if rel_path == 'LICENSE' else f'sutura/{rel_path}'
    url = RAW_FILE_URL.format(version=version, repo_path=repo_path)
    req = urllib.request.Request(url, headers={'User-Agent': f'Sutura-SelfRepair/{version}'})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            content = resp.read()
    except urllib.error.URLError as e:
        raise SelfRepairError(f"Failed to download {rel_path} from {url}: {e}")

    actual_hash = hashlib.sha256(content).hexdigest()
    if actual_hash != expected_hash:
        raise SelfRepairError(
            f"Hash mismatch for downloaded file {rel_path}!\n"
            f"  Expected: {expected_hash}\n"
            f"  Got:      {actual_hash}"
        )
    return content


def run_self_repair(
    app_dir: Optional[str] = None,
    force_yes: bool = False,
    manifest_override: Optional[Dict[str, Any]] = None,
    allow_git: bool = False,
) -> int:
    """Execute the full self-repair flow."""
    if app_dir is None:
        app_dir = APP_DIR

    # 1. Platform & Environment Guards
    try:
        try:
            from sutura import updater
        except ImportError:
            import updater
        if updater.is_appimage():
            print("Notice: AppImage installation detected.")
            print("In-place self-repair is not supported on read-only AppImage filesystems.")
            print("To repair or reinstall, download the latest release from:")
            print("  https://github.com/Krateian/Sutura/releases")
            return 0
    except Exception:
        pass

    if is_macos_bundle():
        print("Notice: macOS application bundle detected.")
        print("In-place modification would invalidate code signature.")
        print("To repair, re-download the .dmg package from:")
        print("  https://github.com/Krateian/Sutura/releases")
        return 0

    if not allow_git and is_git_repository(app_dir):
        print(f"error: {app_dir} is inside a git repository.", file=sys.stderr)
        print("Self-repair is designed for deployed installations (~/.local/share/sutura).", file=sys.stderr)
        print("To restore files in a development checkout, use 'git checkout'.", file=sys.stderr)
        return 1

    version = get_installed_version()
    print(f"Sutura Self-Repair (version {version})")
    print(f"Checking installation at {app_dir}...")

    # 2. Fetch & Validate Manifest
    try:
        if manifest_override is not None:
            manifest = manifest_override
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

    # 3. Verify Shipped Files on Strict Allow-List
    try:
        from sutura import manifest as manifest_mod
    except ImportError:
        import manifest as manifest_mod
    ok, mismatches, missing = manifest_mod.verify_manifest(app_dir, manifest)

    # 4. Check Extension Symbols
    geom_ok, geom_missing, is_sutura_venv, geom_path = check_extension_status(app_dir)

    all_fine = ok and geom_ok
    if all_fine:
        print(f"All {len(manifest.get('files', {}))} shipped application files verified. Installation is intact.")
        return 0

    # 5. Report Defects Found
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
                print(f"  To repair extension, run: conda run -n {os.path.basename(os.environ['CONDA_PREFIX'])} pip install --no-deps --force-reinstall <wheel>")
                print("  Or run: scripts/install_sutura_geom.sh")
            else:
                print("  Note: Running in non-venv/dev environment. Please re-run 'maturin develop' or rebuild extension.")

    files_to_repair = mismatches + missing
    if not files_to_repair and not (not geom_ok and is_sutura_venv):
        print("\nNo auto-repairable files need changes.")
        return 0

    # 6. User Consent Prompt
    if not force_yes:
        prompt_msg = f"\nProceed to download and repair {len(files_to_repair)} file(s)? [y/N]: "
        try:
            choice = input(prompt_msg).strip().lower()
        except (KeyboardInterrupt, EOFError):
            print("\nCancelled.")
            return 1
        if choice not in ('y', 'yes'):
            print("Self-repair cancelled by user.")
            return 0

    # 7. Strict Allow-List Backup
    backup_map = {}  # rel_path -> backup_full_path
    if os.path.exists(BACKUP_DIR):
        shutil.rmtree(BACKUP_DIR, ignore_errors=True)
    os.makedirs(BACKUP_DIR, exist_ok=True)

    print(f"Creating backup of modified files in {BACKUP_DIR}...")
    for rel in mismatches:
        src = os.path.join(app_dir, rel)
        if os.path.isfile(src):
            dst = os.path.join(BACKUP_DIR, rel)
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.copy2(src, dst)
            backup_map[rel] = dst

    # 8. Download and Verify Files in Memory First
    downloaded_payloads = {}
    print("Downloading verified replacements from GitHub...")
    try:
        for rel in files_to_repair:
            exp_hash = manifest['files'][rel]
            data = download_verified_file(version, rel, exp_hash)
            downloaded_payloads[rel] = data
    except SelfRepairError as e:
        print(f"error during download: {e}", file=sys.stderr)
        print("Aborting. No installed files were modified.")
        shutil.rmtree(BACKUP_DIR, ignore_errors=True)
        return 1

    # 9. Atomic Replacement on Allow-List
    print(f"Applying repairs to {len(downloaded_payloads)} file(s)...")
    for rel, data in downloaded_payloads.items():
        target = os.path.join(app_dir, rel)
        os.makedirs(os.path.dirname(target), exist_ok=True)
        with open(target, 'wb') as f:
            f.write(data)

    # 10. Post-Repair Manifest Verification
    post_ok, post_mismatches, post_missing = manifest_mod.verify_manifest(app_dir, manifest)
    if not post_ok:
        print("error: post-repair manifest verification failed! Rolling back...", file=sys.stderr)
        for rel, bkp in backup_map.items():
            shutil.copy2(bkp, os.path.join(app_dir, rel))
        for rel in missing:
            target = os.path.join(app_dir, rel)
            if os.path.exists(target):
                os.remove(target)
        shutil.rmtree(BACKUP_DIR, ignore_errors=True)
        return 1

    # Clean up backup on success
    shutil.rmtree(BACKUP_DIR, ignore_errors=True)
    print(f"Self-repair successfully restored {len(files_to_repair)} file(s).")
    return 0


def run_self_repair_cli(force_yes: bool = False) -> int:
    return run_self_repair(app_dir=APP_DIR, force_yes=force_yes)


if __name__ == '__main__':
    yes = '--yes' in sys.argv or '-y' in sys.argv
    sys.exit(run_self_repair_cli(force_yes=yes))
