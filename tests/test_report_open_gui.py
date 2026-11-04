#!/usr/bin/env python3
"""Offscreen C6/C7 GUI tests: compact verdict + Details + open actions."""
import os
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _qt_test_env import ensure_qt_plugins  # noqa: E402
ensure_qt_plugins()

_TMP = tempfile.mkdtemp(prefix='sutura-report-open-')
os.environ['HOME'] = _TMP
os.environ['XDG_CONFIG_HOME'] = os.path.join(_TMP, 'config')
os.environ.pop('APPIMAGE', None)
os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

from PySide6.QtWidgets import QApplication  # noqa: E402

app = QApplication([])
import gui  # noqa: E402


def _data(out):
    return {
        'input': '/tmp/in.stl', 'output': out,
        'stage1': {'two_manifold': True, 'holes_remaining': 0,
                   'components': 1, 'components_before': 1,
                   'holes_closed': 2},
        'stage2': {'ok': True},
        'category': 'watertight',
        'print_report': {'fixed': [{'code': 'holes_closed', 'count': 2}],
                         'issues': [], 'advisories': [],
                         'geometry': {'components_before': 1,
                                      'components_after': 1,
                                      'deviation_max_pct': 0.5}},
    }


def main():
    out = os.path.join(_TMP, 'thing_fixed.stl')
    with open(out, 'wb') as f:
        f.write(b'solid ok\nendsolid\n')
    w = gui.MainWindow()

    w._render_report_brief('/tmp/in.stl', _data(out), 'DETAILS-TEXT')
    assert '✓' in w.report_verdict.text(), w.report_verdict.text()
    assert 'Ready to print' in w.report_verdict.text()
    assert 'Holes closed: 2' in w.report_counts.text(), w.report_counts.text()
    assert 'Shape change: 0.50' in w.report_counts.text(), w.report_counts.text()
    assert w.report_details.isHidden()
    w.btn_report_details.setChecked(True)
    assert not w.report_details.isHidden()
    assert w.report_details.toPlainText() == 'DETAILS-TEXT'
    w.btn_report_details.setChecked(False)
    print('ok  ready verdict + Details toggle')

    notes = _data(out)
    notes['stage2'] = {'error': 'Stage 2 skipped: no bridge'}
    notes['print_report']['advisories'] = [
        {'code': 'thin_wall', 'thin_ratio': 0.1, 'threshold_norm': 0.02}]
    w._render_report_brief('/tmp/in.stl', notes, None)
    assert '⚠' in w.report_verdict.text()
    assert 'Repaired with notes' in w.report_verdict.text()
    assert 'Some walls are thin' in w.report_notes.text(), w.report_notes.text()
    print('ok  notes verdict + thin wall')

    failed = _data(None)
    failed['stage1']['two_manifold'] = False
    failed['stage1']['holes_remaining'] = 3
    failed.pop('stage2', None)
    w._render_report_brief('/tmp/in.stl', failed, 'ERROR: broken')
    assert '✗' in w.report_verdict.text()
    assert 'Not fully repaired' in w.report_verdict.text()
    assert 'hole(s) remain' in w.report_notes.text(), w.report_notes.text()
    print('ok  failed verdict')

    # Open result / folder use the exact JSON output path.
    opened = []

    class _FakeDesktop:
        @staticmethod
        def openUrl(url):
            opened.append(url.toLocalFile())
            return True

    old_desktop = gui.QDesktopServices
    gui.QDesktopServices = _FakeDesktop
    try:
        w._output_by_path['/tmp/in.stl'] = out
        w._open_result('/tmp/in.stl')
        w._show_output_folder('/tmp/in.stl')
    finally:
        gui.QDesktopServices = old_desktop
    assert opened == [out, _TMP], opened
    print('ok  open result + show folder')

    launched = []

    class _FakePopen:
        def __init__(self, argv, *a, **k):
            launched.append(argv)

    old_popen = gui.subprocess.Popen
    gui.subprocess.Popen = _FakePopen
    try:
        w._open_in_slicer('OrcaSlicer', ['orca-slicer'], '/tmp/in.stl')
    finally:
        gui.subprocess.Popen = old_popen
    assert launched and launched[0] == ['orca-slicer', out], launched
    print('ok  open in slicer command')

    test_format_report_localized()
    w.close()


def test_format_report_localized():
    """format_report runs in EN and TR and survives a sparse report.

    Exercises the real details-body formatter (not a hardcoded string) with
    None stage1/stage2/objects and a 3MF report whose object lists are None.
    """
    sparse = {'output': 'a.stl', 'stage1': None, 'stage2': None, 'objects': None}
    en = gui.format_report(sparse)
    assert 'Output file:' in en, en
    multi = {'output': 'a.3mf', 'stage1': {}, 'objects': 2,
             'objects_watertight': 1, 'object_reports': None}
    assert '3MF objects repaired' in gui.format_report(multi)
    en_tbl = gui.STRINGS['en']
    try:
        gui.STRINGS['en'] = gui.STRINGS['tr']
        tr = gui.format_report(sparse)
        assert 'Çıktı dosyası:' in tr, tr
        assert 'Aşama 1' in tr, tr
        assert 'Onarılan 3MF nesnesi' in gui.format_report(multi)
    finally:
        gui.STRINGS['en'] = en_tbl
    print('ok  format_report EN/TR + sparse report')


if __name__ == '__main__':
    main()
    print('report/open GUI tests passed')
