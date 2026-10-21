#!/usr/bin/env python3
"""Regression tests for the guarded Stage-1 self-intersection excise + re-cap
(pipeline_mod.si_excise_recap, Full Mend path, SUTURA_SI_EXCISE).

Needs only numpy + the Rust ``sutura_geom`` extension (no pymeshlab). pytest-compatible;
runnable directly:
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

import repair  # noqa: E402
import topology  # noqa: E402
import sutura_engine.pipeline.pipeline as pipeline_mod  # noqa: E402
from sutura_engine.pipeline.mesh_io import load_mesh  # noqa: E402

CUBE_V = np.asarray([(x, y, z) for x in (0, 1) for y in (0, 1) for z in (0, 1)],
                    np.float64)
CUBE_T = np.asarray([(0, 1, 3), (0, 3, 2), (4, 6, 7), (4, 7, 5), (0, 4, 5),
                     (0, 5, 1), (2, 3, 7), (2, 7, 6), (0, 2, 6), (0, 6, 4),
                     (1, 5, 7), (1, 7, 3)], np.int64)


def _icosphere(subdiv=3):
    """Dependency-free icosphere (numpy only); connectivity matches the former
    ``trimesh.creation.icosphere``/``pymeshlab.create_sphere`` fixture."""
    t = (1.0 + 5.0 ** 0.5) / 2.0
    raw = [(-1, t, 0), (1, t, 0), (-1, -t, 0), (1, -t, 0),
           (0, -1, t), (0, 1, t), (0, -1, -t), (0, 1, -t),
           (t, 0, -1), (t, 0, 1), (-t, 0, -1), (-t, 0, 1)]
    verts = []
    for a, b, c in raw:
        n = (a * a + b * b + c * c) ** 0.5
        verts.append([a / n, b / n, c / n])
    verts = np.asarray(verts, dtype=np.float64)
    faces = np.asarray([
        [0, 11, 5], [0, 5, 1], [0, 1, 7], [0, 7, 10], [0, 10, 11],
        [1, 5, 9], [5, 11, 4], [11, 10, 2], [10, 7, 6], [7, 1, 8],
        [3, 9, 4], [3, 4, 2], [3, 2, 6], [3, 6, 8], [3, 8, 9],
        [4, 9, 5], [2, 4, 11], [6, 2, 10], [8, 6, 7], [9, 8, 1],
    ], dtype=np.int64)
    for _ in range(subdiv):
        cache = {}
        verts_list = [v for v in verts]

        def mid(a, b):
            key = (a, b) if a < b else (b, a)
            if key not in cache:
                p = verts[a] + verts[b]
                idx = len(verts_list)
                verts_list.append(p / np.linalg.norm(p))
                cache[key] = idx
            return cache[key]

        out = []
        for a, b, c in faces:
            ab, bc, ca = mid(a, b), mid(b, c), mid(c, a)
            out += [[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]]
        verts = np.asarray(verts_list, dtype=np.float64)
        faces = np.asarray(out, dtype=np.int64)
    return verts, faces


def _sphere_with_blade(subdiv=3):
    """A clean icosphere plus a closed tetrahedron piercing its north pole.

    A few faces (sphere + tetra) self-intersect, the rest are clean, so the
    excision has a well-conditioned local region to remove and re-cap."""
    m_v, m_t = _icosphere(subdiv)
    v = np.asarray(m_v, np.float64).tolist()
    t = [tuple(int(x) for x in f) for f in np.asarray(m_t, np.int64)]
    b = len(v)
    v += [(0.0, 0.0, 1.3), (0.4, 0.0, 0.7), (0.0, 0.4, 0.7), (0.0, 0.0, 0.2)]
    t += [(b, b + 1, b + 2), (b, b + 1, b + 3),
          (b, b + 2, b + 3), (b + 1, b + 2, b + 3)]
    return (np.asarray(v, np.float64), np.asarray(t, np.int64))


def _si(v, t):
    return int(topology.self_intersecting_faces(v, t)[1])


def test_clean_mesh_is_noop():
    v0 = CUBE_V.copy()
    t0 = CUBE_T.copy()
    v1, t1, rep = pipeline_mod.si_excise_recap(v0, t0)
    assert rep['ran'] is False and rep['initial_si'] == 0, rep
    assert rep['reason'] == 'no_si', rep
    assert np.array_equal(v0, v1) and np.array_equal(t0, t1)


def test_excise_reduces_si_and_preserves_topology():
    v, t = _sphere_with_blade(3)
    si_before = _si(v, t)
    assert si_before > 0, 'fixture must self-intersect'
    v, t, rep = pipeline_mod.si_excise_recap(v, t, time_budget=30.0)
    assert rep['applied'] and rep['rounds'] >= 1, rep
    assert rep['si_after'] == 0, rep
    assert rep['removed_si'] == rep['initial_si'], rep
    topo = topology.mesh_topology(v, t)
    assert topo['number_holes'] == 0, topo
    assert topo['non_two_manifold_edges'] == 0, topo
    assert topo['non_two_manifold_vertices'] == 0, topo
    assert topology.self_intersecting_faces(v, t)[1] == 0


def test_rollback_on_failed_recap_restores_mesh():
    v, t = _sphere_with_blade(3)
    v0, t0 = np.asarray(v, np.float64), np.asarray(t, np.int64)
    # maxholesize=1 cannot close the excised loops -> the guard must reject the
    # round and restore the pre-round mesh byte-for-byte.
    v1, t1, rep = pipeline_mod.si_excise_recap(v0, t0, maxholesize=1, time_budget=30.0)
    assert rep['applied'] is False, rep
    assert rep['rounds'] == 0, rep
    assert rep['reason'] in ('rollback', 'scope'), rep
    assert np.array_equal(v0, v1) and np.array_equal(t0, t1)


def test_round_that_splits_the_mesh_is_rejected():
    """A committed round must not increase the connected-component count.

    thingi10k_63785: the one-ring excise closed the mesh and dropped SI 81 -> 0
    but left TWO shells (the pre-round mesh is a single component), so every
    round must be rejected and the excise must stay unapplied with the input's
    component count preserved."""
    sample = os.path.join(REPO, 'tests', 'real-world-samples',
                          'thingi10k_63785.stl')
    if not os.path.exists(sample):
        print('    (corpus mesh missing: skipped)')
        return
    m = load_mesh(sample)
    v, t = np.asarray(m[0], np.float64), np.asarray(m[1], np.int64)
    comp_before = topology.mesh_topology(v, t)['connected_components_number']
    assert comp_before == 1, comp_before
    v, t, rep = pipeline_mod.si_excise_recap(v, t, time_budget=30.0)
    assert rep['applied'] is False, rep
    assert topology.mesh_topology(v, t)['connected_components_number'] == comp_before


def test_budget_zero_skips():
    v, t = _sphere_with_blade(3)
    v0, t0 = np.asarray(v, np.float64), np.asarray(t, np.int64)
    v1, t1, rep = pipeline_mod.si_excise_recap(v0, t0, time_budget=0.0)
    assert rep['ran'] is False and rep['reason'] == 'budget', rep
    assert np.array_equal(v0, v1) and np.array_equal(t0, t1)


def _repair_cube(**kw):
    v = CUBE_V.astype(np.float32)
    t = CUBE_T.astype(np.int32)
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

    def fake_recap(v, t, *, maxholesize=1000, time_budget=5.0, **kw):
        seen['maxholesize'] = maxholesize
        seen['time_budget'] = time_budget
        rec = {'ran': True, 'applied': True, 'rounds': 1, 'initial_si': 7,
               'si_after': 0, 'removed_si': 7, 'reason': 'clean',
               'budget_s': time_budget, 'seconds': 0.1, 'history': []}
        return v, t, rec

    saved = pipeline_mod.si_excise_recap
    pipeline_mod.si_excise_recap = fake_recap
    try:
        rep, _v, _t = _repair_cube(mode='medium', deep_repair='full')
    finally:
        pipeline_mod.si_excise_recap = saved
    assert rep['si_excise']['applied'] is True, rep.get('si_excise')
    assert rep['si_excise']['removed_si'] == 7, rep['si_excise']
    assert seen.get('maxholesize'), seen
    assert seen.get('time_budget') == 5.0, seen  # balanced default


def test_close_holes_step_ignores_removed_refine_env():
    """Phase 4 W3d removed SUTURA_REFINE_HOLE: the plain native closer is always
    used (no ``refinehole`` param) and must still close a real hole."""
    from repair import _close_holes_step, apply_chain, stage1_chain

    os.environ.pop('SUTURA_REFINE_HOLE', None)
    name, params = _close_holes_step(1000)
    assert name == 'meshing_close_holes', name
    assert 'refinehole' not in params, params

    os.environ['SUTURA_REFINE_HOLE'] = '1'
    try:
        _name2, params2 = _close_holes_step(1000)
    finally:
        del os.environ['SUTURA_REFINE_HOLE']
    assert params2 == params, params2

    v, t = _sphere_with_blade(3)
    # drop a band of faces to open a real hole
    c = np.asarray(v)[np.asarray(t)].mean(axis=1)
    keep = ~((c[:, 2] > 0.75) & (c[:, 0] > -0.3))
    vh, th = np.asarray(v), np.asarray(t)[keep]

    v, t, _applied, _skipped = apply_chain(
        vh, th, stage1_chain(maxholesize=1000, mincomponentsize=8))
    topo = topology.mesh_topology(v, t)
    assert topo['number_holes'] == 0, topo
    assert topo['non_two_manifold_edges'] == 0, topo


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('si-excise tests passed')
