#!/usr/bin/env python3
"""Reload non-manifold-vertex regression (P-WELD / P-HONEST).

Covers the STL save/reload pinch that the independent gate counts as
``non_two_manifold_vertices``:

  * synthetic two closed tetrahedra that share one vertex position in float32
    (distinct indices in memory) -> clean in memory, pinched after the weld;
  * a stage-1-only report is downgraded and classified as a warning;
  * OBJ / 3MF outputs do not go through the STL P-WELD nudge.

Usage:
    <repair-venv>/bin/python tests/test_reload_n2m.py
"""
import os
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, 'sutura'))

import numpy as np  # noqa: E402

import classification  # noqa: E402
import repair  # noqa: E402
import topology  # noqa: E402
from sutura_engine.pipeline.mesh_io import save_mesh  # noqa: E402
from sutura_engine.pipeline.p_weld import (  # noqa: E402
    p_weld_final, reload_strict_holes_nm, reload_topology,
)


def _tetra_faces():
    return np.array([[0, 1, 2], [0, 3, 1], [0, 2, 3], [1, 3, 2]],
                    dtype=np.int32)


def _two_tetrahedra_shared_vertex():
    """Two closed tetrahedra that share exactly one float32 vertex position.

    In memory they are two disjoint components with distinct vertices at the
    origin. STL has no vertex sharing: the loader welds the two origin
    positions into one vertex with two triangle fans, i.e. a pinched vertex
    (0 holes, 0 non-manifold edges, 1 non-two-manifold vertex)."""
    va = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1]], dtype=np.float64)
    vb = np.array([[0, 0, 0], [-1, 0, 0], [0, -1, 0], [0, 0, -1]], dtype=np.float64)
    ta = _tetra_faces()
    tb = ta + 4
    return np.vstack([va, vb]), np.vstack([ta, tb])


def _cube():
    v = np.array([[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
                  [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]], dtype=np.float64)
    t = np.array([[0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
                  [0, 1, 5], [0, 5, 4], [3, 7, 6], [3, 6, 2],
                  [0, 4, 7], [0, 7, 3], [1, 2, 6], [1, 6, 5]], dtype=np.int32)
    return v, t


def test_synthetic_save_reload_pinch():
    v, t = _two_tetrahedra_shared_vertex()
    # In memory the two components are independently two-manifold.
    topo = topology.mesh_topology(v, t)
    assert topo.get('non_two_manifold_vertices') == 0, topo
    assert reload_strict_holes_nm(v, t) == (0, 0)
    # After the float32 save/reload weld the two origin vertices collapse.
    holes, nm, n2m = reload_topology(v, t)
    assert (holes, nm, n2m) == (0, 0, 1), (holes, nm, n2m)
    nv, nt, rec = p_weld_final(v, t)
    assert rec is not None and rec['applied'], rec
    assert rec['method'] == 'split-collisions', rec
    assert rec['non_two_manifold_before'] == 1, rec
    assert rec['non_two_manifold_after'] == 0, rec
    assert reload_topology(nv, nt) == (0, 0, 0), reload_topology(nv, nt)
    assert len(nt) == len(t), (len(nt), len(t))
    assert np.abs(nv.astype(np.float64) - v.astype(np.float64)).max() < 1e-5
    print('ok  synthetic save/reload pinch')


def test_stage1_only_verdict_is_honest():
    v, t = _two_tetrahedra_shared_vertex()
    report = {'stage1': {'two_manifold': True, 'holes_remaining': 0}}
    assert repair.enforce_reload_verdict(report, v, t) is True, report
    s1 = report['stage1']
    assert s1['reload_non_manifold_vertices'] == 1, report
    assert s1['two_manifold'] is False, report
    assert report['reload_watertight'] is False, report
    category, issues, _key = classification.classify(report)
    assert category == 'warning', (category, report)
    assert 'reload_non_manifold_vertices' in issues, issues
    print('ok  stage-1-only n2m verdict')


def test_obj_and_3mf_do_not_use_stl_p_weld():
    v, t = _cube()
    tmp = tempfile.mkdtemp(prefix='sutura-n2m-')
    for ext in ('.obj', '.3mf'):
        src = os.path.join(tmp, 'cube' + ext)
        out = os.path.join(tmp, 'cube_fixed' + ext)
        save_mesh(src, v, t)
        report, _cat = repair.process_file(
            src, False, out=out, no_history=True, deep_repair='off',
            ftetwild=False, graft=False, flap=False, tamponade=False,
            coaptation=False, dressing=False, splint=False)
        assert os.path.exists(out), out
        assert 'p_weld' not in report, (ext, report.get('p_weld'))
        for obj in (report.get('object_reports') or []):
            assert 'p_weld' not in obj, (ext, obj.get('p_weld'))
    print('ok  OBJ / 3MF do not use STL P-WELD')


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
    print('reload-n2m tests passed')
