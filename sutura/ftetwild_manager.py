#!/usr/bin/env python3
"""fTetWild installation and removal manager backend for Sutura.

Manages the optional fTetWild fallback tier (pytetwild + pyvista + vtk)
for Sutura without touching the system Python. Strictly supports only the two
canonical Sutura environments:
  1. Linux: $APP_DIR/venv311/bin/pip (created by install.sh)
  2. macOS: conda 'sutura-env' bin/pip (created by install-macos.sh)

All other layouts (AppImage, frozen .app/.dmg bundles, system Python,
unmanaged virtual environments, read-only locations) are detected and flagged
as unsupported with clear user instructions.

Stdlib-only: uses subprocess (without shell=True), importlib.metadata, and signal.
"""
import importlib.metadata
import json
import os
import re
import select
import signal
import subprocess
import sys

UNSUPPORTED_MSG = 'unsupported here, use install.sh SUTURA_WITH_FTETWILD=1'
UNSUPPORTED_MACOS_MSG = 'unsupported here, use install-macos.sh SUTURA_WITH_FTETWILD=1'

# Baseline package specifications for the fTetWild extra
DEFAULT_FTETWILD_PACKAGES = ['pytetwild==0.4.2', 'pyvista>=0.44']
FTETWILD_ROOT_NAMES = {'pytetwild', 'pyvista'}

# Known base packages that must never be removed on uninstall
CANONICAL_BASE_PACKAGES = {
    'pymeshlab',
    'manifold3d',
    'trimesh',
    'numpy',
    'pyside6',
    'pyside6-essentials',
    'pyrobust-predicates',
    'pip',
    'setuptools',
    'wheel',
}

# Real measured size constants
# macOS arm64: measured directly on Apple Silicon conda sutura-env
MAC_ARM64_DOWNLOAD_BYTES = 110_100_480   # ~105 MB (vtk 98.2M, pyvista 2.6M, pytetwild 2.3M, deps ~2M)
MAC_ARM64_INSTALLED_BYTES = 564_133_888  # ~538 MB (vtkmodules 517.1M, pyvista 13.5M, pytetwild 5.9M, deps ~1.5M)

# Linux x86_64: measured with 'pip download --no-deps' for manylinux2014_x86_64
LINUX_X86_64_DOWNLOAD_BYTES = 145_752_064   # ~139 MB (vtk 133M, pytetwild 3.0M, pyvista 2.6M, deps ~2M)
LINUX_X86_64_INSTALLED_BYTES = 608_174_080  # ~580 MB uncompressed wheel payload (up to ~1.1 GB historically)

# Fallback default estimate for generic environments
DEFAULT_DOWNLOAD_BYTES = 146_800_640        # ~140 MB
DEFAULT_INSTALLED_BYTES = 1_181_116_006     # ~1.1 GB (VTK uncompressed + caches)


def format_bytes(num_bytes):
    """Format bytes into a human-readable string (e.g. '538 MB', '1.1 GB')."""
    if num_bytes is None or num_bytes < 0:
        return '0 B'
    if num_bytes < 1024:
        return '%d B' % num_bytes
    if num_bytes < 1024 * 1024:
        return '%.1f KB' % (num_bytes / 1024)
    if num_bytes < 1024 * 1024 * 1024:
        return '%.1f MB' % (num_bytes / (1024 * 1024))
    return '%.2f GB' % (num_bytes / (1024 * 1024 * 1024))


def _normalize_name(name):
    """Normalize package name to lowercase with hyphens."""
    return re.sub(r'[-_.]+', '-', name).lower()


def _find_repo_root():
    """Locate the Sutura repository root if running from a source checkout."""
    cur = os.path.dirname(os.path.abspath(__file__))
    for _ in range(3):
        if os.path.isfile(os.path.join(cur, 'requirements-ftetwild.txt')):
            return cur
        parent = os.path.dirname(cur)
        if parent == cur:
            break
        cur = parent
    return None


def _find_requirements_file(filename='requirements-ftetwild.txt'):
    """Locate a requirement file in repo or Sutura app directory."""
    root = _find_repo_root()
    if root:
        cand = os.path.join(root, filename)
        if os.path.isfile(cand):
            return cand
    app_dir = os.environ.get('SUTURA_DIR', os.path.expanduser('~/.local/share/sutura'))
    cand = os.path.join(app_dir, filename)
    if os.path.isfile(cand):
        return cand
    cand_repo = os.path.join(app_dir, 'repo', filename)
    if os.path.isfile(cand_repo):
        return cand_repo
    return None


