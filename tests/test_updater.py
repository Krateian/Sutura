#!/usr/bin/env python3
"""Regression test for sutura/updater.py license-boundary logic.

Checks that crosses_license_boundary() stops auto-update only when an older
install would jump across the license boundary (LICENSE_BOUNDARY_VERSION):
       - 0.1.9 -> 0.2.0        = True  (the boundary crossing)
       - 0.1.9 -> 0.1.10       = False (v0.1.x patch, still auto-updatable)
       - 0.1.9 -> 0.2.0-beta.1 = True  (a v0.2 prerelease already carries
                                        the new license)
       - 0.2.0 -> 0.2.1        = False (already past the boundary)
  3. check_for_update()'s return contract: (status, tag, cfg) with
     statuses 'update' / 'license' / 'none' (AppImage short-circuits to
     'none' before any network call).
Usage: python3 tests/test_updater.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)


def test_crosses_boundary_stops_at_020():
    from updater import crosses_license_boundary
    assert crosses_license_boundary('0.1.9', '0.2.0') is True


def test_crosses_boundary_allows_010_patch():
    from updater import crosses_license_boundary
    assert crosses_license_boundary('0.1.9', '0.1.10') is False


def test_crosses_boundary_includes_v02_prerelease():
    from updater import crosses_license_boundary
    assert crosses_license_boundary('0.1.9', 'v0.2.0-beta.1') is True


def test_crosses_boundary_false_past_boundary():
    from updater import crosses_license_boundary
    assert crosses_license_boundary('0.2.0', '0.2.1') is False
    assert crosses_license_boundary('0.3.0', '0.3.1') is False


def test_crosses_boundary_malformed_is_false():
    from updater import crosses_license_boundary
    assert crosses_license_boundary('garbage', '0.2.0') is False
    assert crosses_license_boundary('0.1.9', 'not-a-version') is False


def test_check_for_update_return_contract():
    """check_for_update returns (status, tag, cfg) with the right status and
    tag per scenario; network and config are mocked so the test never touches
    the real GitHub API or the user's ~/.config/sutura."""
    import updater

    if os.environ.get('APPIMAGE'):
        status, tag, cfg = updater.check_for_update()
        assert status == 'none', status
        assert tag is None
        return

    fake_cfg = {
        'check_for_updates': True, 'last_check': None, 'last_known_version': None,
    }
    real_version = updater.VERSION
    orig = (updater.load_config, updater.save_config,
            updater.should_check, updater.fetch_latest_release)

    updater.load_config = lambda: dict(fake_cfg)
    updater.save_config = lambda cfg: None
    updater.should_check = lambda cfg: True

    def run_scenario(version, latest, expect_status, expect_tag):
        updater.VERSION = version
        updater.fetch_latest_release = lambda: latest
        status, tag, _cfg = updater.check_for_update(force=True)
        assert status == expect_status, (version, latest, status)
        assert tag == expect_tag, (version, latest, tag)

    try:
        run_scenario('0.1.9', '0.1.10', 'update', '0.1.10')
        run_scenario('0.1.9', '0.2.0', 'license', '0.2.0')
        run_scenario('0.1.9', 'v0.2.0-beta.1', 'license', 'v0.2.0-beta.1')
        run_scenario('0.1.9', '0.1.9', 'none', None)
        run_scenario('0.2.0', '0.2.1', 'update', '0.2.1')
    finally:
        updater.VERSION = real_version
        (updater.load_config, updater.save_config,
         updater.should_check, updater.fetch_latest_release) = orig


def test_should_check():
    import updater
    import time
    now = time.time()
    interval = updater.CHECK_INTERVAL_SECONDS
    
    # auto updates off -> always False
    assert not updater.should_check({'check_for_updates': False, 'check_on_startup': False})
    assert not updater.should_check({'check_for_updates': False, 'check_on_startup': True})
    
    # auto updates on, no startup check -> interval based
    assert updater.should_check({'check_for_updates': True, 'last_check': None})
    assert updater.should_check({'check_for_updates': True, 'last_check': now - interval - 10})
    assert not updater.should_check({'check_for_updates': True, 'last_check': now - 10})
    
    # auto updates on + startup check -> always True
    assert updater.should_check({'check_for_updates': True, 'check_on_startup': True, 'last_check': now - 10})


