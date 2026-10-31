#!/usr/bin/env python3
"""Regression test: closing the GUI during a running background worker must
not abort the process.

The real MainWindow closeEvent must cancel RepairWorker/analyze, the four
render workers and UpdateWorker before the parented QThreads are destroyed.
The test starts one slow fake instance of each, closes the window after 200 ms
in an offscreen subprocess, and asserts a clean exit (no
"QThread: Destroyed while thread is still running", rc 0).

Needs PySide6. Usage:
    QT_QPA_PLATFORM=offscreen ~/.local/share/sutura/venv/bin/python \
        tests/test_gui_shutdown_workers.py
"""
import json
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _qt_test_env import ensure_qt_plugins, qt_plugins_path  # noqa: E402
ensure_qt_plugins()


CHILD = r'''
import subprocess
import sys
import time
sys.path.insert(0, {sutura!r})

from PySide6.QtCore import QTimer
import gui
import updater

SLOW = [sys.executable, '-c', 'import time; time.sleep(30)']

def _slow_child(worker):
    worker._proc = subprocess.Popen(SLOW)
    worker._proc.communicate()
    return {{'error': 'cancelled'}}

def _slow_repair(self, path):
    return _slow_child(self)

def _slow_analyze(self, path):
    return _slow_child(self)

gui._script_cmd = lambda *_a, **_k: SLOW
gui.RepairWorker._run_one = _slow_repair
gui.AnalyzeWorker._analyze_one = _slow_analyze

def _fake_update(tag, progress=None, should_stop=None):
    while not (should_stop and should_stop()):
        time.sleep(0.02)
    return False, 'update cancelled', None

updater.perform_update = _fake_update

_orig = gui.MainWindow

class _ClosingWindow(_orig):
    def __init__(self, *a, **k):
        super().__init__(*a, **k)
        self.worker = gui.RepairWorker(['fake.stl'], parent=self)
        self.worker.start()
        self._method_worker = gui.AnalyzeWorker(['fake.stl'], parent=self)
        self._method_worker.start()
        self.heatmap_worker = gui.HeatmapWorker('fake.stl', 'k', 100, 100, self)
        self.heatmap_worker.start()
        self.before_after_worker = gui.BeforeAfterWorker(
            'fake.stl', 'fake_fixed.stl', 100, 100, self)
        self.before_after_worker.start()
        self._viewer_worker = gui.ViewerDataWorker(
            'fake.stl', 'fake_fixed.stl', 100, 100, self)
        self._viewer_worker.start()
        self._repeat_picker_worker = gui.RepeatPickerWorker('fake.stl', self)
        self._repeat_picker_worker.start()
        self.update_worker = gui.UpdateWorker('v0.0.0', parent=self)
        self.update_worker.start()
        print('WORKERS-STARTED', int(all(w.isRunning() for w in (
            self.worker, self._method_worker, self.heatmap_worker,
            self.before_after_worker, self._viewer_worker,
            self._repeat_picker_worker, self.update_worker))))
        QTimer.singleShot(200, self.close)

gui.MainWindow = _ClosingWindow
try:
    gui.main()
except SystemExit as e:
    print('EXEC-RC', e.code)
    raise
'''


def test_close_during_running_workers_exits_cleanly():
    with tempfile.TemporaryDirectory(prefix='sutura-home-') as home:
        cfg_dir = os.path.join(home, '.config', 'sutura')
        os.makedirs(cfg_dir)
        with open(os.path.join(cfg_dir, 'config.json'), 'w') as f:
            json.dump({'check_for_updates': False, 'history_enabled': False}, f)
        env = dict(os.environ)
        env['HOME'] = home
        env['QT_QPA_PLATFORM'] = 'offscreen'
        env.pop('APPIMAGE', None)
        env['QT_PLUGIN_PATH'] = qt_plugins_path()
        r = subprocess.run(
            [sys.executable, '-c', CHILD.format(sutura=SUTURA)],
            capture_output=True, text=True, timeout=90, env=env)

    assert r.returncode == 0, (
        'GUI exited with %s (expected 0)\nstdout:\n%s\nstderr:\n%s'
        % (r.returncode, r.stdout[-2000:], r.stderr[-2000:]))
    assert 'Destroyed while thread' not in r.stderr, r.stderr[-2000:]
    assert 'WORKERS-STARTED 1' in r.stdout, r.stdout[-1000:]
    assert 'EXEC-RC 0' in r.stdout, r.stdout[-1000:]


if __name__ == '__main__':
    test_close_during_running_workers_exits_cleanly()
    print('gui shutdown workers test passed')
