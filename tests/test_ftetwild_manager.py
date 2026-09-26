#!/usr/bin/env python3
"""Regression and unit tests for the fTetWild manager backend.

Tests all status checks, layout detection (Linux venv311, macOS conda sutura-env,
AppImage / frozen / read-only), platform size estimates, safe dependency
pruning, streaming execution, dry runs, and cancellation cleanup.

Uses fake pip scripts and temporary mock directories; NEVER touches or modifies
the real fTetWild installation or system Python.

Usage:
    python tests/test_ftetwild_manager.py
"""
import os
import signal
import stat
import sys
import tempfile
import threading
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import ftetwild_manager as mgr  # noqa: E402


def _create_mock_dist_info(site_packages, name, version, requires=None, files=None):
    """Create a mock .dist-info directory for importlib.metadata."""
    dist_dir = os.path.join(site_packages, '%s-%s.dist-info' % (name, version))
    os.makedirs(dist_dir, exist_ok=True)

    metadata_lines = [
        'Metadata-Version: 2.1',
        'Name: %s' % name,
        'Version: %s' % version,
    ]
    if requires:
        for r in requires:
            metadata_lines.append('Requires-Dist: %s' % r)
    metadata_lines.append('')
    metadata_lines.append('')

    with open(os.path.join(dist_dir, 'METADATA'), 'w', encoding='utf-8') as f:
        f.write('\n'.join(metadata_lines))

    record_lines = []
    if files:
        for rel_path, content in files.items():
            full_path = os.path.join(site_packages, rel_path)
            os.makedirs(os.path.dirname(full_path), exist_ok=True)
            with open(full_path, 'wb') as f:
                f.write(content if isinstance(content, bytes) else content.encode('utf-8'))
            size = len(content)
            record_lines.append('%s,sha256=xxx,%d' % (rel_path, size))

    record_lines.append('%s-%s.dist-info/METADATA,sha256=xxx,100' % (name, version))
    with open(os.path.join(dist_dir, 'RECORD'), 'w', encoding='utf-8') as f:
        f.write('\n'.join(record_lines))


def _create_fake_pip_script(bin_dir, script_body=None):
    """Create a mock executable pip script in bin_dir."""
    pip_path = os.path.join(bin_dir, 'pip')
    default_body = (
        "#!/usr/bin/env python3\n"
        "import sys\n"
        "args = sys.argv[1:]\n"
        "if not args:\n"
        "    sys.exit(0)\n"
        "if args[0] == 'install':\n"
        "    print('Collecting fake packages...')\n"
        "    print('Successfully installed fake-packages')\n"
        "    sys.exit(0)\n"
        "elif args[0] == 'uninstall':\n"
        "    print('Uninstalling packages: ' + ' '.join(args[1:]))\n"
        "    print('Successfully uninstalled')\n"
        "    sys.exit(0)\n"
        "sys.exit(0)\n"
    )
    with open(pip_path, 'w') as f:
        f.write(script_body or default_body)
    os.chmod(pip_path, os.stat(pip_path).st_mode | stat.S_IEXEC)
    return pip_path


def test_format_bytes():
    assert mgr.format_bytes(0) == '0 B'
    assert mgr.format_bytes(512) == '512 B'
    assert mgr.format_bytes(1024) == '1.0 KB'
    assert mgr.format_bytes(105 * 1024 * 1024) == '105.0 MB'
    assert mgr.format_bytes(1_181_116_006) == '1.10 GB'


def test_estimate():
    # macOS Apple Silicon calibration
    est_mac = mgr.estimate('darwin')
    assert est_mac['download_bytes'] == mgr.MAC_ARM64_DOWNLOAD_BYTES
    assert '105' in est_mac['download_human']
    assert est_mac['installed_bytes'] == mgr.MAC_ARM64_INSTALLED_BYTES
    assert '538' in est_mac['installed_human']

    # Linux x86_64 measurement
    est_lin = mgr.estimate('linux')
    assert est_lin['download_bytes'] == mgr.LINUX_X86_64_DOWNLOAD_BYTES
    assert '139' in est_lin['download_human']
    assert est_lin['installed_bytes'] == mgr.LINUX_X86_64_INSTALLED_BYTES
    assert '580' in est_lin['installed_human']

    # Generic fallback
    est_gen = mgr.estimate('other')
    assert est_gen['download_bytes'] == mgr.DEFAULT_DOWNLOAD_BYTES
    assert '1.10 GB' in est_gen['installed_human']


