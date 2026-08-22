#!/usr/bin/env python3
"""Sutura - Qt (PySide6) GUI frontend.

Pick or drop mesh files, run the two-stage repair on each via the installed
CLI, and read back what was fixed. Results are always written to new
"_fixed" files. Repair runs in a background thread and can be stopped.
"""
import os
import sys
import json
import tempfile
import importlib.util
import subprocess

from PySide6.QtCore import Qt, QThread, Signal, QLocale, QPoint, qVersion
from PySide6.QtGui import (
    QIcon, QFontDatabase, QPixmap, QPainter, QColor, QAction, QPolygon, QPalette)
from PySide6.QtWidgets import (
    QApplication, QMainWindow, QWidget, QVBoxLayout, QHBoxLayout,
    QTreeWidget, QTreeWidgetItem, QPushButton, QFileDialog,
    QProgressBar, QPlainTextEdit, QLabel, QAbstractItemView, QToolButton,
    QMessageBox, QDialog, QSlider)

# the updater/repair modules live beside this file in both the repo and the
# installed layout, so put this directory on the path and import them flat.
_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

import updater

# single source of truth: prefer the package, else the repair.py beside us
try:
    from sutura import VERSION
except ImportError:
    _spec = importlib.util.spec_from_file_location(
        'sutura_repair', os.path.join(_HERE, 'repair.py'))
    _mod = importlib.util.module_from_spec(_spec)
    _spec.loader.exec_module(_mod)
    VERSION = _mod.VERSION

# result classification (stdlib-only single source shared with the CLI)
import classification

