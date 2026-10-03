#!/usr/bin/env python3
"""Regression tests for the Flap surface-based hole filler
(sutura_engine.flap) and its Stage-1 wiring (repair.resolve_flap /
repair._flap_step, SUTURA_FLAP).

Needs the venv (pymeshlab), like the other Stage-1 tests. pytest-compatible;
runnable directly:
    ~/.local/share/sutura/venv/bin/python tests/test_flap.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
for p in (SUTURA, REPO):
    if p not in sys.path:
        sys.path.insert(0, p)

import numpy as np  # noqa: E402
import pymeshlab as ml  # noqa: E402
import trimesh  # noqa: E402

import repair  # noqa: E402
from sutura_engine import flap  # noqa: E402
from defects import detect  # noqa: E402


def _holed_sphere(subdiv=4, radius=5.0, cut=4.0):
    m = trimesh.creation.icosphere(subdivisions=subdiv, radius=radius)
    v = np.asarray(m.vertices, np.float64)
    t = np.asarray(m.faces, np.int64)
    c = v[t].mean(axis=1)
    keep = c[:, 2] < cut
    return v, t[keep]


def _face_geom_keys(v, t):
    """Canonical geometric face key (rounded coords, minimal rotation), so a
    remapped but byte-identical original triangle is still recognised."""
    V = np.round(np.asarray(v, np.float64), 6)
    keys = set()
    for f in np.asarray(t):
        pts = V[f]
        rots = [tuple(map(tuple, np.roll(pts, k, axis=0))) for k in range(3)]
        keys.add(min(rots))
    return keys


def test_flap_fill_closes_a_hole_and_keeps_original_faces():
    v, t = _holed_sphere()
    assert len(detect(v.astype(np.float32), t.astype(np.int32))['holes']) == 1
    out_v, out_t, rep = flap.flap_fill(v, t, separate_stl=False)
    assert rep['loops_found'] == 1, rep
    assert rep['loops_filled'] == 1, rep
    assert rep['patch_faces'] > 0, rep
    d = detect(np.asarray(out_v, np.float32), np.asarray(out_t, np.int32))
    assert len(d['holes']) == 0, d
    assert len(d['non_manifold']) == 0, d
    # every original triangle survives verbatim (geometrically; indices may be
    # remapped by the internal boundary weld)
    assert _face_geom_keys(v, t).issubset(_face_geom_keys(out_v, out_t))
    assert rep['loops_skipped'] == 0, rep


def test_flap_fill_is_a_noop_on_a_closed_mesh():
    m = trimesh.creation.box(extents=(10.0, 10.0, 10.0))
    v = np.asarray(m.vertices, np.float64)
    t = np.asarray(m.faces, np.int64)
    out_v, out_t, rep = flap.flap_fill(v, t, separate_stl=False)
    assert rep['loops_found'] == 0, rep
    assert rep['patch_faces'] == 0, rep
    assert len(out_t) == len(t)


def test_resolve_flap_precedence_and_env():
    assert repair.resolve_flap() is False
    assert repair.resolve_flap(no_flap=True) is False
    assert repair.resolve_flap(force=True) is True
    assert repair.resolve_flap(force=True, no_flap=True) is False
    for raw, expected in (('1', True), ('off', False), ('yes', True),
                          ('no', False), ('bogus', False)):
        assert repair.resolve_flap(environ={'SUTURA_FLAP': raw}) is expected
    # explicit arguments beat the env
    assert repair.resolve_flap(force=True,
                               environ={'SUTURA_FLAP': '0'}) is True
    assert repair.resolve_flap(no_flap=True,
                               environ={'SUTURA_FLAP': '1'}) is False


def _meshset(v, t):
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(v, np.float64),
                        face_matrix=np.asarray(t, np.int32)))
    return ms


def test_flap_step_adopts_a_hole_closure():
    v, t = _holed_sphere()
    ms = _meshset(v, t)
    after = ms.apply_filter('get_topological_measures')
    stats = {'stage1': {}}
    out, _ = repair._flap_step(ml, ms, after, stats, {'maxholesize': 1000})
    rec = stats['flap']
    assert rec['ran'] is True, rec
    assert rec['adopted'] is True, rec
    assert rec['baseline_holes'] == 1, rec
    assert rec['candidate_holes'] == 0, rec
    assert rec['candidate_non_manifold'] <= rec['baseline_non_manifold'], rec
    out_v = np.asarray(out.current_mesh().vertex_matrix())
    out_t = np.asarray(out.current_mesh().face_matrix())
    assert len(detect(out_v, out_t)['holes']) == 0


def test_flap_adopt_gate_combined_damage():
    # framebaroque: VCG leaves 67 holes / 16 nm, flap closes to 16 holes but
    # welding the rims adds nm edges (22). Net damage falls -> adopt.
    assert repair._flap_adopt(67, 16, 16, 22) is True
    # artec_metal-nut: 2 holes / 1 nm -> 0 holes / 2 nm. Net damage falls.
    assert repair._flap_adopt(2, 1, 0, 2) is True
    # clean closure -> adopt; unchanged -> adopt.
    assert repair._flap_adopt(3, 0, 0, 0) is True
    assert repair._flap_adopt(0, 0, 0, 0) is True
    # thingi10k_46012: flap opens holes (4 -> 6) and adds nm -> reject.
    assert repair._flap_adopt(4, 0, 6, 3) is False
    # a hole closed but a non-manifold explosion -> reject.
    assert repair._flap_adopt(1, 0, 0, 5) is False
    assert repair._flap_adopt(10, 0, 9, 20) is False


def test_flap_step_skips_a_clean_mesh():
    m = trimesh.creation.box(extents=(10.0, 10.0, 10.0))
    ms = _meshset(np.asarray(m.vertices), np.asarray(m.faces))
    after = ms.apply_filter('get_topological_measures')
    stats = {'stage1': {}}
    _out, _ = repair._flap_step(ml, ms, after, stats, {'maxholesize': 1000})
    rec = stats['flap']
    assert rec['ran'] is False, rec
    assert rec['baseline_holes'] == 0 and rec['baseline_non_manifold'] == 0, rec


def test_flap_prepass_runs_on_original_input():
    # The flap step is the FIRST Stage-1 step and runs on the ORIGINAL input
    # (so the deep-repair tiers that work from the original arrays also see its
    # patches). On a holed sphere the report baseline must therefore count the
    # input's own single hole, not a post-chain state.
    v, t = _holed_sphere()
    import tempfile
    rep, nv, nt = repair.repair_mesh_from_arrays(
        np.asarray(v, np.float32), np.asarray(t, np.int32),
        tempfile.mkdtemp(prefix='flappre-'), mode='medium', flap=True)
    rec = rep.get('flap')
    assert rec is not None, rep
    assert rec['baseline_holes'] == 1, rec
    assert rec['adopted'] is True, rec
    d = detect(np.asarray(nv, np.float32), np.asarray(nt, np.int32))
    assert len(d['holes']) == 0, d


def _box(extents, translate):
    m = trimesh.creation.box(extents=extents)
    m.apply_translation(translate)
    return (np.asarray(m.vertices, np.float32), np.asarray(m.faces, np.int32))


def test_drop_flap_debris_drops_flat_keeps_solid():
    # A solid main body + a second genuine solid part + a collapsed flat sheet:
    # the sheet is debris (effective thickness far below its own diagonal) and
    # must go; both solid parts (a box has 2V/A = side/3) must stay.
    mv, mt = _box((10.0, 10.0, 10.0), (0.0, 0.0, 0.0))
    sv, st = _box((1.0, 1.0, 1.0), (40.0, 0.0, 0.0))          # real, small vol
    fv, ft = _box((2.0, 2.0, 1e-6), (0.0, 40.0, 0.0))          # flat sheet
    v = np.vstack([mv, sv, fv])
    t = np.vstack([mt, st + len(mv), ft + len(mv) + len(sv)])
    out_v, out_t, info = repair.drop_flap_debris(mv, mt, v, t)
    assert info is not None, 'expected the flat sheet to be dropped'
    assert info['components_before'] == 3, info
    assert info['components_after'] == 2, info
    assert len(info['dropped']) == 1, info
    d = info['dropped'][0]
    assert d['faces'] == len(ft), d
    assert d['thickness'] < 1e-4 * d['diag'], d
    # both solid parts survive (face count 24 = two boxes of 12)
    assert len(out_t) == 24, len(out_t)
    # a solid part is never dropped even if it is small: only the flat sheet
    assert info['dropped_faces'] == len(ft), info


def test_drop_flap_debris_noop_on_single_part():
    v, t = _box((10.0, 10.0, 10.0), (0.0, 0.0, 0.0))
    out_v, out_t, info = repair.drop_flap_debris(v, t, v, t)
    assert info is None
    assert len(out_t) == len(t)


def test_flap_off_leaves_no_report_key():
    m = trimesh.creation.box(extents=(10.0, 10.0, 10.0))
    v = np.asarray(m.vertices, np.float32)
    t = np.asarray(m.faces, np.int32)
    import tempfile
    rep, _nv, _nt = repair.repair_mesh_from_arrays(
        v, t, tempfile.mkdtemp(prefix='flapoff-'), mode='medium', flap=False)
    assert 'flap' not in rep, rep.get('flap')


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('flap tests passed')
