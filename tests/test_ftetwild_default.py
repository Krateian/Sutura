#!/usr/bin/env python3
"""Regression test for the fTetWild fallback tier defaults (FAZ17).

The tier is ON by default and installed by default (requirements-ftetwild.txt
installs only pytetwild; PyVista/VTK are not used). It runs only when stage 1
still leaves holes or non-manifold edges. `--no-fallback-ftetwild` turns it
off; `--experimental-fallback-ftetwild` also runs it on a closed result that
still self-intersects. Without the extra the default changes nothing.

The checks that need fTetWild itself run only when the extra is installed;
everything else always runs.

Needs only numpy + the Rust ``sutura_geom`` extension (no pymeshlab). Usage:
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

# PySide6's own Qt6 plugins: a macOS conda install also carries qt-main/Qt5,
# whose bin/qt.conf otherwise hijacks the platform-plugin lookup. Test-side
# helper only (the sutura-gui launcher resolves it via conda run). Setting it
# here also covers the GUI subprocesses spawned below (they inherit os.environ).
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _qt_test_env import ensure_qt_plugins  # noqa: E402
ensure_qt_plugins()

import repair  # noqa: E402
import topology  # noqa: E402
import sutura_engine.pipeline.tiers.ftetwild as ftetwild_mod  # noqa: E402

# Stage 1 leaves this 80-face mesh open (strict watertight only with fTetWild).
OPEN_AFTER_STAGE1 = os.path.join(SAMPLES, 'thingi10k_100827.stl')


def _icosphere(subdivisions, radius):
    """A dependency-free icosphere (numpy only).

    trimesh is a dev-only dependency now and is absent from the installed app
    env the install-E2E job runs this suite in, so the fixture is built here
    instead. Connectivity matches ``trimesh.creation.icosphere``: an
    icosahedron whose faces are midpoint-subdivided ``subdivisions`` times,
    with shared midpoints welded and every vertex normalised to ``radius``.
    """
    import numpy as np
    phi = (1.0 + 5.0 ** 0.5) / 2.0
    verts = np.array([
        [-1, phi, 0], [1, phi, 0], [-1, -phi, 0], [1, -phi, 0],
        [0, -1, phi], [0, 1, phi], [0, -1, -phi], [0, 1, -phi],
        [phi, 0, -1], [phi, 0, 1], [-phi, 0, -1], [-phi, 0, 1],
    ], dtype=np.float64)
    faces = np.array([
        [0, 11, 5], [0, 5, 1], [0, 1, 7], [0, 7, 10], [0, 10, 11],
        [1, 5, 9], [5, 11, 4], [11, 10, 2], [10, 7, 6], [7, 1, 8],
        [3, 9, 4], [3, 4, 2], [3, 2, 6], [3, 6, 8], [3, 8, 9],
        [4, 9, 5], [2, 4, 11], [6, 2, 10], [8, 6, 7], [9, 8, 1],
    ], dtype=np.int64)
    vlist = [v / np.linalg.norm(v) for v in verts]
    cache = {}
    for _ in range(subdivisions):
        cache.clear()
        next_faces = []

        def mid(a, b):
            key = (a, b) if a < b else (b, a)
            idx = cache.get(key)
            if idx is None:
                m = vlist[a] + vlist[b]
                vlist.append(m / np.linalg.norm(m))
                idx = len(vlist) - 1
                cache[key] = idx
            return idx

        for a, b, c in faces:
            ab, bc, ca = mid(a, b), mid(b, c), mid(c, a)
            next_faces += [[a, ab, ca], [ab, b, bc], [ca, bc, c], [ab, bc, ca]]
        faces = np.array(next_faces, dtype=np.int64)
    return np.asarray(vlist, dtype=np.float64) * radius, faces


def _write_closed_with_si(path):
    """A closed, topologically manifold mesh that still self-intersects after
    stage 1: an icosphere with the north vertex pulled through to the south
    pole.  Connectivity is untouched, so stage 1 closes it (0 holes, 0
    non-manifold edges) but does NOT remove the self-intersection, which is the
    precondition for the fTetWild trigger test.

    (Previously `thingi10k_100045.stl` served this role, but the current stage-1
    chain removes its self-intersections, so it no longer exercises the
    trigger.)"""
    import numpy as np
    from sutura_engine.pipeline.mesh_io import stl_write_binary
    verts, tris = _icosphere(subdivisions=2, radius=1.0)
    verts[int(np.argmax(verts[:, 2]))] = [0.0, 0.0, -1.3]
    stl_write_binary(path, verts, tris)


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
    saved = ftetwild_mod._FTETWILD_AVAILABLE
    ftetwild_mod._FTETWILD_AVAILABLE = False
    try:
        rep = _repair(OPEN_AFTER_STAGE1, 'auto')
    finally:
        ftetwild_mod._FTETWILD_AVAILABLE = saved
    assert rep.get('experimental_ftetwild') is False, rep.get('experimental_ftetwild')


def test_auto_is_a_no_op_without_the_bridge_module():
    """A stale install can have pytetwild but no ftetwild_bridge.py; the
    default must then stay silent instead of reporting an error."""
    saved = (ftetwild_mod._FTETWILD_AVAILABLE, ftetwild_mod.FTETWILD_BRIDGE)
    ftetwild_mod._FTETWILD_AVAILABLE = None
    ftetwild_mod.FTETWILD_BRIDGE = os.path.join(SUTURA, 'no-such-bridge.py')
    try:
        assert ftetwild_mod.ftetwild_available() is False
        rep = _repair(OPEN_AFTER_STAGE1, 'auto')
    finally:
        ftetwild_mod._FTETWILD_AVAILABLE, ftetwild_mod.FTETWILD_BRIDGE = saved
    assert rep.get('experimental_ftetwild') is False, rep.get('experimental_ftetwild')


def test_off_never_runs():
    rep = _repair(OPEN_AFTER_STAGE1, False)
    assert rep.get('experimental_ftetwild') is False


def test_auto_closes_open_result_when_installed():
    if not ftetwild_mod.ftetwild_available():
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
    if not ftetwild_mod.ftetwild_available():
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
    if not ftetwild_mod.ftetwild_available():
        print('   (skipped: fTetWild extra not installed)')
        return
    with tempfile.TemporaryDirectory(prefix='sutura-si-fixture-') as d:
        src = os.path.join(d, 'closed_with_si.stl')
        _write_closed_with_si(src)
        # auto: a closed result (0 holes, 0 non-manifold) must NOT trigger the
        # tier even though it self-intersects.
        rep = _repair(src, 'auto')
        assert rep.get('experimental_ftetwild') is False, \
            rep.get('experimental_ftetwild')
        # experimental flag: it must run with trigger 'always'.
        rep = _repair(src, True)
        ft = rep.get('experimental_ftetwild')
        assert ft and ft.get('ran') and ft.get('trigger') == 'always', ft


def test_gui_checkboxes_map_to_cli_flags():
    """Default checked -> no flag ('auto'); unchecked -> --no-fallback-ftetwild;
    both checked -> --experimental-fallback-ftetwild. Runs in a SUBPROCESS so a
    QMainWindow is never created in the same process as the mesh kernel."""
    try:
        import PySide6  # noqa: F401
    except ImportError:
        print('    (PySide6 unavailable: GUI flag mapping skipped)')
        return
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
# shared vertex is pinched, so mesh_topology reports it as not two-manifold.
_BOWTIE_V = [(0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, 1),
             (-1, 0, 0), (0, -1, 0), (0, 0, -1)]
_TET = [(0, 2, 1), (0, 1, 3), (0, 3, 2), (1, 2, 3)]
_BOWTIE_T = _TET + [(0, 5, 4), (0, 4, 6), (0, 6, 5), (4, 5, 6)]
# The same two tetrahedra with the pinched vertex split: two-manifold.
_SPLIT_V = _BOWTIE_V + [(0, 0, 0)]
_SPLIT_T = _TET + [(7, 5, 4), (7, 4, 6), (7, 6, 5), (4, 5, 6)]


def _measure(v, t):
    import numpy as np
    va = np.asarray(v, np.float64)
    ta = np.asarray(t, np.int64)
    topo = topology.mesh_topology(va, ta)
    holes = repair.boundary_loop_stats(va, ta)[0]
    return None, topo, holes, topo.get('non_two_manifold_edges', 0)


def _postprocess(v, t, fake_stage2):
    _ms, topo, holes, nm = _measure(v, t)
    saved = ftetwild_mod.run_stage2
    if fake_stage2 is not None:
        ftetwild_mod.run_stage2 = fake_stage2
    try:
        with tempfile.TemporaryDirectory(prefix='sutura-bowtie-') as tmp:
            return ftetwild_mod._ftetwild_manifold_postprocess(
                tmp, v, t, topo, holes, nm)
    finally:
        ftetwild_mod.run_stage2 = saved


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

    v, t, topo, holes, nm, done = _postprocess(_BOWTIE_V, _BOWTIE_T, fake_stage2)
    assert calls, 'the manifold post-process was not attempted'
    assert done and holes == 0 and nm == 0
    assert topo.get('is_mesh_two_manifold')
    assert len(t) == len(_SPLIT_T)


def test_two_manifold_boundary_is_left_unchanged():
    calls = []

    def fake_stage2(inter, out_obj):
        calls.append(inter)
        return {'error': 'must not be called'}, False

    v, t, _topo, _h, _nm, done = _postprocess(_SPLIT_V, _SPLIT_T, fake_stage2)
    assert not calls and not done
    assert (v, t) == (_SPLIT_V, _SPLIT_T)


def test_bowtie_with_real_stage2():
    """End to end with Manifold when the stage-2 Rust core is usable here."""
    with tempfile.TemporaryDirectory(prefix='sutura-bowtie-') as tmp:
        probe_in = os.path.join(tmp, 'probe.obj')
        repair.write_obj(probe_in, _SPLIT_V, _SPLIT_T)
        _rep, ok = repair.run_stage2(probe_in, os.path.join(tmp, 'probe_out.obj'))
    if not ok:
        print('    (stage 2 unavailable: real Manifold check skipped)')
        return
    _v, _t, topo, holes, nm, done = _postprocess(_BOWTIE_V, _BOWTIE_T, None)
    assert done, 'Manifold post-process not adopted'
    assert holes == 0 and nm == 0 and topo.get('is_mesh_two_manifold')


def test_bridge_imports_pytetwild_without_pyvista():
    """The tier must not need PyVista/VTK: pytetwild's optional PyVista accessor
    is stubbed, so importing the tier still works when both are absent."""
    import builtins
    import ftetwild_bridge

    real_import = builtins.__import__

    def blocked_import(name, *args, **kwargs):
        if (name == 'pyvista' or name.startswith('pyvista.')
                or name == 'vtk' or name.startswith('vtk.')):
            raise ModuleNotFoundError("No module named 'pyvista'", name='pyvista')
        return real_import(name, *args, **kwargs)

    saved = {m: sys.modules.pop(m, None)
             for m in ('pytetwild', 'pytetwild._accessor', 'pytetwild.pytetwild')}
    builtins.__import__ = blocked_import
    try:
        mod = ftetwild_bridge._import_pytetwild()
        assert mod is not None and hasattr(mod, 'tetrahedralize')
    finally:
        builtins.__import__ = real_import
        for m in ('pytetwild', 'pytetwild._accessor', 'pytetwild.pytetwild'):
            sys.modules.pop(m, None)
        for m, v in saved.items():
            if v is not None:
                sys.modules[m] = v


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


def test_ftetwild_optimize_resolution():
    """CLI > SUTURA_FTETWILD_OPTIMIZE > config ftetwild_optimize > off."""
    import json as _json
    with tempfile.TemporaryDirectory() as td:
        cfg = os.path.join(td, 'config.json')
        assert repair.resolve_ftetwild_optimize(None, {}, cfg) is False
        with open(cfg, 'w') as f:
            _json.dump({'ftetwild_optimize': True}, f)
        assert repair.resolve_ftetwild_optimize(None, {}, cfg) is True
        env = {repair.FTETWILD_OPTIMIZE_ENV: '0'}
        assert repair.resolve_ftetwild_optimize(None, env, cfg) is False
        assert repair.resolve_ftetwild_optimize(True, env, cfg) is True
    old = os.environ.pop(repair.FTETWILD_OPTIMIZE_ENV, None)
    try:
        os.environ[repair.FTETWILD_OPTIMIZE_ENV] = '1'
        assert repair.ftetwild_params() == {'optimize': True}
        os.environ[repair.FTETWILD_OPTIMIZE_ENV] = '0'
        assert repair.ftetwild_params() is None
    finally:
        os.environ.pop(repair.FTETWILD_OPTIMIZE_ENV, None)
        if old is not None:
            os.environ[repair.FTETWILD_OPTIMIZE_ENV] = old


def test_gui_ftetwild_optimize_flag():
    """The Repair-tab checkbox maps to --ftetwild-optimize (off by default)
    and is disabled with the fTetWild fallback. Subprocess, as above."""
    try:
        import PySide6  # noqa: F401
    except ImportError:
        print('    (PySide6 unavailable: GUI flag mapping skipped)')
        return
    code = (
        "import os, sys\n"
        "os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')\n"
        "sys.path.insert(0, %r)\n"
        "from PySide6.QtWidgets import QApplication\n"
        "import gui\n"
        "app = QApplication([])\n"
        "w = gui.MainWindow()\n"
        "assert not w.chk_ftetwild_optimize.isChecked()\n"
        "w.chk_fallback_ftetwild.setChecked(False)\n"
        "assert not w.chk_ftetwild_optimize.isEnabled()\n"
        "w.chk_fallback_ftetwild.setChecked(True)\n"
        "assert w.chk_ftetwild_optimize.isEnabled()\n"
        "seen = []\n"
        "def fake_popen(args, **kw):\n"
        "    seen.append(list(args))\n"
        "    raise OSError('captured')\n"
        "gui.subprocess.Popen = fake_popen\n"
        "gui.SUTURA_CMD = ['sutura']\n"
        "for on in (False, True):\n"
        "    wk = gui.RepairWorker(['x.stl'], ftetwild_optimize=on)\n"
        "    wk._run_one('x.stl')\n"
        "    assert ('--ftetwild-optimize' in seen[-1]) == on, seen[-1]\n"
        "w.close()\n"
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
