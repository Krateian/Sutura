#!/usr/bin/env python3
"""Regression tests for the guarded Stage-1 self-intersection excise + re-cap
(repair.si_excise_recap, Full Mend path, SUTURA_SI_EXCISE).

Needs the venv (pymeshlab). pytest-compatible; runnable directly:
    ~/.local/share/sutura/venv/bin/python tests/test_si_excise.py
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
import trimesh  # noqa: E402

import repair  # noqa: E402


def _sphere_with_blade(subdiv=3):
    """A clean icosphere plus a closed tetrahedron piercing its north pole.

    A few faces (sphere + tetra) self-intersect, the rest are clean, so the
    excision has a well-conditioned local region to remove and re-cap."""
    m = trimesh.creation.icosphere(subdivisions=subdiv, radius=1.0)
    v = np.asarray(m.vertices, np.float64).tolist()
    t = [tuple(int(x) for x in f) for f in np.asarray(m.faces, np.int64)]
    b = len(v)
    v += [(0.0, 0.0, 1.3), (0.4, 0.0, 0.7), (0.0, 0.4, 0.7), (0.0, 0.0, 0.2)]
    t += [(b, b + 1, b + 2), (b, b + 1, b + 3),
          (b, b + 2, b + 3), (b + 1, b + 2, b + 3)]
    return (np.asarray(v, np.float64), np.asarray(t, np.int64))


def _meshset(v, t):
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(v, np.float64),
                        face_matrix=np.asarray(t, np.int32)))
    return ms


def _topo(ms):
    return ms.apply_filter('get_topological_measures')


def _si(ms):
    ms.apply_filter('compute_selection_by_self_intersections_per_face')
    n = int(ms.current_mesh().face_selection_array().sum())
    ms.apply_filter('set_selection_none')
    return n


def _arrays(ms):
    return (ms.current_mesh().vertex_matrix().copy(),
            ms.current_mesh().face_matrix().copy())


def test_clean_mesh_is_noop():
    cube = trimesh.creation.box()
    ms = _meshset(cube.vertices, cube.faces)
    v0, t0 = _arrays(ms)
    _ms, rep = repair.si_excise_recap(ms, ml)
    assert rep['ran'] is False and rep['initial_si'] == 0, rep
    assert rep['reason'] == 'no_si', rep
    v1, t1 = _arrays(ms)
    assert np.array_equal(v0, v1) and np.array_equal(t0, t1)


def test_excise_reduces_si_and_preserves_topology():
    v, t = _sphere_with_blade(3)
    ms = _meshset(v, t)
    si_before = _si(ms)
    assert si_before > 0, 'fixture must self-intersect'
    ms, rep = repair.si_excise_recap(ms, ml, time_budget=30.0)
    assert rep['applied'] and rep['rounds'] >= 1, rep
    assert rep['si_after'] == 0, rep
    assert rep['removed_si'] == rep['initial_si'], rep
    topo = _topo(ms)
    assert topo['number_holes'] == 0, topo
    assert topo['non_two_manifold_edges'] == 0, topo
    assert topo['non_two_manifold_vertices'] == 0, topo
    assert _si(ms) == 0


def test_rollback_on_failed_recap_restores_mesh():
    v, t = _sphere_with_blade(3)
    ms = _meshset(v, t)
    v0, t0 = _arrays(ms)
    # maxholesize=1 cannot close the excised loops -> the guard must reject the
    # round and restore the pre-round mesh byte-for-byte.
    ms, rep = repair.si_excise_recap(ms, ml, maxholesize=1, time_budget=30.0)
    assert rep['applied'] is False, rep
    assert rep['rounds'] == 0, rep
    assert rep['reason'] in ('rollback', 'scope'), rep
    v1, t1 = _arrays(ms)
    assert np.array_equal(v0, v1) and np.array_equal(t0, t1)


def test_budget_zero_skips():
    v, t = _sphere_with_blade(3)
    ms = _meshset(v, t)
    v0, t0 = _arrays(ms)
    _ms, rep = repair.si_excise_recap(ms, ml, time_budget=0.0)
    assert rep['ran'] is False and rep['reason'] == 'budget', rep
    v1, t1 = _arrays(ms)
    assert np.array_equal(v0, v1) and np.array_equal(t0, t1)


def _repair_cube(**kw):
    cube = trimesh.creation.box()
    v = np.asarray(cube.vertices, np.float32)
    t = np.asarray(cube.faces, np.int32)
    with tempfile.TemporaryDirectory(prefix='sutura-si-') as tmp:
        return repair.repair_mesh_from_arrays(v, t, tmp, **kw)


def test_full_path_is_noop_on_clean_mesh():
    # A 0-SI mesh must keep the pre-existing report AND the exact output: no
    # si_excise key, byte-identical geometry vs. a run with the switch off.
    rep_full, vf, tf = _repair_cube(mode='medium', deep_repair='full')
    rep_off, vo, to = _repair_cube(mode='medium', deep_repair='off')
    assert 'si_excise' not in rep_full, rep_full.keys()
    assert 'si_excise' not in rep_off, rep_off.keys()
    os.environ['SUTURA_SI_EXCISE'] = '0'
    try:
        rep_dis, vd, td = _repair_cube(mode='medium', deep_repair='full')
    finally:
        del os.environ['SUTURA_SI_EXCISE']
    assert 'si_excise' not in rep_dis, rep_dis.keys()
    assert np.array_equal(vf, vo) and np.array_equal(tf, to)
    assert np.array_equal(vf, vd) and np.array_equal(tf, td)


def test_full_path_plumbs_a_positive_result():
    """The integration must expose a positive excise report and re-measure the
    topology afterwards (stubbed helper; the real reduction is unit-tested
    above)."""
    import contextlib

    seen = {}

    def fake_recap(ms, _ml, *, maxholesize=1000, time_budget=5.0, **kw):
        seen['maxholesize'] = maxholesize
        seen['time_budget'] = time_budget
        rec = {'ran': True, 'applied': True, 'rounds': 1, 'initial_si': 7,
               'si_after': 0, 'removed_si': 7, 'reason': 'clean',
               'budget_s': time_budget, 'seconds': 0.1, 'history': []}
        return ms, rec

    saved = repair.si_excise_recap
    repair.si_excise_recap = fake_recap
    try:
        rep, _v, _t = _repair_cube(mode='medium', deep_repair='full')
    finally:
        repair.si_excise_recap = saved
    assert rep['si_excise']['applied'] is True, rep.get('si_excise')
    assert rep['si_excise']['removed_si'] == 7, rep['si_excise']
    assert seen.get('maxholesize'), seen
    assert seen.get('time_budget') == 5.0, seen  # balanced default


def test_refine_hole_flag_closes_a_hole():
    """The experimental Stage-1 refine flag must still produce a two-manifold
    result on a mesh with a hole (evaluation; kept behind the env flag)."""
    v, t = _sphere_with_blade(3)
    # drop a band of faces to open a real hole
    c = np.asarray(v)[np.asarray(t)].mean(axis=1)
    keep = ~((c[:, 2] > 0.75) & (c[:, 0] > -0.3))
    vh, th = np.asarray(v), np.asarray(t)[keep]

    def run():
        ms = _meshset(vh, th)
        from repair import apply_chain, stage1_chain
        apply_chain(ms, stage1_chain(ml, maxholesize=1000, mincomponentsize=8))
        topo = _topo(ms)
        return topo, int(ms.current_mesh().vertex_number())

    os.environ.pop('SUTURA_REFINE_HOLE', None)
    topo_off, _nv_off = run()
    os.environ['SUTURA_REFINE_HOLE'] = '1'
    try:
        topo_on, nv_on = run()
    finally:
        del os.environ['SUTURA_REFINE_HOLE']
    assert topo_off['number_holes'] == 0, topo_off
    assert topo_on['number_holes'] == 0, topo_on
    assert topo_off['non_two_manifold_edges'] == 0, topo_off
    assert topo_on['non_two_manifold_edges'] == 0, topo_on
    # refinement inserts interior vertices -> the two fills differ
    assert nv_on != _nv_off or topo_on['faces_number'] != topo_off['faces_number']


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('si-excise tests passed')
