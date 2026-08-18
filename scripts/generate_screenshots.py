#!/usr/bin/env python3
"""Regenerate the README GUI screenshots (assets/*.png) from the real GUI.

Runs the installed sutura GUI offscreen, drives a real batch repair, and
renders the key moments to PNGs under assets/. This keeps the screenshots in
sync with the actual UI whenever a visible behaviour changes.

Run from the repo root after `./install.sh` (so the installed copy matches):

    ~/.local/share/sutura/venv/bin/python scripts/generate_screenshots.py

Outputs (overwrites in assets/):
  screenshot.png     - main GUI: batch summary strip + defect panel, a
                       broken mesh selected (shows mm diameters + type)
  defect-panel.png   - defect detail panel with the selected file's defects

The meshes used are generated into a temp dir and removed afterwards.
"""
import os
import subprocess
import sys
import tempfile

from PySide6.QtCore import QTimer
from PySide6.QtWidgets import QApplication

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GUI_PY = os.path.expanduser('~/.local/share/sutura/gui.py')
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


def run(m, files, size, select_broken, out_path):
    app = QApplication.instance() or QApplication([])
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
            win.grab().save(out_path)
            done['ok'] = True
            app.quit()
        else:
            QTimer.singleShot(200, poll)

    win.repair()
    QTimer.singleShot(200, poll)
    app.exec()
    if not done.get('ok'):
        raise RuntimeError('screenshot render did not finish: %s' % out_path)
    print('wrote', out_path)


def main():
    m = load_gui()
    with tempfile.TemporaryDirectory(prefix='sutura-ss-') as tmp:
        broken, clean = make_meshes(tmp)
        files = [broken, clean]
        run(m, files, MAIN_SIZE, select_broken=True,
            out_path=os.path.join(ASSETS, 'screenshot.png'))
        run(m, files, PANEL_SIZE, select_broken=True,
            out_path=os.path.join(ASSETS, 'defect-panel.png'))
    print('done')


if __name__ == '__main__':
    main()
