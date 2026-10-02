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

# PySide6's own Qt6 plugins: a macOS conda install also carries qt-main/Qt5,
# whose bin/qt.conf otherwise hijacks the platform-plugin lookup. Test-side
# helper only (the sutura-gui launcher resolves it via conda run).
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _qt_test_env import ensure_qt_plugins  # noqa: E402
ensure_qt_plugins()

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

    # Use method menu: 16 entries, placeholders / needs-input disabled
    w._on_tag_toggle([b], 'method', 3)
    m = gui.QMenu(w.tree)
    um = gui._KeepOpenMenu(gui._t('menu_use_method'), m)
    w._build_use_method_menu(um, [b], b)
    acts = um.actions()
    assert len(acts) == 16, len(acts)
    assert acts[2].isChecked() and '\u2713 1' in acts[2].text(), acts[2].text()
    # 8-12 are implemented and enabled (12 opens the repeat picker); 14
    # (Mirror Complete) is enabled.  13 Graft needs the Rust sutura_geom
    # extension, so it is enabled iff the registry says it is available and is
    # otherwise disabled with the unavailable reason in its tooltip.
    for num in (8, 9, 10, 11, 12):
        assert acts[num - 1].isEnabled(), num
    g_ok, g_reason = methods.get_method(13).available()
    assert acts[12].isEnabled() is g_ok, (acts[12].isEnabled(), g_ok, g_reason)
    if not g_ok:
        tip = acts[12].toolTip()
        assert tip, tip
        if 'sutura_geom' in (g_reason or ''):
            assert 'sutura_geom' in tip, (tip, g_reason)
    assert acts[13].isEnabled(), 'method 14 (Mirror Complete) must be enabled'
    # 16 Dressing is opt-in (needs_user_input) but still selectable.
    assert acts[15].text().startswith('Dressing') or 'Dressing' in acts[15].text(), \
        acts[15].text()
    print('ok  Use method list, availability and order badge')

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

    # Item 4: KeepOpenMenu keeps multiple checks and preserves order on simulated triggers
    kd = '/tmp/sutura_keep_open.stl'
    w._add_path(kd)
    w._clear_tags([kd])
    km = gui.QMenu(w.tree)
    kum = gui._KeepOpenMenu(gui._t('menu_use_method'), km)
    w._build_use_method_menu(kum, [kd], kd)

    ka1 = kum.actions()[0]  # method 1
    ka4 = kum.actions()[3]  # method 4
    ka5 = kum.actions()[4]  # method 5

    from PySide6.QtCore import QPointF, QEvent
    from PySide6.QtGui import QMouseEvent

    def click_action(act):
        kum.setActiveAction(act)
        ev = QMouseEvent(QEvent.Type.MouseButtonRelease, QPointF(10.0, 10.0),
                         QPointF(10.0, 10.0),
                         gui.Qt.MouseButton.LeftButton,
                         gui.Qt.MouseButton.LeftButton,
                         gui.Qt.KeyboardModifier.NoModifier)
        kum.mouseReleaseEvent(ev)
        return ev

    # Trigger #1 via mouseReleaseEvent on the keep-open menu
    e1 = click_action(ka1)
    assert e1.isAccepted()
    assert ka1.isChecked() and '\u2713 1' in ka1.text(), ka1.text()

    # Trigger #4
    e4 = click_action(ka4)
    assert e4.isAccepted()
    assert ka4.isChecked() and '\u2713 2' in ka4.text(), ka4.text()

    # Trigger #5
    e5 = click_action(ka5)
    assert e5.isAccepted()
    assert ka5.isChecked() and '\u2713 3' in ka5.text(), ka5.text()

    # All three remain checked simultaneously in try-order
    assert ka1.isChecked() and ka4.isChecked() and ka5.isChecked()
    assert w._method_tags_by_path[kd] == [('method', 1), ('method', 4), ('method', 5)]
    assert w._item_by_path[kd].text(1) == '#1 \u2192 #4 \u2192 #5'

    # Untag middle (#4): verify remaining keep checked and #5 renumbers to 2
    e4_off = click_action(ka4)
    assert e4_off.isAccepted()
    assert not ka4.isChecked() and '\u2713' not in ka4.text()
    assert ka1.isChecked() and '\u2713 1' in ka1.text()
    assert ka5.isChecked() and '\u2713 2' in ka5.text()
    assert w._method_tags_by_path[kd] == [('method', 1), ('method', 5)]
    assert w._item_by_path[kd].text(1) == '#1 \u2192 #5'
    print('ok  KeepOpenMenu multiple checks and order preservation via simulated triggers')

    # 7.1: every classification summary key is localizable (EN + TR)
    assert 'res_extreme_removed_object' in gui.STRINGS['en'], 'EN string missing'
    assert 'res_extreme_removed_object' in gui.STRINGS['tr'], 'TR string missing'
    assert gui._t('res_extreme_removed_object') != 'res_extreme_removed_object'
    # 7.2: recommendation reasons localize through reason_key
    rr = {'reason_key': 'rec_reason_si', 'reason_args': (123,),
          'reason': 'self-intersections present (123)'}
    assert '123' in gui._rec_reason(rr), gui._rec_reason(rr)
    print('ok  summary/recommendation i18n keys')

    # Repeat picker dialog (P-REP): builds offscreen and maps clicks to points
    import numpy as _np
    dv = _np.array([[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
                    [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]], float)
    dt = _np.array([[0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
                    [0, 1, 5], [0, 5, 4], [1, 2, 6], [1, 6, 5],
                    [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7]], _np.int64)
    dlg = gui.RepeatPickerDialog({'verts': dv, 'tris': dt})
    assert dlg.btn_ok.isEnabled() is False
    from PySide6.QtCore import QPointF, QEvent
    from PySide6.QtGui import QMouseEvent
    for _ in range(2):
        ev = QMouseEvent(QEvent.MouseButtonPress, QPointF(320.0, 240.0),
                         gui.Qt.LeftButton, gui.Qt.LeftButton,
                         gui.Qt.NoModifier)
        dlg.view.mousePressEvent(ev)
    assert dlg.view.source_point is not None, 'first click picked no source'
    assert dlg.view.target_point is not None, 'second click picked no target'
    assert dlg.btn_ok.isEnabled()
    dlg.close()
    print('ok  repeat picker dialog builds and picks points')

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

    # Dressing (#16) batch-wide: checkbox -> --experimental-dressing, drain
    # combo -> --dressing-drain <mode>
    captured.clear()
    gui.subprocess.Popen = _fake_popen
    try:
        rw = gui.RepairWorker([b], dressing=True, dressing_drain='deep')
        rw._run_one(b)
    finally:
        gui.subprocess.Popen = orig_popen
    dargs = captured['args']
    assert '--experimental-dressing' in dargs, dargs
    assert dargs[dargs.index('--dressing-drain') + 1] == 'deep', dargs
    # preset default (None) adds no drain flag
    captured.clear()
    gui.subprocess.Popen = _fake_popen
    try:
        rw = gui.RepairWorker([b], dressing=True, dressing_drain=None)
        rw._run_one(b)
    finally:
        gui.subprocess.Popen = orig_popen
    assert '--dressing-drain' not in captured['args'], captured['args']
    print('ok  RepairWorker Dressing flag and drain override')

    # SI policy: default 'report' adds no flag; 'repair'/'off' add --si-mode
    captured.clear()
    gui.subprocess.Popen = _fake_popen
    try:
        rw = gui.RepairWorker([b], si_mode='report')
        rw._run_one(b)
    finally:
        gui.subprocess.Popen = orig_popen
    assert '--si-mode' not in captured['args'], captured['args']
    for mode in ('repair', 'off'):
        captured.clear()
        gui.subprocess.Popen = _fake_popen
        try:
            rw = gui.RepairWorker([b], si_mode=mode)
            rw._run_one(b)
        finally:
            gui.subprocess.Popen = orig_popen
        sargs = captured['args']
        assert sargs[sargs.index('--si-mode') + 1] == mode, sargs
    print('ok  RepairWorker SI-mode flag')

    # The SI-mode combo maps Report/Repair/Off to report/repair/off
    w.cmb_si_mode.setCurrentIndex(0)
    assert w._si_mode == 'report'
    w.cmb_si_mode.setCurrentIndex(1)
    assert w._si_mode == 'repair'
    w.cmb_si_mode.setCurrentIndex(2)
    assert w._si_mode == 'off'
    print('ok  SI-mode combo mapping')

    # The drain combo maps indices 0..4 to None/none/half/full/deep
    w.cmb_dressing_drain.setCurrentIndex(0)
    assert w._dressing_drain is None
    for idx, mode in enumerate(('none', 'half', 'full', 'deep'), start=1):
        w.cmb_dressing_drain.setCurrentIndex(idx)
        assert w._dressing_drain == mode, (idx, w._dressing_drain)
    print('ok  Dressing drain combo mapping')

    # Defect-set combo maps Preset/all/holes_nm to None/'all'/'holes_nm'
    w.cmb_dressing_defects.setCurrentIndex(0)
    assert w._dressing_defects is None
    w.cmb_dressing_defects.setCurrentIndex(1)
    assert w._dressing_defects == 'all'
    w.cmb_dressing_defects.setCurrentIndex(2)
    assert w._dressing_defects == 'holes_nm'
    print('ok  Dressing defect-set combo mapping')

    # Scale combos map Preset/x0.25../x2 to None/0.25../2.0
    for cmb, attr in ((w.cmb_dressing_rmax_scale, '_dressing_rmax_scale'),
                      (w.cmb_dressing_sigma_scale, '_dressing_sigma_scale')):
        cmb.setCurrentIndex(0)
        assert getattr(w, attr) is None
        for idx, factor in enumerate((0.25, 0.5, 0.75, 1.0, 1.5, 2.0), 1):
            cmb.setCurrentIndex(idx)
            assert getattr(w, attr) == factor, (attr, idx, getattr(w, attr))
    print('ok  Dressing scale-factor combo mapping')

    # RepairWorker adds the defect-set / scale flags when set
    captured.clear()
    gui.subprocess.Popen = _fake_popen
    try:
        rw = gui.RepairWorker([b], dressing=True, dressing_defects='holes_nm',
                              dressing_rmax_scale=0.5,
                              dressing_sigma_scale=2.0)
        rw._run_one(b)
    finally:
        gui.subprocess.Popen = orig_popen
    xargs = captured['args']
    assert xargs[xargs.index('--dressing-defects') + 1] == 'holes_nm', xargs
    assert xargs[xargs.index('--dressing-rmax-scale') + 1] == '0.5', xargs
    assert xargs[xargs.index('--dressing-sigma-scale') + 1] == '2.0', xargs
    # preset defaults (None) add no defect-set / scale flags
    captured.clear()
    gui.subprocess.Popen = _fake_popen
    try:
        rw = gui.RepairWorker([b], dressing=True, dressing_defects=None,
                              dressing_rmax_scale=None,
                              dressing_sigma_scale=None)
        rw._run_one(b)
    finally:
        gui.subprocess.Popen = orig_popen
    for flag in ('--dressing-defects', '--dressing-rmax-scale',
                 '--dressing-sigma-scale'):
        assert flag not in captured['args'], (flag, captured['args'])
    print('ok  RepairWorker Dressing defect-set / scale overrides')

    # --dressing-force-adopt implies --experimental-dressing (the flag skips
    # Dressing's shape gate but only means anything with Dressing enabled)
    captured.clear()
    gui.subprocess.Popen = _fake_popen
    try:
        rw = gui.RepairWorker([b], dressing_force_adopt=True)
        rw._run_one(b)
    finally:
        gui.subprocess.Popen = orig_popen
    fargs = captured['args']
    assert '--dressing-force-adopt' in fargs, fargs
    assert '--experimental-dressing' in fargs, fargs
    captured.clear()
    gui.subprocess.Popen = _fake_popen
    try:
        rw = gui.RepairWorker([b])
        rw._run_one(b)
    finally:
        gui.subprocess.Popen = orig_popen
    assert '--dressing-force-adopt' not in captured['args'], captured['args']
    assert '--experimental-dressing' not in captured['args'], captured['args']
    print('ok  RepairWorker force-adopt implies experimental dressing')

    # The force-adopt checkbox maps to MainWindow._dressing_force_adopt and is
    # registered in the options defaults (options-window counter).
    w.chk_dressing_force_adopt.setChecked(True)
    assert w._dressing_force_adopt is True
    w.chk_dressing_force_adopt.setChecked(False)
    assert w._dressing_force_adopt is False
    assert (w.chk_dressing_force_adopt, False) in w._options_defaults
    print('ok  Dressing force-adopt checkbox + defaults')

    # The post-batch "Try Dressing" button appears only when a finished result
    # suggested Dressing, and clicking it force-reruns exactly those files.
    assert w.btn_try_dressing.isHidden()
    w._suggestion_by_path = {a: [{'method': 'dressing', 'force': True}],
                             b: [{'method': 'other'}]}
    w._refresh_dressing_suggestion()
    assert not w.btn_try_dressing.isHidden()
    assert w._dressing_suggestion_files() == [a], w._dressing_suggestion_files()
    saved_worker = gui.RepairWorker
    gui.RepairWorker = _FakeWorker
    try:
        w._on_try_dressing()
    finally:
        gui.RepairWorker = saved_worker
    assert w.chk_dressing_force_adopt.isChecked(), 'force checkbox not set'
    assert w._dressing_force_adopt is True
    assert _FakeWorker.captured.get('dressing_force_adopt') is True, \
        _FakeWorker.captured
    assert _FakeWorker.captured.get('force') is True, _FakeWorker.captured
    assert w.btn_try_dressing.isHidden()
    print('ok  Try Dressing button + force re-run')

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