# --- i18n ---------------------------------------------------------------
STRINGS = {
    'en': {
        'app_title': 'Sutura',
        'col_file': 'File', 'col_result': 'Result',
        'add_files': 'Add files…', 'add_folder': 'Add folder…',
        'remove': 'Remove selected', 'clear': 'Clear',
        'repair': 'Repair', 'stop': 'Stop',
        'ready': 'Ready', 'repairing': 'Repairing…',
        'repairing_n': 'Repairing… %d/%d', 'done': 'Done',
        'done_stopped': 'Done (stopped)', 'select_files': 'Select mesh files',
        'mesh_filter': 'Mesh files (*.stl *.STL *.3mf *.3MF);;All files (*)',
        'select_folder': 'Select a folder with mesh files',
        'no_mesh': 'No mesh files in that folder',
        'added_n': 'Added %d file(s)', 'added_folder': 'Added %d file(s) from %s',
        'added_drag': 'Added %d file(s) (drag)',
        'update_btn_tooltip_idle': 'Check for updates',
        'update_btn_tooltip': 'Update available: v%s',
        'appimage_update_msg': ('You are running the AppImage build. Sutura '
                               'cannot update itself in this mode.\nPlease '
                               'download the latest AppImage from:\n'
                               'https://github.com/Krateian/Sutura/releases'),
        'first_run_title': 'Enable update checks?',
        'first_run_msg': ("Should Sutura check for new versions once a week? "
                          "(One request to GitHub, no other data sent)"),
        'update_confirm_title': 'Update available',
        'update_confirm_msg': ('Update to v%s? The current version will be '
                               'backed up and a rollback guarantee provided.'),
        'updating': 'Updating…', 'no_update': 'Already up to date',
        'update_success': 'Updated to %s', 'update_failed': 'Update failed',
        'rollback_notice': 'rollback', 'issue_prompt': 'You can open an issue with the log.',
        'update_check_failed': 'Update check failed',
        'checked_days_ago': 'Last checked %d day(s) ago',
        'sum_watertight': 'watertight', 'sum_warning': 'with warnings', 'sum_error': 'failed',
        'sum_show_issues': 'show issues', 'sum_issues_detail': 'Issue detail',
        'res_watertight': 'watertight', 'res_stage2_skipped': 'stage 2 skipped',
        'res_stage2_error': 'stage 2 error', 'res_holes': '%d hole(s)',
        'res_partial': 'partial', 'res_error': 'ERROR',
        'issue_volume_warning': 'Volume change', 'issue_stage2_skipped': 'Stage 2 skipped',
        'issue_stage2_error': 'Stage 2 error', 'issue_partial': 'Partial repair (holes remaining)',
        'issue_malformed': 'Malformed input', 'issue_error': 'Error',
        'defects_header': 'Input defects (selected file):',
        'defect_hole': 'hole: centroid=(%.3f, %.3f, %.3f), diameter=%.3f mm',
        'defect_nm': 'non-manifold: centroid=(%.3f, %.3f, %.3f), %d faces',
        'defect_none': 'no defects', 'defect_empty': 'No defects available for this file.',
        'type_detected': 'Detected: %s (%.2f)',
        'type_tuned': ' — tuned thresholds',
        'type_default': ' — default thresholds (confidence below gate)',
        'diff_line': 'Volume: %s%% \u00b7 Surface: %s%% \u00b7 Vertex: %s\u2192%s',
        'show_heatmap': 'Show heatmap',
        'heatmap_rendering': 'Rendering heatmap…',
        'heatmap_failed': 'Could not render heatmap',
        'heatmap_thumb_tooltip': 'Click to enlarge',
        'heatmap_zoom_title': 'Heatmap — %s',
        'show_before_after': 'Show before/after',
        'before_after_rendering': 'Rendering before/after…',
        'before_after_failed': 'Could not render before/after',
        'before_after_title': 'Before/after — %s',
        'ba_original': 'Original',
        'ba_repaired': 'Repaired',
        'mode_btn': 'Mode: %s',
        'mode_dialog_title': 'Repair mode',
        'mode_name_low': 'Low',
        'mode_name_medium': 'Medium',
        'mode_name_auto': 'Auto',
        'mode_name_aggressive': 'Aggressive',
        'mode_name_extreme': 'Extreme',
        'mode_desc_low': ('Low — most conservative: closes only small holes, '
                          'minimal debris removal.'),
        'mode_desc_medium': 'Medium — the historical default thresholds.',
        'mode_desc_auto': ('Auto — the mesh classifier + confidence gate pick '
                           'per-type thresholds (default).'),
        'mode_desc_aggressive': ('Aggressive — more debris removed, larger '
                                 'holes closed.'),
        'mode_desc_extreme': ('Extreme — most aggressive; can delete a whole '
                              'object whose connected part has fewer than 20 '
                              'faces.'),
        'mode_ok': 'OK',
        'mode_cancel': 'Cancel',
    },
    'tr': {
        'app_title': 'Sutura',
        'col_file': 'Dosya', 'col_result': 'Sonuç',
        'add_files': 'Dosya ekle…', 'add_folder': 'Klasör ekle…',
        'remove': 'Seçileni kaldır', 'clear': 'Temizle',
        'repair': 'Onar', 'stop': 'Durdur',
        'ready': 'Hazır', 'repairing': 'Onarılıyor…',
        'repairing_n': 'Onarılıyor… %d/%d', 'done': 'Bitti',
        'done_stopped': 'Bitti (durduruldu)', 'select_files': 'Mesh dosyası seç',
        'mesh_filter': 'Mesh dosyaları (*.stl *.STL *.3mf *.3MF);;Tüm dosyalar (*)',
        'select_folder': 'Mesh dosyası olan klasörü seç',
        'no_mesh': 'Klasörde mesh dosyası yok',
        'added_n': '%d dosya eklendi', 'added_folder': '%s klasöründen %d dosya eklendi',
        'added_drag': 'Sürüklenen %d dosya eklendi',
        'update_btn_tooltip_idle': 'Güncellemeleri kontrol et',
        'update_btn_tooltip': 'Güncelleme var: v%s',
        'appimage_update_msg': ('AppImage derlemesini kullanıyorsun. Bu modda '
                               'Sutura kendini güncelleyemez.\nEn son AppImage\'ı '
                               'şuradan indir:\n'
                               'https://github.com/Krateian/Sutura/releases'),
        'first_run_title': 'Güncelleme kontrolü açılsın mı?',
        'first_run_msg': ('Sutura haftada bir yeni sürüm kontrol etsin mi? '
                          '(GitHub\'a tek istek, başka veri gönderilmez)'),
        'update_confirm_title': 'Güncelleme var',
        'update_confirm_msg': ('v%s sürümüne güncellensin mi? Mevcut sürüm '
                               'yedeklenip geri dönüş garantisi sağlanacak.'),
        'updating': 'Güncelleniyor…', 'no_update': 'Zaten güncel',
        'update_success': 'v%s sürümüne güncellendi', 'update_failed': 'Güncelleme başarısız',
        'rollback_notice': 'geri dönüldü', 'issue_prompt': 'Log ile issue açabilirsin.',
        'update_check_failed': 'Güncelleme kontrolü başarısız',
        'checked_days_ago': 'Son kontrol %d gün önce',
        'sum_watertight': 'su geçirmez', 'sum_warning': 'uyarılı', 'sum_error': 'hata',
        'sum_show_issues': 'sorunları göster', 'sum_issues_detail': 'Sorun detayı',
        'res_watertight': 'su geçirmez', 'res_stage2_skipped': 'stage 2 atlandı',
        'res_stage2_error': 'stage 2 hatası', 'res_holes': '%d delik',
        'res_partial': 'kısmi', 'res_error': 'HATA',
        'issue_volume_warning': 'Hacim değişimi', 'issue_stage2_skipped': 'Stage 2 atlandı',
        'issue_stage2_error': 'Stage 2 hatası', 'issue_partial': 'Kısmi onarım (delik kaldı)',
        'issue_malformed': 'Hatalı girdi', 'issue_error': 'Hata',
        'defects_header': 'Girdi kusurları (seçili dosya):',
        'defect_hole': 'delik: merkez=(%.3f, %.3f, %.3f), çap=%.3f mm',
        'defect_nm': 'non-manifold: merkez=(%.3f, %.3f, %.3f), %d yüz',
        'defect_none': 'kusur yok', 'defect_empty': 'Bu dosya için kusur bilgisi yok.',
        'type_detected': 'Tespit edilen: %s (%.2f)',
        'type_tuned': ' — ayarlanmış eşikler',
        'type_default': ' — varsayılan eşikler (güven eşiğinin altında)',
        'diff_line': 'Hacim: %s%% \u00b7 Y\u00fczey: %s%% \u00b7 Vertex: %s\u2192%s',
        'show_heatmap': 'Isı haritası göster',
        'heatmap_rendering': 'Isı haritası çiziliyor…',
        'heatmap_failed': 'Isı haritası çizilemedi',
        'heatmap_thumb_tooltip': 'Büyütmek için tıkla',
        'heatmap_zoom_title': 'Isı haritası — %s',
        'show_before_after': 'Öncesi/sonrası göster',
        'before_after_rendering': 'Öncesi/sonrası çiziliyor…',
        'before_after_failed': 'Öncesi/sonrası çizilemedi',
        'before_after_title': 'Öncesi/Sonrası — %s',
        'ba_original': 'Orijinal',
        'ba_repaired': 'Onarılmış',
        'mode_btn': 'Mod: %s',
        'mode_dialog_title': 'Onarım modu',
        'mode_name_low': 'Düşük',
        'mode_name_medium': 'Orta',
        'mode_name_auto': 'Otomatik',
        'mode_name_aggressive': 'Agresif',
        'mode_name_extreme': 'Aşırı',
        'mode_desc_low': ('Düşük — en muhafazakâr: yalnızca küçük delikleri '
                          'kapatır, en az moloz temizliği.'),
        'mode_desc_medium': 'Orta — tarihsel varsayılan eşikler.',
        'mode_desc_auto': ('Otomatik — mesh sınıflandırıcı + güven eşiği '
                           'türüne göre eşikleri seçer (varsayılan).'),
        'mode_desc_aggressive': ('Agresif — daha fazla moloz temizlenir, daha '
                                 'büyük delikler kapatılır.'),
        'mode_desc_extreme': ('Aşırı — en agresif; bağlantılı parçası 20 yüzden '
                              'az olan tüm nesneyi silebilir.'),
        'mode_ok': 'Tamam',
        'mode_cancel': 'İptal',
    },
}


def _t(key, *args):
    lang = QLocale.system().name().split('_')[0]
    table = STRINGS.get(lang, STRINGS['en'])
    s = table.get(key, STRINGS['en'].get(key, key))
    return s % args if args else s


