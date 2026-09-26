#!/usr/bin/env python3
"""GUI test for the tabbed Options window (OptionsDialog).

Checks that the dialog exists with its five tabs, that the batch-wide
checkboxes live in it, that "Check on every start" is only enabled while
automatic updates is on (and both write config.json), and that toggling an
option still updates the count on the Options button.

Runs the GUI in a SUBPROCESS with QT_QPA_PLATFORM=offscreen and a temporary
HOME, so the user's real ~/.config/sutura/config.json is never touched.

Usage:
    ~/.local/share/sutura/venv/bin/python tests/test_options_dialog.py
"""
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')


def _run_gui(code):
    with tempfile.TemporaryDirectory() as home:
        env = dict(os.environ, HOME=home, QT_QPA_PLATFORM='offscreen')
        r = subprocess.run([sys.executable, '-c', code % SUTURA],
                           capture_output=True, text=True, timeout=120,
                           env=env)
    assert r.returncode == 0, (r.returncode, r.stderr[-800:])
    assert 'GUI-OK' in r.stdout, r.stdout[-500:]


def test_options_dialog_tabs_and_updates():
    _run_gui(
        "import sys, json\n"
        "sys.path.insert(0, %r)\n"
        "from PySide6.QtWidgets import QApplication\n"
        "import gui, updater\n"
        "app = QApplication([])\n"
        "w = gui.MainWindow()\n"
        "d = w._options_dialog\n"
        "assert d.tabs.count() == 5, d.tabs.count()\n"
        "assert not d.isVisible()\n"
        "w.btn_options.click()\n"
        "assert d.isVisible() and d.tabs.currentIndex() == d.TAB_REPAIR\n"
        "assert w.chk_fallback_ftetwild.window() is d\n"
        "assert w.chk_autorefine.window() is d\n"
        "d.chk_auto_update.setChecked(False)\n"
        "assert not d.chk_check_on_start.isEnabled()\n"
        "d.chk_auto_update.setChecked(True)\n"
        "assert d.chk_check_on_start.isEnabled()\n"
        "d.chk_check_on_start.setChecked(True)\n"
        "cfg = updater.load_config()\n"
        "assert cfg['check_for_updates'] is True and cfg['check_on_startup'] is True, cfg\n"
        "d.chk_auto_update.setChecked(False)\n"
        "assert not d.chk_check_on_start.isEnabled()\n"
        "assert updater.should_check(updater.load_config()) is False\n"
        "d.tabs.setCurrentIndex(d.TAB_CHANGELOG)\n"
        "assert d._changelog_worker is None  # no network when headless\n"
        "assert d.changelog.toPlainText().strip()\n"
        "d.close()\n"
        "assert not d.isVisible()\n"
        "w.close()\n"
        "print('GUI-OK')\n")


def test_options_button_count():
    _run_gui(
        "import sys\n"
        "sys.path.insert(0, %r)\n"
        "from PySide6.QtWidgets import QApplication\n"
        "import gui\n"
        "app = QApplication([])\n"
        "w = gui.MainWindow()\n"
        "base = gui._t('options_btn')\n"
        "assert w.btn_options.text() == base, w.btn_options.text()\n"
        "w.chk_autorefine.setChecked(True)\n"
        "assert w.btn_options.text() == gui._t('options_btn_n', 1)\n"
        "w.chk_fallback_ftetwild.setChecked(False)\n"
        "assert w.btn_options.text() == gui._t('options_btn_n', 2)\n"
        "w.chk_autorefine.setChecked(False)\n"
        "w.chk_fallback_ftetwild.setChecked(True)\n"
        "assert w.btn_options.text() == base\n"
        "assert w._ftetwild == 'auto'\n"
        "w.close()\n"
        "print('GUI-OK')\n")


def test_intensity_combo_and_reset():
    _run_gui(
        "import sys\n"
        "sys.path.insert(0, %r)\n"
        "from PySide6.QtWidgets import QApplication\n"
        "from PySide6.QtCore import Qt\n"
        "import gui, updater\n"
        "app = QApplication([])\n"
        "w = gui.MainWindow()\n"
        "combo = w.intensity_combo\n"
        "assert combo.count() == 4, combo.count()\n"
        "names = [combo.itemData(i) for i in range(combo.count())]\n"
        "assert names == ['quick', 'balanced', 'thorough', 'extreme'], names\n"
        "for i in range(combo.count()):\n"
        "    assert combo.itemData(i, Qt.ToolTipRole), i\n"
        "assert combo.currentData() == 'balanced', combo.currentData()\n"
        "assert w._intensity == 'balanced'\n"
        "# choosing a non-default preset updates state, the fTetWild checkbox\n"
        "# and the persisted config value\n"
        "w.chk_autorefine.setChecked(True)\n"
        "w.chk_edge_tiebreak.setChecked(True)\n"
        "combo.setCurrentIndex(combo.findData('quick'))\n"
        "assert w._intensity == 'quick', w._intensity\n"
        "assert not w.chk_fallback_ftetwild.isChecked()\n"
        "assert updater.load_config().get('intensity') == 'quick'\n"
        "# Reset to recommended restores Balanced AND every checkbox default\n"
        "w.btn_intensity_reset.click()\n"
        "assert w._intensity == 'balanced', w._intensity\n"
        "assert combo.currentData() == 'balanced'\n"
        "assert w.chk_fallback_ftetwild.isChecked()\n"
        "assert not w.chk_autorefine.isChecked()\n"
        "assert not w.chk_edge_tiebreak.isChecked()\n"
        "assert updater.load_config().get('intensity') == 'balanced'\n"
        "assert gui._t('intensity_tip_quick')\n"
        "w.close()\n"
        "print('GUI-OK')\n")


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok ', name)
    print('options dialog tests passed')


if __name__ == '__main__':
    main()
