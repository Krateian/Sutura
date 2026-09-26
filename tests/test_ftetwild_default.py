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


def test_auto_is_a_no_op_without_the_bridge_module():
    """A stale install can have pytetwild but no ftetwild_bridge.py; the
    default must then stay silent instead of reporting an error."""
    saved = (repair._FTETWILD_AVAILABLE, repair.FTETWILD_BRIDGE)
    repair._FTETWILD_AVAILABLE = None
    repair.FTETWILD_BRIDGE = os.path.join(SUTURA, 'no-such-bridge.py')
    try:
        assert repair.ftetwild_available() is False
        rep = _repair(OPEN_AFTER_STAGE1, 'auto')
    finally:
        repair._FTETWILD_AVAILABLE, repair.FTETWILD_BRIDGE = saved
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


def test_cli_default_leaves_no_file_in_cwd():
    """fTetWild writes __tracked_surface.stl into its working directory; the
    CLI must run it elsewhere, so a repair started from the user's folder
    (e.g. a file-manager right-click) leaves only the _fixed output there."""
    if not repair.ftetwild_available():
        print('   (skipped: fTetWild extra not installed)')
        return
    with tempfile.TemporaryDirectory(prefix='sutura-ftw-cwd-') as cwd:
        out = os.path.join(cwd, 'out_fixed.stl')
        r = subprocess.run([sys.executable, os.path.join(SUTURA, 'repair.py'),
                            OPEN_AFTER_STAGE1, '-o', out, '--no-history'],
                           cwd=cwd, capture_output=True, text=True, timeout=600)
        assert r.returncode == 0, r.stderr[-500:]
        assert '"experimental_ftetwild"' in r.stdout, r.stdout[-500:]
        assert sorted(os.listdir(cwd)) == ['out_fixed.stl'], os.listdir(cwd)


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



# Two tetrahedra sharing only vertex 0: closed, no non-manifold edge, but the
# shared vertex is pinched, so pymeshlab reports it as not two-manifold.
_BOWTIE_V = [(0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, 1),
             (-1, 0, 0), (0, -1, 0), (0, 0, -1)]
_TET = [(0, 2, 1), (0, 1, 3), (0, 3, 2), (1, 2, 3)]
_BOWTIE_T = _TET + [(0, 5, 4), (0, 4, 6), (0, 6, 5), (4, 5, 6)]
# The same two tetrahedra with the pinched vertex split: two-manifold.
_SPLIT_V = _BOWTIE_V + [(0, 0, 0)]
_SPLIT_T = _TET + [(7, 5, 4), (7, 4, 6), (7, 6, 5), (4, 5, 6)]


def _measure(v, t):
    import numpy as np
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(v, np.float64),
                        face_matrix=np.asarray(t, np.int32)))
    topo = ms.apply_filter('get_topological_measures')
    holes = repair.boundary_loop_stats(np.asarray(v, np.float64), np.asarray(t))[0]
    return ms, topo, holes, topo.get('non_two_manifold_edges', 0)


def _postprocess(v, t, fake_stage2):
    import pymeshlab as ml
    ms, topo, holes, nm = _measure(v, t)
    saved = repair.run_stage2
    if fake_stage2 is not None:
        repair.run_stage2 = fake_stage2
    try:
        with tempfile.TemporaryDirectory(prefix='sutura-bowtie-') as tmp:
            return repair._ftetwild_manifold_postprocess(
                ml, tmp, v, t, ms, topo, holes, nm)
    finally:
        repair.run_stage2 = saved


def test_bowtie_fixture_is_closed_but_not_two_manifold():
    _ms, topo, holes, nm = _measure(_BOWTIE_V, _BOWTIE_T)
    assert holes == 0 and nm == 0
    assert not topo.get('is_mesh_two_manifold')
    _ms, topo, holes, nm = _measure(_SPLIT_V, _SPLIT_T)
    assert holes == 0 and nm == 0 and topo.get('is_mesh_two_manifold')


def test_bowtie_boundary_gets_the_manifold_postprocess():
    """A closed fTetWild boundary with a pinched vertex is rebuilt by stage 2
    (here a stand-in that returns the split mesh), so the adopted result is
    two-manifold and stage 2 can run on it."""
    calls = []

    def fake_stage2(inter, out_obj):
        calls.append(inter)
        repair.write_obj(out_obj, _SPLIT_V, _SPLIT_T)
        return {'output_triangles': len(_SPLIT_T)}, True

    v, t, _ms, topo, holes, nm, done = _postprocess(_BOWTIE_V, _BOWTIE_T, fake_stage2)
    assert calls, 'the manifold post-process was not attempted'
    assert done and holes == 0 and nm == 0
    assert topo.get('is_mesh_two_manifold')
    assert len(t) == len(_SPLIT_T)


def test_two_manifold_boundary_is_left_unchanged():
    calls = []

    def fake_stage2(inter, out_obj):
        calls.append(inter)
        return {'error': 'must not be called'}, False

    v, t, _ms, _topo, _h, _nm, done = _postprocess(_SPLIT_V, _SPLIT_T, fake_stage2)
    assert not calls and not done
    assert (v, t) == (_SPLIT_V, _SPLIT_T)


def test_bowtie_with_real_stage2():
    """End to end with manifold3d when the stage-2 bridge is usable here."""
    with tempfile.TemporaryDirectory(prefix='sutura-bowtie-') as tmp:
        probe_in = os.path.join(tmp, 'probe.obj')
        repair.write_obj(probe_in, _SPLIT_V, _SPLIT_T)
        _rep, ok = repair.run_stage2(probe_in, os.path.join(tmp, 'probe_out.obj'))
    if not ok:
        print('    (stage 2 unavailable: real manifold3d check skipped)')
        return
    _v, _t, _ms, topo, holes, nm, done = _postprocess(_BOWTIE_V, _BOWTIE_T, None)
    assert done, 'manifold3d post-process not adopted'
    assert holes == 0 and nm == 0 and topo.get('is_mesh_two_manifold')


def test_bridge_default_params_and_override():
    """fTetWild runs with optimize=False unless the caller overrides it."""
    import ftetwild_bridge
    assert ftetwild_bridge.DEFAULT_PARAMS == {'optimize': False}
    assert ftetwild_bridge.tetrahedralize_params() == {'optimize': False}
    assert ftetwild_bridge.tetrahedralize_params(
        {'optimize': True, 'edge_length_fac': 0.1}) == {
            'optimize': True, 'edge_length_fac': 0.1}
    assert ftetwild_bridge.DEFAULT_PARAMS == {'optimize': False}  # not mutated


def test_run_ftetwild_passes_params_to_the_bridge():
    """run_ftetwild forwards params as a JSON argument; without params the
    bridge is called exactly as before (two arguments)."""
    import json as _json
    import types
    seen = []

    def fake_run(cmd, **kw):
        seen.append(cmd)
        return types.SimpleNamespace(returncode=0, stdout='{"ok": true}\n', stderr='')
    saved = repair.subprocess.run
    repair.subprocess.run = fake_run
    try:
        repair.run_ftetwild('/tmp/in.obj', '/tmp/out.obj')
        repair.run_ftetwild('/tmp/in.obj', '/tmp/out.obj', {'optimize': True})
    finally:
        repair.subprocess.run = saved
    assert seen[0][-2:] == ['/tmp/in.obj', '/tmp/out.obj'], seen[0]
    assert _json.loads(seen[1][-1]) == {'optimize': True}, seen[1]

if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('fTetWild default tests passed')
