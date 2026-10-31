#!/usr/bin/env python3
"""Offscreen Method control panel tests.

Usage: QT_QPA_PLATFORM=offscreen <venv>/bin/python tests/test_control_panel_gui.py
"""
import os
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

_TMP = tempfile.mkdtemp(prefix='sutura-control-panel-')
os.environ['HOME'] = _TMP
os.environ['XDG_CONFIG_HOME'] = os.path.join(_TMP, 'config')
os.environ['SUTURA_POLICY_USER'] = os.path.join(_TMP, 'policy.toml')
os.environ.pop('APPIMAGE', None)
os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _qt_test_env import ensure_qt_plugins  # noqa: E402
ensure_qt_plugins()

from PySide6.QtWidgets import QApplication  # noqa: E402
app = QApplication([])
import gui  # noqa: E402
import sutura_engine.policy as policy_mod  # noqa: E402

USER = os.environ['SUTURA_POLICY_USER']


def _fresh_window():
    if os.path.exists(USER):
        os.unlink(USER)
    w = gui.MainWindow()
    d = gui.OptionsDialog(w)
    return w, d, d.method_panel


def test_panel_lists_and_no_write_on_open():
    w, d, p = _fresh_window()
    assert d.tabs.count() == 7
    assert d.TAB_METHODS == 2
    assert p.lst_auto.count() == 6, p.lst_auto.count()
    assert p.lst_manual.count() == 19, p.lst_manual.count()
    assert p.lst_gates.count() > 0
    assert not hasattr(p, 'btn_auto_up'), 'Auto order must be read-only'
    assert not os.path.exists(USER), 'opening the panel must not write policy'
    assert p.lbl_hash.text().startswith(gui._t('method_panel_hash').split('%')[0])
    assert p.lbl_source.text() == gui._t('method_panel_source_default'), p.lbl_source.text()
    assert p._checked_auto() == list(policy_mod.ORDER['deep_tier_order'])
    for tier in ('dressing', 'splint'):
        item = p._auto_items[tier]
        assert gui._t('method_panel_tier_off_suffix') in item.text(), item.text()
        assert item.toolTip() == gui._t('method_panel_tier_off_tip'), item.toolTip()
    base_flags = None
    for item in p._manual_items.values():
        assert item.flags() & gui.Qt.ItemIsUserCheckable, item.text()
        if base_flags is None:
            base_flags = item.flags()
        assert item.flags() == base_flags, (item.text(), item.flags(), base_flags)
        if item.data(gui.Qt.UserRole + 1) == 'blocked':
            assert item.toolTip(), item.text()
            item.setCheckState(gui.Qt.Checked)
            assert item.checkState() == gui.Qt.Unchecked, item.text()
    for i in range(p.lst_gates.count()):
        text = p.lst_gates.item(i).text()
        assert '...' not in text and '…' not in text, text
    print('ok  panel lists, locked gates, no write on open')


def test_empty_auto_disables_apply():
    w, d, p = _fresh_window()
    for i in range(p.lst_auto.count()):
        p.lst_auto.item(i).setCheckState(gui.Qt.Unchecked)
    assert not p.btn_apply.isEnabled()
    assert p.auto_warn.text()
    p.rb_manual.setChecked(True)
    for i in range(p.lst_manual.count()):
        p.lst_manual.item(i).setCheckState(gui.Qt.Unchecked)
    assert not p.btn_apply.isEnabled()
    print('ok  empty Auto/Manual disables Apply')


def test_auto_apply_writes_only_panel_order():
    w, d, p = _fresh_window()
    order = ['local', 'splint', 'coaptation', 'ftetwild']
    for i in range(p.lst_auto.count()):
        tier = p.lst_auto.item(i).data(gui.Qt.UserRole)
        p.lst_auto.item(i).setCheckState(
            gui.Qt.Checked if tier in order else gui.Qt.Unchecked)
    p.rb_auto.setChecked(True)
    p.apply()
    assert os.path.exists(USER), p.warn.text()
    loaded = policy_mod.load_policy()
    assert loaded['order']['deep_tier_order'] == order, loaded['order']
    text = open(USER, encoding='utf-8').read()
    assert 'Method control panel' in text
    assert '[order]' in text
    assert '\n[methods]' not in text, 'v1 must not write a methods table'
    assert 'si_escalation' not in text
    print('ok  Auto Apply writes only the panel-owned order')


