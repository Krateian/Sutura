#!/usr/bin/env python3
"""Regression test for the shutdown abort caused by the background update
check (sutura/gui.py, UpdateCheckWorker).

With an existing ~/.config/sutura/config.json that enables the update check,
MainWindow starts a parented UpdateCheckWorker at construction. Closing the
window shortly after launch used to destroy that still-running QThread when
main() returned, aborting the process with

    QThread: Destroyed while thread '' is still running

(SIGABRT / rc 134). The test runs the GUI in a subprocess with a temporary
HOME and a monkeypatched updater.check_for_update that sleeps 5 s, closes the
window after 200 ms and asserts a clean exit.

The subprocess pins QT_PLUGIN_PATH to PySide6's own Qt plugin directory,
derived at runtime, the way the installed macOS launcher does: without it the
offscreen platform plugin can resolve to an unrelated Qt stack.

Needs PySide6. Usage:
    QT_QPA_PLATFORM=offscreen ~/.local/share/sutura/venv/bin/python \
        tests/test_update_check_shutdown.py
"""
import json
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)


def _qt_plugins_path():
    """PySide6's own Qt plugin directory, derived at runtime (no hardcoded
    path). The installed macOS launcher exports QT_PLUGIN_PATH this way."""
    from PySide6.QtCore import QLibraryInfo
    return QLibraryInfo.path(QLibraryInfo.LibraryPath.PluginsPath)


CHILD = r'''
import sys
import time
sys.path.insert(0, {sutura!r})
import updater

def _slow_check(force=False):
    # stands in for the real network call; it must still be running when the
    # window closes at 200 ms (the abort happened on interpreter teardown)
    time.sleep(5.0)
    return ('none', None, updater.load_config())

updater.check_for_update = _slow_check

from PySide6.QtCore import QTimer
import gui

# main() skips the background check on windowless Qt platforms
# (offscreen/minimal), so force it on here: this regression is specifically
# about the shutdown path while a check IS running, which the close handler
# must handle regardless of platform.
gui.HEADLESS_QPA_PLATFORMS = ()

_orig = gui.MainWindow

class _ClosingWindow(_orig):
    def __init__(self, *a, **k):
        super().__init__(*a, **k)
        print('UPDATE-THREAD-RUNNING', int(
            self.update_check is not None and self.update_check.isRunning()))
        QTimer.singleShot(200, self.close)

gui.MainWindow = _ClosingWindow
try:
    gui.main()
except SystemExit as e:
    print('EXEC-RC', e.code)
    raise
'''


def test_close_during_update_check_exits_cleanly():
    with tempfile.TemporaryDirectory(prefix='sutura-home-') as home:
        cfg_dir = os.path.join(home, '.config', 'sutura')
        os.makedirs(cfg_dir)
        with open(os.path.join(cfg_dir, 'config.json'), 'w') as f:
            json.dump({'check_for_updates': True, 'last_check': 0,
                       'history_enabled': False}, f)
        env = dict(os.environ)
        env['HOME'] = home
        env['QT_QPA_PLATFORM'] = 'offscreen'
        env.pop('APPIMAGE', None)   # AppImage skips the background check
        env['QT_PLUGIN_PATH'] = _qt_plugins_path()
        r = subprocess.run(
            [sys.executable, '-c', CHILD.format(sutura=SUTURA)],
            capture_output=True, text=True, timeout=60, env=env)

    assert r.returncode == 0, (
        'GUI exited with %s (expected 0)\nstderr:\n%s'
        % (r.returncode, r.stderr[-1500:]))
    assert 'Destroyed while thread' not in r.stderr, r.stderr[-1500:]
    # the scenario is only valid if the update-check thread had actually
    # started (otherwise the test would pass vacuously)
    assert 'UPDATE-THREAD-RUNNING 1' in r.stdout, r.stdout[-800:]
    assert 'EXEC-RC 0' in r.stdout, r.stdout[-800:]


if __name__ == '__main__':
    test_close_during_update_check_exits_cleanly()
    print('update-check shutdown test passed')