def test_unsupported_appimage():
    env = {'APPIMAGE': '/tmp/Sutura-x86_64.AppImage'}
    res = mgr.resolve_environment(environ=env)
    assert not res['supported']
    assert res['reason'] == mgr.UNSUPPORTED_MSG
    assert not res['writable']

    # status() should report unsupported
    st = mgr.status(res)
    assert not st['installed']
    assert not st['supported']
    assert st['reason'] == mgr.UNSUPPORTED_MSG


def test_unsupported_frozen():
    res = mgr.resolve_environment(is_frozen=True, platform='darwin')
    assert not res['supported']
    assert res['reason'] == mgr.UNSUPPORTED_MACOS_MSG

    res_lin = mgr.resolve_environment(is_frozen=True, platform='linux')
    assert not res_lin['supported']
    assert res_lin['reason'] == mgr.UNSUPPORTED_MSG


def test_unsupported_unknown_layout():
    with tempfile.TemporaryDirectory() as td:
        # Linux with no venv311
        res = mgr.resolve_environment(app_dir=td, platform='linux')
        assert not res['supported']
        assert res['reason'] == mgr.UNSUPPORTED_MSG

        # macOS with no sutura-env
        res_mac = mgr.resolve_environment(conda_env_dir=os.path.join(td, 'nonexistent'), platform='darwin')
        assert not res_mac['supported']
        assert res_mac['reason'] == mgr.UNSUPPORTED_MACOS_MSG


def test_supported_linux_venv311():
    with tempfile.TemporaryDirectory() as td:
        venv311 = os.path.join(td, 'venv311')
        bin_dir = os.path.join(venv311, 'bin')
        sp_dir = os.path.join(venv311, 'lib', 'python3.11', 'site-packages')
        os.makedirs(bin_dir)
        os.makedirs(sp_dir)

        # Create dummy python and pip
        py_path = os.path.join(bin_dir, 'python')
        with open(py_path, 'w') as f:
            f.write('#!/bin/sh\n')
        _create_fake_pip_script(bin_dir)

        res = mgr.resolve_environment(app_dir=td, platform='linux')
        assert res['supported']
        assert res['layout'] == 'linux-venv311'
        assert res['python_path'] == py_path
        assert res['site_packages'] == sp_dir
        assert res['writable']


def test_supported_macos_conda():
    with tempfile.TemporaryDirectory() as td:
        bin_dir = os.path.join(td, 'bin')
        sp_dir = os.path.join(td, 'lib', 'python3.11', 'site-packages')
        os.makedirs(bin_dir)
        os.makedirs(sp_dir)

        py_path = os.path.join(bin_dir, 'python')
        with open(py_path, 'w') as f:
            f.write('#!/bin/sh\n')
        _create_fake_pip_script(bin_dir)

        res = mgr.resolve_environment(conda_env_dir=td, platform='darwin')
        assert res['supported']
        assert res['layout'] == 'macos-conda'
        assert res['python_path'] == py_path
        assert res['site_packages'] == sp_dir
        assert res['writable']


def test_status_not_installed():
    with tempfile.TemporaryDirectory() as td:
        sp_dir = os.path.join(td, 'site-packages')
        os.makedirs(sp_dir)
        env_info = {
            'supported': True,
            'layout': 'macos-conda',
            'site_packages': sp_dir,
            'pip_path': os.path.join(td, 'pip'),
        }
        # Only base packages installed
        _create_mock_dist_info(sp_dir, 'numpy', '2.0.0')
        _create_mock_dist_info(sp_dir, 'manifold3d', '3.5.3')

        st = mgr.status(env_info)
        assert not st['installed']
        assert st['supported']
        assert st['version'] is None
        assert st['size_bytes'] == 0