def _parse_requirements_file(filepath):
    """Extract package names from a requirements.txt file."""
    names = set()
    if not filepath or not os.path.isfile(filepath):
        return names
    try:
        with open(filepath, 'r', encoding='utf-8') as f:
            for line in f:
                line = line.strip()
                if not line or line.startswith('#'):
                    continue
                # Extract the leading package name before version specifiers
                m = re.match(r'^([A-Za-z0-9_.-]+)', line)
                if m:
                    names.add(_normalize_name(m.group(1)))
    except OSError:
        pass
    return names


def _get_base_package_names():
    """Return the set of package names required by base Sutura."""
    names = set(CANONICAL_BASE_PACKAGES)
    for fname in ('requirements.txt', 'requirements-gui.txt', 'requirements-311.txt'):
        fpath = _find_requirements_file(fname)
        if fpath:
            names.update(_parse_requirements_file(fpath))
    return names


def _find_conda_sutura_env(conda_env_dir=None, environ=None, sys_prefix=None):
    """Find the conda sutura-env directory on macOS."""
    if conda_env_dir is not None:
        return conda_env_dir if os.path.isdir(conda_env_dir) else None

    prefix = sys_prefix if sys_prefix is not None else sys.prefix

    # 1. If currently executing inside sutura-env
    if 'sutura-env' in prefix:
        return prefix

    env = environ if environ is not None else os.environ

    # 2. Check standard Miniforge / Miniconda / Anaconda locations
    candidates = [
        '/opt/homebrew/Caskroom/miniforge/base/envs/sutura-env',
        os.path.expanduser('~/.conda/envs/sutura-env'),
        os.path.expanduser('~/miniforge3/envs/sutura-env'),
        os.path.expanduser('~/miniconda3/envs/sutura-env'),
        os.path.expanduser('~/anaconda3/envs/sutura-env'),
    ]
    for cand in candidates:
        if os.path.isdir(cand) and os.path.isfile(os.path.join(cand, 'bin', 'python')):
            return cand

    # 3. Query conda CLI if available
    try:
        r = subprocess.run(['conda', 'info', '--envs', '--json'],
                           capture_output=True, text=True, timeout=5)
        if r.returncode == 0:
            data = json.loads(r.stdout)
            for path in data.get('envs', []):
                if os.path.basename(path) == 'sutura-env' and os.path.isdir(path):
                    return path
    except (subprocess.SubprocessError, FileNotFoundError, json.JSONDecodeError):
        pass

    return None


