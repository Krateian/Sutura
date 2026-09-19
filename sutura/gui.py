#!/usr/bin/env python3
"""Sutura - Qt (PySide6) GUI frontend.

Pick or drop mesh files, run the two-stage repair on each via the installed
CLI, and read back what was fixed. Results are always written to new
"_fixed" files. Repair runs in a background thread and can be stopped.
"""
import os
import sys
import json
import math
import shutil
import tempfile
import threading
import importlib.util
import subprocess
import webbrowser

import numpy as np

from PySide6.QtCore import Qt, QThread, Signal, QLocale, QPoint, qVersion, QTimer
from PySide6.QtGui import (
    QIcon, QFontDatabase, QPixmap, QPainter, QColor, QPolygon, QPalette, QPen)
from PySide6.QtWidgets import (
    QApplication, QMainWindow, QWidget, QVBoxLayout, QHBoxLayout,
    QTreeWidget, QTreeWidgetItem, QPushButton, QFileDialog,
    QProgressBar, QPlainTextEdit, QLabel, QAbstractItemView, QToolButton,
    QMessageBox, QDialog, QSlider, QStyle, QButtonGroup, QRadioButton,
    QCheckBox)

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

# interactive mesh rendering (numpy + QPainter only, no pymeshlab - it stays
# in the subprocesses that load/render the meshes)
from heatmap import _ISOMETRIC, draw_frame, prepare_render

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
        'license_update_tooltip': ('v%s changes the license terms — update '
                                   'not applied automatically'),
        'license_block_title': 'License change',
        'license_block_msg': ('Sutura %s changes its license terms '
                              '(PolyForm Noncommercial 1.0.0 — commercial use '
                              'requires a separate agreement). This '
                              'update will not be applied automatically — if '
                              'you accept the new terms, download and install '
                              'it manually from the releases page.'),
        'license_block_open': 'Open releases page…',
        'appimage_update_msg': ('You are running the AppImage build. Sutura '
                               'cannot update itself in this mode.\nPlease '
                               'download the latest AppImage from:\n'
                               'https://github.com/Krateian/Sutura/releases'),
        'first_run_title': 'Enable update checks?',
        'first_run_msg': ("Should Sutura check for new versions once a week? "
                          "(One request to GitHub, no other data sent)"),
        'first_run_history_checkbox': ('Share anonymous usage history (mesh '
                                       'geometry and repair results only — '
                                       'never file names or paths)'),
        'first_run_history_title': 'Share anonymous usage history?',
        'first_run_history_msg': ('Sutura records anonymous technical repair '
                                  'data (mesh size, defect counts, classifier '
                                  'result, timing) so the community can '
                                  'improve the engine. No file names, paths '
                                  'or personal data are ever stored.'),
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
        'issue_extreme_removed_object': ('Extreme mode removed all geometry '
                                         '(component below size threshold) — try '
                                         'a less aggressive mode'),
        'defects_header': 'Input defects (selected file):',
        'defect_hole': 'hole: centroid=(%.3f, %.3f, %.3f), diameter=%.3f mm',
        'defect_nm': 'non-manifold: centroid=(%.3f, %.3f, %.3f), %d faces',
        'defect_none': 'no defects', 'defect_empty': 'No defects available for this file.',
        'type_detected': 'Detected: %s (%.2f)',
        'type_tuned': ' — tuned thresholds',
        'type_default': ' — default thresholds (confidence below gate)',
        'confidence_high': 'High', 'confidence_medium': 'Medium', 'confidence_low': 'Low',
        'confidence_score': 'Confidence: %d/100 — %s',
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
        'before_after_detail': 'Detail — worst defect region',
        'viewer_static': 'Static',
        'viewer_interactive': 'Interactive',
        'viewer_ready': 'Preparing 3D view…',
        'viewer_finalizing': 'Finalizing view…',
        'viewer_ready_failed': 'Could not prepare the 3D view',
        'viewer_status_mode': 'Repair status',
        'viewer_deviation_mode': 'Surface deviation',
        'viewer_max_dev': 'Max deviation: %.2f mm',
        'viewer_hint': 'Drag to rotate · wheel to zoom',
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
        'analyze': 'Analyze',
        'analyze_tip': ('Run read-only analysis (validate + dry-run) on the '
                        'selected files — never modifies the input.'),
        'analyze_header': 'Analysis (selected file):',
        'analyze_empty': 'No analysis yet — select files and press Analyze.',
        'analyze_running': 'Analyzing…',
        'analyze_n': 'Analyzing %d/%d',
        'analyze_error': 'Analysis failed: %s',
        'analyze_type': 'Detected: %s (%.2f) · Mode: %s · %s',
        'analyze_counts': ('Holes: %d · Self-intersections: %d · '
                           'Non-manifold: %d · Debris: %d'),
        'analyze_watertight_yes': 'Watertight: YES',
        'analyze_watertight_no': 'Watertight: NO',
        'analyze_est': ('Estimated confidence: %d/100 (%s) — actual result '
                        'may differ after repair'),
        'tune_tuned': 'tuned thresholds',
        'tune_default': 'default thresholds',
        'add_files_tip': 'Add one or more mesh files',
        'add_folder_tip': 'Add every STL/3MF in a folder',
        'remove_tip': 'Remove the selected files',
        'clear_tip': 'Clear the whole list',
        'repair_tip': 'Repair all files',
        'stop_tip': 'Stop the running batch',
        'mode_tip': 'Choose the repair mode for the whole batch',
        'profile_tip': 'Repair profile (whole batch): Auto uses the classifier; '
                       'named profiles opt in to fixed Stage 1 thresholds.',
        'profile_auto': 'Profile: Auto',
        'profile_name_mechanical': 'Mechanical',
        'profile_name_organic': 'Organic',
        'profile_name_scan': 'Scan',
        'profile_name_miniature': 'Miniature',
        'profile_name_fast': 'Fast',
        'sug_title': 'Suggestions:',
        'sug_si_many': "Extreme mode's extra cleanup passes may help with these self-intersections.",
        'sug_si_few': 'Extreme mode also tries to clean these up.',
        'sug_holes_many': 'Probably scan-derived; Aggressive/Extreme close large holes more easily.',
        'sug_holes_few': 'The current mode may be enough, but a step up can be tried if unsatisfied.',
        'sug_nm': 'Complex geometry may leave a few micro-cracks.',
        'sug_cc': "If this is an assembly-type model, note the current mode's debris cutoff may remove small parts.",
        'sug_unknown': 'The type was not determined / stayed below the gate; default thresholds will be used.',
        'sug_watertight': 'The input is already watertight — low mode is probably enough.',
        'sug_low_conf': 'Trying the next mode up and comparing results could help.',
        'sug_extreme_caveat': ('Extreme can delete parts with fewer than 20 faces '
                               'entirely — be careful with assembly-type models.'),
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
        'license_update_tooltip': ('v%s lisans şartlarını değiştiriyor — '
                                   'güncelleme otomatik uygulanmıyor'),
        'license_block_title': 'Lisans değişikliği',
        'license_block_msg': ('Sutura %s lisans şartlarını değiştiriyor '
                              '(PolyForm Noncommercial 1.0.0 — ticari kullanım '
                              'için ayrı bir anlaşma gerekiyor). Bu '
                              'güncelleme otomatik uygulanmayacak — yeni '
                              'şartları kabul ediyorsanız releases sayfasından '
                              'elle indirip kurabilirsiniz.'),
        'license_block_open': 'Releases sayfasını aç…',
        'appimage_update_msg': ('AppImage derlemesini kullanıyorsun. Bu modda '
                               'Sutura kendini güncelleyemez.\nEn son AppImage\'ı '
                               'şuradan indir:\n'
                               'https://github.com/Krateian/Sutura/releases'),
        'first_run_title': 'Güncelleme kontrolü açılsın mı?',
        'first_run_msg': ('Sutura haftada bir yeni sürüm kontrol etsin mi? '
                          '(GitHub\'a tek istek, başka veri gönderilmez)'),
        'first_run_history_checkbox': ('Anonim kullanım geçmişini paylaş '
                                       '(sadece mesh geometrisi ve onarım '
                                       'sonuçları — dosya adı/yolu asla)'),
        'first_run_history_title': 'Anonim kullanım geçmişi paylaşılsın mı?',
        'first_run_history_msg': ('Sutura anonim teknik onarım verisini kaydeder '
                                  '(mesh boyutu, kusur sayıları, sınıflandırıcı '
                                  'sonucu, süre) böylece topluluk motoru '
                                  'geliştirebilir. Dosya adı, yol veya kişisel '
                                  'veri asla saklanmaz.'),
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
        'issue_extreme_removed_object': ('Extreme mod tüm geometriyi sildi '
                                         '(bileşen boyut eşiğinin altında) — '
                                         'daha az agresif bir mod deneyin'),
        'defects_header': 'Girdi kusurları (seçili dosya):',
        'defect_hole': 'delik: merkez=(%.3f, %.3f, %.3f), çap=%.3f mm',
        'defect_nm': 'non-manifold: merkez=(%.3f, %.3f, %.3f), %d yüz',
        'defect_none': 'kusur yok', 'defect_empty': 'Bu dosya için kusur bilgisi yok.',
        'type_detected': 'Tespit edilen: %s (%.2f)',
        'type_tuned': ' — ayarlanmış eşikler',
        'type_default': ' — varsayılan eşikler (güven eşiğinin altında)',
        'confidence_high': 'Yüksek', 'confidence_medium': 'Orta', 'confidence_low': 'Düşük',
        'confidence_score': 'Güven: %d/100 — %s',
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
        'before_after_detail': 'Detay — en yoğun bozukluk bölgesi',
        'viewer_static': 'Statik',
        'viewer_interactive': 'İnteraktif',
        'viewer_ready': '3D görünüm hazırlanıyor…',
        'viewer_finalizing': 'Görünüm sonlandırılıyor…',
        'viewer_ready_failed': '3D görünüm hazırlanamadı',
        'viewer_status_mode': 'Onarım durumu',
        'viewer_deviation_mode': 'Yüzey sapması',
        'viewer_max_dev': 'Maks. sapma: %.2f mm',
        'viewer_hint': 'Sürükleyerek döndür · tekerlekle yakınlaştır',
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
        'analyze': 'Analiz Et',
        'analyze_tip': ('Seçili dosyalar için salt-okunur analiz çalıştır '
                        '(validate + dry-run) — girdiyi asla değiştirmez.'),
        'analyze_header': 'Analiz (seçili dosya):',
        'analyze_empty': 'Henüz analiz yok — dosya seçip Analiz Et\u2019e bas.',
        'analyze_running': 'Analiz ediliyor…',
        'analyze_n': 'Analiz ediliyor %d/%d',
        'analyze_error': 'Analiz başarısız: %s',
        'analyze_type': 'Tespit edilen: %s (%.2f) · Mod: %s · %s',
        'analyze_counts': ('Delik: %d · Self-intersection: %d · '
                           'Non-manifold: %d · Döküntü: %d'),
        'analyze_watertight_yes': 'Su geçirmez: EVET',
        'analyze_watertight_no': 'Su geçirmez: HAYIR',
        'analyze_est': ('Tahmini güven: %d/100 (%s) — sonuç onarımdan sonra '
                        'farklı olabilir'),
        'tune_tuned': 'ayarlanmış eşikler',
        'tune_default': 'varsayılan eşikler',
        'add_files_tip': 'Bir veya daha fazla mesh dosyası ekle',
        'add_folder_tip': 'Bir klasördeki tüm STL/3MF dosyalarını ekle',
        'remove_tip': 'Seçili dosyaları kaldır',
        'clear_tip': 'Listeyi tamamen temizle',
        'repair_tip': 'Tüm dosyaları onar',
        'stop_tip': 'Çalışan batch\u2019i durdur',
        'mode_tip': 'Batch geneli onarım modunu seç',
        'profile_tip': 'Onarım profili (batch geneli): Auto sınıflandırıcıyı '
                       'kullanır; adlandırılmış profiller sabit Aşama 1 '
                       'eşiklerine geçer.',
        'profile_auto': 'Profil: Auto',
        'profile_name_mechanical': 'Mekanik',
        'profile_name_organic': 'Organik',
        'profile_name_scan': 'Tarama',
        'profile_name_miniature': 'Miniatür',
        'profile_name_fast': 'Hızlı',
        'sug_title': 'Öneriler:',
        'sug_si_many': "Extreme modun ekstra temizleme adımları bu self-intersection'lara işe yarayabilir.",
        'sug_si_few': 'Extreme mod bunları ayrıca temizlemeyi dener.',
        'sug_holes_many': 'Muhtemelen tarama kaynaklı; Aggressive/Extreme büyük delikleri daha rahat kapatır.',
        'sug_holes_few': 'Mevcut mod yeterli olabilir ama tatmin etmezse bir üst mod denenebilir.',
        'sug_nm': 'Karmaşık geometri birkaç mikro-çatlak bırakabilir.',
        'sug_cc': 'Montaj tipi bir modelse dikkat: mevcut modun döküntü eşiği küçük parçaları silebilir.',
        'sug_unknown': 'Tip net belirlenemedi / eşik altı kaldı; varsayılan eşikler kullanılacak.',
        'sug_watertight': 'Girdi zaten su geçirmez — düşük mod muhtemelen yeterli.',
        'sug_low_conf': 'Bir üst mod denenip sonucu karşılaştırmak faydalı olabilir.',
        'sug_extreme_caveat': ('Extreme, 20 yüzden küçük parçaları tamamen silebilir — '
                               'montaj tipi modellerde dikkatli olun.'),
    },
}