def test_status_installed():
    with tempfile.TemporaryDirectory() as td:
        sp_dir = os.path.join(td, 'site-packages')
        os.makedirs(sp_dir)
        env_info = {
            'supported': True,
            'layout': 'macos-conda',
            'site_packages': sp_dir,
            'pip_path': os.path.join(td, 'pip'),
        }

        # Install pytetwild, pyvista, vtk
        _create_mock_dist_info(sp_dir, 'pytetwild', '0.4.2', files={'pytetwild/foo.py': 'x' * 5000})
        _create_mock_dist_info(sp_dir, 'pyvista', '0.49.0', requires=['vtk'], files={'pyvista/bar.py': 'y' * 10000})
        _create_mock_dist_info(sp_dir, 'vtk', '9.7.0', files={'vtkmodules/vtk.so': 'z' * 50000})

        st = mgr.status(env_info)
        assert st['installed']
        assert st['supported']
        assert st['version'] == '0.4.2'
        assert st['size_bytes'] >= 65000
        assert 'KB' in st['size_human'] or 'MB' in st['size_human']


def test_compute_ftetwild_removal_set():
    """Verify that uninstall removes fTetWild packages & VTK, but protects base packages."""
    with tempfile.TemporaryDirectory() as td:
        sp_dir = os.path.join(td, 'site-packages')
        os.makedirs(sp_dir)
        env_info = {
            'supported': True,
            'layout': 'macos-conda',
            'site_packages': sp_dir,
            'pip_path': os.path.join(td, 'pip'),
        }

        # Base packages: manifold3d and numpy
        _create_mock_dist_info(sp_dir, 'numpy', '2.0.0', files={'numpy/n.py': 'a' * 1000})
        _create_mock_dist_info(sp_dir, 'manifold3d', '3.5.3', requires=['numpy'], files={'m3d.py': 'b' * 1000})

        # fTetWild packages: pytetwild, pyvista (requires numpy, vtk, pyvista-validation), vtk
        _create_mock_dist_info(sp_dir, 'pytetwild', '0.4.2', requires=['numpy'], files={'ptw.py': 'p' * 5000})
        _create_mock_dist_info(sp_dir, 'pyvista', '0.49.0',
                               requires=['numpy', 'vtk', 'pyvista-validation'], files={'pv.py': 'v' * 10000})
        _create_mock_dist_info(sp_dir, 'vtk', '9.7.0', files={'vtk.so': 'k' * 500000})
        _create_mock_dist_info(sp_dir, 'pyvista-validation', '0.2.2', files={'pvv.py': 'v' * 2000})

        to_remove, freed = mgr.compute_ftetwild_removal_set(env_info)

        # Must remove fTetWild suite and VTK
        assert 'pytetwild' in to_remove
        assert 'pyvista' in to_remove
        assert 'vtk' in to_remove
        assert 'pyvista-validation' in to_remove

        # MUST NEVER remove numpy or manifold3d
        assert 'numpy' not in to_remove
        assert 'manifold3d' not in to_remove

        # Freed bytes must include vtk size
        assert freed >= 517000


def test_install_dry_run():
    with tempfile.TemporaryDirectory() as td:
        pip_path = os.path.join(td, 'pip')
        env_info = {
            'supported': True,
            'layout': 'linux-venv311',
            'pip_path': pip_path,
            'site_packages': td,
        }
        res = mgr.install(dry_run=True, env_info=env_info)
        assert res['ok']
        assert res['dry_run']
        assert res['command'][0] == pip_path
        assert res['command'][1] == 'install'


def test_install_live_streaming():
    with tempfile.TemporaryDirectory() as td:
        bin_dir = os.path.join(td, 'bin')
        sp_dir = os.path.join(td, 'site-packages')
        os.makedirs(bin_dir)
        os.makedirs(sp_dir)

        fake_script = (
            "#!/usr/bin/env python3\n"
            "import sys\n"
            "print('Collecting pytetwild')\n"
            "print('Collecting pyvista')\n"
            "print('Successfully installed pytetwild-0.4.2 pyvista-0.49.0')\n"
            "sys.exit(0)\n"
        )
        pip_path = _create_fake_pip_script(bin_dir, fake_script)
        env_info = {
            'supported': True,
            'layout': 'macos-conda',
            'pip_path': pip_path,
            'site_packages': sp_dir,
        }

        lines = []
        res = mgr.install(progress_cb=lines.append, dry_run=False, env_info=env_info)
        assert res['ok']
        assert not res['cancelled']
        assert 'Collecting pytetwild' in lines
        assert 'Successfully installed pytetwild-0.4.2 pyvista-0.49.0' in lines


