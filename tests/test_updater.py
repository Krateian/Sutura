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
        'check_for_updates': True, 'last_check': None,
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


def test_default_config_has_no_orphan_keys():
    """Persisted settings must be read somewhere; the two orphan keys are gone."""
    import updater
    cfg = updater.DEFAULT_CONFIG
    assert 'last_known_version' not in cfg, cfg
    assert 'deep_repair' not in cfg, cfg
    assert cfg.get('coaptation') is True, cfg
    assert cfg.get('si_mode') == 'report', cfg


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


def test_install_linux_geom_helper_nonzero_is_fatal():
    """A failing geom install aborts the update (the extension is required), so
    perform_update's handler can roll back with the actionable message."""
    import tempfile
    import shutil
    tmp = tempfile.mkdtemp(prefix='sutura-updater-test-')
    try:
        src, app = _geom_install_fixture(tmp)
        try:
            _run_install_linux(src, app, lambda cmd, *a, **k: _RunResult(1))
        except RuntimeError as e:
            assert 'sutura_geom' in str(e) and 'exit 1' in str(e), e
        else:
            raise AssertionError('a failed geom install must be fatal')
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_install_linux_missing_geom_helper_is_fatal():
    """An update tarball without scripts/install_sutura_geom.sh cannot satisfy
    the required extension, so the update fails instead of skipping it."""
    import tempfile
    import shutil
    tmp = tempfile.mkdtemp(prefix='sutura-updater-test-')
    try:
        src, app = _geom_install_fixture(tmp)
        os.remove(os.path.join(src, 'scripts', 'install_sutura_geom.sh'))
        calls = []
        try:
            _run_install_linux(
                src, app,
                lambda cmd, *a, **k: (calls.append(cmd), _RunResult(0))[1])
        except RuntimeError as e:
            assert 'install_sutura_geom.sh' in str(e), e
        else:
            raise AssertionError('a missing geom helper must be fatal')
        assert calls == [], calls
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_perform_update_geom_failure_rolls_back():
    """A geom install failure during self-update triggers the existing
    rollback and surfaces the named error instead of an opaque health-check
    failure after the fact."""
    import updater
    calls = []
    orig = (updater.VERSION, updater.is_appimage,
            updater.crosses_license_boundary, updater.backup_install,
            updater.download_source, updater.install_source,
            updater.health_check, updater.rollback, updater.fetch_release_json,
            updater.APP_DIR)
    # No Rust artifact in this release, so these tests keep covering the
    # source-tarball path and never touch the network. `APP_DIR` is also pinned:
    # without it the layout guard reads the tester's real install and the result
    # depends on what happens to be on their machine.
    updater.fetch_release_json = lambda: None
    updater.APP_DIR = '/nonexistent/sutura-test-app'
    updater.VERSION = '0.9.1'
    updater.is_appimage = lambda: False
    updater.crosses_license_boundary = lambda v, t: False
    updater.backup_install = lambda: calls.append('backup')
    updater.download_source = lambda tag, d: (
        calls.append('download'), '/tmp/fake-src')[1]
    def _fail_install(_src):
        calls.append('install')
        raise RuntimeError('sutura_geom is required but the install into venv '
                           'failed (exit 1)')
    updater.install_source = _fail_install
    updater.health_check = lambda tag: (True, 'ok')
    updater.rollback = lambda v: calls.append('rollback:' + v)
    try:
        ok, msg, prev = updater.perform_update('0.9.2')
    finally:
        (updater.VERSION, updater.is_appimage,
         updater.crosses_license_boundary, updater.backup_install,
         updater.download_source, updater.install_source,
         updater.health_check, updater.rollback,
         updater.fetch_release_json, updater.APP_DIR) = orig
    assert ok is False, (ok, msg)
    assert prev == '0.9.1', prev
    assert calls == ['backup', 'download', 'install', 'rollback:0.9.1'], calls
    assert 'sutura_geom' in msg, msg


