#!/usr/bin/env python3
"""Regression test for the experimental join-components prototype
(repair.join_small_components, --experimental-join-components, FAZ14).

A small separate component must be MOVED onto the nearest larger component
(not deleted) when the flag is on, and DELETED by the default chain. Also
checks the GUI checkbox wires the batch-wide flag.

Needs the venv (pymeshlab). Usage:
    ~/.local/share/sutura/venv/bin/python tests/test_join_components.py
"""
import os
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
for p in (SUTURA,):
    if p not in sys.path:
        sys.path.insert(0, p)

import numpy as np  # noqa: E402

import repair  # noqa: E402


def _cube_with_debris():
    """A closed unit cube (12 faces) + a far-away open 2-triangle debris."""
    v = np.array([[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
                  [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]], dtype=np.float64)
    def sq(a, b, c, d):
        return [[a, b, c], [a, c, d]]
    t = np.array(sq(0, 3, 2, 1) + sq(4, 5, 6, 7) + sq(0, 4, 7, 3) +
                 sq(1, 2, 6, 5) + sq(0, 1, 5, 4) + sq(3, 7, 6, 2),
                 dtype=np.int64)
    dv = np.array([[10, 10, 10], [11, 10, 10], [10, 11, 10], [11, 11, 10]],
                  dtype=np.float64)
    dt = np.array([[0, 1, 2], [1, 3, 2]], dtype=np.int64)
    v = np.vstack([v, dv])
    t = np.vstack([t, dt + 8])
    return v, t


def _run(join_components):
    v, t = _cube_with_debris()
    with tempfile.TemporaryDirectory(prefix='sutura-join-') as tmp:
        rep, nv, nt = repair.repair_mesh_from_arrays(
            v, t, tmp, mode='medium', join_components=join_components)
        return rep, nv, nt


def test_default_deletes_small_component():
    rep, _nv, nt = _run(join_components=False)
    assert rep.get('experimental_join_components') is False, rep
    assert len(nt) == 12, len(nt)  # only the cube remains (debris deleted)


def test_join_moves_small_component():
    rep, nv, nt = _run(join_components=True)
    jc = rep.get('experimental_join_components')
    assert jc and jc.get('moved') >= 1, rep
    assert jc.get('remaining_small', 0) == 0, jc
    # the debris was MOVED, not deleted: output has more faces than the cube
    assert len(nt) > 12, len(nt)
    # and the moved debris now sits AT the cube (was at ~(10,10,10)): it is
    # no longer far away, it was moved, not left in place or deleted.
    lo = nv.min(axis=0)
    hi = nv.max(axis=0)
    assert hi[0] <= 2.5 and hi[1] <= 2.5 and hi[2] <= 1.5, (lo, hi)


def test_gui_checkbox_wires_flag():
    """The GUI checkbox must exist and flip the batch-wide flag. Runs in a
    SUBPROCESS: mixing pymeshlab (imported by repair in this process) with a
    QMainWindow corrupts the heap at interpreter shutdown (documented)."""
    code = (
        "import os, sys\n"
        "os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')\n"
        "sys.path.insert(0, %r)\n"
        "from PySide6.QtWidgets import QApplication\n"
        "import gui\n"
        "app = QApplication([])\n"
        "w = gui.MainWindow()\n"
        "assert hasattr(w, 'chk_join_components')\n"
        "assert w._join_components is False\n"
        "w.chk_join_components.setChecked(True)\n"
        "assert w._join_components is True\n"
        "w.chk_join_components.setChecked(False)\n"
        "assert w._join_components is False\n"
        "print('GUI-OK')\n" % SUTURA
    )
    import subprocess
    r = subprocess.run([sys.executable, '-c', code], capture_output=True,
                       text=True, timeout=120)
    assert r.returncode == 0, r.stderr[-500:]
    assert 'GUI-OK' in r.stdout, r.stdout[-500:]


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('join-components tests passed')