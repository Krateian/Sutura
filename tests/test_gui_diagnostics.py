#!/usr/bin/env python3
"""Offscreen regression for the health-banner diagnostics export button.

Runs as its own process (the GUI must never import pymeshlab; this file does
not import repair). A temp HOME/XDG_CONFIG_HOME isolates config/history.

Usage: QT_QPA_PLATFORM=offscreen <venv>/bin/python tests/test_gui_diagnostics.py
"""
import os
import sys
import tempfile
import zipfile
from unittest.mock import patch

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import diagnostics  # noqa: E402  (top-level module; gui's `sutura.diagnostics` fallback)

# PySide6's own Qt6 plugins (a macOS conda install also carries Qt5).
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _qt_test_env import ensure_qt_plugins  # noqa: E402
ensure_qt_plugins()

_TMP = tempfile.mkdtemp(prefix='sutura-gui-diag-')
os.environ['HOME'] = _TMP
os.environ['XDG_CONFIG_HOME'] = os.path.join(_TMP, 'config')
os.environ.pop('APPIMAGE', None)
os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

from PySide6.QtWidgets import QApplication, QWidget  # noqa: E402

app = QApplication([])
import gui  # noqa: E402


class _FakeFileDialog:
    path = None

    @staticmethod
    def getSaveFileName(*_a, **_k):
        return _FakeFileDialog.path, ''


class _FakeMessageBox:
    info = 0
    warn = 0

    @staticmethod
    def information(*_a, **_k):
        _FakeMessageBox.info += 1

    @staticmethod
    def warning(*_a, **_k):
        _FakeMessageBox.warn += 1


def _reset():
    _FakeMessageBox.info = 0
    _FakeMessageBox.warn = 0


def test_health_banner_export_creates_zip():
    out = os.path.join(_TMP, 'diag.zip')
    _FakeFileDialog.path = out
    _reset()
    with patch.object(gui, 'QFileDialog', _FakeFileDialog), \
            patch.object(gui, 'QMessageBox', _FakeMessageBox):
        # The handler uses ``self`` only as a dialog parent.
        gui.MainWindow._on_health_diag_clicked(QWidget())
    assert os.path.isfile(out) and os.path.getsize(out) > 0
    with zipfile.ZipFile(out) as zf:
        assert 'system_info.json' in zf.namelist()
    assert _FakeMessageBox.info == 1, _FakeMessageBox.info
    assert _FakeMessageBox.warn == 0, _FakeMessageBox.warn


def test_health_banner_export_failure_is_shown_not_raised():
    _FakeFileDialog.path = os.path.join(_TMP, 'bad.zip')
    _reset()
    with patch.object(gui, 'QFileDialog', _FakeFileDialog), \
            patch.object(gui, 'QMessageBox', _FakeMessageBox), \
            patch.object(diagnostics, 'create_diagnostics_zip',
                         side_effect=RuntimeError('boom')):
        gui.MainWindow._on_health_diag_clicked(QWidget())  # must not raise
    assert _FakeMessageBox.warn == 1, _FakeMessageBox.warn
    assert _FakeMessageBox.info == 0


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('gui diagnostics tests passed')


if __name__ == '__main__':
    main()