def test_perform_update_cancels_before_install():
    """A stop request before install must leave the installed files alone."""
    import updater
    calls = []
    orig = (updater.VERSION, updater.is_appimage,
            updater.crosses_license_boundary, updater.backup_install,
            updater.download_source, updater.install_source,
            updater.health_check, updater.rollback, updater.fetch_release_json,
            updater.APP_DIR)
    # No Rust artifact in this release, so these tests keep covering the
    # source-tarball path and never touch the network. `APP_DIR` is also pinned:
    # without it the layout guard reads the tester's real install and the result
    # depends on what happens to be on their machine.
    updater.fetch_release_json = lambda: None
    updater.APP_DIR = '/nonexistent/sutura-test-app'
    updater.VERSION = '0.9.1'
    updater.is_appimage = lambda: False
    updater.crosses_license_boundary = lambda v, t: False
    updater.backup_install = lambda: calls.append('backup')
    updater.download_source = lambda tag, d: (
        calls.append('download'), '/tmp/fake-src')[1]
    updater.install_source = lambda src: calls.append('install')
    updater.health_check = lambda tag: (True, 'ok')
    updater.rollback = lambda v: calls.append('rollback:' + v)
    try:
        ok, msg, prev = updater.perform_update(
            '0.9.2', should_stop=lambda: 'download' in calls)
    finally:
        (updater.VERSION, updater.is_appimage,
         updater.crosses_license_boundary, updater.backup_install,
         updater.download_source, updater.install_source,
         updater.health_check, updater.rollback,
         updater.fetch_release_json, updater.APP_DIR) = orig
    assert ok is False, (ok, msg)
    assert prev == '0.9.1', prev
    assert calls == ['backup', 'download'], calls
    assert 'cancelled' in msg, msg


def _copy_python_fixture(tmp):
    """Temp source with a sutura/ tree whose modules are NOT all in
    APP_MODULES, plus a temp APP_DIR. Returns (src_dir, app_dir)."""
    src = os.path.join(tmp, 'src')
    mod = os.path.join(src, 'sutura')
    os.makedirs(mod)
    for name in ('__init__.py', 'repair.py', 'brand_new_module.py'):
        with open(os.path.join(mod, name), 'w') as f:
            f.write('# %s\n' % name)
    app = os.path.join(tmp, 'app')
    os.makedirs(app)
    return src, app


def _run_copy_python(src, app):
    """Call _copy_python_files with the fixed-list copy and the engine copy
    neutralised, so only the list-independent source scan is exercised."""
    import updater
    orig = (updater.APP_DIR, updater.APP_MODULES, updater.COPY_EXTRA_MODULES,
            updater._copy_sutura_engine)
    updater.APP_DIR = app
    updater.APP_MODULES = ()
    updater.COPY_EXTRA_MODULES = ()
    updater._copy_sutura_engine = lambda _src: None
    try:
        updater._copy_python_files(src)
    finally:
        (updater.APP_DIR, updater.APP_MODULES, updater.COPY_EXTRA_MODULES,
         updater._copy_sutura_engine) = orig


def test_install_copies_every_source_module():
    """A module the new release ships is installed even when it is absent from
    every fixed list. Regression for the 0.8.1 -> 0.9.3 update, where the
    updater's own hard-coded APP_MODULES left newly added modules behind and
    --self-check failed."""
    import tempfile
    import shutil
    tmp = tempfile.mkdtemp(prefix='sutura-updater-test-')
    try:
        src, app = _copy_python_fixture(tmp)
        _run_copy_python(src, app)
        for name in ('__init__.py', 'repair.py', 'brand_new_module.py'):
            assert os.path.isfile(os.path.join(app, name)), name
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_source_py_modules_excludes_nested_dirs_and_non_py():
    """The scan lists only top-level *.py; the sutura_engine/ package and any
    non-module files are not flattened into APP_DIR."""
    import tempfile
    import shutil
    import updater
    tmp = tempfile.mkdtemp(prefix='sutura-updater-test-')
    try:
        src, _app = _copy_python_fixture(tmp)
        os.makedirs(os.path.join(src, 'sutura', 'sutura_engine'))
        with open(os.path.join(src, 'sutura', 'sutura_engine', 'core.py'), 'w') as f:
            f.write('')
        with open(os.path.join(src, 'sutura', 'repair_score_config.json'), 'w') as f:
            f.write('{}')
        found = updater._source_py_modules(src)
        assert found == ['__init__.py', 'brand_new_module.py', 'repair.py'], found
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_missing_installed_flags_uninstalled_module():
    """_missing_installed names exactly the modules absent from APP_DIR."""
    import tempfile
    import shutil
    import updater
    tmp = tempfile.mkdtemp(prefix='sutura-updater-test-')
    orig = updater.APP_DIR
    try:
        updater.APP_DIR = tmp
        with open(os.path.join(tmp, 'present.py'), 'w') as f:
            f.write('')
        assert updater._missing_installed(
            ['present.py', 'absent.py']) == ['absent.py']
        assert updater._missing_installed(['present.py']) == []
    finally:
        updater.APP_DIR = orig
        shutil.rmtree(tmp, ignore_errors=True)


