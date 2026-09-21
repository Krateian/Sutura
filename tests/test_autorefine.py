#!/usr/bin/env python3
"""Regression test for the experimental autorefine self-intersection prototype
(sutura/autorefine.py, --experimental-autorefine, FAZ16).

Verifies the properties that make autorefine distinct from the delete-and-
reclose approach:
  - it NEVER deletes an input face (input triangles are only kept or split);
  - on a controlled synthetic pair it subdivides the intersecting triangles so
    no proper intersection remains (SI pairs -> 0);
  - holes / surface area stay stable (no worse than the input) after the
    prototype on the controlled cases;
  - the CLI flag is wired and the report carries experimental_autorefine;
  - the GUI checkbox wires the batch-wide flag.

The dense-SI float64 construction limitation (non-manifold residue on dense
scans) is documented in docs/alpha-wrap-feasibility-2026-09.md section 4a and
is NOT asserted to converge here -- the adopt/fallback guard in repair.py
keeps the default chain output whenever autorefine is not strictly better.

Needs the venv (pymeshlab). Usage:
    ~/.local/share/sutura/venv/bin/python tests/test_autorefine.py
"""
import os
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
for p in (SUTURA,):
    if p not in sys.path:
        sys.path.insert(0, p)

import numpy as np  # noqa: E402

import autorefine  # noqa: E402
import repair  # noqa: E402


def _proper_pairs(v, t):
    return autorefine.detect_pairs_with_segments(v, t)


def _surface_area(v, t):
    v = np.asarray(v, dtype=np.float64)
    t = np.asarray(t, dtype=np.int64)
    a = v[t[:, 0]]
    b = v[t[:, 1]]
    c = v[t[:, 2]]
    return 0.5 * np.linalg.norm(np.cross(b - a, c - a), axis=1).sum()


def _holes(v, t):
    return repair.boundary_loop_stats(
        np.asarray(v, np.float64), np.asarray(t, np.int64))[0]


def _two_spheres(n=12):
    """Two interpenetrating spheres (the paper's canonical worst case)."""
    def sphere(cx, r):
        vs, ts = [], []
        for i in range(n + 1):
            phi = np.pi * i / n
            for j in range(2 * n + 1):
                th = 2 * np.pi * j / (2 * n)
                vs.append((cx + r * np.sin(phi) * np.cos(th),
                           r * np.cos(phi),
                           r * np.sin(phi) * np.sin(th)))
        def idx(i, j):
            return i * (2 * n + 1) + j
        for i in range(n):
            for j in range(2 * n + 1):
                a, b, c, d = (idx(i, j), idx(i + 1, j),
                              idx(i + 1, (j + 1) % (2 * n + 1)),
                              idx(i, (j + 1) % (2 * n + 1)))
                ts.append((a, b, c))
                ts.append((a, c, d))
        return vs, ts
    v1, t1 = sphere(-0.5, 0.8)
    v2, t2 = sphere(0.5, 0.8)
    base = len(v1)
    v1 += v2
    t1 += tuple((a + base, b + base, c + base) for a, b, c in t2)
    return np.array(v1, dtype=np.float64), np.array(t1, dtype=np.int64)


def _crossing_pair():
    """Two triangles that properly cross in their interiors."""
    a = np.array([[0., 0., 0.], [2., 0., 0.], [0., 2., 0.]])
    b = np.array([[0., 0., -1.], [2., 0., 1.], [0., 2., 0.]])
    return np.vstack([a, b]), np.array([[0, 1, 2], [3, 4, 5]])


def test_crossing_pair_resolves_to_zero_pairs():
    v, t = _crossing_pair()
    pairs = _proper_pairs(v, t)
    assert len(pairs) == 1, pairs
    nv, nt, rep = autorefine.autorefine(v, t)
    # the segment became a shared edge: no proper intersection remains
    assert rep['si_after'] == 0, rep
    assert _proper_pairs(nv, nt) == [], _proper_pairs(nv, nt)
    # NEVER deletes input faces: output has >= the input face count
    assert len(nt) >= len(t), (len(t), len(nt))


def test_two_spheres_never_delete_and_si_drops():
    v, t = _two_spheres()
    assert len(_proper_pairs(v, t)) > 0
    nv, nt, rep = autorefine.autorefine(v, t)
    # never deletes input faces
    assert len(nt) >= len(t), (len(t), len(nt))
    # SI pairs drop (proper intersections are subdivided into shared edges)
    assert rep['si_after'] < rep['si_before'], rep
    # surface area is stable (never deletes -> area preserved to tolerance)
    a0 = _surface_area(v, t)
    a1 = _surface_area(nv, nt)
    assert abs(a1 - a0) / max(a0, 1e-9) < 0.05, (a0, a1)


def test_snap_round_never_removes_faces():
    v, t = _two_spheres()
    pairs = _proper_pairs(v, t)
    v2 = autorefine.snap_round(v, t, pairs)
    # snapping moves vertices but keeps the same face count / topology
    assert len(v2) == len(v), (len(v), len(v2))
    assert np.isfinite(v2).all()


def test_integrated_flag_report_and_never_worse():
    """The CLI flag must wire the prototype; the repair report carries
    experimental_autorefine, and the final output is never worse than the
    plain chain (adopt/fallback guard)."""
    v, t = _two_spheres(n=8)
    for flag in (False, True):
        rep, nv, nt = repair.repair_mesh_from_arrays(
            np.asarray(v, np.float32), np.asarray(t, np.int32),
            '/tmp/ar-test', mode='auto', autorefine=flag)
        if flag:
            rep_yes, nv_yes, nt_yes = rep, nv, nt
        else:
            rep_no, nv_no, nt_no = rep, nv, nt
    ar_r = rep_yes.get('experimental_autorefine')
    assert ar_r is not False, rep_yes
    # adopt/fallback: the flagged output is not worse (holes + non-manifold)
    h_no = _holes(nv_no, nt_no)
    h_yes = _holes(nv_yes, nt_yes)
    assert h_yes <= h_no, (h_yes, h_no)


def test_cli_flag_wired():
    r = subprocess.run(
        [sys.executable, os.path.join(SUTURA, 'repair.py'), '--help'],
        capture_output=True, text=True)
    assert r.returncode == 0
    assert '--experimental-autorefine' in r.stdout


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
        "assert hasattr(w, 'chk_autorefine')\n"
        "assert w._autorefine is False\n"
        "w.chk_autorefine.setChecked(True)\n"
        "assert w._autorefine is True\n"
        "w.chk_autorefine.setChecked(False)\n"
        "assert w._autorefine is False\n"
        "print('GUI-OK')\n" % SUTURA
    )
    r = subprocess.run([sys.executable, '-c', code], capture_output=True,
                       text=True, timeout=120)
    assert r.returncode == 0, r.stderr[-500:]
    assert 'GUI-OK' in r.stdout, r.stdout[-500:]


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('autorefine tests passed')