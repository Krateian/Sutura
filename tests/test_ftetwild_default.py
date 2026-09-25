#!/usr/bin/env python3
"""Regression test for the fTetWild fallback tier defaults (FAZ17).

The tier is ON by default when the optional extra (pytetwild + pyvista,
requirements-ftetwild.txt) is installed, and then runs only when stage 1
still leaves holes or non-manifold edges. `--no-fallback-ftetwild` turns it
off; `--experimental-fallback-ftetwild` also runs it on a closed result that
still self-intersects. Without the extra the default changes nothing.

The checks that need fTetWild itself run only when the extra is installed
(it is ~1.1 GB, so CI does not install it); everything else always runs.

Needs the venv (pymeshlab). Usage:
    ~/.local/share/sutura/venv/bin/python tests/test_ftetwild_default.py
"""
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
SAMPLES = os.path.join(REPO, 'tests', 'real-world-samples')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import repair  # noqa: E402

# Stage 1 leaves this 80-face mesh open (strict watertight only with fTetWild).
OPEN_AFTER_STAGE1 = os.path.join(SAMPLES, 'thingi10k_100827.stl')
# Stage 1 closes this one, but 33 self-intersecting faces remain.
CLOSED_WITH_SI = os.path.join(SAMPLES, 'thingi10k_100045.stl')


def _repair(src, ftetwild):
    with tempfile.TemporaryDirectory(prefix='sutura-ftw-') as tmp:
        out = os.path.join(tmp, 'out.stl')
        return repair.repair_file(src, out, tmp, ftetwild=ftetwild)


def test_flag_resolution():
    assert repair.resolve_ftetwild() == 'auto'
    assert repair.resolve_ftetwild(no_fallback=True) is False
    assert repair.resolve_ftetwild(experimental=True) is True
    # the opt-out wins over the experimental flag
    assert repair.resolve_ftetwild(no_fallback=True, experimental=True) is False


def test_cli_flags_wired():
    r = subprocess.run([sys.executable, os.path.join(SUTURA, 'repair.py'), '--help'],
                       capture_output=True, text=True)
    assert r.returncode == 0
    assert '--no-fallback-ftetwild' in r.stdout
    assert '--experimental-fallback-ftetwild' in r.stdout


def test_auto_is_a_no_op_without_the_extra():
    saved = repair._FTETWILD_AVAILABLE
    repair._FTETWILD_AVAILABLE = False
    try:
        rep = _repair(OPEN_AFTER_STAGE1, 'auto')
    finally:
        repair._FTETWILD_AVAILABLE = saved
    assert rep.get('experimental_ftetwild') is False, rep.get('experimental_ftetwild')


def test_off_never_runs():
    rep = _repair(OPEN_AFTER_STAGE1, False)
    assert rep.get('experimental_ftetwild') is False


def test_auto_closes_open_result_when_installed():
    if not repair.ftetwild_available():
        print('   (skipped: fTetWild extra not installed)')
        return
    rep = _repair(OPEN_AFTER_STAGE1, 'auto')
    ft = rep.get('experimental_ftetwild')
    assert ft and ft.get('ran') and ft.get('trigger') == 'auto', ft
    assert ft.get('adopted'), ft
    assert rep['stage1']['holes_remaining'] == 0, rep['stage1']
    assert rep['stage1']['non_manifold_edges_remaining'] == 0, rep['stage1']


def test_auto_leaves_closed_si_result_alone():
    if not repair.ftetwild_available():
        print('   (skipped: fTetWild extra not installed)')
        return
    rep = _repair(CLOSED_WITH_SI, 'auto')
    assert rep.get('experimental_ftetwild') is False, rep.get('experimental_ftetwild')
    rep = _repair(CLOSED_WITH_SI, True)
    ft = rep.get('experimental_ftetwild')
    assert ft and ft.get('ran') and ft.get('trigger') == 'always', ft


def test_gui_checkboxes_map_to_cli_flags():
    """Default checked -> no flag ('auto'); unchecked -> --no-fallback-ftetwild;
    both checked -> --experimental-fallback-ftetwild. Runs in a SUBPROCESS:
    mixing pymeshlab (imported by repair in this process) with a QMainWindow
    corrupts the heap at interpreter shutdown (documented)."""
    code = (
        "import os, sys\n"
        "os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')\n"
        "sys.path.insert(0, %r)\n"
        "from PySide6.QtWidgets import QApplication\n"
        "import gui\n"
        "app = QApplication([])\n"
        "w = gui.MainWindow()\n"
        "assert w.chk_fallback_ftetwild.isChecked()\n"
        "assert not w.chk_ftetwild_si.isChecked()\n"
        "assert w.chk_ftetwild_si.isEnabled()\n"
        "assert w._ftetwild == 'auto'\n"
        "w.chk_ftetwild_si.setChecked(True)\n"
        "assert w._ftetwild is True\n"
        "w.chk_fallback_ftetwild.setChecked(False)\n"
        "assert w._ftetwild is False and not w.chk_ftetwild_si.isEnabled()\n"
        "w.chk_fallback_ftetwild.setChecked(True)\n"
        "assert w._ftetwild is True\n"
        "seen = []\n"
        "def fake_popen(args, **kw):\n"
        "    seen.append(list(args))\n"
        "    raise OSError('captured')\n"
        "gui.subprocess.Popen = fake_popen\n"
        "gui.SUTURA_CMD = ['sutura']\n"
        "for state, want in (('auto', []), (False, ['--no-fallback-ftetwild']),\n"
        "                    (True, ['--experimental-fallback-ftetwild'])):\n"
        "    wk = gui.RepairWorker(['x.stl'], ftetwild=state)\n"
        "    wk._run_one('x.stl')\n"
        "    got = [a for a in seen[-1] if 'ftetwild' in a]\n"
        "    assert got == want, (state, seen[-1])\n"
        "print('GUI-OK')\n" % SUTURA
    )
    r = subprocess.run([sys.executable, '-c', code], capture_output=True,
                       text=True, timeout=120)
    assert r.returncode == 0, r.stderr[-800:]
    assert 'GUI-OK' in r.stdout, r.stdout[-500:]


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('fTetWild default tests passed')