def test_install_cancellation_and_cleanup():
    with tempfile.TemporaryDirectory() as td:
        bin_dir = os.path.join(td, 'bin')
        sp_dir = os.path.join(td, 'site-packages')
        os.makedirs(bin_dir)
        os.makedirs(sp_dir)

        # Pip script that sleeps unless killed, or uninstalls when called with uninstall
        fake_script = (
            "#!/usr/bin/env python3\n"
            "import sys, time\n"
            "args = sys.argv[1:]\n"
            "if args and args[0] == 'install':\n"
            "    print('Beginning slow installation...')\n"
            "    sys.stdout.flush()\n"
            "    time.sleep(30)\n"
            "    sys.exit(0)\n"
            "elif args and args[0] == 'uninstall':\n"
            "    print('Cleaning up cancelled install')\n"
            "    sys.exit(0)\n"
            "sys.exit(0)\n"
        )
        pip_path = _create_fake_pip_script(bin_dir, fake_script)
        env_info = {
            'supported': True,
            'layout': 'linux-venv311',
            'pip_path': pip_path,
            'site_packages': sp_dir,
        }

        cancel_ev = threading.Event()
        lines = []

        def trigger_cancel():
            time.sleep(0.3)
            cancel_ev.set()

        t = threading.Thread(target=trigger_cancel)
        t.start()
        res = mgr.install(progress_cb=lines.append, cancel_event=cancel_ev,
                          dry_run=False, env_info=env_info)
        t.join()

        assert not res['ok']
        assert res['cancelled']
        assert 'cancelled' in res['error'].lower()
        # Progress callback should record cancellation and cleanup attempt
        assert any('cancelled' in l.lower() for l in lines)


def test_uninstall_dry_run():
    with tempfile.TemporaryDirectory() as td:
        sp_dir = os.path.join(td, 'site-packages')
        os.makedirs(sp_dir)
        pip_path = os.path.join(td, 'pip')
        env_info = {
            'supported': True,
            'layout': 'macos-conda',
            'pip_path': pip_path,
            'site_packages': sp_dir,
        }
        _create_mock_dist_info(sp_dir, 'pytetwild', '0.4.2', files={'p.py': 'x' * 2000})
        _create_mock_dist_info(sp_dir, 'pyvista', '0.49.0', files={'v.py': 'y' * 5000})

        res = mgr.uninstall(dry_run=True, env_info=env_info)
        assert res['ok']
        assert res['dry_run']
        assert 'pytetwild' in res['packages']
        assert 'pyvista' in res['packages']
        assert res['freed_bytes'] >= 7000
        assert res['command'] == [pip_path, 'uninstall', '-y', 'pytetwild', 'pyvista']


def test_uninstall_live():
    with tempfile.TemporaryDirectory() as td:
        bin_dir = os.path.join(td, 'bin')
        sp_dir = os.path.join(td, 'site-packages')
        os.makedirs(bin_dir)
        os.makedirs(sp_dir)

        fake_script = (
            "#!/usr/bin/env python3\n"
            "import sys\n"
            "args = sys.argv[1:]\n"
            "print('Uninstalling ' + ' '.join(args))\n"
            "sys.exit(0)\n"
        )
        pip_path = _create_fake_pip_script(bin_dir, fake_script)
        env_info = {
            'supported': True,
            'layout': 'linux-venv311',
            'pip_path': pip_path,
            'site_packages': sp_dir,
        }
        _create_mock_dist_info(sp_dir, 'pytetwild', '0.4.2', files={'p.py': 'x' * 2000})

        lines = []
        res = mgr.uninstall(progress_cb=lines.append, dry_run=False, env_info=env_info)
        assert res['ok']
        assert 'pytetwild' in res['packages']
        assert any('Uninstalling' in l for l in lines)


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('ftetwild manager tests passed')
