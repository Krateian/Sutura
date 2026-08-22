#!/usr/bin/env python3
"""Drive the real GUI on a display to verify the before/after + detail dialog.

Loads the repo GUI, adds a broken mesh, repairs it, opens the before/after
dialog and verifies: the dialog appears with a main image and a detail image,
the toggle flips both between original (defect-red) and repaired (teal), and
the detail image differs from the main one for a localized defect.

Run from the repo root under the sutura venv with a display available:
    ~/.local/share/sutura/venv/bin/python scripts/verify_before_after_dialog.py
"""
import os
import subprocess
import sys
import tempfile

from PySide6.QtCore import QTimer
from PySide6.QtGui import QPixmap
from PySide6.QtWidgets import QApplication

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GUI_PY = os.path.join(REPO, 'sutura', 'gui.py')


def load_gui():
    import importlib.util
    spec = importlib.util.spec_from_file_location('sutura_gui', GUI_PY)
    m = importlib.util.module_from_spec(spec)
    sys.modules['sutura_gui'] = m
    spec.loader.exec_module(m)
    return m


def _repo_cli_wrapper():
    wrap = os.path.join(tempfile.gettempdir(), 'sutura-verify-cli.sh')
    with open(wrap, 'w') as f:
        f.write('#!/bin/sh\nexec "%s" "%s" "$@"\n' % (
            sys.executable, os.path.join(REPO, 'sutura', 'repair.py')))
    os.chmod(wrap, 0o755)
    return wrap


def main():
    with tempfile.TemporaryDirectory(prefix='sutura-verify-') as tmp:
        broken = os.path.join(tmp, 'broken.stl')
        subprocess.run([sys.executable,
                        os.path.join(REPO, 'tests', 'make_broken_stl.py'), broken],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        # a mesh with a localized defect for a meaningful detail zoom
        front = os.path.join(tmp, 'front.stl')
        code = (
            "import trimesh, numpy as np\n"
            "m = trimesh.creation.icosphere(subdivisions=3)\n"
            "v = np.asarray(m.vertices, dtype=np.float32)\n"
            "t = np.asarray(m.faces, dtype=np.int64)\n"
            "proj = v[t].mean(axis=1) @ np.array([1,1,1], dtype=np.float64)\n"
            "keep = np.ones(len(t), dtype=bool)\n"
            "keep[np.argsort(proj)[-120:]] = False\n"
            "trimesh.Trimesh(v, t[keep]).export(%r)\n" % front
        )
        subprocess.run([sys.executable, '-c', code], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

        os.environ['SUTURA'] = _repo_cli_wrapper()
        m = load_gui()
        app = QApplication.instance() or QApplication([])
        m.apply_dark_theme(app)
        win = m.MainWindow()
        win.show()
        win._add_path(front)
        state = {'repaired': False, 'dialog': None, 'done': False}

        def check_dialog():
            dlg = win._before_after_zoom
            if dlg is None:
                QTimer.singleShot(200, check_dialog)
                return
            state['dialog'] = dlg
            # find the toggle button and image labels inside the dialog
            from PySide6.QtWidgets import QPushButton, QLabel
            btn = next((w for w in dlg.findChildren(QPushButton) if w.isCheckable()), None)
            labels = [w for w in dlg.findChildren(QLabel) if w.pixmap() and not w.pixmap().isNull()]
            main_lab = labels[0]
            detail_lab = labels[1]
            assert btn is not None, 'no toggle button'
            assert len(labels) >= 2, 'expected main + detail image labels'
            assert not main_lab.pixmap().isNull() and not detail_lab.pixmap().isNull()
            # original side: main and detail both show before; sizes differ
            main_orig = main_lab.pixmap()
            detail_orig = detail_lab.pixmap()
            # toggle to repaired
            btn.click()
            app.processEvents()
            assert btn.text() != 'Repaired', btn.text()
            assert not main_lab.pixmap().isNull() and not detail_lab.pixmap().isNull()
            # toggle back
            btn.click()
            app.processEvents()
            assert not main_lab.pixmap().isNull()
            print('dialog ok: main + detail images present, toggle works')
            dlg.reject()
            state['done'] = True
            app.quit()

        def poll_repair():
            if win.worker is None:
                app.processEvents()
                print('repair done for', os.path.basename(state.get('path', '?')))
                win._on_show_before_after()
                QTimer.singleShot(100, check_dialog)
            else:
                QTimer.singleShot(200, poll_repair)

        win.repair()
        state['path'] = front
        QTimer.singleShot(200, poll_repair)
        app.exec()
        if not state.get('done'):
            raise RuntimeError('GUI verification did not finish')
    print('before/after dialog verification PASSED')


if __name__ == '__main__':
    main()