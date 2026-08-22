#!/usr/bin/env python3
"""Update check and self-update for Sutura.

Offline-first: no network request is made unless the user has opted in
(config.json check_for_updates=true) and at least 7 days have passed since
the last check. On update, the previous install is backed up, the new source
is installed, and a self-check runs; if it fails the backup is restored.

Platforms:
  * Linux:  install.sh logic (pip + venv). requirements hash changes trigger
            a pip install in the venv.
  * macOS:  install-macos.sh logic (conda env). Only the Python source files
            are copied; if requirements changed we re-run the conda-forge
            install (never pip into the conda env). The conda env is not
            rolled back - only the files are restored - and the user is told
            to re-run install-macos.sh if dependency versions may have moved.

Assumptions made for macOS (cannot be tested from this Linux box; verify by
hand on a Mac):
  * ~/.local/share/sutura is the install dir on both platforms (the
    install-macos.sh copies files there).
  * The conda env is named `sutura-env` (as in install-macos.sh); we only
    snapshot it with `conda list`, never modify it ourselves.
"""
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
import urllib.error

CONFIG_DIR = os.path.expanduser('~/.config/sutura')
CONFIG_PATH = os.path.join(CONFIG_DIR, 'config.json')
APP_DIR = os.path.expanduser('~/.local/share/sutura')
BACKUP_DIR = os.path.expanduser('~/.local/share/sutura.backup')
GITHUB_API = 'https://api.github.com/repos/Krateian/Sutura/releases/latest'
CHECK_INTERVAL_SECONDS = 7 * 24 * 60 * 60

APPIMAGE_RELEASE_URL = 'https://github.com/Krateian/Sutura/releases'


def is_appimage():
    """True when running inside an AppImage.

    The AppImage runtime exports APPIMAGE (path to the AppImage file) and
    APPDIR (its mount point) to every launched process, so a bundled app can
    tell it is running from an AppImage. A self-contained AppImage cannot be
    updated in place (its payload is a read-only squashfs), so callers use
    this to disable the self-update flow and point at the releases page.
    """
    return bool(os.environ.get('APPIMAGE'))

# single source of truth: read from repair.py (works both as a package and as
# a standalone file next to updater.py in the install/AppImage dir)
_HERE = os.path.dirname(os.path.abspath(__file__))

try:
    from sutura import VERSION
except ImportError:
    import importlib.util
    VERSION = 'unknown'
    for _candidate in (os.path.join(_HERE, 'repair.py'),
                       os.path.join(APP_DIR, 'repair.py')):
        if os.path.isfile(_candidate):
            _spec = importlib.util.spec_from_file_location(
                'sutura_repair', _candidate)
            _mod = importlib.util.module_from_spec(_spec)
            _spec.loader.exec_module(_mod)
            VERSION = _mod.VERSION
            break

DEFAULT_CONFIG = {
    'check_for_updates': False,
    'last_check': None,
    'last_known_version': None,
}


# ---------------------------------------------------------------- config

def load_config():
    if os.path.exists(CONFIG_PATH):
        try:
            with open(CONFIG_PATH) as f:
                data = json.load(f)
            cfg = dict(DEFAULT_CONFIG)
            cfg.update(data)
            return cfg
        except Exception:
            pass
    return dict(DEFAULT_CONFIG)


def save_config(cfg):
    os.makedirs(CONFIG_DIR, exist_ok=True)
    with open(CONFIG_PATH, 'w') as f:
        json.dump(cfg, f, indent=2)


def config_exists():
    return os.path.exists(CONFIG_PATH)


def opt_in_check_updates():
    cfg = load_config()
    cfg['check_for_updates'] = True
    cfg['last_check'] = time.time()
    save_config(cfg)


# ---------------------------------------------------------------- semver

# Prerelease ordering rank: any stable release sorts after any prerelease of
# the same (major, minor, patch), so a beta tester is offered the eventual
# stable release even though both share the same core version.
_PRE_BETA = 0
_PRE_STABLE = 1