def resolve_environment(app_dir=None, platform=None, conda_env_dir=None,
                        environ=None, sys_executable=None, sys_prefix=None,
                        is_frozen=None):
    """Resolve and validate the target Sutura environment.

    Only two environments are supported:
      - Linux: $APP_DIR/venv311/bin/pip (install.sh)
      - macOS: conda 'sutura-env' (install-macos.sh)

    Returns a dict with:
      - supported: bool
      - layout: 'linux-venv311' | 'macos-conda' | 'unsupported'
      - reason: None or error message
      - python_path: path to python executable
      - pip_path: path to pip executable
      - site_packages: path to site-packages dir
      - writable: bool
    """
    env = environ if environ is not None else os.environ
    plat = platform if platform is not None else sys.platform
    frozen = is_frozen if is_frozen is not None else getattr(sys, 'frozen', False)

    # Detect unsupported packaging modes (AppImage, frozen .app/.dmg)
    if env.get('APPIMAGE'):
        return {
            'supported': False,
            'layout': 'unsupported',
            'reason': UNSUPPORTED_MSG,
            'python_path': None,
            'pip_path': None,
            'site_packages': None,
            'writable': False,
        }

    if frozen:
        msg = UNSUPPORTED_MACOS_MSG if plat == 'darwin' else UNSUPPORTED_MSG
        return {
            'supported': False,
            'layout': 'unsupported',
            'reason': msg,
            'python_path': None,
            'pip_path': None,
            'site_packages': None,
            'writable': False,
        }

    # macOS layout: conda 'sutura-env'
    if plat == 'darwin':
        env_dir = _find_conda_sutura_env(conda_env_dir, environ=env, sys_prefix=sys_prefix)
        if not env_dir:
            return {
                'supported': False,
                'layout': 'unsupported',
                'reason': UNSUPPORTED_MACOS_MSG,
                'python_path': None,
                'pip_path': None,
                'site_packages': None,
                'writable': False,
            }

        py_path = os.path.join(env_dir, 'bin', 'python')
        pip_path = os.path.join(env_dir, 'bin', 'pip')
        if not os.path.isfile(py_path) or not os.path.isfile(pip_path):
            return {
                'supported': False,
                'layout': 'unsupported',
                'reason': UNSUPPORTED_MACOS_MSG,
                'python_path': None,
                'pip_path': None,
                'site_packages': None,
                'writable': False,
            }

        # Find site-packages
        site_packages = None
        lib_dir = os.path.join(env_dir, 'lib')
        if os.path.isdir(lib_dir):
            for entry in os.listdir(lib_dir):
                if entry.startswith('python3'):
                    sp = os.path.join(lib_dir, entry, 'site-packages')
                    if os.path.isdir(sp):
                        site_packages = sp
                        break

        writable = os.access(site_packages or env_dir, os.W_OK)
        if not writable:
            return {
                'supported': False,
                'layout': 'unsupported',
                'reason': '%s (read-only environment)' % UNSUPPORTED_MACOS_MSG,
                'python_path': py_path,
                'pip_path': pip_path,
                'site_packages': site_packages,
                'writable': False,
            }

        return {
            'supported': True,
            'layout': 'macos-conda',
            'reason': None,
            'python_path': py_path,
            'pip_path': pip_path,
            'site_packages': site_packages,
            'writable': True,
        }

    # Linux layout: $APP_DIR/venv311
    if plat.startswith('linux'):
        target_app_dir = app_dir or env.get('SUTURA_DIR', os.path.expanduser('~/.local/share/sutura'))
        venv311 = os.path.join(target_app_dir, 'venv311')
        py_path = os.path.join(venv311, 'bin', 'python')
        pip_path = os.path.join(venv311, 'bin', 'pip')

        if not os.path.isfile(py_path) or not os.path.isfile(pip_path):
            return {
                'supported': False,
                'layout': 'unsupported',
                'reason': UNSUPPORTED_MSG,
                'python_path': None,
                'pip_path': None,
                'site_packages': None,
                'writable': False,
            }

        site_packages = None
        lib_dir = os.path.join(venv311, 'lib')
        if os.path.isdir(lib_dir):
            for entry in os.listdir(lib_dir):
                if entry.startswith('python3'):
                    sp = os.path.join(lib_dir, entry, 'site-packages')
                    if os.path.isdir(sp):
                        site_packages = sp
                        break

        writable = os.access(site_packages or venv311, os.W_OK)
        if not writable:
            return {
                'supported': False,
                'layout': 'unsupported',
                'reason': '%s (read-only environment)' % UNSUPPORTED_MSG,
                'python_path': py_path,
                'pip_path': pip_path,
                'site_packages': site_packages,
                'writable': False,
            }

        return {
            'supported': True,
            'layout': 'linux-venv311',
            'reason': None,
            'python_path': py_path,
            'pip_path': pip_path,
            'site_packages': site_packages,
            'writable': True,
        }

    # Any other OS
    return {
        'supported': False,
        'layout': 'unsupported',
        'reason': UNSUPPORTED_MSG,
        'python_path': None,
        'pip_path': None,
        'site_packages': None,
        'writable': False,
    }


def _calc_dist_size(dist):
    """Calculate the total installed file size in bytes for a distribution."""
    total = 0
    if dist.files:
        for f in dist.files:
            try:
                p = dist.locate_file(f)
                if p.is_file():
                    total += p.stat().st_size
            except (OSError, AttributeError):
                pass
    return total


