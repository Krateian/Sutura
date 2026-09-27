#!/usr/bin/env python3
"""Offscreen regression test for the P3 per-file method tagging UI.

Runs as its OWN process (the GUI must never import pymeshlab; this file does
not import repair). A temp HOME/XDG_CONFIG_HOME isolates config and engines.

Checks:
- the third "Method" column shows Auto / the top recommendation / the tag chain,
- tagging order is preserved (methods and external engines),
- the "Use method" popup lists all #1..#12 with placeholders / needs-input
  disabled and the running order badge on the checked ones,
- the "External engines" and "Recommended methods" submenus build,
- RepairWorker emits per-file ``--methods``/``--engines`` args,
- MainWindow passes the per-file tag mapping to RepairWorker.

Usage:
    QT_QPA_PLATFORM=offscreen <venv>/bin/python tests/test_methods_gui.py
"""
import os
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

_TMP = tempfile.mkdtemp(prefix='sutura-gui-method-')
os.environ['HOME'] = _TMP
os.environ['XDG_CONFIG_HOME'] = os.path.join(_TMP, 'config')
os.environ.pop('APPIMAGE', None)
os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

ENG_DIR = os.path.join(os.environ['XDG_CONFIG_HOME'], 'sutura', 'engines')
os.makedirs(ENG_DIR, exist_ok=True)
with open(os.path.join(ENG_DIR, 'copycat.toml'), 'w') as f:
    f.write('name = "copycat"\ncommand = ["/bin/cp", "{input}", "{output}"]\n'
            'placement = "after_stage1"\nenabled = true\n')

from PySide6.QtWidgets import QApplication  # noqa: E402

app = QApplication([])
import gui  # noqa: E402
import methods  # noqa: E402


class _Sig:
    def connect(self, *_a, **_k):
        pass


class _FakeWorker:
    """Stand-in for RepairWorker: records the tag mapping, starts nothing."""

    captured = {}

    def __init__(self, files, *args, **kwargs):
        _FakeWorker.captured = kwargs
        self.file_done = _Sig()
        self.progress = _Sig()
        self.all_done = _Sig()

    def start(self):
        pass


def _fake_proc(out='{"ok": true}'):
    class _P:
        def poll(self):
            return None

        def communicate(self, timeout=None):
            return out, ''

    return _P()