def _find_sutura_cmd():
    """Resolve the CLI: $SUTURA env, the Linux wrapper, or the bundled
    repair.py run with the current interpreter (uninstalled/macOS case)."""
    env = os.environ.get('SUTURA')
    if env:
        return [env]

    wrapper = os.path.expanduser('~/.local/bin/sutura')
    if os.path.isfile(wrapper) and os.access(wrapper, os.X_OK):
        return [wrapper]

    repair = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'repair.py')
    if os.path.isfile(repair):
        return [sys.executable, repair]
    return None


SUTURA_CMD = _find_sutura_cmd()

# PySide6 bundles its own Qt plugins and misses the system platform theme
# (plasma-integration), so QFileDialog would fall back to Qt's embedded
# widget (no rubber-band selection). Point Qt at the system plugin dir.
#
# This must only happen when the system Qt version matches the bundled
# PySide6 Qt: the system platform plugins (libqwayland.so/libqxcb.so) are
# built against the system Qt's private API, so loading them into a different
# PySide6 Qt version aborts startup with an "undefined symbol
# ... Qt_6_PRIVATE_API" error (e.g. system Qt 6.11.2 with bundled Qt 6.11.1).
# On a mismatch, leave Qt on its own bundled plugins so the GUI still opens
# (with Qt's embedded file dialog instead of the native KDE one).
def _system_qt_matches_bundle():
    for qmake in ('qmake6', 'qmake'):
        try:
            out = subprocess.run(
                [qmake, '-query', 'QT_VERSION'],
                capture_output=True, text=True, timeout=5)
        except Exception:
            continue
        if out.returncode == 0:
            return out.stdout.strip() == qVersion()
    return False


if sys.platform.startswith('linux'):
    os.environ.setdefault('QT_QPA_PLATFORMTHEME', 'kde')
    _sys_plugins = '/usr/lib/qt6/plugins'
    if os.path.isdir(_sys_plugins) and _system_qt_matches_bundle():
        _existing = os.environ.get('QT_PLUGIN_PATH', '')
        if _sys_plugins not in _existing.split(os.pathsep):
            os.environ['QT_PLUGIN_PATH'] = (
                (_existing + os.pathsep) if _existing else '') + _sys_plugins


# --- Self-contained dark theme (independent of the system Qt/theme) ---
# Instead of relying on the system platform theme (KDE/Breeze), the GUI ships
# its own look: Qt's bundled Fusion style plus a dark QPalette. This works on
# every platform and every Qt version, whether or not the system theme is
# available or matches the bundled PySide6 Qt. The accent is the same teal
# (#14b8a6) already used for the repair button/progress/update arrow, so the
# highlight/link roles stay consistent with the rest of the UI.
_ACCENT = QColor('#14b8a6')
_ACCENT_DARK_TEXT = QColor('#0b0f11')


def _dark_palette():
    p = QPalette()
    _grp = (
        (QPalette.Window, QColor('#353535')),
        (QPalette.WindowText, QColor('#f2f4f6')),
        (QPalette.Base, QColor('#2b2d30')),
        (QPalette.AlternateBase, QColor('#353535')),
        (QPalette.ToolTipBase, QColor('#2b2d30')),
        (QPalette.ToolTipText, QColor('#f2f4f6')),
        (QPalette.Text, QColor('#f2f4f6')),
        (QPalette.Button, QColor('#3a3d42')),
        (QPalette.ButtonText, QColor('#f2f4f6')),
        (QPalette.BrightText, QColor('#ff5b5b')),
        (QPalette.Link, _ACCENT),
        (QPalette.Highlight, _ACCENT),
        (QPalette.HighlightedText, _ACCENT_DARK_TEXT),
        (QPalette.PlaceholderText, QColor('#8b949e')),
    )
    for role, color in _grp:
        p.setColor(QPalette.All, role, color)
    # muted disabled variants
    for role in (QPalette.WindowText, QPalette.Text, QPalette.ButtonText,
                 QPalette.HighlightedText):
        p.setColor(QPalette.Disabled, role, QColor('#6b7480'))
    p.setColor(QPalette.Disabled, QPalette.Button, QColor('#2b2d30'))
    p.setColor(QPalette.Disabled, QPalette.Base, QColor('#232426'))
    return p


def apply_dark_theme(app):
    """Apply the self-contained Fusion dark theme to a QApplication.

    Used by ``main()`` and by the screenshot generator, so screenshots always
    match the real GUI regardless of the system Qt/theme.
    """
    app.setStyle('Fusion')
    app.setPalette(_dark_palette())
    return app


def parse_cli_output(out, err):
    try:
        return json.loads(out.strip().splitlines()[-1])
    except Exception:
        return {'error': err.strip() or out.strip()}


def summarize(data):
    """Short per-file result label, using the shared classifier, localized."""
    _cat, _issues, key = classification.classify(data)
    if key == 'holes':
        return _t('res_holes', *classification.summary_args(data))
    return _t('res_' + key)


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


class UpdateCheckWorker(QThread):
    """Background check for a newer release. Emits found(new_tag|None)."""

    finished_check = Signal(object)

    def __init__(self, force=False, parent=None):
        super().__init__(parent)
        self.force = force

    def run(self):
        new_tag, _cfg = updater.check_for_update(force=self.force)
        self.finished_check.emit(new_tag)


class UpdateWorker(QThread):
    """Background update: backup, download, install, health check, rollback."""

    progress_msg = Signal(str)
    finished_update = Signal(bool, str)   # ok, message

    def __init__(self, tag, parent=None):
        super().__init__(parent)
        self.tag = tag

    def run(self):
        ok, msg, _prev = updater.perform_update(self.tag, progress=self.progress_msg.emit)
        self.finished_update.emit(ok, msg)