def compute_ftetwild_removal_set(env_info=None):
    """Compute the exact list of packages to remove for an fTetWild uninstall.

    Rules:
      - Packages must have been introduced by requirements-ftetwild.txt (or its deps).
      - Packages must NOT be needed by base requirements (directly or transitively).
      - Packages must NOT be required by any remaining package in the environment.

    Returns:
      (packages_to_remove: list[str], freed_bytes: int)
    """
    if env_info is None:
        env_info = resolve_environment()

    sp = env_info.get('site_packages')
    if not sp or not os.path.isdir(sp):
        return [], 0

    dists = list(importlib.metadata.distributions(path=[sp]))
    dist_map = {}
    for d in dists:
        raw_name = d.metadata.get('Name') if d.metadata else None
        if raw_name:
            dist_map[_normalize_name(raw_name)] = d

    # Build dependency maps
    requires = {}
    required_by = {k: set() for k in dist_map}

    for name, d in dist_map.items():
        reqs = set()
        if d.requires:
            for r in d.requires:
                m = re.match(r'^([A-Za-z0-9_.-]+)', r.strip())
                if m:
                    dep = _normalize_name(m.group(1))
                    # Ignore optional extras when determining mandatory deps
                    if 'extra ==' not in r:
                        reqs.add(dep)
                        if dep in required_by:
                            required_by[dep].add(name)
        requires[name] = reqs

    # 1. Base requirements and everything transitively needed by base
    base_names = _get_base_package_names()
    needed_by_base = set()
    queue = [p for p in base_names if p in dist_map]
    while queue:
        cur = queue.pop()
        if cur not in needed_by_base:
            needed_by_base.add(cur)
            for dep in requires.get(cur, ()):
                if dep in dist_map and dep not in needed_by_base:
                    queue.append(dep)

    # 2. Collect packages reachable from fTetWild roots
    ftetwild_req_file = _find_requirements_file('requirements-ftetwild.txt')
    ft_roots = _parse_requirements_file(ftetwild_req_file) or FTETWILD_ROOT_NAMES

    ftetwild_candidates = set()
    queue = [p for p in ft_roots if p in dist_map]
    while queue:
        cur = queue.pop()
        if cur not in ftetwild_candidates:
            ftetwild_candidates.add(cur)
            for dep in requires.get(cur, ()):
                if dep in dist_map and dep not in ftetwild_candidates and dep not in needed_by_base:
                    queue.append(dep)

    # 3. Prune candidate packages that are required by any package outside candidate set
    to_remove = set(ftetwild_candidates)
    changed = True
    while changed:
        changed = False
        for pkg in list(to_remove):
            external_dependents = required_by.get(pkg, set()) - to_remove
            if external_dependents or pkg in needed_by_base:
                to_remove.remove(pkg)
                changed = True

    # Sort packages so dependents are removed before their prerequisites
    sorted_removal = sorted(to_remove)

    # Compute freed bytes
    freed_bytes = 0
    for pkg in sorted_removal:
        d = dist_map.get(pkg)
        if d:
            freed_bytes += _calc_dist_size(d)

    return sorted_removal, freed_bytes


def status(env_info=None):
    """Return the current status of the fTetWild extra.

    Returns dict:
      - installed: bool
      - supported: bool
      - version: str or None
      - location: str or None
      - size_bytes: int
      - size_human: str
      - layout: str
      - reason: str or None
    """
    if env_info is None:
        env_info = resolve_environment()

    if not env_info.get('supported'):
        return {
            'installed': False,
            'supported': False,
            'version': None,
            'location': None,
            'size_bytes': 0,
            'size_human': '0 B',
            'layout': env_info.get('layout', 'unsupported'),
            'reason': env_info.get('reason'),
        }

    sp = env_info.get('site_packages')
    if not sp or not os.path.isdir(sp):
        return {
            'installed': False,
            'supported': True,
            'version': None,
            'location': None,
            'size_bytes': 0,
            'size_human': '0 B',
            'layout': env_info.get('layout'),
            'reason': None,
        }

    dists = list(importlib.metadata.distributions(path=[sp]))
    dist_map = {}
    for d in dists:
        raw_name = d.metadata.get('Name') if d.metadata else None
        if raw_name:
            dist_map[_normalize_name(raw_name)] = d

    pytetwild_dist = dist_map.get('pytetwild')
    pyvista_dist = dist_map.get('pyvista')

    if not pytetwild_dist or not pyvista_dist:
        return {
            'installed': False,
            'supported': True,
            'version': None,
            'location': sp,
            'size_bytes': 0,
            'size_human': '0 B',
            'layout': env_info.get('layout'),
            'reason': None,
        }

    # Both pytetwild and pyvista are installed
    version = pytetwild_dist.version
    removal_set, total_bytes = compute_ftetwild_removal_set(env_info)

    # Fallback to pytetwild + pyvista + vtk size if removal_set calculation yielded 0
    if total_bytes == 0:
        for pkg_name in ('pytetwild', 'pyvista', 'vtk', 'pyvista-validation'):
            d = dist_map.get(pkg_name)
            if d:
                total_bytes += _calc_dist_size(d)

    return {
        'installed': True,
        'supported': True,
        'version': version,
        'location': sp,
        'size_bytes': total_bytes,
        'size_human': format_bytes(total_bytes),
        'layout': env_info.get('layout'),
        'reason': None,
    }