def test_health_check_fails_on_missing_module():
    """A shipped module absent from the install makes the health check fail
    before it touches the installed wrapper."""
    import tempfile
    import shutil
    import updater
    tmp = tempfile.mkdtemp(prefix='sutura-updater-test-')
    orig = updater.APP_DIR
    try:
        updater.APP_DIR = tmp
        ok, msg = updater.health_check(
            'v9.9.9', expected_modules=['definitely_absent_module.py'])
        assert ok is False, (ok, msg)
        assert 'definitely_absent_module.py' in msg, msg
    finally:
        updater.APP_DIR = orig
        shutil.rmtree(tmp, ignore_errors=True)


def test_perform_update_verifies_source_modules():
    """perform_update snapshots the source's modules and hands them to the
    health check, so the completeness gate is actually wired in."""
    import tempfile
    import shutil
    import updater
    tmp = tempfile.mkdtemp(prefix='sutura-updater-test-')
    src, _app = _copy_python_fixture(tmp)
    seen = []
    orig = (updater.VERSION, updater.is_appimage,
            updater.crosses_license_boundary, updater.backup_install,
            updater.download_source, updater.install_source,
            updater.health_check, updater.rollback, updater.fetch_release_json,
            updater.APP_DIR)
    # No Rust artifact in this release, so these tests keep covering the
    # source-tarball path and never touch the network. `APP_DIR` is also pinned:
    # without it the layout guard reads the tester's real install and the result
    # depends on what happens to be on their machine.
    updater.fetch_release_json = lambda: None
    updater.APP_DIR = '/nonexistent/sutura-test-app'
    updater.VERSION = '0.9.1'
    updater.is_appimage = lambda: False
    updater.crosses_license_boundary = lambda v, t: False
    updater.backup_install = lambda: None
    updater.download_source = lambda tag, d: src
    updater.install_source = lambda _src: None

    def _hc(tag, expected_modules=None):
        seen.append(expected_modules)
        return True, 'ok'

    updater.health_check = _hc
    updater.rollback = lambda v: None
    try:
        ok, msg, prev = updater.perform_update('0.9.3')
    finally:
        (updater.VERSION, updater.is_appimage,
         updater.crosses_license_boundary, updater.backup_install,
         updater.download_source, updater.install_source,
         updater.health_check, updater.rollback,
         updater.fetch_release_json, updater.APP_DIR) = orig
        shutil.rmtree(tmp, ignore_errors=True)
    assert ok is True, (ok, msg)
    assert seen == [['__init__.py', 'brand_new_module.py', 'repair.py']], seen


def test_rust_layout_refuses_to_update():
    """The Python updater must never overwrite a Rust install with a source tree.

    ``perform_update`` unpacks a GitHub source tarball over APP_DIR. On a machine
    the Rust installer set up, that is not an upgrade -- it is a downgrade to a
    Python tree, and once the Python pipeline is retired the tree has no working
    pipeline at all. The guard runs before the backup and before any download.
    """
    import tempfile
    import updater

    orig = updater.APP_DIR
    try:
        with tempfile.TemporaryDirectory() as tmp:
            # A Rust layout: no venv, just the installed binary.
            os.makedirs(os.path.join(tmp, 'bin'))
            with open(os.path.join(tmp, 'bin', 'sutura'), 'w') as f:
                f.write('#!/bin/sh\n')
            updater.APP_DIR = tmp
            assert updater._rust_layout_installed() is True

            # Refused, and refused without reaching the network: a tag that
            # does not exist would raise if download_source ran.
            ok, msg, _ver = updater.perform_update('v99.99.99')
            assert ok is False, 'a Rust layout must not be updated'
            assert 'Rust updater' in msg, msg

        with tempfile.TemporaryDirectory() as tmp:
            # A Python layout: venv, no bin/sutura.
            os.makedirs(os.path.join(tmp, 'venv', 'bin'))
            with open(os.path.join(tmp, 'repair.py'), 'w') as f:
                f.write('')
            updater.APP_DIR = tmp
            assert updater._rust_layout_installed() is False

        # macOS bundle layout is recognised too.
        with tempfile.TemporaryDirectory() as tmp:
            macos = os.path.join(tmp, 'Contents', 'MacOS')
            os.makedirs(macos)
            with open(os.path.join(macos, 'sutura'), 'w') as f:
                f.write('')
            updater.APP_DIR = tmp
            assert updater._rust_layout_installed() is True

        # The APFS case trap: the Python .app's launcher is
        # `Contents/MacOS/Sutura`, and a plain isfile('.../sutura') finds it on
        # a case-insensitive filesystem. Treating that as a Rust layout would
        # make this updater refuse to update an ordinary Python install.
        with tempfile.TemporaryDirectory() as tmp:
            macos = os.path.join(tmp, 'Contents', 'MacOS')
            os.makedirs(macos)
            with open(os.path.join(macos, 'Sutura'), 'w') as f:
                f.write('python launcher')
            # Only meaningful where the filesystem really is case-insensitive.
            if os.path.isfile(os.path.join(macos, 'sutura')):
                updater.APP_DIR = tmp
                assert updater._rust_layout_installed() is False, (
                    'a `Sutura` GUI launcher must not read as the Rust CLI')
    finally:
        updater.APP_DIR = orig