def parse_version(tag):
    """Parse a tag like 'v0.1.2' or 'v0.1.8-beta.1' into a sortable tuple.

    Returns (major, minor, patch, prerelease_rank, prerelease_id). A stable
    release carries the sentinel rank and therefore compares NEWER than any
    prerelease of the same version (v0.1.8 > v0.1.8-beta.1), so an update
    check on a beta build offers the stable release once it exists. Returns
    None on a malformed version."""
    m = re.match(r'^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.\-]+))?$', tag)
    if not m:
        return None
    major, minor, patch = (int(m.group(i)) for i in (1, 2, 3))
    pre = m.group(4)
    if pre is None:
        return (major, minor, patch, _PRE_STABLE, '')
    return (major, minor, patch, _PRE_BETA, pre)


def is_newer(tag, current):
    a = parse_version(tag)
    b = parse_version(current)
    if a is None or b is None:
        return False
    return a > b


# ---------------------------------------------------------------- github

def fetch_latest_release():
    """Return the latest release/tag, or None on any network/parse error.

    Prefers /releases/latest; falls back to the newest tag via /tags when no
    release has been published yet (we tag but may not publish a release)."""
    try:
        req = urllib.request.Request(GITHUB_API, headers={'User-Agent': 'sutura-updater'})
        with urllib.request.urlopen(req, timeout=15) as resp:
            data = json.loads(resp.read().decode())
        return data.get('tag_name')
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return _latest_tag()
        return None
    except Exception:
        return None


def _latest_tag():
    url = 'https://api.github.com/repos/Krateian/Sutura/tags'
    try:
        req = urllib.request.Request(url, headers={'User-Agent': 'sutura-updater'})
        with urllib.request.urlopen(req, timeout=15) as resp:
            tags = json.loads(resp.read().decode())
        for t in tags:
            name = t.get('name', '')
            # skip prerelease tags (e.g. v0.1.8-beta.1) so the fallback never
            # surfaces a beta to a normal user
            if name.startswith('v') and '-' not in name:
                return name
        return None
    except Exception:
        return None


def should_check(cfg):
    if not cfg.get('check_for_updates'):
        return False
    last = cfg.get('last_check')
    if last is None:
        return True
    return (time.time() - last) >= CHECK_INTERVAL_SECONDS


def check_for_update(force=False):
    """Run the check if enabled and due (or forced). Returns (new_tag|None,
    cfg). Marks last_check only when a check actually ran.

    Disabled entirely for AppImage builds: the update flow cannot reinstall
    into a read-only AppImage payload."""
    cfg = load_config()
    if is_appimage():
        return None, cfg
    if not force and not should_check(cfg):
        return None, cfg
    try:
        latest = fetch_latest_release()
    except Exception:
        # transient network issue - don't nag, keep last_known_version
        cfg['last_check'] = time.time()
        save_config(cfg)
        return None, cfg
    cfg['last_check'] = time.time()
    if latest and is_newer(latest, VERSION):
        cfg['last_known_version'] = latest
    else:
        cfg['last_known_version'] = None
    save_config(cfg)
    return (latest if latest and is_newer(latest, VERSION) else None), cfg


# ---------------------------------------------------------------- download

def download_source(tag, dest_dir):
    """Download the source tarball for a tag and extract into dest_dir."""
    url = 'https://github.com/Krateian/Sutura/archive/refs/tags/%s.tar.gz' % tag
    tmp_tar = os.path.join(dest_dir, 'src.tar.gz')
    req = urllib.request.Request(url, headers={'User-Agent': 'sutura-updater'})
    with urllib.request.urlopen(req, timeout=60) as resp:
        with open(tmp_tar, 'wb') as f:
            shutil.copyfileobj(resp, f)
    # the archive extracts to a single <repo>-<tag>/ directory
    extract = os.path.join(dest_dir, 'src')
    os.makedirs(extract, exist_ok=True)
    subprocess.run(['tar', 'xzf', tmp_tar, '-C', extract], check=True)
    top = os.path.join(extract, os.listdir(extract)[0])
    return top


# ---------------------------------------------------------------- hashes

def _file_hash(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(65536), b''):
            h.update(chunk)
    return h.hexdigest()