def estimate(platform=None, arch=None):
    """Return download and installed size estimates for fTetWild.

    Estimates are calibrated against real measurements:
      - macOS arm64: measured directly on Apple Silicon (~105 MB dl / ~538 MB disk)
      - Linux x86_64: measured with pip download --no-deps (~139 MB dl / ~580 MB disk, up to ~1.1 GB)

    Returns dict with byte counts and formatted strings.
    """
    plat = platform if platform is not None else sys.platform

    if plat == 'darwin':
        dl = MAC_ARM64_DOWNLOAD_BYTES
        disk = MAC_ARM64_INSTALLED_BYTES
        note = 'Calibrated on macOS Apple Silicon (VTK 517 MB, pyvista 13.5 MB, pytetwild 5.9 MB)'
    elif plat.startswith('linux'):
        dl = LINUX_X86_64_DOWNLOAD_BYTES
        disk = LINUX_X86_64_INSTALLED_BYTES
        note = 'Measured on Linux x86_64 manylinux wheels (~580 MB uncompressed, up to 1.1 GB with caches)'
    else:
        dl = DEFAULT_DOWNLOAD_BYTES
        disk = DEFAULT_INSTALLED_BYTES
        note = 'Generic estimate (~1.1 GB installed)'

    return {
        'download_bytes': dl,
        'download_human': format_bytes(dl),
        'installed_bytes': disk,
        'installed_human': format_bytes(disk),
        'platform': plat,
        'notes': note,
    }


def _kill_process_group(proc):
    """Terminate the entire process group cleanly with SIGTERM then SIGKILL."""
    try:
        pgid = os.getpgid(proc.pid)
        os.killpg(pgid, signal.SIGTERM)
        try:
            proc.wait(timeout=2.0)
        except subprocess.TimeoutExpired:
            os.killpg(pgid, signal.SIGKILL)
            proc.wait(timeout=2.0)
    except (ProcessLookupError, OSError):
        pass


def install(progress_cb=None, cancel_event=None, dry_run=False,
            env_info=None, req_file=None):
    """Install fTetWild dependencies into the target Sutura environment.

    Args:
      progress_cb: callable(str) called with each line of output
      cancel_event: threading.Event or object with is_set() method
      dry_run: if True, only prints/returns the command without running
      env_info: optional environment dict from resolve_environment()
      req_file: optional path to requirements-ftetwild.txt

    Returns dict:
      ok: bool
      command: list[str]
      cancelled: bool
      error: str or None
    """
    if env_info is None:
        env_info = resolve_environment()

    if not env_info.get('supported'):
        return {
            'ok': False,
            'dry_run': dry_run,
            'cancelled': False,
            'error': env_info.get('reason', UNSUPPORTED_MSG),
        }

    pip_cmd = env_info['pip_path']
    rf = req_file or _find_requirements_file('requirements-ftetwild.txt')

    if rf and os.path.isfile(rf):
        cmd = [pip_cmd, 'install', '-r', rf]
    else:
        cmd = [pip_cmd, 'install'] + DEFAULT_FTETWILD_PACKAGES

    if dry_run:
        if progress_cb:
            progress_cb('Dry run: ' + ' '.join(cmd))
        return {
            'ok': True,
            'dry_run': True,
            'command': cmd,
            'cancelled': False,
            'error': None,
        }

    # Live execution via subprocess.Popen in its own process group
    try:
        proc = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
            start_new_session=True,
        )
    except OSError as e:
        return {
            'ok': False,
            'dry_run': False,
            'cancelled': False,
            'error': 'Failed to launch pip: %s' % e,
        }

    cancelled = False
    try:
        while True:
            if cancel_event is not None and cancel_event.is_set():
                cancelled = True
                _kill_process_group(proc)
                break
            rlist, _, _ = select.select([proc.stdout], [], [], 0.05)
            if rlist:
                line = proc.stdout.readline()
                if not line:
                    if proc.poll() is not None:
                        break
                    continue
                cleaned = line.rstrip('\r\n')
                if progress_cb:
                    progress_cb(cleaned)
            elif proc.poll() is not None:
                for line in proc.stdout:
                    cleaned = line.rstrip('\r\n')
                    if progress_cb:
                        progress_cb(cleaned)
                break
    finally:
        if proc.poll() is None:
            _kill_process_group(proc)

    if cancelled:
        if progress_cb:
            progress_cb('Installation cancelled. Cleaning up half-installed packages...')
        # Cleanup: uninstall any partially unpacked fTetWild packages
        cleanup_res = uninstall(progress_cb=progress_cb, dry_run=False, env_info=env_info)
        return {
            'ok': False,
            'dry_run': False,
            'cancelled': True,
            'error': 'Installation cancelled by user',
            'cleanup': cleanup_res,
        }

    rc = proc.wait()
    if rc != 0:
        return {
            'ok': False,
            'dry_run': False,
            'cancelled': False,
            'returncode': rc,
            'error': 'pip install failed with return code %d' % rc,
        }

    st = status(env_info)
    return {
        'ok': True,
        'dry_run': False,
        'cancelled': False,
        'command': cmd,
        'size_bytes': st.get('size_bytes', 0),
        'size_human': st.get('size_human', '0 B'),
        'error': None,
    }