def test_manual_sequence_sets_existing_tags():
    w, d, p = _fresh_window()
    path = os.path.join(_TMP, 'part.stl')
    w._add_path(path)
    p.rb_manual.setChecked(True)
    for n in (2, 5):
        p._manual_items[n].setCheckState(gui.Qt.Checked)
    p.apply()
    assert w._method_tags_by_path[path] == [('method', 2), ('method', 5)]
    assert not os.path.exists(USER), 'manual Apply must not write policy'
    print('ok  Manual Apply fills the existing per-file tags')


def test_reset_defaults_restores_order_and_checks():
    w, d, p = _fresh_window()
    for i in range(p.lst_auto.count()):
        tier = p.lst_auto.item(i).data(gui.Qt.UserRole)
        p.lst_auto.item(i).setCheckState(
            gui.Qt.Checked if tier == 'local' else gui.Qt.Unchecked)
    p.apply()
    assert os.path.exists(USER)
    p.reset_defaults()
    assert not os.path.exists(USER), 'Reset must remove the panel-owned override'
    assert p._checked_auto() == p._default_order()
    assert p.rb_auto.isChecked()
    print('ok  Reset removes the order override and restores the packaged order')


def test_cancel_writes_nothing():
    w, d, p = _fresh_window()
    d.close()
    assert not os.path.exists(USER)
    print('ok  Cancel/close writes nothing')


def test_noncanonical_error_points_to_reset():
    if os.path.exists(USER):
        os.unlink(USER)
    with open(USER, 'w', encoding='utf-8') as f:
        f.write('[order]\ndeep_tier_order = ["local", "splint", "graft"]\n')
    w = gui.MainWindow()
    d = gui.OptionsDialog(w)
    p = d.method_panel
    p.apply()
    assert p.warn.text() == gui._t('method_panel_noncanonical'), p.warn.text()
    os.unlink(USER)
    print('ok  non-canonical Apply points to Reset with a translated message')


def test_reset_recovers_unreadable_with_status():
    if os.path.exists(USER):
        os.unlink(USER)
    with open(USER, 'w', encoding='utf-8') as f:
        f.write('this is not toml [[[\n')
    w = gui.MainWindow()
    d = gui.OptionsDialog(w)
    p = d.method_panel
    p.reset_defaults()
    assert not os.path.exists(USER)
    assert os.path.exists(USER + '.bak')
    assert p.warn.text() == gui._t('method_panel_reset_recovered'), p.warn.text()
    for name in (USER, USER + '.bak'):
        if os.path.exists(name):
            os.unlink(name)
    print('ok  Reset recovers an unreadable file and reports it')


def test_keyed_write_error_is_translated():
    if os.path.exists(USER):
        os.unlink(USER)
    with open(USER, 'w', encoding='utf-8') as f:
        f.write('[[foo]]\nx = 1\n')
    w = gui.MainWindow()
    d = gui.OptionsDialog(w)
    p = d.method_panel
    p.apply()
    assert p.warn.text() == gui._t('method_panel_key_error', 'foo'), p.warn.text()
    os.unlink(USER)
    print('ok  keyed writer errors are translated and name the key')


if __name__ == '__main__':
    test_panel_lists_and_no_write_on_open()
    test_empty_auto_disables_apply()
    test_auto_apply_writes_only_panel_order()
    test_manual_sequence_sets_existing_tags()
    test_reset_defaults_restores_order_and_checks()
    test_cancel_writes_nothing()
    test_noncanonical_error_points_to_reset()
    test_reset_recovers_unreadable_with_status()
    test_keyed_write_error_is_translated()
    print('control panel GUI tests passed')
