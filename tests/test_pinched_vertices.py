#!/usr/bin/env python3
"""Regression test for pinched ("bowtie") vertices on a closed stage-1 result.

A mesh can come out of the stage-1 chain closed, with no non-manifold edge,
and still not be two-manifold because two parts of the surface meet in a
single vertex. Stage 2 only runs on a two-manifold result, so such a mesh was
reported as a warning although it is strictly watertight
(thingi10k_145065: 18 pinched vertices). `repair._split_pinched_vertices`
repeats `meshing_repair_non_manifold_vertices` until the mesh is two-manifold
and keeps the result only when it still has no hole and no non-manifold edge.

Needs the venv (pymeshlab). Usage:
    ~/.local/share/sutura/venv/bin/python tests/test_pinched_vertices.py
"""
import os
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402
import pymeshlab as ml  # noqa: E402

import classification  # noqa: E402
import repair  # noqa: E402

# Two tetrahedra sharing only vertex 0: closed, no non-manifold edge, pinched.
_TET = [(0, 2, 1), (0, 1, 3), (0, 3, 2), (1, 2, 3)]
BOWTIE_V = [(0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, 1),
            (-1, 0, 0), (0, -1, 0), (0, 0, -1)]
BOWTIE_T = _TET + [(0, 5, 4), (0, 4, 6), (0, 6, 5), (4, 5, 6)]
CUBE_V = [(x, y, z) for x in (0, 1) for y in (0, 1) for z in (0, 1)]
CUBE_T = [(0, 1, 3), (0, 3, 2), (4, 6, 7), (4, 7, 5), (0, 4, 5), (0, 5, 1),
          (2, 3, 7), (2, 7, 6), (0, 2, 6), (0, 6, 4), (1, 5, 7), (1, 7, 3)]
SAMPLE = os.path.join(REPO, 'tests', 'real-world-samples', 'thingi10k_145065.stl')


def _meshset(v, t):
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(v, np.float64),
                        face_matrix=np.asarray(t, np.int32)))
    return ms, ms.apply_filter('get_topological_measures')


def test_bowtie_is_split_into_a_two_manifold_mesh():
    ms, topo = _meshset(BOWTIE_V, BOWTIE_T)
    assert not topo.get('is_mesh_two_manifold')
    assert topo.get('non_two_manifold_edges', 0) == 0
    assert topo.get('non_two_manifold_vertices', 0) > 0
    out_ms, out_topo, rep = repair._split_pinched_vertices(ml, ms, topo)
    assert rep and rep['adopted'], rep
    assert rep['before'] > 0 and rep['after'] == 0 and 1 <= rep['passes'] <= 5
    assert out_topo.get('is_mesh_two_manifold')
    m = out_ms.current_mesh()
    assert len(m.face_matrix()) == len(BOWTIE_T)
    assert repair.boundary_loop_stats(m.vertex_matrix(), m.face_matrix())[0] == 0
    # geometry unchanged: the split only duplicates vertex 0
    assert len(m.vertex_matrix()) == len(BOWTIE_V) + 1


def test_clean_cube_is_left_alone():
    ms, topo = _meshset(CUBE_V, CUBE_T)
    assert topo.get('is_mesh_two_manifold')
    out_ms, out_topo, rep = repair._split_pinched_vertices(ml, ms, topo)
    assert rep is False
    assert out_ms is ms and out_topo is topo


def test_open_mesh_is_left_alone():
    ms, topo = _meshset(CUBE_V, CUBE_T[:-1])     # one missing face -> a hole
    out_ms, _topo, rep = repair._split_pinched_vertices(ml, ms, topo)
    assert rep is False and out_ms is ms


def test_real_world_pinched_sample():
    with tempfile.TemporaryDirectory(prefix='sutura-pinch-') as tmp:
        rep = repair.repair_file(SAMPLE, os.path.join(tmp, 'out.stl'), tmp,
                                 ftetwild=False)
    pv = rep.get('pinched_vertices_split')
    assert pv and pv['adopted'] and pv['after'] == 0, pv
    s1 = rep['stage1']
    assert s1['two_manifold'] and s1['holes_remaining'] == 0
    assert s1['non_manifold_edges_remaining'] == 0
    category, _issues, _key = classification.classify(rep)
    s2 = rep.get('stage2')
    if s2 is not None and 'error' not in s2:
        assert category == 'watertight', category
    else:
        print('    (stage 2 unavailable: category %s, watertight check skipped)'
              % category)


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('pinched-vertex tests passed')