def main():
    w = gui.MainWindow()
    assert 'pymeshlab' not in sys.modules, 'pymeshlab leaked into the GUI'
    assert w.tree.columnCount() == 3
    assert w.tree.headerItem().text(1) == gui._t('col_method')
    print('ok  third Method column present, no pymeshlab')

    a = '/tmp/sutura_p3_a.stl'
    b = '/tmp/sutura_p3_b.stl'
    w._add_path(a)
    w._add_path(b)
    assert w._item_by_path[a].text(1) == gui._t('method_auto')
    print('ok  default cell is Auto')

    # tag order: methods then an engine, arrow-joined; untag preserves order
    w._on_tag_toggle([b], 'method', 2)
    w._on_tag_toggle([b], 'method', 5)
    w._on_tag_toggle([b], 'engine', 'copycat')
    assert w._item_by_path[b].text(1) == '#2 \u2192 #5 \u2192 @copycat', \
        w._item_by_path[b].text(1)
    w._on_tag_toggle([b], 'method', 2)
    assert w._item_by_path[b].text(1) == '#5 \u2192 @copycat', \
        w._item_by_path[b].text(1)
    w._clear_tags([b])
    assert w._item_by_path[b].text(1) == gui._t('method_auto')
    print('ok  tag order, removal and clear')

    # a completed analysis shows the top recommendation
    w._method_analysis_by_path[a] = {'recommendations': [
        {'num': 3, 'id': 'deep_full', 'name': 'Full deep repair',
         'score': 0.82, 'reason': 'template mechanical'}]}
    w._refresh_method_cell(a)
    assert w._item_by_path[a].text(1) == gui._t(
        'method_auto_rec', 3, gui._method_name(3), 82), w._item_by_path[a].text(1)
    print('ok  recommendation shown in the Method column')

    # Use method menu: 12 entries, placeholders / needs-input disabled
    w._on_tag_toggle([b], 'method', 3)
    m = gui.QMenu(w.tree)
    um = gui._KeepOpenMenu(gui._t('menu_use_method'), m)
    w._build_use_method_menu(um, [b], b)
    acts = um.actions()
    assert len(acts) == 12, len(acts)
    assert acts[2].isChecked() and '\u2713 1' in acts[2].text(), acts[2].text()
    # 8/9/10 are implemented and enabled; 11 is still a placeholder, 12 needs input
    for num in (8, 9, 10):
        assert acts[num - 1].isEnabled(), num
    assert not acts[10].isEnabled(), 11
    assert 'not implemented' in acts[10].toolTip(), acts[10].toolTip()
    assert not acts[11].isEnabled(), 'repeat_manual must be disabled (needs input)'
    assert acts[11].toolTip() == gui._t('menu_needs_input')
    print('ok  Use method list, placeholders and order badge')

    # 4.1: unchecking a method renumbers the siblings' order badges live
    cb = '/tmp/sutura_p3_c.stl'
    w._add_path(cb)
    w._clear_tags([cb])
    w._on_tag_toggle([cb], 'method', 2)
    w._on_tag_toggle([cb], 'method', 5)
    mm = gui.QMenu(w.tree)
    um2 = gui._KeepOpenMenu(gui._t('menu_use_method'), mm)
    w._build_use_method_menu(um2, [cb], cb)
    a2 = um2.actions()[1]   # method 2 -> order 1
    a5 = um2.actions()[4]   # method 5 -> order 2
    assert '\u2713 1' in a2.text() and '\u2713 2' in a5.text(), \
        (a2.text(), a5.text())
    w._on_use_action(methods.get_method(2), [cb], cb, a2, um2)
    assert '\u2713' not in a2.text(), a2.text()
    assert '\u2713 1' in a5.text(), a5.text()      # renumbered 2 -> 1
    print('ok  Use method badge refresh on untag')

    # 7.1: every classification summary key is localizable (EN + TR)
    assert 'res_extreme_removed_object' in gui.STRINGS['en'], 'EN string missing'
    assert 'res_extreme_removed_object' in gui.STRINGS['tr'], 'TR string missing'
    assert gui._t('res_extreme_removed_object') != 'res_extreme_removed_object'
    # 7.2: recommendation reasons localize through reason_key
    rr = {'reason_key': 'rec_reason_si', 'reason_args': (123,),
          'reason': 'self-intersections present (123)'}
    assert '123' in gui._rec_reason(rr), gui._rec_reason(rr)
    print('ok  summary/recommendation i18n keys')

    # Engine submenu lists the configured engine
    em = gui.QMenu(w.tree)
    w._build_engines_menu(em, [b], b)
    assert em.actions(), 'expected the configured engine'
    assert em.actions()[0].text().startswith('copycat'), em.actions()[0].text()
    print('ok  External engines submenu')

    # Recommended submenu before analysis offers "Analyze first"
    ng = '/tmp/sutura_p3_none.stl'
    rm = gui.QMenu(w.tree)
    w._build_recommended_menu(rm, [ng], ng)
    assert rm.actions()[0].text() == gui._t('menu_analyze_first'), \
        rm.actions()[0].text()
    print('ok  Recommended submenu suggests Analyze first')

    # full context menu builds (shown separately by _on_tree_context_menu)
    menu = w._build_context_menu([b])
    titles = [a.text() for a in menu.actions()]
    for key in ('menu_analyze', 'menu_recommended', 'menu_use_method',
                'menu_engines', 'menu_clear_tags'):
        assert gui._t(key) in titles, (gui._t(key), titles)
    print('ok  context menu builds with all entries')

    # RepairWorker emits the per-file --methods/--engines args
    saved_cmd = gui.SUTURA_CMD
    orig_popen = gui.subprocess.Popen
    captured = {}
    gui.SUTURA_CMD = ['sutura-test']

    def _fake_popen(args, **_k):
        captured['args'] = list(args)
        return _fake_proc()

    gui.subprocess.Popen = _fake_popen
    try:
        rw = gui.RepairWorker([b], methods_by_path={b: [5, 3]},
                              engines_by_path={b: ['copycat']})
        rw._run_one(b)
    finally:
        gui.subprocess.Popen = orig_popen
        gui.SUTURA_CMD = saved_cmd
    args = captured['args']
    assert args[args.index('--methods') + 1] == '5,3', args
    assert args[args.index('--engines') + 1] == 'copycat', args
    print('ok  RepairWorker per-file --methods/--engines')

    # MainWindow maps the tag lists onto RepairWorker kwargs
    saved_worker = gui.RepairWorker
    gui.RepairWorker = _FakeWorker
    try:
        w._clear_tags([b])
        w._on_tag_toggle([b], 'method', 2)   # b -> #2 -> #3 (order preserved)
        w._on_tag_toggle([b], 'method', 3)
        w._run_batch([b], force=False)
    finally:
        gui.RepairWorker = saved_worker
    assert _FakeWorker.captured.get('methods_by_path') == {b: [2, 3]}, \
        _FakeWorker.captured.get('methods_by_path')
    assert _FakeWorker.captured.get('engines_by_path') == {}, \
        _FakeWorker.captured.get('engines_by_path')
    print('ok  MainWindow -> RepairWorker tag mapping')

    w.close()
    print('\nmethods GUI tests passed (GUI-OK)')


if __name__ == '__main__':
    main()
