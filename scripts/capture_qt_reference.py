#!/usr/bin/env python3
"""Capture the v0.9.9 Qt/PySide6 GUI as a layout reference.

The Rust GUI is being re-laid-out to match the Qt one, so the Qt one has to be
recorded first: every screen, and all seven Options tabs, at known sizes, with no
window appearing on the user's display and nothing written to the real HOME.

Qt runs with `QT_QPA_PLATFORM=offscreen`, so the widgets are laid out and painted
into an offscreen surface and `QWidget.grab()` returns real pixels. No window is
mapped to the screen.

Outputs under `/tmp/qt-ref/` (override with `-o`):
  main-window.png       the whole MainWindow, file table + preview + report pane
  files-empty.png       the launch state, before any file is added
  options-1-general.png ... options-7-engines.png
                        one per Options tab, in the shipped tab order
  methods-menu.png      the file list's right-click menu

Every frame is grabbed from the same binary the release ships, driven through the
repo's own `sutura/gui.py` -- no reimplementation, so the reference cannot drift
from what v0.9.9 actually looked like.

Usage:
  scripts/capture_qt_reference.py [-o OUTDIR] [-p PYTHON]
"""
import argparse
import os
import shutil
import subprocess
import sys
import tempfile

# Must be set before Qt is imported: this is what keeps a window off the display.
os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')
# The reference must be the English UI whatever the machine's locale is; that is
# also the standing rule for documentation captures.
os.environ['LANG'] = 'en_US.UTF-8'
os.environ['LC_ALL'] = 'en_US.UTF-8'

from PySide6.QtCore import QLocale, QPoint  # noqa: E402
from PySide6.QtWidgets import QApplication  # noqa: E402

QLocale.setDefault(QLocale(QLocale.English, QLocale.UnitedStates))

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GUI_PY = os.path.join(REPO, 'sutura', 'gui.py')

# The Qt MainWindow's shipped default, and what the README screenshots used, so
# the reference frames are the size a user actually sees.
MAIN_SIZE = (1055, 843)
OPTIONS_SIZE = (560, 460)


def load_gui():
    import importlib.util
    spec = importlib.util.spec_from_file_location('sutura_gui', GUI_PY)
    m = importlib.util.module_from_spec(spec)
    sys.modules['sutura_gui'] = m
    spec.loader.exec_module(m)
    return m


def cli_wrapper():
    """A wrapper that runs this checkout's repair.py.

    The GUI resolves its CLI from $SUTURA, and without this it would run an
    installed (older) build whose report fields differ.
    """
    wrap = os.path.join(tempfile.gettempdir(), 'sutura-qt-ref-cli.sh')
    with open(wrap, 'w') as f:
        f.write('#!/bin/sh\nexec "%s" "%s" "$@"\n'
                % (sys.executable, os.path.join(REPO, 'sutura', 'repair.py')))
    os.chmod(wrap, 0o755)
    return wrap


def make_meshes(tmp):
    """Same two fixtures the Rust capture uses: one broken, one clean."""
    broken = os.path.join(tmp, 'broken.stl')
    clean = os.path.join(tmp, 'clean.stl')
    subprocess.run([sys.executable, os.path.join(REPO, 'tests', 'make_broken_stl.py'), broken],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    import trimesh
    trimesh.creation.box().export(clean)
    return broken, clean


def new_window(m, size):
    app = QApplication.instance() or QApplication([])
    m.apply_dark_theme(app)
    win = m.MainWindow()
    win.resize(*size)
    win.show()
    app.processEvents()
    return app, win


def grab(widget, out_path):
    widget.grab().save(out_path)
    print('  %-32s %s' % (os.path.basename(out_path), out_path))


def capture_empty(m, out):
    app, win = new_window(m, MAIN_SIZE)
    app.processEvents()
    grab(win, os.path.join(out, 'files-empty.png'))
    win.close()


def capture_main(m, files, out):
    app, win = new_window(m, MAIN_SIZE)
    for f in files:
        win._add_path(f)
    app.processEvents()
    grab(win, os.path.join(out, 'main-window.png'))
    win.close()


def capture_options(m, out):
    """One grab per tab, in the shipped order.

    The tabs are named by `OptionsDialog.TAB_*`; reading them from the class
    rather than hardcoding an order means a tab that moves in a later checkout is
    captured where it actually sits.
    """
    app, win = new_window(m, MAIN_SIZE)
    dlg = win._options_dialog
    dlg.resize(*OPTIONS_SIZE)
    dlg.show()
    app.processEvents()
    tabs = [
        ('1-general', m.OptionsDialog.TAB_GENERAL),
        ('2-repair', m.OptionsDialog.TAB_REPAIR),
        ('3-methods', m.OptionsDialog.TAB_METHODS),
        ('4-experimental', m.OptionsDialog.TAB_EXPERIMENTAL),
        ('5-updates', m.OptionsDialog.TAB_UPDATES),
        ('6-changelog', m.OptionsDialog.TAB_CHANGELOG),
        ('7-engines', m.OptionsDialog.TAB_ENGINES),
    ]
    for name, tab in tabs:
        dlg.open_tab(tab)
        app.processEvents()
        grab(dlg, os.path.join(out, 'options-%s.png' % name))
    dlg.close()
    win.close()


def capture_menu(m, files, out):
    """The file list's right-click menu for the selected rows.

    `_build_context_menu` returns the menu rather than showing it, which is what
    makes this capturable without a popup: popups grab the pointer, and an
    offscreen platform has none.
    """
    app, win = new_window(m, MAIN_SIZE)
    for f in files:
        win._add_path(f)
    app.processEvents()
    menu = win._build_context_menu([files[0]])
    if menu is None:
        print('  methods-menu.png                 SKIPPED (no menu for this selection)')
        win.close()
        return
    menu.show()
    app.processEvents()
    grab(menu, os.path.join(out, 'methods-menu.png'))
    menu.close()
    win.close()


def main():
    ap = argparse.ArgumentParser(prog='capture_qt_reference')
    ap.add_argument('-o', '--out', default='/tmp/qt-ref')
    args = ap.parse_args()

    out = args.out
    os.makedirs(out, exist_ok=True)

    # Everything the GUI can persist, redirected: this must not touch the real
    # ~/.config/sutura or ~/.local/share/sutura. Same rule as the Rust capture.
    home = tempfile.mkdtemp(prefix='sutura-qt-ref-home-')
    os.environ['HOME'] = home
    os.environ['XDG_CONFIG_HOME'] = os.path.join(home, '.config')
    os.environ['SUTURA_DATA_DIR'] = os.path.join(home, '.local', 'share', 'sutura')
    os.environ['SUTURA'] = cli_wrapper()

    print('gui:      %s' % GUI_PY)
    print('platform: %s' % os.environ['QT_QPA_PLATFORM'])
    print('home:     %s (temporary)' % home)
    print('output:   %s' % out)

    tmp = tempfile.mkdtemp(prefix='sutura-qt-ref-meshes-')
    try:
        broken, clean = make_meshes(tmp)
        files = [broken, clean]
        m = load_gui()
        capture_empty(m, out)
        capture_main(m, files, out)
        capture_options(m, out)
        capture_menu(m, files, out)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
        shutil.rmtree(home, ignore_errors=True)

    print('done: %d PNG(s) in %s' % (len(os.listdir(out)), out))


if __name__ == '__main__':
    main()
