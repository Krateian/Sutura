#!/usr/bin/env python3
"""Sutura - Qt (PySide6) GUI frontend.

Pick or drop mesh files, run the two-stage repair on each via the installed
CLI, and read back what was fixed. Results are always written to new
"_fixed" files. Repair runs in a background thread and can be stopped.
"""
import os
import sys
import json
import subprocess

from PySide6.QtCore import Qt, QThread, Signal
from PySide6.QtGui import QIcon, QFontDatabase
from PySide6.QtWidgets import (
    QApplication, QMainWindow, QWidget, QVBoxLayout, QHBoxLayout,
    QTreeWidget, QTreeWidgetItem, QPushButton, QFileDialog,
    QProgressBar, QPlainTextEdit, QLabel, QAbstractItemView)

SUTURA = os.path.expanduser('~/.local/bin/sutura')

# PySide6 bundles its own Qt plugins and cannot see the system platform
# theme (plasma-integration), so QFileDialog would fall back to Qt's
# embedded widget (no rubber-band rectangle selection). Point Qt at the
# system plugin directory and select the KDE theme before QApplication.
# Safe because the system Qt version matches the bundled one.
if sys.platform.startswith('linux'):
    os.environ.setdefault('QT_QPA_PLATFORMTHEME', 'kde')
    _sys_plugins = '/usr/lib/qt6/plugins'
    if os.path.isdir(_sys_plugins):
        _existing = os.environ.get('QT_PLUGIN_PATH', '')
        if _sys_plugins not in _existing.split(os.pathsep):
            os.environ['QT_PLUGIN_PATH'] = (
                (_existing + os.pathsep) if _existing else '') + _sys_plugins

# --- CLI output parsing. Kept from the previous GUI - do not rewrite. ------
def parse_cli_output(out, err):
    try:
        return json.loads(out.strip().splitlines()[-1])
    except Exception:
        return {'error': err.strip() or out.strip()}


def summarize(data):
    if data.get('error') and 'stage1' not in data:
        return 'ERROR'
    s1 = data.get('stage1', {})
    if s1.get('two_manifold') and s1.get('holes_remaining', 0) == 0:
        return 'watertight'
    if s1.get('two_manifold'):
        return '%d hole(s)' % s1.get('holes_remaining', 0)
    return 'partial'


def format_report(data):
    if data.get('error') and 'stage1' not in data:
        return 'ERROR: %s' % data['error']
    s1 = data.get('stage1', {})
    lines = []
    lines.append('Output file: %s' % data.get('output'))
    lines.append('')
    lines.append('Stage 1 (MeshLab):')
    lines.append('  Holes closed              : %d' % s1.get('holes_closed', 0))
    lines.append('  Holes remaining           : %d' % s1.get('holes_remaining', 0))
    lines.append('  Non-manifold edges fixed  : %d' % s1.get('non_manifold_edges_fixed', 0))
    lines.append('  Faces removed             : %d' % s1.get('faces_removed', 0))
    lines.append('  Connected components      : %d' % s1.get('components', 0))
    lines.append('  Two-manifold              : %s' % ('YES' if s1.get('two_manifold') else 'NO'))
    if 'stage2' in data:
        s2 = data['stage2']
        lines.append('')
        lines.append('Stage 2 (Manifold):')
        if 'error' in s2:
            if s2['error'].startswith('Stage 2 skipped'):
                lines.append('  SKIPPED: %s' % s2['error'])
            else:
                lines.append('  ERROR: %s' % s2['error'])
        else:
            lines.append('  Input triangles : %d' % s2.get('input_faces', 0))
            lines.append('  Output triangles: %d' % s2.get('output_faces', 0))
            if s2.get('shells_merged'):
                lines.append('  Shells merged   : %d' % s2['shells_merged'])
            lines.append('  Volume (after)  : %.4f' % s2.get('volume_after', 0))
    if 'objects' in data:
        lines.append('')
        lines.append('3MF objects repaired: %d' % data.get('objects', 0))
        for i, rep in enumerate(data.get('object_reports', [])):
            s1o = rep.get('stage1', {})
            ok = s1o.get('two_manifold') and s1o.get('holes_remaining', 0) == 0
            lines.append('  object %d: %s (%d hole(s) remaining, two-manifold=%s)' % (
                i, 'watertight' if ok else 'partial',
                s1o.get('holes_remaining', 0), 'YES' if s1o.get('two_manifold') else 'NO'))
    return '\n'.join(lines)