def _t(key, *args):
    lang = QLocale.system().name().split('_')[0]
    table = STRINGS.get(lang, STRINGS['en'])
    s = table.get(key, STRINGS['en'].get(key, key))
    return s % args if args else s


def _bundle_tool(name):
    """Path of a sibling executable in a PyInstaller bundle, or None.

    In a PyInstaller ``.app`` each Python entry point is bundled as its own
    executable placed next to the app binary (``Contents/MacOS``), so the
    GUI can spawn the CLI and the heatmap/before-after/viewer renderers
    without a system Python. Both layouts are resolved: a onefile binary
    (``<dir>/<name>``) or a onedir tool (``<dir>/<name>/<name>``). Outside a
    bundle (normal installs, source checkouts) ``sys.frozen`` is absent and
    this returns None so callers keep the historical
    ``[sys.executable, script.py]`` behaviour.
    """
    if not getattr(sys, 'frozen', False):
        return None
    base = os.path.dirname(sys.executable)
    onefile = os.path.join(base, name)
    if os.path.isfile(onefile):
        return onefile
    onedir = os.path.join(base, name, name)
    if os.path.isfile(onedir):
        return onedir
    return None


def _script_cmd(script_name, script_path=None):
    """Command for a Python entry-point script.

    Inside a PyInstaller bundle this is the sibling ``script_name``
    executable; outside it is ``[sys.executable, script_path]`` exactly as
    before. ``script_path`` defaults to the script next to this file.
    """
    exe = _bundle_tool(script_name)
    if exe:
        return [exe]
    path = script_path or os.path.join(
        os.path.dirname(os.path.abspath(__file__)), script_name)
    return [sys.executable, path]


