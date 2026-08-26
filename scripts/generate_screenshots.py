#!/usr/bin/env python3
"""Regenerate the README GUI screenshots (assets/*.png) from the real GUI.

Runs the GUI offscreen, drives a real batch repair, and renders the key
moments to PNGs under assets/. This keeps the screenshots in sync with the
actual UI whenever a visible behaviour changes. The locale is forced to
English regardless of the system locale, so the screenshots are always the
English variant (standing rule).

Run from the repo root with a Python that has the GUI dependencies
(PySide6-Essentials + trimesh). The repo's own sutura/gui.py is loaded, so
no prior install is required:

    ~/.local/share/sutura/venv/bin/python scripts/generate_screenshots.py
    # or: python3 scripts/generate_screenshots.py   (if PySide6 + trimesh are present)

Outputs (overwrites in assets/):
  screenshot.png     - main GUI: batch summary strip + defect panel, a
                       broken mesh selected (shows mm diameters + type)
  defect-panel.png   - defect detail panel with the selected file's defects
  analyze-panel.png  - pre-repair analysis: the new 3-row button layout plus
                       the Analysis pane (type/mode/tuning, defect counts,
                       watertight verdict, estimated confidence + warning,
                       mode suggestions)
  before-after-panel.png - the before/after dialog (static view): the
                       Static/Interactive mode switch, the main
                       original/repaired image, the worst-defect detail
                       close-up and the toggle button

The meshes used are generated into a temp dir and removed afterwards.
"""
import os
import subprocess
import sys
import tempfile

from PySide6.QtCore import QTimer
from PySide6.QtWidgets import QApplication

# The README screenshots are always the English UI, whatever the system
# locale. Qt's QLocale.system() reads the environment at app startup, so set
# LANG before anything creates a QApplication.
os.environ['LANG'] = 'en_US.UTF-8'

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GUI_PY = os.path.join(REPO, 'sutura', 'gui.py')
ASSETS = os.path.join(REPO, 'assets')

# sizes match the historical README screenshots
MAIN_SIZE = (1055, 843)
PANEL_SIZE = (780, 620)


def load_gui():
    import importlib.util
    spec = importlib.util.spec_from_file_location('sutura_gui', GUI_PY)
    m = importlib.util.module_from_spec(spec)
    sys.modules['sutura_gui'] = m
    spec.loader.exec_module(m)
    return m


def _repo_cli_wrapper():
    """Return a path to an executable that runs the repo's own repair.py with
    the current interpreter. The GUI resolves the CLI from $SUTURA first, and
    without this it would pick up an installed (possibly older) CLI that lacks
    the latest report fields."""
    wrap = os.path.join(tempfile.gettempdir(), 'sutura-repo-cli.sh')
    with open(wrap, 'w') as f:
        f.write('#!/bin/sh\nexec "%s" "%s" "$@"\n' % (
            sys.executable, os.path.join(REPO, 'sutura', 'repair.py')))
    os.chmod(wrap, 0o755)
    return wrap