def test_release_with_a_rust_asset_uses_the_handoff():
    """A release that publishes a Rust artifact must be installed by the Rust
    binary's own migration, not by unpacking the source tarball.

    This is the bridge: once the Rust build is the distribution, the source
    tarball for that tag is not an upgrade, and after the Python pipeline is
    retired it has no working pipeline at all. So the decision is made on the
    release payload, before the backup and before download_source.
    """
    import updater

    orig = (updater.VERSION, updater.is_appimage,
            updater.crosses_license_boundary, updater.fetch_release_json,
            updater.find_rust_asset, updater.perform_rust_handoff,
            updater.download_source, updater.backup_install,
            updater._rust_layout_installed)
    calls = []
    try:
        updater.VERSION = '0.9.9'
        updater.is_appimage = lambda: False
        updater.crosses_license_boundary = lambda v, t: False
        updater._rust_layout_installed = lambda: False
        updater.fetch_release_json = lambda: {'assets': [{'name': 'x'}]}
        updater.find_rust_asset = lambda rel, tag: ('u', 's')
        updater.perform_rust_handoff = (
            lambda tag, progress=None, released=None: (
                calls.append('handoff:%s' % tag),
                (True, 'updated to %s' % tag, '0.9.9'))[1])
        updater.download_source = lambda tag, d: calls.append('download')
        updater.backup_install = lambda: calls.append('backup')

        ok, msg, prev = updater.perform_update('v0.10.0')
        assert ok is True, msg
        assert calls == ['handoff:v0.10.0'], calls
        assert prev == '0.9.9', prev
    finally:
        (updater.VERSION, updater.is_appimage,
         updater.crosses_license_boundary, updater.fetch_release_json,
         updater.find_rust_asset, updater.perform_rust_handoff,
         updater.download_source, updater.backup_install,
         updater._rust_layout_installed) = orig


def test_rust_asset_names_match_the_rust_updater():
    """The bridge looks for the exact names install::asset_name writes.

    A mismatch here would make the bridge silently find nothing on release day
    and fall back to the source tarball, which is the failure this whole path
    exists to prevent. The Rust table is read from source rather than trusted,
    so a rename there fails here with both spellings visible.
    """
    import updater

    install_rs = os.path.join(REPO, 'rust', 'sutura-core', 'src', 'install.rs')
    with open(install_rs) as f:
        src = f.read()

    # The Rust side formats `Sutura-rust-v{ver}-<tail>`; the bridge writes
    # `Sutura-rust-v%s-<tail>`. Both must name the same file per platform.
    expected = {
        'darwin-arm64': 'Sutura-rust-v{ver}-arm64.dmg',
        'linux-x86_64': 'Sutura-rust-v{ver}-x86_64.tar.gz',
    }
    for key, rust_name in expected.items():
        assert '"%s"' % rust_name in src, (
            'install.rs no longer formats %r; update RUST_ASSETS to match'
            % rust_name)
        py_name = updater.RUST_ASSETS[key] % 'VER'
        assert py_name == rust_name.replace('{ver}', 'VER'), (
            'bridge asset for %s is %r, Rust writes %r'
            % (key, py_name, rust_name.replace('{ver}', 'VER')))


def test_sha256_parsing_matches_the_rust_side():
    """`shasum`/`sha256sum` output parses, junk does not."""
    import updater

    good = 'a' * 64
    assert updater._parse_sha256('%s  Sutura-rust-v1.0.0.tar.gz' % good) == good
    assert updater._parse_sha256('%s\n' % good.upper()) == good
    assert updater._parse_sha256('not-a-digest') is None
    assert updater._parse_sha256('') is None
    assert updater._parse_sha256('abc  file') is None


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('updater tests passed')


def test_legacy_updater_tombstones_present():
    """Updaters shipped in 0.5.1-0.9.1 copy these names from the new release
    without an existence check; deleting them breaks self-update for those
    installs (0.9.2 regression)."""
    for name in ("manifold_bridge.py", "csg_bridge.py"):
        assert os.path.exists(os.path.join(REPO, "sutura", name)), name


if __name__ == '__main__':
    main()