def _find_sutura_cmd():
    """Resolve the CLI: the bundled ``sutura-cli`` (PyInstaller .app), the
    $SUTURA env, the Linux wrapper, or the bundled repair.py run with the
    current interpreter (uninstalled/macOS case)."""
    exe = _bundle_tool('sutura-cli')
    if exe:
        return [exe]
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
    """Background check for a newer release. Emits found((status, tag))."""

    finished_check = Signal(object)

    def __init__(self, force=False, parent=None):
        super().__init__(parent)
        self.force = force

    def run(self):
        status, tag, _cfg = updater.check_for_update(force=self.force)
        self.finished_check.emit((status, tag))


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

    def __init__(self, files, mode='auto', profile=None, parent=None):
        super().__init__(parent)
        self._files = list(files)
        self._mode = mode
        self._profile = profile
        self._cancelled = False
        self._proc = None
        cfg = updater.load_config()
        self._no_history = not bool(cfg.get('history_enabled', True))

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
            args = [*SUTURA_CMD, '--mode', self._mode]
            if self._profile:
                args += ['--profile', self._profile]
            if self._no_history:
                args.append('--no-history')
            args.append(path)
            self._proc = subprocess.Popen(
                args,
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


class AnalyzeWorker(QThread):
    """Runs a read-only pre-repair analysis (validate + --dry-run) for each
    file, sequentially, in a background thread. Same subprocess pattern as
    RepairWorker: the CLI does all the pymeshlab work, so the GUI process
    never imports it. Never writes or modifies anything."""

    file_done = Signal(str, object)      # path, analysis dict
    progress = Signal(int, int)          # current, total
    all_done = Signal(bool)              # cancelled

    def __init__(self, files, mode='auto', profile=None, parent=None):
        super().__init__(parent)
        self._files = list(files)
        self._mode = mode
        self._profile = profile
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
                self.file_done.emit(path, {})
                continue
            data = self._analyze_one(path)
            self.file_done.emit(path, data)
            self.progress.emit(idx, n)
        self.all_done.emit(self._cancelled)

    def _analyze_one(self, path):
        if SUTURA_CMD is None:
            return {'error': 'sutura not found: no $SUTURA, no '
                             '~/.local/bin/sutura, and no repair.py next to '
                             'the GUI'}
        result = {}
        # dry-run first: carries mode/tuning/defect counts/estimated_confidence
        try:
            _args = [*SUTURA_CMD, '--dry-run', '--mode', self._mode]
            if self._profile:
                _args += ['--profile', self._profile]
            _args.append(path)
            self._proc = subprocess.Popen(
                _args,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            out, err = self._proc.communicate(timeout=600)
        except (OSError, subprocess.TimeoutExpired) as e:
            result['error'] = 'could not run dry-run: %s' % e
            self._proc = None
            return result
        finally:
            self._proc = None
        dry = parse_cli_output(out, err)
        if 'error' in dry:
            result['error'] = dry['error']
        else:
            for k in ('repair_mode', 'repair_profile', 'detected_type', 'detected_confidence',
                      'tuning_applied', 'would_apply', 'holes_found',
                      'largest_hole_diameter', 'non_manifold_regions',
                      'debris_faces_removable', 'self_intersections',
                      'connected_components', 'stage2_bridge_available',
                      'estimated_confidence', 'estimated_confidence_label',
                      'estimated_confidence_factors'):
                if k in dry:
                    result[k] = dry[k]
        # validate: adds the watertight pre-verdict + volume/orientation
        try:
            self._proc = subprocess.Popen(
                [*SUTURA_CMD, 'validate', path],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            out, err = self._proc.communicate(timeout=600)
        except (OSError, subprocess.TimeoutExpired) as e:
            self._proc = None
            result.setdefault('error', 'could not run validate: %s' % e)
            return result
        finally:
            self._proc = None
        val = parse_cli_output(out, err)
        v = val.get('validation')
        if v:
            result['watertight'] = bool(v.get('watertight'))
            result['signed_volume'] = v.get('signed_volume')
            result['surface_area'] = v.get('surface_area')
            result['orientation'] = v.get('orientation')
            result['validation_vertices'] = v.get('vertices')
            result['validation_faces'] = v.get('faces')
        elif 'error' in val and 'error' not in result:
            result['error'] = val['error']
        return result


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
                [*_script_cmd('heatmap-render', renderer), self._path, outfile,
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
    and hands four PNG byte blobs back to the main thread: before, after,
    detail_before, detail_after.
    """

    done = Signal(str, str, bytes, bytes, bytes, bytes)  # path, repaired, before, after, detail_before, detail_after
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
        dbefore_file = prefix + '_detail_before.png'
        dafter_file = prefix + '_detail_after.png'
        try:
            proc = subprocess.run(
                [*_script_cmd('before-after-render', renderer), self._path,
                 self._repaired, before_file, after_file, dbefore_file,
                 dafter_file, str(self._w), str(self._h)],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=600)
            if (proc.returncode != 0 or not os.path.getsize(before_file)
                    or not os.path.getsize(after_file)
                    or not os.path.getsize(dbefore_file)
                    or not os.path.getsize(dafter_file)):
                self.failed.emit(self._path, _t('before_after_failed'))
                return
            with open(before_file, 'rb') as f:
                before = f.read()
            with open(after_file, 'rb') as f:
                after = f.read()
            with open(dbefore_file, 'rb') as f:
                dbefore = f.read()
            with open(dafter_file, 'rb') as f:
                dafter = f.read()
        except (OSError, subprocess.TimeoutExpired):
            self.failed.emit(self._path, _t('before_after_failed'))
            return
        finally:
            try:
                os.unlink(before_file)
                os.unlink(after_file)
                os.unlink(dbefore_file)
                os.unlink(dafter_file)
                os.rmdir(tmpdir)
            except OSError:
                pass
        self.done.emit(self._path, self._repaired, before, after, dbefore, dafter)


class ViewerDataWorker(QThread):
    """Builds the interactive-viewer dataset (.npz) in a subprocess.

    Same isolation rule as ``HeatmapWorker``/``BeforeAfterWorker``: pymeshlab
    stays out of the GUI process. Runs ``viewer_data_render.py`` on the
    original + repaired meshes and loads the resulting npz (mesh arrays,
    defect/healed masks, distances, the shared camera) back into a dict.
    Spawned LAZILY the first time the user switches the before/after dialog
    to the interactive view; the result is cached for the dialog's lifetime.
    """

    done = Signal(str, object)      # path, npz data dict
    failed = Signal(str, str)       # path, message

    def __init__(self, path, repaired, w, h, parent=None):
        super().__init__(parent)
        self._path = path
        self._repaired = repaired
        self._w = w
        self._h = h

    def run(self):
        renderer = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                'viewer_data_render.py')
        tmpdir = tempfile.mkdtemp(prefix='sutura-vd-')
        outfile = os.path.join(tmpdir, 'viewer.npz')
        try:
            proc = subprocess.run(
                [*_script_cmd('viewer-data-render', renderer), self._path,
                 self._repaired, outfile, str(self._w), str(self._h)],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=900)
            if proc.returncode != 0 or not os.path.getsize(outfile):
                self.failed.emit(self._path, _t('viewer_ready_failed'))
                return
            with np.load(outfile) as d:
                data = {k: d[k] for k in d.files}
        except (OSError, subprocess.TimeoutExpired):
            self.failed.emit(self._path, _t('viewer_ready_failed'))
            return
        finally:
            try:
                shutil.rmtree(tmpdir, ignore_errors=True)
            except OSError:
                pass
        self.done.emit(self._path, data)


class _ClickableLabel(QLabel):
    """QLabel that emits ``clicked`` on a left mouse press."""

    clicked = Signal()

    def mousePressEvent(self, event):
        if event.button() == Qt.LeftButton:
            self.clicked.emit()
        super().mousePressEvent(event)


def _orbit_basis(base, dyaw, dpitch):
    """Rotate a 3x3 camera basis (rows = right/up/forward) by yaw (around
    world +Y) then pitch (around world +X). Returns a new orthonormal basis."""
    cy, sy = math.cos(dyaw), math.sin(dyaw)
    cp, sp = math.cos(dpitch), math.sin(dpitch)
    ry = np.array([[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]])
    rx = np.array([[1.0, 0.0, 0.0], [0.0, cp, -sp], [0.0, sp, cp]])
    return (ry @ rx) @ np.asarray(base, dtype=np.float64)


class _RenderThread(QThread):
    """Background rasteriser for the interactive mesh view.

    Consumes camera jobs (latest wins: a job submitted while one is drawing
    replaces the pending one) and emits the resulting QImage. Pure numpy +
    heatmap.draw_frame -- pymeshlab is never involved, so this thread is
    safe alongside the QMainWindow (the documented heap corruption is
    specific to pymeshlab inside a Qt thread).
    """

    frame_ready = Signal(object)    # QImage
    stage_changed = Signal(str)     # 'drag' | 'final'

    def __init__(self, parent=None):
        super().__init__(parent)
        self._lock = threading.Lock()
        self._job = None
        self._stopped = False

    def stop(self):
        with self._lock:
            self._stopped = True

    def submit(self, ctx, rotation, scale, stage='drag'):
        with self._lock:
            self._job = (ctx, np.asarray(rotation, dtype=np.float64), scale, stage)

    def run(self):
        while True:
            with self._lock:
                if self._stopped:
                    return
                job = self._job
                self._job = None
            if job is None:
                self.msleep(5)
                continue
            ctx, rot, scale, stage = job
            self.stage_changed.emit(stage)
            img = draw_frame(ctx, rotation=rot, scale=scale)
            self.frame_ready.emit(img)


class MeshViewport(QWidget):
    """Interactive before/after mesh view (drag to rotate, wheel to zoom).

    Pure numpy + heatmap (no pymeshlab). Rendering runs on a ``_RenderThread``
    with latest-wins frame dropping: while dragging/zooming the interactive
    LOD is drawn; ~300 ms after the input stops the full-resolution mesh is
    rendered once ("finalizing"). The camera starts at the same defect-facing
    orientation the static comparison uses.
    """

    rendering_stage = Signal(str)   # '' (idle) | 'final'

    def __init__(self, parent=None):
        super().__init__(parent)
        self._data = None
        self._side = 'repaired'
        self._deviation = False
        self._rot = None            # 3x3 camera basis
        self._scale = None          # zoom override (None = npz frame scale)
        self._dragging = False
        self._last_pos = None
        self._current = None        # latest QImage (scaled on paint)
        self._ctx = None            # active RenderContext
        self._ctx_lod = {}
        self._ctx_full = {}
        self.setMouseTracking(True)
        self._idle = QTimer(self)
        self._idle.setSingleShot(True)
        self._idle.setInterval(300)
        self._idle.timeout.connect(self._request_final)
        self._thread = _RenderThread(self)
        self._thread.frame_ready.connect(self._on_frame_ready)
        self._thread.stage_changed.connect(self._on_stage)
        self._thread.start()

    # --- public API -------------------------------------------------------

    def set_data(self, data):
        self._data = data
        self._rot = (np.asarray(data['initial_rotation'], dtype=np.float64)
                     if data['initial_rotation'].any() else _ISOMETRIC)
        self._scale = None
        self._build_contexts()
        self._apply_mode()
        self._request_frame(drag=True)
        self._idle.start()

    def set_side(self, side):
        if side not in ('original', 'repaired'):
            return
        self._side = side
        self._apply_mode()
        self._request_frame(drag=True)
        self._idle.start()

    def set_view_mode(self, mode):
        self._deviation = (mode == 'deviation')
        self._apply_mode()
        self._request_frame(drag=True)
        self._idle.start()

    def shutdown(self):
        self._thread.stop()
        self._thread.wait(2000)

    # --- internals --------------------------------------------------------

    def _build_contexts(self):
        """Prepare status + deviation RenderContexts for LOD and full mesh of
        both sides (deviation only exists on the repaired side)."""
        d = self._data
        w, h, pad = int(d['viewport_w']), int(d['viewport_h']), 24
        frame = (np.asarray(d['frame_center'], dtype=np.float64),
                 float(d['frame_scale']))

        def prep(verts, tris, vidx, healed=None, deviation=None, defect_col=None):
            holes = [{'verts_idx': np.asarray(vidx, dtype=np.int64)}] if len(vidx) else None
            return prepare_render(verts, tris, holes=holes, healed=healed,
                                  deviation=deviation, w=w, h=h, pad=pad,
                                  frame=frame,
                                  defect=(defect_col or (235, 60, 70)),
                                  healed_color=(46, 204, 113))

        # original side: defect red, no healed/deviation
        self._ctx_lod['original'] = {
            'status': prep(d['lverts'], d['ltris'], d['ldefect_vidx']),
            'deviation': None}
        self._ctx_full['original'] = {
            'status': prep(d['verts'], d['tris'], d['defect_vidx']),
            'deviation': None}
        # repaired side: still-broken orange, healed green, deviation ramp
        self._ctx_lod['repaired'] = {
            'status': prep(d['rlverts'], d['rltris'], d['lbroken_vidx'],
                           healed=d['lhealed'], defect_col=(255, 140, 60)),
            'deviation': prep(d['rlverts'], d['rltris'], d['lbroken_vidx'],
                              deviation=d['ldistance'], defect_col=(255, 140, 60))}
        self._ctx_full['repaired'] = {
            'status': prep(d['rverts'], d['rtris'], d['broken_vidx'],
                           healed=d['healed'], defect_col=(255, 140, 60)),
            'deviation': prep(d['rverts'], d['rtris'], d['broken_vidx'],
                              deviation=d['distance'], defect_col=(255, 140, 60))}

    def _apply_mode(self):
        # deviation mode only recolors the repaired side; the original side
        # keeps its status view.
        mode = 'deviation' if (self._deviation and self._side == 'repaired') else 'status'
        self._ctx = self._ctx_full[self._side][mode]

    def _request_frame(self, drag=True):
        if self._data is None or self._ctx is None:
            return
        lod = self._ctx_lod[self._side]
        mode = 'deviation' if (self._deviation and self._side == 'repaired') else 'status'
        ctx = lod[mode] if drag else self._ctx
        self._thread.submit(ctx, self._rot, self._scale,
                            'drag' if drag else 'final')

    def _request_final(self):
        if self._data is not None and self._ctx is not None:
            self._request_frame(drag=False)

    def _on_frame_ready(self, img):
        self._current = img
        self.update()

    def _on_stage(self, stage):
        self.rendering_stage.emit('final' if stage == 'final' else '')

    # --- events -----------------------------------------------------------

    def mousePressEvent(self, event):
        if event.button() == Qt.LeftButton:
            self._dragging = True
            self._last_pos = event.position().toPoint()
            event.accept()

    def mouseMoveEvent(self, event):
        if not self._dragging or self._last_pos is None:
            return
        p = event.position().toPoint()
        dx = p.x() - self._last_pos.x()
        dy = p.y() - self._last_pos.y()
        self._last_pos = p
        self._rot = _orbit_basis(self._rot, math.radians(dx * 0.4),
                                 math.radians(dy * 0.4))
        self._request_frame(drag=True)
        self._idle.start()
        event.accept()

    def mouseReleaseEvent(self, event):
        if event.button() == Qt.LeftButton:
            self._dragging = False
            self._last_pos = None
            self._idle.start()
            event.accept()

    def wheelEvent(self, event):
        delta = event.angleDelta().y()
        if delta == 0:
            delta = event.pixelDelta().y()
        if delta == 0:
            return
        base = float(self._data['frame_scale']) if self._data is not None else 1.0
        self._scale = (self._scale if self._scale is not None else base)
        self._scale *= (1.0 + 0.1 if delta > 0 else 1.0 - 0.1)
        self._scale = max(self._scale, base * 0.02)
        self._request_frame(drag=True)
        self._idle.start()
        event.accept()

    def paintEvent(self, event):
        if self._current is None:
            return
        p = QPainter(self)
        scaled = self._current.scaled(self.size(), Qt.KeepAspectRatio,
                                      Qt.SmoothTransformation)
        p.drawImage((self.width() - scaled.width()) // 2,
                    (self.height() - scaled.height()) // 2, scaled)
        p.end()

    def hideEvent(self, event):
        self._idle.stop()
        super().hideEvent(event)


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


def mode_suggestion_keys(a):
    """Priority-ordered mode-suggestion keys for an analysis dict `a`
    (AnalyzeWorker's validate + --dry-run result). Returns at most three
    suggestion keys, plus one `sug_extreme_caveat` when extreme was
    suggested AND the mesh is small enough (< 20 faces) that extreme could
    actually delete it (the `extreme_removed_object` risk). Informational
    only — never changes the mode."""
    s = []
    extreme_fired = False
    si = a.get('self_intersections', 0)
    holes = a.get('holes_found', 0)
    if si > 10:
        s.append('sug_si_many')
        extreme_fired = True
    elif si >= 1:
        s.append('sug_si_few')
        extreme_fired = True
    if holes > 10:
        s.append('sug_holes_many')
        extreme_fired = True
    elif holes >= 1:
        s.append('sug_holes_few')
    if (a.get('non_manifold_regions') or 0) > 3:
        s.append('sug_nm')
    if (a.get('connected_components') or 1) > 1:
        s.append('sug_cc')
    if a.get('detected_type') == 'unknown' or a.get('tuning_applied') is False:
        s.append('sug_unknown')
    if a.get('watertight'):
        s.append('sug_watertight')
    if (a.get('estimated_confidence_label') == 'low'
            and (a.get('repair_mode') or 'auto') in ('low', 'medium', 'auto')):
        s.append('sug_low_conf')
    s = s[:3]
    faces = a.get('validation_faces')
    if extreme_fired and faces is not None and faces < 20:
        s.append('sug_extreme_caveat')
    return s


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
        self.license_tag = None
        self._defects_by_path = {}
        self._type_by_path = {}
        self._diff_by_path = {}
        self._output_by_path = {}
        self._analysis_by_path = {}   # path -> analyze worker result dict
        self._heatmap_cache = {}      # path -> {size_key: QPixmap}
        self.heatmap_worker = None
        self._heatmap_zoom = None
        self.before_after_worker = None
        self._before_after_zoom = None
        self._repair_mode = 'auto'    # batch-wide repair mode (not per file)
        self._repair_profile = None   # batch-wide repair profile (not per file)

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
        self.btn_add_files.setIcon(
            self.style().standardIcon(QStyle.SP_DialogOpenButton))
        self.btn_add_files.setToolTip(_t('add_files_tip'))
        self.btn_add_folder = QPushButton(_t('add_folder'))
        self.btn_add_folder.setIcon(
            self.style().standardIcon(QStyle.SP_FileDialogNewFolder))
        self.btn_add_folder.setToolTip(_t('add_folder_tip'))
        self.btn_remove = QPushButton(_t('remove'))
        self.btn_remove.setIcon(
            self.style().standardIcon(QStyle.SP_DialogDiscardButton))
        self.btn_remove.setToolTip(_t('remove_tip'))
        self.btn_clear = QPushButton(_t('clear'))
        self.btn_clear.setIcon(
            self.style().standardIcon(QStyle.SP_TrashIcon))
        self.btn_clear.setToolTip(_t('clear_tip'))
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
        layout.addLayout(buttons)

        # second row: analysis/actions — Analyze + Mode (prep) | Repair + Stop
        actions = QHBoxLayout()
        self.btn_analyze = QPushButton(_t('analyze'))
        self.btn_analyze.setIcon(self._analyze_icon())
        self.btn_analyze.setToolTip(_t('analyze_tip'))
        self.btn_analyze.setEnabled(False)
        self.btn_mode = QPushButton(
            _t('mode_btn', _t('mode_name_' + self._repair_mode)))
        self.btn_mode.setToolTip(_t('mode_tip'))
        self.btn_repair = QPushButton(_t('repair'))
        self.btn_repair.setIcon(self._repair_icon())
        self.btn_repair.setObjectName('repairBtn')
        self.btn_repair.setToolTip(_t('repair_tip'))
        self.btn_stop = QPushButton(_t('stop'))
        self.btn_stop.setIcon(
            self.style().standardIcon(QStyle.SP_MediaStop))
        self.btn_stop.setToolTip(_t('stop_tip'))
        actions.addWidget(self.btn_analyze)
        actions.addWidget(self.btn_mode)
        # repair profile dropdown (batch-wide, like the mode): "Auto" = the
        # current classifier-driven default; the named profiles opt in to a
        # fixed Stage 1 threshold preset (see repair.PROFILES).
        from PySide6.QtWidgets import QComboBox as _QComboBox
        self.profile_combo = _QComboBox()
        self.profile_combo.addItem(_t('profile_auto'), None)
        for name in ('mechanical', 'organic', 'scan', 'miniature', 'fast'):
            self.profile_combo.addItem(_t('profile_name_' + name), name)
        self.profile_combo.setToolTip(_t('profile_tip'))
        self.profile_combo.currentIndexChanged.connect(self._on_profile_changed)
        actions.addWidget(self.profile_combo)
        actions.addStretch(1)
        actions.addWidget(self.btn_repair)
        actions.addWidget(self.btn_stop)
        layout.addLayout(actions)

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

        # read-only pre-repair analysis pane (validate + --dry-run)
        self.analyze_label = QLabel(_t('analyze_empty'))
        layout.addWidget(self.analyze_label)
        self.analysis = QPlainTextEdit()
        self.analysis.setReadOnly(True)
        self.analysis.setFont(QFontDatabase.systemFont(QFontDatabase.FixedFont))
        self.analysis.setFixedHeight(120)
        layout.addWidget(self.analysis)

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
        self.btn_analyze.clicked.connect(self.analyze)
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

    def _repair_icon(self):
        """Play-triangle icon, drawn dark so it is visible on the teal Repair
        button (same QPainter technique as _update_icon)."""
        size = 16
        pm = QPixmap(size, size)
        pm.fill(Qt.transparent)
        p = QPainter(pm)
        p.setRenderHint(QPainter.Antialiasing)
        color = QColor('#0b0f11')
        p.setPen(Qt.NoPen)
        p.setBrush(color)
        p.drawPolygon(QPolygon([QPoint(5, 3), QPoint(14, 8), QPoint(5, 13)]))
        p.end()
        return QIcon(pm)

    def _analyze_icon(self):
        """Magnifying-glass icon in the teal accent, drawn with QPainter."""
        size = 16
        pm = QPixmap(size, size)
        pm.fill(Qt.transparent)
        p = QPainter(pm)
        p.setRenderHint(QPainter.Antialiasing)
        color = QColor('#14b8a6')
        pen = QPen(color, 1.8)
        p.setPen(pen)
        p.setBrush(Qt.NoBrush)
        p.drawEllipse(4, 3, 8, 8)
        p.drawLine(10, 9, 14, 13)
        p.end()
        return QIcon(pm)

    def _maybe_ask_update_on_first_run(self):
        """Ask once (on first run, no config) about update checks and the
        anonymous usage history. The update question is skipped for AppImage
        builds (no self-update there); the history preference is always asked
        and defaults to enabled."""
        if updater.config_exists():
            return
        ask_updates = not updater.is_appimage()
        dlg = QMessageBox(self)
        dlg.setWindowTitle(_t('first_run_title') if ask_updates
                           else _t('first_run_history_title'))
        dlg.setText(_t('first_run_msg') if ask_updates
                    else _t('first_run_history_msg'))
        dlg.setStandardButtons(QMessageBox.Yes | QMessageBox.No)
        dlg.setDefaultButton(QMessageBox.No)
        hist = QCheckBox(_t('first_run_history_checkbox'), dlg)
        hist.setChecked(True)
        dlg.setCheckBox(hist)
        ret = dlg.exec()
        cfg = updater.load_config()
        cfg['history_enabled'] = bool(hist.isChecked())
        updater.save_config(cfg)  # record the history decision
        if ret == QMessageBox.Yes and ask_updates:
            updater.opt_in_check_updates()

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

    def _on_update_check_done(self, result):
        self.update_check = None
        status, new_tag = result
        if status == 'license':
            self.available_tag = None
            self.license_tag = new_tag
            self.update_btn.setIcon(self._update_icon(active=True))
            self.update_btn.setToolTip(_t('license_update_tooltip', new_tag))
            self.update_btn.setEnabled(True)
            self.update_btn.setVisible(True)
            if not updater.load_config().get('license_boundary_seen'):
                self._show_license_notice(new_tag)
            return
        if new_tag:
            self.available_tag = new_tag
            self.update_btn.setIcon(self._update_icon(active=True))
            self.update_btn.setToolTip(_t('update_btn_tooltip', new_tag))
            self.update_btn.setEnabled(True)
            self.update_btn.setVisible(True)

    def _show_license_notice(self, tag):
        """Warn that the new version crosses the license boundary and open the
        releases page on request. Shown once per install from a background
        check; every manual click of the update button also shows it."""
        cfg = updater.load_config()
        cfg['license_boundary_seen'] = True
        updater.save_config(cfg)
        box = QMessageBox(self)
        box.setWindowTitle(_t('license_block_title'))
        box.setIcon(QMessageBox.Information)
        box.setText(_t('license_block_msg', tag))
        open_btn = box.addButton(_t('license_block_open'), QMessageBox.AcceptRole)
        box.addButton(QMessageBox.Close)
        box.exec_()
        if box.clickedButton() is open_btn:
            webbrowser.open(updater.APPIMAGE_RELEASE_URL)

    def _on_update_clicked(self):
        if updater.is_appimage():
            QMessageBox.information(
                self, _t('app_title'), _t('appimage_update_msg'))
            return
        if self.license_tag is not None:
            self._show_license_notice(self.license_tag)
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
        self.btn_analyze.setEnabled(False)
        self.status.setText(_t('updating'))
        self.update_worker = UpdateWorker(tag, parent=self)
        self.update_worker.progress_msg.connect(self.status.setText)
        self.update_worker.finished_update.connect(self._on_update_done)
        self.update_worker.start()

    def _on_update_done(self, ok, msg):
        self.update_worker = None
        self.update_btn.setEnabled(True)
        self.btn_repair.setEnabled(bool(self.files))
        self.btn_analyze.setEnabled(bool(self.files))
        if ok:
            self.available_tag = None
            self.license_tag = None
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
        # select the newly added file so the analysis/defect panels update to
        # it (QTreeWidget only auto-selects the FIRST item implicitly)
        self.tree.setCurrentItem(item)
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
        self._analysis_by_path.clear()
        self.tree.clear()
        self._set_heatmap_thumb(None)
        self.btn_before_after.setEnabled(False)
        self.analysis.clear()
        self.analyze_label.setText(_t('analyze_empty'))
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
        self.btn_analyze.setEnabled(False)
        self.btn_stop.setEnabled(True)
        self.progress.setRange(0, len(self.files))
        self.progress.setValue(0)
        self.status.setText(_t('repairing'))
        self.log.clear()

        self.worker = RepairWorker(self.files, self._repair_mode,
                                  self._repair_profile, self)
        self.worker.file_done.connect(self._on_file_done)
        self.worker.progress.connect(self._on_progress)
        self.worker.all_done.connect(self._on_all_done)
        self.worker.start()

    def analyze(self):
        """Run the read-only pre-repair analysis (validate + --dry-run) on
        every file. Never modifies anything; results are shown per selected
        file in the analysis pane, separate from the repair confidence."""
        if not self.files or self.worker is not None:
            return
        self._analysis_by_path = {}
        self.analysis.clear()
        self.analyze_label.setText(_t('analyze_header'))
        self.btn_analyze.setEnabled(False)
        self.btn_repair.setEnabled(False)
        self.btn_stop.setEnabled(True)
        self.progress.setRange(0, len(self.files))
        self.progress.setValue(0)
        self.status.setText(_t('analyze_running'))

        self.worker = AnalyzeWorker(self.files, self._repair_mode,
                                   self._repair_profile, self)
        self.worker.file_done.connect(self._on_analyze_done)
        self.worker.progress.connect(self._on_analyze_progress)
        self.worker.all_done.connect(self._on_analyze_all_done)
        self.worker.start()

    def _on_analyze_progress(self, current, total):
        self.progress.setValue(current)
        self.status.setText(_t('analyze_n', current, total))

    def _on_analyze_done(self, path, data):
        if data:
            self._analysis_by_path[path] = data
        if self._item_by_path.get(path) is self.tree.currentItem():
            self._show_analysis(path)

    def _on_analyze_all_done(self, cancelled):
        self.status.setText(_t('done_stopped') if cancelled else _t('done'))
        self.btn_stop.setEnabled(False)
        self.btn_analyze.setEnabled(bool(self.files))
        self.btn_repair.setEnabled(bool(self.files))
        self.worker = None

    def _show_analysis(self, path):
        """Render the selected file's pre-repair analysis into its pane."""
        a = self._analysis_by_path.get(path)
        if a is None:
            self.analysis.clear()
            self.analyze_label.setText(_t('analyze_empty'))
            return
        self.analyze_label.setText(_t('analyze_header'))
        if a.get('error'):
            self.analysis.setPlainText(_t('analyze_error', a['error']))
            return
        lines = []
        tuning = a.get('tuning_applied')
        lines.append(_t('analyze_type',
                        a.get('detected_type') or '?',
                        a.get('detected_confidence') or 0.0,
                        a.get('repair_mode') or 'auto',
                        _t('tune_tuned') if tuning else _t('tune_default')))
        lines.append(_t('analyze_counts',
                        a.get('holes_found', 0),
                        a.get('self_intersections', 0),
                        a.get('non_manifold_regions', 0),
                        a.get('debris_faces_removable', 0)))
        wt = a.get('watertight')
        if wt is not None:
            lines.append(_t('analyze_watertight_yes') if wt
                         else _t('analyze_watertight_no'))
        ec = a.get('estimated_confidence')
        if ec is not None:
            lines.append(_t('analyze_est', ec,
                            _t('confidence_' + (a.get('estimated_confidence_label')
                                                or 'medium'))))
        sugs = self._analysis_suggestions(a)
        if sugs:
            lines.append('')
            lines.append(_t('sug_title'))
            for key in sugs:
                lines.append('  \u2022 %s' % _t(key))
        self.analysis.setPlainText('\n'.join(lines))

    def _analysis_suggestions(self, a):
        """Mode suggestions for an analysis result; see mode_suggestion_keys."""
        return mode_suggestion_keys(a)

    def stop(self):
        if self.worker is not None:
            self.worker.cancel()
            self.btn_stop.setEnabled(False)

    def _on_profile_changed(self, index):
        """Batch-wide repair profile selection (None = auto/classifier)."""
        self._repair_profile = self.profile_combo.itemData(index)

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
                                        data.get('tuning_applied'),
                                        data.get('repair_confidence'),
                                        data.get('repair_confidence_label'))
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
            self._show_analysis(current.text(0))
            self._show_defects(current.text(0))
            self._refresh_heatmap_thumb(current.text(0))
            self._refresh_before_after_btn(current.text(0))
        else:
            self.analysis.clear()
            self.analyze_label.setText(_t('analyze_empty'))
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
            # post-repair confidence score (None for multi-object 3MF, which
            # has no top-level aggregate)
            rc = dt[3]
            if rc is not None:
                base += ' — ' + _t('confidence_score', rc,
                                   _t('confidence_' + (dt[4] or 'medium')))
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

    def _on_before_after_done(self, path, repaired, before_png, after_png, dbefore_png, dafter_png):
        self.before_after_worker = None
        before = QPixmap()
        after = QPixmap()
        dbefore = QPixmap()
        dafter = QPixmap()
        if (not before.loadFromData(before_png, 'PNG') or before.isNull()
                or not after.loadFromData(after_png, 'PNG') or after.isNull()
                or not dbefore.loadFromData(dbefore_png, 'PNG') or dbefore.isNull()
                or not dafter.loadFromData(dafter_png, 'PNG') or dafter.isNull()):
            self._on_before_after_failed(path, _t('before_after_failed'))
            return
        if self._current_path() == path:
            self._open_before_after(path, repaired, before, after, dbefore, dafter)
            self.status.setText(_t('ready'))
        self._refresh_before_after_btn(self._current_path())

    def _on_before_after_failed(self, path, msg):
        self.before_after_worker = None
        self.status.setText(msg)
        self._refresh_before_after_btn(self._current_path())

    def _open_before_after(self, path, repaired, before, after, dbefore, dafter):
        """Modal dialog: the static before/after pair (main image + detail
        close-up, toggled together) is always shown first, with a
        Static/Interactive mode switch on top. The interactive 3D view
        (drag to rotate, wheel to zoom, surface-deviation ramp) is built
        LAZILY: the viewer_data_render.py subprocess runs the first time the
        user picks Interactive, and its npz data is cached for the dialog's
        lifetime."""
        if self._before_after_zoom is not None:
            self._before_after_zoom.close()
        dlg = QDialog(self)
        dlg.setWindowTitle(_t('before_after_title', os.path.basename(path)))
        lay = QVBoxLayout(dlg)

        # --- Static/Interactive mode switch (static is the default) --------
        mode_row = QHBoxLayout()
        mode_group = QButtonGroup(dlg)
        rad_static = QRadioButton(_t('viewer_static'))
        rad_static.setChecked(True)
        rad_inter = QRadioButton(_t('viewer_interactive'))
        mode_group.addButton(rad_static)
        mode_group.addButton(rad_inter)
        mode_row.addWidget(rad_static)
        mode_row.addWidget(rad_inter)
        mode_row.addStretch(1)
        lay.addLayout(mode_row)

        # --- Static panel (unchanged behaviour) ----------------------------
        static_panel = QWidget(dlg)
        slay = QVBoxLayout(static_panel)
        slay.setContentsMargins(0, 0, 0, 0)
        view = QLabel()
        view.setAlignment(Qt.AlignCenter)
        slay.addWidget(view, 1)
        detail_lab = QLabel()
        detail_lab.setAlignment(Qt.AlignCenter)
        detail_lab.setStyleSheet('font-size: 11px; color: #8891a0;')
        slay.addWidget(detail_lab)
        detail_caption = QLabel(_t('before_after_detail'))
        detail_caption.setAlignment(Qt.AlignCenter)
        detail_caption.setStyleSheet('font-size: 10px; color: #5b6472;')
        slay.addWidget(detail_caption)
        btn = QPushButton(_t('ba_original'))
        btn.setCheckable(True)
        state = {'show_after': False}

        def _thumb(pix):
            return pix.scaled(pix.width() // 2, pix.height() // 2,
                              Qt.KeepAspectRatio, Qt.SmoothTransformation)

        def show():
            if state['show_after']:
                view.setPixmap(after)
                detail_lab.setPixmap(_thumb(dafter))
            else:
                view.setPixmap(before)
                detail_lab.setPixmap(_thumb(dbefore))
            btn.setText(_t('ba_repaired' if state['show_after'] else 'ba_original'))

        def toggle(_checked):
            state['show_after'] = btn.isChecked()
            show()

        btn.toggled.connect(toggle)
        slay.addWidget(btn)
        show()
        lay.addWidget(static_panel, 1)

        # --- Interactive panel (hidden until first Interactive click) ------
        inter_panel = QWidget(dlg)
        ilay = QVBoxLayout(inter_panel)
        ilay.setContentsMargins(0, 0, 0, 0)
        viewport = MeshViewport(inter_panel)
        ilay.addWidget(viewport, 1)
        hint = QLabel(_t('viewer_hint'))
        hint.setAlignment(Qt.AlignCenter)
        hint.setStyleSheet('font-size: 10px; color: #5b6472;')
        ilay.addWidget(hint)
        ctrl = QHBoxLayout()
        side_group = QButtonGroup(inter_panel)
        rad_orig = QRadioButton(_t('ba_original'))
        rad_rep = QRadioButton(_t('ba_repaired'))
        rad_rep.setChecked(True)
        side_group.addButton(rad_orig)
        side_group.addButton(rad_rep)
        ctrl.addWidget(rad_orig)
        ctrl.addWidget(rad_rep)
        dev_group = QButtonGroup(inter_panel)
        rad_status = QRadioButton(_t('viewer_status_mode'))
        rad_status.setChecked(True)
        rad_dev = QRadioButton(_t('viewer_deviation_mode'))
        dev_group.addButton(rad_status)
        dev_group.addButton(rad_dev)
        ctrl.addWidget(rad_status)
        ctrl.addWidget(rad_dev)
        ctrl.addStretch(1)
        maxdev = QLabel('')
        maxdev.setStyleSheet('font-size: 11px; color: #8891a0;')
        ctrl.addWidget(maxdev)
        ilay.addLayout(ctrl)
        status_lab = QLabel('')
        status_lab.setAlignment(Qt.AlignCenter)
        status_lab.setStyleSheet('font-size: 11px; color: #e8b86d;')
        ilay.addWidget(status_lab)
        lay.addWidget(inter_panel, 1)
        inter_panel.hide()

        # --- interactive lazy build + cache (dialog-lifetime) -------------
        viewer = {'data': None, 'worker': None}

        def _show_mode():
            if rad_inter.isChecked():
                static_panel.hide()
                inter_panel.show()
                if viewer['data'] is None:
                    if viewer['worker'] is None:
                        viewer['worker'] = ViewerDataWorker(
                            path, repaired, 720, 540, dlg)
                        viewer['worker'].done.connect(_on_viewer_done)
                        viewer['worker'].failed.connect(_on_viewer_failed)
                        viewer['worker'].start()
                    status_lab.setText(_t('viewer_ready'))
                else:
                    viewport.set_data(viewer['data'])
                    _apply_inter_state()
                    status_lab.setText('')
            else:
                inter_panel.hide()
                static_panel.show()
                status_lab.setText('')

        def _apply_inter_state():
            side = 'repaired' if rad_rep.isChecked() else 'original'
            viewport.set_side(side)
            viewport.set_view_mode('deviation' if rad_dev.isChecked() else 'status')
            maxdev.setText(_t('viewer_max_dev', viewer['data']['hausdorff'])
                           if side == 'repaired' else '')

        def _on_viewer_done(_p, data):
            viewer['worker'] = None
            viewer['data'] = data
            status_lab.setText('')
            viewport.set_data(data)
            _apply_inter_state()

        def _on_viewer_failed(_p, msg):
            viewer['worker'] = None
            status_lab.setText(msg)
            rad_static.setChecked(True)
            _show_mode()

        def _on_rendering_stage(stage):
            status_lab.setText(_t('viewer_finalizing') if stage == 'final' else '')

        viewport.rendering_stage.connect(_on_rendering_stage)
        side_group.buttonClicked.connect(lambda _b: _apply_inter_state())
        dev_group.buttonClicked.connect(lambda _b: _apply_inter_state())
        mode_group.buttonClicked.connect(lambda _b: _show_mode())

        dlg.resize(before.width() + 24, before.height() + 150)
        self._before_after_zoom = dlg
        dlg.exec()
        viewport.shutdown()

    def _on_all_done(self, cancelled):
        self.status.setText(_t('done_stopped') if cancelled else _t('done'))
        self.btn_stop.setEnabled(False)
        self.btn_repair.setEnabled(bool(self.files))
        self.btn_analyze.setEnabled(bool(self.files))
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
        self.btn_analyze.setEnabled(bool(self.files) and self.worker is None)


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