def test_module_lists_match_install_sh():
    """The self-update module lists must cover every module install.sh installs.

    A module imported by repair.py/gui.py but missing from APP_MODULES
    silently survives an update at its old version (the AGENTS.md standing
    rule that triage.py/engines.py/ftetwild_manager.py stay listed). Comparing
    against install.sh keeps one source of truth for the application modules.
    """
    import re
    import updater

    with open(os.path.join(REPO, 'install.sh')) as f:
        src = f.read()
    installed = set(re.findall(r'\$SRC/sutura/(\w+\.py)\b', src))
    assert installed, 'no sutura modules found in install.sh'
    listed = set(updater.APP_MODULES)
    assert installed == listed, (
        'updater.APP_MODULES != install.sh list; missing %s, extra %s'
        % (sorted(installed - listed), sorted(listed - installed)))


class _RunResult:
    def __init__(self, returncode=0):
        self.returncode = returncode


def _geom_install_fixture(tmp):
    """Temp source tree with the geom helper + temp APP_DIR with both Linux
    venvs. Returns (src_dir, app_dir)."""
    src = os.path.join(tmp, 'src')
    os.makedirs(os.path.join(src, 'sutura'))
    os.makedirs(os.path.join(src, 'scripts'))
    with open(os.path.join(src, 'scripts', 'install_sutura_geom.sh'), 'w') as f:
        f.write('#!/usr/bin/env bash\nexit 0\n')
    app = os.path.join(tmp, 'app')
    for venv in ('venv', 'venv311'):
        os.makedirs(os.path.join(app, venv, 'bin'))
        with open(os.path.join(app, venv, 'bin', 'python'), 'w') as f:
            f.write('')
    return src, app


def _run_install_linux(src, app, runner):
    """Call _install_linux with copy/pip work neutralised and subprocess.run
    replaced, so only the geom-helper calls reach the runner."""
    import updater
    orig = (updater.APP_DIR, updater.APP_MODULES, updater.COPY_EXTRA_MODULES,
            updater.LINUX_EXTRA_FILES, updater._copy_sutura_engine,
            updater.requirements_changed, updater.subprocess.run)
    updater.APP_DIR = app
    updater.APP_MODULES = ()
    updater.COPY_EXTRA_MODULES = ()
    updater.LINUX_EXTRA_FILES = ()
    updater._copy_sutura_engine = lambda _src: None
    updater.requirements_changed = lambda _src, _reqs: []
    updater.subprocess.run = runner
    try:
        updater._install_linux(src, ('requirements.txt',))
    finally:
        (updater.APP_DIR, updater.APP_MODULES, updater.COPY_EXTRA_MODULES,
         updater.LINUX_EXTRA_FILES, updater._copy_sutura_engine,
         updater.requirements_changed, updater.subprocess.run) = orig


def test_install_linux_runs_geom_helper_for_both_venvs():
    """A self-update installs the sutura_geom extension into BOTH Linux venvs
    via the downloaded helper (main venv for Graft, venv311 for the indirect
    bridge)."""
    import tempfile
    import shutil
    tmp = tempfile.mkdtemp(prefix='sutura-updater-test-')
    try:
        src, app = _geom_install_fixture(tmp)
        calls = []
        _run_install_linux(
            src, app,
            lambda cmd, *a, **k: (calls.append(cmd), _RunResult(0))[1])
        helper = os.path.join(src, 'scripts', 'install_sutura_geom.sh')
        geom = [c for c in calls if c[:2] == ['bash', helper]]
        pys = {c[2] for c in geom}
        assert os.path.join(app, 'venv', 'bin', 'python') in pys, calls
        assert os.path.join(app, 'venv311', 'bin', 'python') in pys, calls
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_install_linux_geom_helper_nonzero_is_non_fatal():
    """The geom install never aborts the update: a non-zero helper exit is
    swallowed (Graft simply stays unavailable)."""
    import tempfile
    import shutil
    tmp = tempfile.mkdtemp(prefix='sutura-updater-test-')
    try:
        src, app = _geom_install_fixture(tmp)
        _run_install_linux(src, app, lambda cmd, *a, **k: _RunResult(1))
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_install_linux_missing_geom_helper_skips():
    """An old tarball without scripts/install_sutura_geom.sh makes the update
    skip the geom step entirely (no subprocess call, no failure)."""
    import tempfile
    import shutil
    tmp = tempfile.mkdtemp(prefix='sutura-updater-test-')
    try:
        src, app = _geom_install_fixture(tmp)
        os.remove(os.path.join(src, 'scripts', 'install_sutura_geom.sh'))
        calls = []
        _run_install_linux(
            src, app,
            lambda cmd, *a, **k: (calls.append(cmd), _RunResult(0))[1])
        assert calls == [], calls
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('updater tests passed')


if __name__ == '__main__':
    main()