def requirements_changed(src_dir, req_files):
    """Return the subset of req_files whose hash differs from the installed
    copies. Installed requirements live next to APP_DIR/repair.py in the
    install dir (install.sh copies them as requirements.txt)."""
    changed = []
    for name in req_files:
        new = os.path.join(src_dir, name)
        old = os.path.join(APP_DIR, name)
        if not os.path.exists(old):
            changed.append(name)
            continue
        try:
            if _file_hash(new) != _file_hash(old):
                changed.append(name)
        except Exception:
            changed.append(name)
    return changed


# ---------------------------------------------------------------- install

def _copy_python_files(src_dir):
    for f in ('repair.py', 'manifold_bridge.py', 'classification.py',
              'defects.py', 'mesh_classifier.py', 'updater.py', 'gui.py',
              'heatmap.py', 'heatmap_render.py', 'before_after_render.py',
              '__init__.py'):
        shutil.copy2(os.path.join(src_dir, 'sutura', f), os.path.join(APP_DIR, f))


def _install_linux(src_dir, req_files):
    """install.sh-style copy + venv update only when requirements change."""
    sutura_src = os.path.join(src_dir, 'sutura')
    for f in ('repair.py', 'manifold_bridge.py', 'classification.py',
              'defects.py', 'mesh_classifier.py', 'updater.py', 'gui.py',
              'heatmap.py', 'heatmap_render.py', 'before_after_render.py',
              '__init__.py', 'open.sh'):
        shutil.copy2(os.path.join(sutura_src, f), os.path.join(APP_DIR, f))
    for f in ('install.sh', 'uninstall.sh'):
        src = os.path.join(src_dir, f)
        if os.path.exists(src):
            shutil.copy2(src, os.path.join(APP_DIR, f))
    for f in ('requirements.txt', 'requirements-gui.txt', 'requirements-311.txt'):
        src = os.path.join(src_dir, f)
        if os.path.exists(src):
            shutil.copy2(src, os.path.join(APP_DIR, f))
    changed = requirements_changed(src_dir, ('requirements.txt', 'requirements-gui.txt'))
    if changed:
        venv_py = os.path.join(APP_DIR, 'venv', 'bin', 'python')
        if os.path.exists(venv_py):
            for req in changed:
                subprocess.run([venv_py, '-m', 'pip', 'install', '--quiet',
                                '-r', os.path.join(APP_DIR, req)], check=True)


def _install_macos(src_dir, req_files):
    """Copy Python files; if requirements changed, re-run the conda-forge
    install (never pip into the conda env). Snapshot the env for diagnostics.
    """
    _copy_python_files(src_dir)
    env_name = os.environ.get('SUTURA_ENV', 'sutura-env')
    if shutil.which('conda'):
        try:
            with open(os.path.join(BACKUP_DIR, 'conda-env-snapshot.txt'), 'w') as f:
                subprocess.run(['conda', 'list', '-n', env_name], stdout=f, check=False)
        except Exception:
            pass
    changed = requirements_changed(src_dir, ('requirements.txt',))
    if changed:
        # conda-forge only; pymeshlab cannot come from pip on Apple Silicon.
        subprocess.run(['conda', 'install', '-y', '-n', env_name, '-c', 'conda-forge',
                        'pymeshlab'], check=True)
        # pip-only deps inside the conda env (manifold3d/trimesh/PySide6)
        subprocess.run(['conda', 'run', '-n', env_name, 'pip', 'install',
                        'manifold3d', 'trimesh', 'PySide6-Essentials'], check=True)


def install_source(src_dir):
    """Copy the new source into place and refresh dependencies if needed."""
    req_files = ('requirements.txt', 'requirements-gui.txt', 'requirements-311.txt')
    if sys.platform == 'darwin':
        _install_macos(src_dir, req_files)
    else:
        _install_linux(src_dir, req_files)


# ---------------------------------------------------------------- health