class RepairWorker(QThread):
    """Repairs files sequentially in a background thread."""

    file_started = Signal(str)
    file_done = Signal(str, str, str, object)   # path, summary, report, data
    progress = Signal(int, int)          # current, total
    all_done = Signal(bool)              # cancelled

    def __init__(self, files, mode='auto', parent=None):
        super().__init__(parent)
        self._files = list(files)
        self._mode = mode
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
                self.file_done.emit(path, 'Cancelled', '', {})
                continue
            self.file_started.emit(path)
            data = self._run_one(path)
            if self._cancelled:
                self.file_done.emit(path, 'Stopped', '', {})
                continue
            self.file_done.emit(path, summarize(data), format_report(data), data)
            self.progress.emit(idx, n)
        self.all_done.emit(self._cancelled)

    def _run_one(self, path):
        if SUTURA_CMD is None:
            return {'error': 'sutura not found: no $SUTURA, no ~/.local/bin/sutura, '
                             'and no repair.py next to the GUI'}
        try:
            self._proc = subprocess.Popen(
                [*SUTURA_CMD, '--mode', self._mode, path],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        except OSError as e:
            # e.g. FileNotFoundError - never crash the worker thread silently.
            return {'error': 'could not start sutura: %s' % e}
        try:
            out, err = self._proc.communicate(timeout=600)
        except subprocess.TimeoutExpired:
            self._proc.kill()
            out, err = self._proc.communicate()
            return {'error': 'timeout while repairing'}
        finally:
            self._proc = None
        return parse_cli_output(out, err)


class HeatmapWorker(QThread):
    """Renders a mesh heatmap off the GUI thread and off the GUI process.

    The render (pymeshlab mesh load + defect detect + rasterise) runs as a
    subprocess (``heatmap_render.py``) writing a PNG to a temp file. This
    keeps pymeshlab out of the GUI process entirely -- using pymeshlab inside
    a Qt worker thread while a QMainWindow exists corrupts the heap at
    interpreter shutdown (PySide6 6.11 + Python 3.14). The PNG is read back
    as bytes and decoded to a QPixmap in the main thread.
    """

    done = Signal(str, str, bytes)     # path, size_key, png bytes
    failed = Signal(str, str)          # path, message

    def __init__(self, path, size_key, w, h, parent=None):
        super().__init__(parent)
        self._path = path
        self._size_key = size_key
        self._w = w
        self._h = h

    def run(self):
        renderer = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                'heatmap_render.py')
        tmp = tempfile.NamedTemporaryFile(suffix='.png', delete=False)
        outfile = tmp.name
        tmp.close()
        try:
            proc = subprocess.run(
                [sys.executable, renderer, self._path, outfile,
                 str(self._w), str(self._h)],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                timeout=300)
            if proc.returncode != 0 or not os.path.getsize(outfile):
                self.failed.emit(self._path, _t('heatmap_failed'))
                return
            with open(outfile, 'rb') as f:
                png = f.read()
        except (OSError, subprocess.TimeoutExpired):
            self.failed.emit(self._path, _t('heatmap_failed'))
            return
        finally:
            try:
                os.unlink(outfile)
            except OSError:
                pass
        self.done.emit(self._path, self._size_key, png)


class BeforeAfterWorker(QThread):
    """Renders the original vs repaired comparison in a subprocess.

    Same isolation rule as ``HeatmapWorker``: pymeshlab stays out of the GUI
    process. Loads both meshes in the subprocess, renders them with a SHARED
    isometric camera frame (identical framing, so the toggle is meaningful),
    and hands two PNG byte blobs back to the main thread.
    """

    done = Signal(str, bytes, bytes)     # path, before_png, after_png
    failed = Signal(str, str)            # path, message

    def __init__(self, path, repaired, w, h, parent=None):
        super().__init__(parent)
        self._path = path
        self._repaired = repaired
        self._w = w
        self._h = h

    def run(self):
        renderer = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                'before_after_render.py')
        tmpdir = tempfile.mkdtemp(prefix='sutura-ba-')
        prefix = os.path.join(tmpdir, 'sutura_ba')
        before_file = prefix + '_before.png'
        after_file = prefix + '_after.png'
        try:
            proc = subprocess.run(
                [sys.executable, renderer, self._path, self._repaired,
                 before_file, after_file, str(self._w), str(self._h)],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=600)
            if (proc.returncode != 0 or not os.path.getsize(before_file)
                    or not os.path.getsize(after_file)):
                self.failed.emit(self._path, _t('before_after_failed'))
                return
            with open(before_file, 'rb') as f:
                before = f.read()
            with open(after_file, 'rb') as f:
                after = f.read()
        except (OSError, subprocess.TimeoutExpired):
            self.failed.emit(self._path, _t('before_after_failed'))
            return
        finally:
            try:
                os.unlink(before_file)
                os.unlink(after_file)
                os.rmdir(tmpdir)
            except OSError:
                pass
        self.done.emit(self._path, before, after)


class _ClickableLabel(QLabel):
    """QLabel that emits ``clicked`` on a left mouse press."""

    clicked = Signal()

    def mousePressEvent(self, event):
        if event.button() == Qt.LeftButton:
            self.clicked.emit()
        super().mousePressEvent(event)


class RepairModeDialog(QDialog):
    """Modal picker for the batch repair mode.

    A five-step horizontal slider (Low-Medium-Auto-Aggressive-Extreme, left to
    right) with a live description label that updates as the slider moves, and
    OK/Cancel buttons. The mode is a batch-wide setting (one value for the
    whole run), so the dialog only hands back the chosen value.
    """

    MODES = ('low', 'medium', 'auto', 'aggressive', 'extreme')

    def __init__(self, current='auto', parent=None):
        super().__init__(parent)
        self.setWindowTitle(_t('mode_dialog_title'))
        self.setModal(True)
        lay = QVBoxLayout(self)

        self.slider = QSlider(Qt.Horizontal)
        self.slider.setRange(0, len(self.MODES) - 1)
        self.slider.setValue(self.MODES.index(current))
        self.slider.setTickPosition(QSlider.TicksBelow)
        self.slider.setTickInterval(1)
        self.slider.setMinimumWidth(360)
        lay.addWidget(self.slider)

        ticks = QHBoxLayout()
        for mode in self.MODES:
            lab = QLabel(_t('mode_name_' + mode))
            lab.setAlignment(Qt.AlignCenter)
            ticks.addWidget(lab, 1)
        lay.addLayout(ticks)

        self.desc = QLabel()
        self.desc.setWordWrap(True)
        lay.addWidget(self.desc)

        btns = QHBoxLayout()
        btns.addStretch(1)
        ok = QPushButton(_t('mode_ok'))
        cancel = QPushButton(_t('mode_cancel'))
        ok.setDefault(True)
        btns.addWidget(ok)
        btns.addWidget(cancel)
        lay.addLayout(btns)

        ok.clicked.connect(self.accept)
        cancel.clicked.connect(self.reject)
        self.slider.valueChanged.connect(self._update_desc)
        self._update_desc(self.slider.value())

    def _update_desc(self, idx):
        self.desc.setText(_t('mode_desc_' + self.MODES[idx]))

    def selected_mode(self):
        return self.MODES[self.slider.value()]


