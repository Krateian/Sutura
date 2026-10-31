#!/usr/bin/env python3
"""D13: the GUI analyze pane renders per-object suggestions (EN/TR)."""
import os
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _qt_test_env import ensure_qt_plugins  # noqa: E402
ensure_qt_plugins()

_TMP = tempfile.mkdtemp(prefix='sutura-sug-gui-')
os.environ['HOME'] = _TMP
os.environ['XDG_CONFIG_HOME'] = os.path.join(_TMP, 'config')
os.environ.pop('APPIMAGE', None)
os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

from PySide6.QtWidgets import QApplication  # noqa: E402

app = QApplication([])
import gui  # noqa: E402


def _s():
    return {
        'num': 3, 'display_name': 'Full Mend', 'confidence': 92,
        'confidence_label': 'high', 'reason_key': 'sugg_method_3',
        'reason': 'rebuilds the surface and closes holes',
    }


def main():
    w = gui.MainWindow()
    data = {'object_analyses': [
        {'model': 'a.model', 'suggestions': [_s()]},
        {'model': 'b.model', 'suggestions': [_s()]},
    ], 'suggestions': [_s()]}
    lines = w._suggestion_lines(data)
    text = '\n'.join(lines)
    assert 'Suggested methods' in text, text
    assert 'a.model' in text and 'b.model' in text, text
    assert '#3 Full Mend' in text and '92% High' in text, text
    assert 'rebuilds the surface' in text, text
    print('ok  EN suggestions block')

    w._analysis_by_path.pop('/tmp/x.3mf', None)
    w._method_analysis_by_path['/tmp/x.3mf'] = data
    w._show_analysis('/tmp/x.3mf')
    assert '#3 Full Mend' in w.analysis.toPlainText(), w.analysis.toPlainText()
    print('ok  analyze pane without a dry-run result')

    orig = gui._t
    try:
        def tr(key, *args):
            s = gui.STRINGS['tr'].get(key, gui.STRINGS['en'].get(key, key))
            return s % args if args else s
        gui._t = tr
        lines = w._suggestion_lines(data)
        text = '\n'.join(lines)
        assert 'Önerilen yöntemler' in text, text
        assert '92% Yüksek' in text, text
        assert 'yüzeyi yeniden kurar' in text, text
        print('ok  TR suggestions block')
    finally:
        gui._t = orig
    w.close()


if __name__ == '__main__':
    main()
    print('suggestion GUI tests passed')