def make_meshes(tmp):
    broken = os.path.join(tmp, 'broken.stl')
    clean = os.path.join(tmp, 'clean.stl')
    subprocess.run([sys.executable,
                    os.path.join(REPO, 'tests', 'make_broken_stl.py'), broken],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run([sys.executable, '-c',
                    "import trimesh; trimesh.creation.box().export(%r)" % clean],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return broken, clean


def run(m, files, size, select_broken, out_path, show_heatmap=False):
    app = QApplication.instance() or QApplication([])
    m.apply_dark_theme(app)
    win = m.MainWindow()
    win.resize(*size)
    win.show()
    for f in files:
        win._add_path(f)

    done = {}

    def poll():
        if win.worker is None:
            win._render_summary()
            app.processEvents()
            if select_broken:
                win.tree.setCurrentItem(win.tree.topLevelItem(0))
                app.processEvents()
            if show_heatmap:
                win._on_show_heatmap()
                QTimer.singleShot(100, wait_heatmap)
                return
            win.grab().save(out_path)
            done['ok'] = True
            app.quit()
        else:
            QTimer.singleShot(200, poll)

    def wait_heatmap():
        if win.heatmap_worker is None:
            app.processEvents()
            win.grab().save(out_path)
            done['ok'] = True
            app.quit()
        else:
            QTimer.singleShot(100, wait_heatmap)

    win.repair()
    QTimer.singleShot(200, poll)
    app.exec()
    if not done.get('ok'):
        raise RuntimeError('screenshot render did not finish: %s' % out_path)
    print('wrote', out_path)


def run_analyze(m, files, size, select_broken, out_path):
    """Drive the pre-repair Analyze flow and grab the window once it is done.

    The Analysis pane shows per-file results; we then switch the selection to
    the broken mesh (index 0) so the screenshot captures a meaningful case
    with suggestions and an estimated confidence estimate."""
    app = QApplication.instance() or QApplication([])
    m.apply_dark_theme(app)
    win = m.MainWindow()
    win.resize(*size)
    win.show()
    for f in files:
        win._add_path(f)

    done = {}

    def poll():
        if win.worker is None:
            app.processEvents()
            if select_broken:
                win.tree.setCurrentItem(win.tree.topLevelItem(0))
                app.processEvents()
            win.grab().save(out_path)
            done['ok'] = True
            app.quit()
        else:
            QTimer.singleShot(200, poll)

    win.analyze()
    QTimer.singleShot(200, poll)
    app.exec()
    if not done.get('ok'):
        raise RuntimeError('analyze screenshot render did not finish: %s' % out_path)
    print('wrote', out_path)


def run_before_after(m, files, size, out_path):
    """Drive the before/after dialog and grab it (static view).

    Repairs the batch, selects the broken mesh (index 0), triggers
    ``_on_show_before_after`` (which spawns the BeforeAfterWorker subprocess)
    and grabs the modal dialog once it is open. The modal ``dlg.exec()``
    keeps running its own event loop, so the QTimer poll can fire while it
    is shown."""
    app = QApplication.instance() or QApplication([])
    m.apply_dark_theme(app)
    win = m.MainWindow()
    win.resize(*size)
    win.show()
    for f in files:
        win._add_path(f)

    done = {}

    def grab_dialog():
        dlg = win._before_after_zoom
        if (dlg is not None and dlg.isVisible()
                and win.before_after_worker is None):
            QTimer.singleShot(150, do_grab)
            return
        QTimer.singleShot(100, grab_dialog)

    def do_grab():
        dlg = win._before_after_zoom
        dlg.grab().save(out_path)
        done['ok'] = True
        dlg.close()
        app.quit()

    def poll():
        if win.worker is None:
            app.processEvents()
            win.tree.setCurrentItem(win.tree.topLevelItem(0))
            app.processEvents()
            win._on_show_before_after()
            QTimer.singleShot(200, grab_dialog)
        else:
            QTimer.singleShot(200, poll)

    win.repair()
    QTimer.singleShot(200, poll)
    app.exec()
    if not done.get('ok'):
        raise RuntimeError('before/after screenshot render did not finish: %s'
                           % out_path)
    print('wrote', out_path)


def main():
    os.environ['SUTURA'] = _repo_cli_wrapper()
    m = load_gui()
    with tempfile.TemporaryDirectory(prefix='sutura-ss-') as tmp:
        broken, clean = make_meshes(tmp)
        files = [broken, clean]
        run(m, files, MAIN_SIZE, select_broken=True, show_heatmap=True,
            out_path=os.path.join(ASSETS, 'screenshot.png'))
        run(m, files, PANEL_SIZE, select_broken=True, show_heatmap=True,
            out_path=os.path.join(ASSETS, 'defect-panel.png'))
        run_analyze(m, files, PANEL_SIZE, select_broken=True,
                    out_path=os.path.join(ASSETS, 'analyze-panel.png'))
        run_before_after(m, files, MAIN_SIZE,
                         out_path=os.path.join(ASSETS, 'before-after-panel.png'))
    print('done')


if __name__ == '__main__':
    main()