def uninstall(progress_cb=None, cancel_event=None, dry_run=False, env_info=None):
    """Uninstall fTetWild dependencies safely from the target Sutura environment.

    Removes precisely the packages that requirements-ftetwild.txt added and
    that are not required by base Sutura or other remaining packages.

    Args:
      progress_cb: callable(str) called with each line of output
      cancel_event: threading.Event or object with is_set() method
      dry_run: if True, computes the package set and freed size without removing
      env_info: optional environment dict from resolve_environment()

    Returns dict:
      ok: bool
      dry_run: bool
      command: list[str]
      packages: list[str]
      freed_bytes: int
      freed_human: str
      error: str or None
    """
    if env_info is None:
        env_info = resolve_environment()

    if not env_info.get('supported'):
        return {
            'ok': False,
            'dry_run': dry_run,
            'packages': [],
            'freed_bytes': 0,
            'freed_human': '0 B',
            'error': env_info.get('reason', UNSUPPORTED_MSG),
        }

    pkgs_to_remove, freed_bytes = compute_ftetwild_removal_set(env_info)

    if not pkgs_to_remove:
        return {
            'ok': True,
            'dry_run': dry_run,
            'packages': [],
            'freed_bytes': 0,
            'freed_human': '0 B',
            'error': None,
        }

    pip_cmd = env_info['pip_path']
    cmd = [pip_cmd, 'uninstall', '-y'] + pkgs_to_remove

    if dry_run:
        if progress_cb:
            progress_cb('Dry run uninstall: %s (freed: %s)' %
                        (', '.join(pkgs_to_remove), format_bytes(freed_bytes)))
        return {
            'ok': True,
            'dry_run': True,
            'command': cmd,
            'packages': pkgs_to_remove,
            'freed_bytes': freed_bytes,
            'freed_human': format_bytes(freed_bytes),
            'error': None,
        }

    try:
        proc = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
            start_new_session=True,
        )
    except OSError as e:
        return {
            'ok': False,
            'dry_run': False,
            'packages': pkgs_to_remove,
            'freed_bytes': 0,
            'freed_human': '0 B',
            'error': 'Failed to launch pip uninstall: %s' % e,
        }

    cancelled = False
    try:
        while True:
            if cancel_event is not None and cancel_event.is_set():
                cancelled = True
                _kill_process_group(proc)
                break
            rlist, _, _ = select.select([proc.stdout], [], [], 0.05)
            if rlist:
                line = proc.stdout.readline()
                if not line:
                    if proc.poll() is not None:
                        break
                    continue
                cleaned = line.rstrip('\r\n')
                if progress_cb:
                    progress_cb(cleaned)
            elif proc.poll() is not None:
                for line in proc.stdout:
                    cleaned = line.rstrip('\r\n')
                    if progress_cb:
                        progress_cb(cleaned)
                break
    finally:
        if proc.poll() is None:
            _kill_process_group(proc)

    if cancelled:
        return {
            'ok': False,
            'dry_run': False,
            'cancelled': True,
            'packages': pkgs_to_remove,
            'freed_bytes': 0,
            'freed_human': '0 B',
            'error': 'Uninstall cancelled by user',
        }

    rc = proc.wait()
    if rc != 0:
        return {
            'ok': False,
            'dry_run': False,
            'packages': pkgs_to_remove,
            'freed_bytes': 0,
            'freed_human': '0 B',
            'error': 'pip uninstall failed with return code %d' % rc,
        }

    return {
        'ok': True,
        'dry_run': False,
        'command': cmd,
        'packages': pkgs_to_remove,
        'freed_bytes': freed_bytes,
        'freed_human': format_bytes(freed_bytes),
        'error': None,
    }