def _make_test_stl(path):
    """A cube with a missing face and one inverted winding - a repair target."""
    import struct
    V = {'lb': (-1, -1, -1), 'rb': (1, -1, -1), 'rt': (1, 1, -1), 'lt': (-1, 1, -1),
         'lT': (-1, -1, 1), 'rT': (1, -1, 1), 'RT': (1, 1, 1), 'LT': (-1, 1, 1)}

    def tri(a, b, c):
        return (V[a], V[b], V[c])

    faces = [
        tri('lb', 'rb', 'rt'), tri('lb', 'rt', 'lt'),   # bottom
        tri('lT', 'LT', 'RT'), tri('lT', 'RT', 'rT'),   # top (removed)
        tri('lb', 'lT', 'rT'), tri('lb', 'rT', 'rb'),   # back
        tri('rb', 'rT', 'RT'), tri('rb', 'RT', 'rt'),   # right
        tri('rt', 'RT', 'LT'), tri('rt', 'LT', 'lt'),   # front
        tri('lt', 'LT', 'lT'), tri('lt', 'lT', 'lb'),   # left
    ]
    tris = faces[:2] + faces[4:]                        # no top -> a hole
    tris[2] = (tris[2][2], tris[2][1], tris[2][0])      # inverted winding
    with open(path, 'wb') as f:
        f.write(b'health'.ljust(80, b'\0'))
        f.write(struct.pack('<I', len(tris)))
        for a, b, c in tris:
            f.write(struct.pack('<3f', 0, 0, 0))
            f.write(struct.pack('<3f', *a))
            f.write(struct.pack('<3f', *b))
            f.write(struct.pack('<3f', *c))
            f.write(struct.pack('<H', 0))


def health_check(expected_version):
    """Return (ok, message). Verifies --version and a real repair round-trip."""
    want = expected_version.lstrip('v')
    wrapper = os.path.expanduser('~/.local/bin/sutura')
    # version
    try:
        r = subprocess.run([wrapper, '--version'], capture_output=True, text=True, timeout=30)
        if r.returncode != 0 or want not in r.stdout:
            return False, 'version mismatch: %s' % r.stdout.strip()
    except Exception as e:
        return False, 'version check failed: %s' % e
    # repair round-trip
    tmp = tempfile.mkdtemp(prefix='sutura-health-')
    try:
        test_stl = os.path.join(tmp, 'test.stl')
        _make_test_stl(test_stl)
        r = subprocess.run([wrapper, test_stl], capture_output=True, text=True, timeout=120)
        if r.returncode != 0:
            return False, 'repair failed (exit %d): %s' % (r.returncode, r.stdout[-200:])
        import json
        rep = json.loads(r.stdout.strip().splitlines()[-1])
        if not rep.get('stage1', {}).get('two_manifold'):
            return False, 'repair did not yield a two-manifold result'
    except Exception as e:
        return False, 'health check error: %s' % e
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    return True, 'ok'


# ---------------------------------------------------------------- backup / rollback

def backup_install():
    if os.path.exists(BACKUP_DIR):
        shutil.rmtree(BACKUP_DIR)
    shutil.copytree(APP_DIR, BACKUP_DIR, symlinks=True)


def rollback(previous_version):
    if os.path.exists(APP_DIR):
        shutil.rmtree(APP_DIR)
    shutil.copytree(BACKUP_DIR, APP_DIR, symlinks=True)
    return previous_version


# ---------------------------------------------------------------- top-level

def perform_update(tag, progress=None):
    """Backup, download, install, health-check; roll back on failure.

    Returns (ok, message, previous_version). progress is an optional
    callable(status: str).

    Not supported for AppImage builds: the payload is a read-only squashfs,
    so the caller should point the user at the GitHub releases page instead.
    """
    if is_appimage():
        return (False,
                'The AppImage build cannot update itself. Download the latest '
                'AppImage from %s' % APPIMAGE_RELEASE_URL, VERSION)
    previous = VERSION
    if progress:
        progress('backing up')
    backup_install()
    try:
        tmp = tempfile.mkdtemp(prefix='sutura-update-')
        try:
            if progress:
                progress('downloading')
            src = download_source(tag, tmp)
            if progress:
                progress('installing')
            install_source(src)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)
        if progress:
            progress('verifying')
        ok, msg = health_check(tag)
        if not ok:
            raise RuntimeError(msg)
    except Exception as e:
        # roll back - keep the backup for diagnostics
        rollback(previous)
        return False, 'update failed, rolled back to v%s. Detail: %s' % (previous, e), previous
    return True, 'updated to %s' % tag, previous