class MainWindow(QMainWindow):
    MESH_EXTS = ('.stl', '.3mf')

    def __init__(self):
        super().__init__()
        self.setWindowTitle(_t('app_title'))
        self.resize(780, 620)
        self.setWindowIcon(self._load_icon())
        self.setAcceptDrops(True)

        self.files = []
        self._item_by_path = {}
        self._batch_results = []
        self.worker = None
        self.update_check = None
        self.update_worker = None
        self.available_tag = None
        self._defects_by_path = {}
        self._type_by_path = {}
        self._diff_by_path = {}
        self._output_by_path = {}
        self._heatmap_cache = {}      # path -> {size_key: QPixmap}
        self.heatmap_worker = None
        self._heatmap_zoom = None
        self.before_after_worker = None
        self._before_after_zoom = None
        self._repair_mode = 'auto'    # batch-wide repair mode (not per file)

        self._build_ui()
        self._apply_accent()
        self._maybe_ask_update_on_first_run()
        self._maybe_check_updates()

    # --- UI ---------------------------------------------------------------
    def _build_ui(self):
        central = QWidget(self)
        self.setCentralWidget(central)
        layout = QVBoxLayout(central)

        self.tree = QTreeWidget()
        self.tree.setColumnCount(2)
        self.tree.setHeaderLabels([_t('col_file'), _t('col_result')])
        self.tree.setColumnWidth(0, 580)
        self.tree.setSelectionMode(QAbstractItemView.ExtendedSelection)
        layout.addWidget(self.tree)

        buttons = QHBoxLayout()
        self.btn_add_files = QPushButton(_t('add_files'))
        self.btn_add_folder = QPushButton(_t('add_folder'))
        self.btn_remove = QPushButton(_t('remove'))
        self.btn_clear = QPushButton(_t('clear'))
        self.btn_repair = QPushButton(_t('repair'))
        self.btn_repair.setObjectName('repairBtn')
        self.btn_stop = QPushButton(_t('stop'))
        # update indicator (top-right corner)
        self.update_btn = QToolButton()
        self.update_btn.setIcon(self._update_icon(active=False))
        self.update_btn.setToolTip(_t('update_btn_tooltip_idle'))
        self.update_btn.setFixedSize(22, 22)
        self.update_btn.setAutoRaise(True)
        self.update_btn.setEnabled(False)
        self.update_btn.setVisible(
            updater.is_appimage() or updater.config_exists()
            or updater.load_config().get('check_for_updates'))
        self.update_btn.clicked.connect(self._on_update_clicked)
        buttons.addWidget(self.btn_add_files)
        buttons.addWidget(self.btn_add_folder)
        buttons.addWidget(self.btn_remove)
        buttons.addWidget(self.btn_clear)
        buttons.addStretch(1)
        buttons.addWidget(self.update_btn)
        buttons.addWidget(self.btn_repair)
        buttons.addWidget(self.btn_stop)
        layout.addLayout(buttons)

        row = QHBoxLayout()
        self.progress = QProgressBar()
        self.progress.setRange(0, 1)
        self.progress.setValue(0)
        self.status = QLabel(_t('ready'))
        row.addWidget(self.progress, 1)
        row.addWidget(self.status)
        # decorative, muted version label in the bottom-right corner
        self.version_label = QLabel('v' + VERSION)
        self.version_label.setObjectName('versionLabel')
        row.addWidget(self.version_label)
        layout.addLayout(row)

        # batch summary strip (populated when a batch finishes)
        self.summary = QLabel('')
        self.summary.setTextFormat(Qt.RichText)
        self.summary.setTextInteractionFlags(Qt.TextBrowserInteraction)
        self.summary.linkActivated.connect(self._on_summary_link)
        self.summary.setVisible(False)
        layout.addWidget(self.summary)

        self.log = QPlainTextEdit()
        self.log.setReadOnly(True)
        self.log.setFont(QFontDatabase.systemFont(QFontDatabase.FixedFont))
        layout.addWidget(self.log, 1)

        # per-file defect detail panel (selected file's holes / non-manifold)
        self.defect_label = QLabel(_t('defects_header'))
        layout.addWidget(self.defect_label)
        self.defects = QPlainTextEdit()
        self.defects.setReadOnly(True)
        self.defects.setFont(QFontDatabase.systemFont(QFontDatabase.FixedFont))
        self.defects.setFixedHeight(110)
        layout.addWidget(self.defects)

        # on-demand defect heatmap: render button + clickable thumbnail
        heat_row = QHBoxLayout()
        self.btn_show_heatmap = QPushButton(_t('show_heatmap'))
        self.btn_show_heatmap.setEnabled(False)
        heat_row.addWidget(self.btn_show_heatmap, 0, Qt.AlignTop)
        self.btn_before_after = QPushButton(_t('show_before_after'))
        self.btn_before_after.setEnabled(False)
        heat_row.addWidget(self.btn_before_after, 0, Qt.AlignTop)
        # batch-wide repair mode picker (low/medium/auto/aggressive/extreme)
        self.btn_mode = QPushButton(
            _t('mode_btn', _t('mode_name_' + self._repair_mode)))
        heat_row.addWidget(self.btn_mode, 0, Qt.AlignTop)
        self.heatmap_thumb = _ClickableLabel(_t('heatmap_failed'))
        self.heatmap_thumb.setAlignment(Qt.AlignCenter)
        self.heatmap_thumb.setFixedSize(220, 150)
        self.heatmap_thumb.setStyleSheet(
            'QLabel { background-color: #14181c; color: palette(mid); '
            'border: 1px solid palette(mid); border-radius: 4px; }')
        self.heatmap_thumb.setToolTip(_t('heatmap_thumb_tooltip'))
        self.heatmap_thumb.setVisible(False)
        heat_row.addWidget(self.heatmap_thumb, 1)
        layout.addLayout(heat_row)

        self.btn_add_files.clicked.connect(self.add_files)
        self.btn_add_folder.clicked.connect(self.add_folder)
        self.btn_remove.clicked.connect(self.remove_selected)
        self.btn_clear.clicked.connect(self.clear_files)
        self.btn_repair.clicked.connect(self.repair)
        self.btn_stop.clicked.connect(self.stop)
        self.tree.currentItemChanged.connect(self._on_selection)
        self.btn_show_heatmap.clicked.connect(self._on_show_heatmap)
        self.heatmap_thumb.clicked.connect(self._on_heatmap_thumb_clicked)
        self.btn_before_after.clicked.connect(self._on_show_before_after)
        self.btn_mode.clicked.connect(self._on_choose_repair_mode)

        self.btn_repair.setEnabled(False)
        self.btn_stop.setEnabled(False)

    def _apply_accent(self):
        self.setStyleSheet('''
            QPushButton#repairBtn {
                background-color: #14b8a6; color: #0b0f11;
                border: none; border-radius: 4px; padding: 6px 18px; font-weight: bold;
            }
            QPushButton#repairBtn:hover { background-color: #17c9b4; }
            QPushButton#repairBtn:disabled { background-color: palette(mid); color: palette(midlight); }
            QProgressBar::chunk { background-color: #14b8a6; }
            QLabel#versionLabel {
                color: #9aa4ae; font-size: 10px;
            }
        ''')

    def _load_icon(self):
        for size in (48, 32, 64, 128):
            path = os.path.expanduser(
                '~/.local/share/icons/hicolor/%dx%d/apps/sutura.png' % (size, size))
            if os.path.exists(path):
                return QIcon(path)
        return QIcon()

    def _update_icon(self, active=False):
        """Small up-arrow icon; teal when an update is available."""
        size = 16
        pm = QPixmap(size, size)
        pm.fill(Qt.transparent)
        p = QPainter(pm)
        p.setRenderHint(QPainter.Antialiasing)
        color = QColor('#14b8a6') if active else QColor('#5a646c')
        p.setPen(Qt.NoPen)
        p.setBrush(color)
        p.drawPolygon(QPolygon([QPoint(3, 10), QPoint(8, 4), QPoint(13, 10)]))
        p.setPen(color)
        p.drawLine(8, 4, 8, 13)
        p.end()
        return QIcon(pm)

    def _maybe_ask_update_on_first_run(self):
        """Ask once (on first run, no config) whether to enable update checks.

        Skipped for AppImage builds: self-update is not available there."""
        if updater.is_appimage():
            return
        if updater.config_exists():
            return
        ret = QMessageBox.question(
            self, _t('first_run_title'), _t('first_run_msg'),
            QMessageBox.Yes | QMessageBox.No, QMessageBox.No)
        if ret == QMessageBox.Yes:
            updater.opt_in_check_updates()
        else:
            updater.save_config(updater.load_config())  # record the decision

    def _maybe_check_updates(self):
        """Start a background check if enabled and due.

        Skipped for AppImage builds: self-update is not available there."""
        if updater.is_appimage():
            return
        cfg = updater.load_config()
        if not updater.should_check(cfg):
            return
        if self.update_check is not None:
            return
        self.update_check = UpdateCheckWorker(parent=self)
        self.update_check.finished_check.connect(self._on_update_check_done)
        self.update_check.start()

    def _on_update_check_done(self, new_tag):
        self.update_check = None
        if new_tag:
            self.available_tag = new_tag
            self.update_btn.setIcon(self._update_icon(active=True))
            self.update_btn.setToolTip(_t('update_btn_tooltip', new_tag))
            self.update_btn.setEnabled(True)
            self.update_btn.setVisible(True)

    def _on_update_clicked(self):
        if updater.is_appimage():
            QMessageBox.information(
                self, _t('app_title'), _t('appimage_update_msg'))
            return
        if self.available_tag is None:
            # manual check (idle icon click)
            self.update_btn.setEnabled(False)
            self.status.setText(_t('updating'))
            self.update_check = UpdateCheckWorker(force=True, parent=self)
            self.update_check.finished_check.connect(self._on_update_check_done)
            self.update_check.start()
            return
        tag = self.available_tag
        ret = QMessageBox.question(
            self, _t('update_confirm_title'),
            _t('update_confirm_msg', tag),
            QMessageBox.Yes | QMessageBox.No, QMessageBox.No)
        if ret != QMessageBox.Yes:
            return
        self.update_btn.setEnabled(False)
        self.btn_repair.setEnabled(False)
        self.status.setText(_t('updating'))
        self.update_worker = UpdateWorker(tag, parent=self)
        self.update_worker.progress_msg.connect(self.status.setText)
        self.update_worker.finished_update.connect(self._on_update_done)
        self.update_worker.start()

    def _on_update_done(self, ok, msg):
        self.update_worker = None
        self.update_btn.setEnabled(True)
        self.btn_repair.setEnabled(bool(self.files))
        if ok:
            self.available_tag = None
            self.update_btn.setIcon(self._update_icon(active=False))
            self.update_btn.setToolTip(_t('update_btn_tooltip_idle'))
            QMessageBox.information(self, _t('app_title'), _t('update_success', msg.split()[-1]))
        else:
            self.status.setText(_t('update_failed'))
            QMessageBox.warning(
                self, _t('update_failed'),
                '%s\n\n%s' % (msg, _t('issue_prompt')))

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
            self, _t('select_files'), '',
            _t('mesh_filter'))
        added = sum(1 for p in paths if self._add_path(p))
        if added:
            self._log(_t('added_n', added))
        self._refresh_buttons()

    def add_folder(self):
        folder = QFileDialog.getExistingDirectory(self, _t('select_folder'))
        if folder:
            added = self._add_meshes_from_folder(folder)
            if added:
                self._log(_t('added_folder', added, folder))
            else:
                self.status.setText(_t('no_mesh'))
            self._refresh_buttons()

    def remove_selected(self):
        for item in self.tree.selectedItems():
            path = item.text(0)
            if path in self.files:
                self.files.remove(path)
            self._item_by_path.pop(path, None)
            self._heatmap_cache.pop(path, None)
            self._output_by_path.pop(path, None)
            self.tree.takeTopLevelItem(self.tree.indexOfTopLevelItem(item))
        self._refresh_buttons()
        if not self.tree.currentItem():
            self._set_heatmap_thumb(None)
            self.btn_before_after.setEnabled(False)

    def clear_files(self):
        self.files.clear()
        self._item_by_path.clear()
        self._heatmap_cache.clear()
        self._output_by_path.clear()
        self.tree.clear()
        self._set_heatmap_thumb(None)
        self.btn_before_after.setEnabled(False)
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
            self._log(_t('added_drag', added))
            self._refresh_buttons()
        event.acceptProposedAction()

    # --- repair ------------------------------------------------------------
    def repair(self):
        if not self.files or self.worker is not None:
            return
        for i in range(self.tree.topLevelItemCount()):
            self.tree.topLevelItem(i).setText(1, '')
        self._batch_results = []
        self._defects_by_path = {}
        self._type_by_path = {}
        self._diff_by_path = {}
        self.defects.clear()
        self.defect_label.setText(_t('defects_header'))
        self.summary.setVisible(False)
        self.summary.setText('')
        self.btn_repair.setEnabled(False)
        self.btn_stop.setEnabled(True)
        self.progress.setRange(0, len(self.files))
        self.progress.setValue(0)
        self.status.setText(_t('repairing'))
        self.log.clear()

        self.worker = RepairWorker(self.files, self._repair_mode, self)
        self.worker.file_done.connect(self._on_file_done)
        self.worker.progress.connect(self._on_progress)
        self.worker.all_done.connect(self._on_all_done)
        self.worker.start()

    def stop(self):
        if self.worker is not None:
            self.worker.cancel()
            self.btn_stop.setEnabled(False)

    def _on_choose_repair_mode(self):
        """Open the repair-mode dialog; apply the chosen mode to the next batch."""
        dlg = RepairModeDialog(self._repair_mode, self)
        if dlg.exec() == QDialog.Accepted:
            self._repair_mode = dlg.selected_mode()
            self.btn_mode.setText(
                _t('mode_btn', _t('mode_name_' + self._repair_mode)))

    def _on_file_done(self, path, summary, report, data):
        item = self._item_by_path.get(path)
        if item is not None:
            item.setText(1, summary)
        if data:
            self._batch_results.append(data)
            self._defects_by_path[path] = data.get('defects')
            self._type_by_path[path] = (data.get('detected_type'),
                                        data.get('detected_confidence'),
                                        data.get('tuning_applied'))
            self._diff_by_path[path] = data.get('stage1', {})
            self._output_by_path[path] = data.get('output')
            if self._item_by_path.get(path) is self.tree.currentItem():
                self._show_defects(path)
                self._refresh_before_after_btn(path)
        if report:
            self._log('%s\n%s' % (path, report))

    def _on_progress(self, current, total):
        self.progress.setValue(current)
        self.status.setText(_t('repairing_n', current, total))

    def _on_selection(self, current, _prev):
        if current is not None:
            self._show_defects(current.text(0))
            self._refresh_heatmap_thumb(current.text(0))
            self._refresh_before_after_btn(current.text(0))
        else:
            self.defects.clear()
            self._set_heatmap_thumb(None)
            self.btn_before_after.setEnabled(False)

    def _show_defects(self, path):
        """Render the selected file's input defects into the defect panel."""
        d = self._defects_by_path.get(path)
        # header: base label + detected mesh type (separate from the defect
        # list itself and from the batch summary strip)
        base = _t('defects_header')
        dt = self._type_by_path.get(path)
        if dt and dt[0]:
            base += ' — ' + _t('type_detected', dt[0], dt[1] or 0.0)
            # tuning status: tuned thresholds vs default (below confidence gate)
            tuning = dt[2]
            if tuning is True:
                base += _t('type_tuned')
            elif tuning is False:
                base += _t('type_default')
        self.defect_label.setText(base)
        lines = []
        # before/after geometry diff summary (from stage1)
        diff = self._diff_by_path.get(path)
        if diff and ('vertices_before' in diff or 'surface_area_change_percent' in diff):
            def _signed(pct):
                return '%+.2f' % (pct or 0.0)
            lines.append(_t(
                'diff_line',
                _signed(diff.get('volume_change_percent')),
                _signed(diff.get('surface_area_change_percent')),
                diff.get('vertices_before', 0), diff.get('vertices_after', 0)))
        if d:
            holes = d.get('holes', [])
            nm = d.get('non_manifold', [])
            for h in holes:
                c = h['centroid']
                lines.append(_t('defect_hole', c[0], c[1], c[2], h['diameter']))
            for r in nm:
                c = r['centroid']
                lines.append(_t('defect_nm', c[0], c[1], c[2], r['faces']))
            if not holes and not nm:
                lines.append(_t('defect_none'))
        if not lines:
            lines.append(_t('defect_empty'))
        self.defects.setPlainText('\n'.join(lines))

    # --- heatmap -----------------------------------------------------------
    def _refresh_heatmap_thumb(self, path):
        """Enable/disable the heatmap button and show any cached thumbnail."""
        self.btn_show_heatmap.setEnabled(bool(path) and self.heatmap_worker is None)
        cached = (self._heatmap_cache.get(path) or {}).get('thumb')
        if cached is not None:
            self._set_heatmap_thumb(cached)
        else:
            self._set_heatmap_thumb(None)

    def _set_heatmap_thumb(self, pixmap):
        if pixmap is None:
            self.heatmap_thumb.setText(_t('heatmap_failed'))
            self.heatmap_thumb.setPixmap(QPixmap())
            self.heatmap_thumb.setVisible(False)
        else:
            self.heatmap_thumb.setPixmap(pixmap.scaled(
                self.heatmap_thumb.size(), Qt.KeepAspectRatio, Qt.SmoothTransformation))
            self.heatmap_thumb.setText('')
            self.heatmap_thumb.setVisible(True)

    def _current_path(self):
        item = self.tree.currentItem()
        return item.text(0) if item is not None else None

    def _on_show_heatmap(self):
        path = self._current_path()
        if not path or self.heatmap_worker is not None:
            return
        self._start_heatmap_render(path, 'thumb', 240, 180)

    def _on_heatmap_thumb_clicked(self):
        path = self._current_path()
        if not path or self.heatmap_worker is not None:
            return
        cached = (self._heatmap_cache.get(path) or {}).get('zoom')
        if cached is not None:
            self._open_heatmap_zoom(path, cached)
        else:
            self._start_heatmap_render(path, 'zoom', 720, 540)

    def _start_heatmap_render(self, path, size_key, w, h):
        self.status.setText(_t('heatmap_rendering'))
        self.btn_show_heatmap.setEnabled(False)
        self.heatmap_worker = HeatmapWorker(path, size_key, w, h, self)
        self.heatmap_worker.done.connect(self._on_heatmap_done)
        self.heatmap_worker.failed.connect(self._on_heatmap_failed)
        self.heatmap_worker.start()

    def _on_heatmap_done(self, path, size_key, png):
        self.heatmap_worker = None
        pix = QPixmap()
        if not pix.loadFromData(png, 'PNG') or pix.isNull():
            self._on_heatmap_failed(path, _t('heatmap_failed'))
            return
        self._heatmap_cache.setdefault(path, {})[size_key] = pix
        if self._current_path() == path:
            if size_key == 'thumb':
                self._set_heatmap_thumb(pix)
                self.status.setText(_t('ready'))
            elif size_key == 'zoom':
                self._open_heatmap_zoom(path, pix)
                self.status.setText(_t('ready'))
        self.btn_show_heatmap.setEnabled(bool(self._current_path()))

    def _on_heatmap_failed(self, path, msg):
        self.heatmap_worker = None
        self.status.setText(msg)
        if self._current_path() == path:
            self._set_heatmap_thumb(None)
        self.btn_show_heatmap.setEnabled(bool(self._current_path()))

    def _open_heatmap_zoom(self, path, pix):
        if self._heatmap_zoom is not None:
            self._heatmap_zoom.close()
        dlg = QDialog(self)
        dlg.setWindowTitle(_t('heatmap_zoom_title', os.path.basename(path)))
        lay = QVBoxLayout(dlg)
        label = QLabel()
        label.setPixmap(pix)
        label.setAlignment(Qt.AlignCenter)
        lay.addWidget(label)
        dlg.resize(pix.width() + 24, pix.height() + 24)
        dlg.exec()
        self._heatmap_zoom = dlg

    # --- before/after comparison ------------------------------------------
    def _refresh_before_after_btn(self, path):
        """Enable the button only when the selected file has a repaired output."""
        self.btn_before_after.setEnabled(
            bool(path) and bool(self._output_by_path.get(path))
            and self.before_after_worker is None)

    def _on_show_before_after(self):
        path = self._current_path()
        repaired = self._output_by_path.get(path) if path else None
        if not path or not repaired or self.before_after_worker is not None:
            return
        self.status.setText(_t('before_after_rendering'))
        self.btn_before_after.setEnabled(False)
        self.before_after_worker = BeforeAfterWorker(path, repaired, 720, 540, self)
        self.before_after_worker.done.connect(self._on_before_after_done)
        self.before_after_worker.failed.connect(self._on_before_after_failed)
        self.before_after_worker.start()

    def _on_before_after_done(self, path, before_png, after_png):
        self.before_after_worker = None
        before = QPixmap()
        after = QPixmap()
        if (not before.loadFromData(before_png, 'PNG') or before.isNull()
                or not after.loadFromData(after_png, 'PNG') or after.isNull()):
            self._on_before_after_failed(path, _t('before_after_failed'))
            return
        if self._current_path() == path:
            self._open_before_after(path, before, after)
            self.status.setText(_t('ready'))
        self._refresh_before_after_btn(self._current_path())

    def _on_before_after_failed(self, path, msg):
        self.before_after_worker = None
        self.status.setText(msg)
        self._refresh_before_after_btn(self._current_path())

    def _open_before_after(self, path, before, after):
        """Modal dialog: a single image area plus a toggle button that flips
        between the original and the repaired view (no dragging, no slider —
        deliberately, the CPU renderer is static)."""
        if self._before_after_zoom is not None:
            self._before_after_zoom.close()
        dlg = QDialog(self)
        dlg.setWindowTitle(_t('before_after_title', os.path.basename(path)))
        lay = QVBoxLayout(dlg)
        view = QLabel()
        view.setAlignment(Qt.AlignCenter)
        lay.addWidget(view, 1)
        btn = QPushButton(_t('ba_original'))
        btn.setCheckable(True)
        state = {'show_after': False}

        def show():
            pix = after if state['show_after'] else before
            view.setPixmap(pix)
            btn.setText(_t('ba_repaired' if state['show_after'] else 'ba_original'))

        def toggle(_checked):
            state['show_after'] = btn.isChecked()
            show()

        btn.toggled.connect(toggle)
        lay.addWidget(btn)
        show()
        dlg.resize(before.width() + 24, before.height() + 60)
        self._before_after_zoom = dlg
        dlg.exec()

    def _on_all_done(self, cancelled):
        self.status.setText(_t('done_stopped') if cancelled else _t('done'))
        self.btn_stop.setEnabled(False)
        self.btn_repair.setEnabled(bool(self.files))
        self.worker = None
        if not cancelled:
            self._render_summary()

    def _render_summary(self):
        """Build the batch summary strip: counts + clickable issue detail."""
        if not self._batch_results:
            return
        n_wt = sum(1 for d in self._batch_results
                   if d.get('category') == 'watertight')
        n_wa = sum(1 for d in self._batch_results
                   if d.get('category') == 'warning')
        n_er = sum(1 for d in self._batch_results
                   if d.get('category') == 'error')
        issue_counts = {}
        for d in self._batch_results:
            for code in d.get('issues', []):
                issue_counts[code] = issue_counts.get(code, 0) + 1
        text = ('<b>%d %s</b> &nbsp;·&nbsp; %d %s &nbsp;·&nbsp; %d %s'
                % (n_wt, _t('sum_watertight'), n_wa, _t('sum_warning'),
                   n_er, _t('sum_error')))
        if issue_counts:
            text += ' &nbsp;·&nbsp; <a href="issues">%s</a>' % _t('sum_show_issues')
        self.summary.setText(text)
        self.summary.setVisible(True)
        # stash counts for the link handler
        self._summary_issue_counts = issue_counts

    def _on_summary_link(self, _link):
        if not getattr(self, '_summary_issue_counts', None):
            return
        self._log('--- ' + _t('sum_issues_detail') + ' ---')
        for code, n in self._summary_issue_counts.items():
            self._log('  - %s: %d' % (_t('issue_' + code), n))

    def _log(self, text):
        self.log.appendPlainText(text)
        self.log.verticalScrollBar().setValue(self.log.verticalScrollBar().maximum())

    def _refresh_buttons(self):
        self.btn_repair.setEnabled(bool(self.files) and self.worker is None)


def main():
    app = QApplication(sys.argv)
    apply_dark_theme(app)
    win = MainWindow()
    for p in sys.argv[1:]:
        win._add_path(p)
    win._refresh_buttons()
    win.show()
    sys.exit(app.exec())


if __name__ == '__main__':
    main()