class RepairWorker(QThread):
    """Repairs files sequentially in a background thread."""

    file_started = Signal(str)
    file_done = Signal(str, str, str)   # path, summary, report
    progress = Signal(int, int)          # current, total
    all_done = Signal(bool)              # cancelled

    def __init__(self, files, parent=None):
        super().__init__(parent)
        self._files = list(files)
        self._cancelled = False
        self._proc = None

    def cancel(self):
        self._cancelled = True
        if self._proc is not None and self._proc.poll() is None:
            self._proc.terminate()

    def run(self):
        n = len(self._files)
        for idx, path in enumerate(self._files, 1):
            if self._cancelled:
                self.file_done.emit(path, 'Cancelled', '')
                continue
            self.file_started.emit(path)
            data = self._run_one(path)
            if self._cancelled:
                self.file_done.emit(path, 'Stopped', '')
                continue
            self.file_done.emit(path, summarize(data), format_report(data))
            self.progress.emit(idx, n)
        self.all_done.emit(self._cancelled)

    def _run_one(self, path):
        self._proc = subprocess.Popen(
            [SUTURA, path], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            out, err = self._proc.communicate(timeout=600)
        except subprocess.TimeoutExpired:
            self._proc.kill()
            out, err = self._proc.communicate()
            return {'error': 'timeout while repairing'}
        finally:
            self._proc = None
        return parse_cli_output(out, err)


class MainWindow(QMainWindow):
    MESH_EXTS = ('.stl', '.3mf')

    def __init__(self):
        super().__init__()
        self.setWindowTitle('Sutura')
        self.resize(780, 620)
        self.setWindowIcon(self._load_icon())
        self.setAcceptDrops(True)

        self.files = []
        self._item_by_path = {}
        self.worker = None

        self._build_ui()
        self._apply_accent()

    # --- UI ---------------------------------------------------------------
    def _build_ui(self):
        central = QWidget(self)
        self.setCentralWidget(central)
        layout = QVBoxLayout(central)

        self.tree = QTreeWidget()
        self.tree.setColumnCount(2)
        self.tree.setHeaderLabels(['File', 'Result'])
        self.tree.setColumnWidth(0, 580)
        self.tree.setSelectionMode(QAbstractItemView.ExtendedSelection)
        layout.addWidget(self.tree)

        buttons = QHBoxLayout()
        self.btn_add_files = QPushButton('Add files…')
        self.btn_add_folder = QPushButton('Add folder…')
        self.btn_remove = QPushButton('Remove selected')
        self.btn_clear = QPushButton('Clear')
        self.btn_repair = QPushButton('Repair')
        self.btn_repair.setObjectName('repairBtn')
        self.btn_stop = QPushButton('Stop')
        buttons.addWidget(self.btn_add_files)
        buttons.addWidget(self.btn_add_folder)
        buttons.addWidget(self.btn_remove)
        buttons.addWidget(self.btn_clear)
        buttons.addStretch(1)
        buttons.addWidget(self.btn_repair)
        buttons.addWidget(self.btn_stop)
        layout.addLayout(buttons)

        row = QHBoxLayout()
        self.progress = QProgressBar()
        self.progress.setRange(0, 1)
        self.progress.setValue(0)
        self.status = QLabel('Ready')
        row.addWidget(self.progress, 1)
        row.addWidget(self.status)
        layout.addLayout(row)

        self.log = QPlainTextEdit()
        self.log.setReadOnly(True)
        self.log.setFont(QFontDatabase.systemFont(QFontDatabase.FixedFont))
        layout.addWidget(self.log, 1)

        self.btn_add_files.clicked.connect(self.add_files)
        self.btn_add_folder.clicked.connect(self.add_folder)
        self.btn_remove.clicked.connect(self.remove_selected)
        self.btn_clear.clicked.connect(self.clear_files)
        self.btn_repair.clicked.connect(self.repair)
        self.btn_stop.clicked.connect(self.stop)

        self.btn_repair.setEnabled(False)
        self.btn_stop.setEnabled(False)

    def _apply_accent(self):
        # Only the brand teal accent; the rest comes from the system theme.
        self.setStyleSheet('''
            QPushButton#repairBtn {
                background-color: #14b8a6; color: #0b0f11;
                border: none; border-radius: 4px; padding: 6px 18px; font-weight: bold;
            }
            QPushButton#repairBtn:hover { background-color: #17c9b4; }
            QPushButton#repairBtn:disabled { background-color: palette(mid); color: palette(midlight); }
            QProgressBar::chunk { background-color: #14b8a6; }
        ''')

    def _load_icon(self):
        for size in (48, 32, 64, 128):
            path = os.path.expanduser(
                '~/.local/share/icons/hicolor/%dx%d/apps/sutura.png' % (size, size))
            if os.path.exists(path):
                return QIcon(path)
        return QIcon()

    # --- adding files / folders --------------------------------------------
    def _add_path(self, path):
        if not path or path in self.files:
            return False
        self.files.append(path)
        item = QTreeWidgetItem([path, ''])
        self.tree.addTopLevelItem(item)
        self._item_by_path[path] = item
        return True

    def _add_meshes_from_folder(self, folder):
        added = 0
        try:
            entries = sorted(os.listdir(folder))
        except OSError:
            return 0
        for name in entries:
            if name.lower().endswith(self.MESH_EXTS):
                if self._add_path(os.path.join(folder, name)):
                    added += 1
        return added

    def _add_path_or_folder(self, path):
        if os.path.isdir(path):
            return self._add_meshes_from_folder(path)
        return 1 if self._add_path(path) else 0

    def add_files(self):
        paths, _ = QFileDialog.getOpenFileNames(
            self, 'Select mesh files', '',
            'Mesh files (*.stl *.STL *.3mf *.3MF);;All files (*)')
        added = sum(1 for p in paths if self._add_path(p))
        if added:
            self._log('Added %d file(s)' % added)
        self._refresh_buttons()

    def add_folder(self):
        folder = QFileDialog.getExistingDirectory(self, 'Select a folder with mesh files')
        if folder:
            added = self._add_meshes_from_folder(folder)
            if added:
                self._log('Added %d file(s) from %s' % (added, folder))
            else:
                self.status.setText('No mesh files in that folder')
            self._refresh_buttons()

    def remove_selected(self):
        for item in self.tree.selectedItems():
            path = item.text(0)
            if path in self.files:
                self.files.remove(path)
            self._item_by_path.pop(path, None)
            self.tree.takeTopLevelItem(self.tree.indexOfTopLevelItem(item))
        self._refresh_buttons()

    def clear_files(self):
        self.files.clear()
        self._item_by_path.clear()
        self.tree.clear()
        self._refresh_buttons()

    # --- drag & drop (whole window) ----------------------------------------
    def dragEnterEvent(self, event):
        if event.mimeData().hasUrls():
            event.acceptProposedAction()

    def dragMoveEvent(self, event):
        if event.mimeData().hasUrls():
            event.acceptProposedAction()

    def dropEvent(self, event):
        added = 0
        for url in event.mimeData().urls():
            path = url.toLocalFile()
            if path:
                added += self._add_path_or_folder(path)
        if added:
            self._log('Added %d file(s) (drag)' % added)
            self._refresh_buttons()
        event.acceptProposedAction()

    # --- repair ------------------------------------------------------------
    def repair(self):
        if not self.files or self.worker is not None:
            return
        for i in range(self.tree.topLevelItemCount()):
            self.tree.topLevelItem(i).setText(1, '')
        self.btn_repair.setEnabled(False)
        self.btn_stop.setEnabled(True)
        self.progress.setRange(0, len(self.files))
        self.progress.setValue(0)
        self.status.setText('Repairing…')
        self.log.clear()

        self.worker = RepairWorker(self.files, self)
        self.worker.file_done.connect(self._on_file_done)
        self.worker.progress.connect(self._on_progress)
        self.worker.all_done.connect(self._on_all_done)
        self.worker.start()

    def stop(self):
        if self.worker is not None:
            self.worker.cancel()
            self.btn_stop.setEnabled(False)

    def _on_file_done(self, path, summary, report):
        item = self._item_by_path.get(path)
        if item is not None:
            item.setText(1, summary)
        if report:
            self._log('%s\n%s' % (path, report))

    def _on_progress(self, current, total):
        self.progress.setValue(current)
        self.status.setText('Repairing… %d/%d' % (current, total))

    def _on_all_done(self, cancelled):
        self.status.setText('Done' + (' (stopped)' if cancelled else ''))
        self.btn_stop.setEnabled(False)
        self.btn_repair.setEnabled(bool(self.files))
        self.worker = None

    def _log(self, text):
        self.log.appendPlainText(text)
        self.log.verticalScrollBar().setValue(self.log.verticalScrollBar().maximum())

    def _refresh_buttons(self):
        self.btn_repair.setEnabled(bool(self.files) and self.worker is None)


def main():
    app = QApplication(sys.argv)
    win = MainWindow()
    for p in sys.argv[1:]:
        win._add_path(p)
    win._refresh_buttons()
    win.show()
    sys.exit(app.exec())


if __name__ == '__main__':
    main()