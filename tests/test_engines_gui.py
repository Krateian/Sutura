#!/usr/bin/env python3
"""Offscreen regression test for the Options -> Engines tab.

Runs as its OWN process (the GUI must never import pymeshlab; this file does
not import repair). A temp HOME/XDG_CONFIG_HOME isolates config and engines.

Checks:
- the tab builds and reports the fTetWild status without freezing,
- the configured engine list shows name / placement / binary state,
- Install/Remove follow the status,
- on an AppImage (APPIMAGE env) the section is disabled with the reason.

Usage:
    QT_QPA_PLATFORM=offscreen <venv>/bin/python tests/test_engines_gui.py
"""
import os
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

_TMP = tempfile.mkdtemp(prefix='sutura-gui-eng-')
os.environ['HOME'] = _TMP
os.environ['XDG_CONFIG_HOME'] = os.path.join(_TMP, 'config')
os.environ.pop('APPIMAGE', None)
os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

ENG_DIR = os.path.join(os.environ['XDG_CONFIG_HOME'], 'sutura', 'engines')
os.makedirs(ENG_DIR, exist_ok=True)
with open(os.path.join(ENG_DIR, 'copycat.toml'), 'w') as f:
    f.write('name = "copycat"\ncommand = ["/bin/cp", "{input}", "{output}"]\n'
            'placement = "after_stage1"\nenabled = true\n')

from PySide6.QtWidgets import QApplication  # noqa: E402

app = QApplication([])
import gui  # noqa: E402


def _wait(dialog):
    w = dialog._engines_worker
    if w is not None:
        w.wait(20000)
    app.processEvents()


def main():
    win = gui.MainWindow()
    d = win._options_dialog
    d.open_tab(gui.OptionsDialog.TAB_ENGINES)
    _wait(d)
    text = d.lbl_ftetwild_status.text()
    assert text and text != gui._t('engines_status_checking'), text
    assert d.engines_list.count() == 1, d.engines_list.count()
    item = d.engines_list.item(0).text()
    assert 'copycat' in item and 'after_stage1' in item, item
    assert gui._t('engines_binary_ok') in item, item
    print('ok  engines tab shows the configured engine')

    # AppImage: unsupported -> disabled with a reason.
    os.environ['APPIMAGE'] = _TMP
    d.refresh_engines(force=True)
    _wait(d)
    assert not d.btn_ftetwild_install.isEnabled()
    assert not d.btn_ftetwild_remove.isEnabled()
    assert d.lbl_ftetwild_status.text() != text, d.lbl_ftetwild_status.text()
    os.environ.pop('APPIMAGE', None)
    print('ok  appimage disables the fTetWild manager')

    win.close()
    print('\nengines GUI tests passed')


if __name__ == '__main__':
    main()
