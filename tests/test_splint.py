# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Splint tier (method #18) regression.

Runs under the PyMeshLab venv (the tier welds/repairs via ``ml``). The Rust
``sutura_geom.splint_cut`` core is faked so no real rebuild is needed:

  * ``resolve_splint`` precedence (--no-splint > --experimental-splint > env >
    default switch) and the OFF-by-default constant;
  * the registry entry (#18) and the deep-tier order table;
  * ``splint_tier`` adopt on a strict-watertight candidate, reject on a holed
    one, the input-cap skip and the wall-clock abort.

Plain script, no pytest required: run
  ~/.local/share/sutura/venv/bin/python tests/test_splint.py
"""
import os
import sys

import importlib.machinery

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
for _p in (REPO, SUTURA):
    if _p not in sys.path:
        sys.path.insert(0, _p)

import numpy as np  # noqa: E402


def _cube():
    v = np.array([[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
                  [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]], float)
    f = np.array([[0, 2, 1], [0, 3, 2],   # bottom (outward -z)
                  [4, 5, 6], [4, 6, 7],   # top (+z)
                  [0, 1, 5], [0, 5, 4],   # front (-y)
                  [1, 2, 6], [1, 6, 5],   # right (+x)
                  [2, 3, 7], [2, 7, 6],   # back (+y)
                  [3, 0, 4], [3, 4, 7]],  # left (-x)
                 dtype=np.int64)
    return v, f


def test_resolve_splint_precedence():
    from sutura_engine.pipeline.presets import (
        SPLINT_DEFAULT_ENABLED, resolve_splint,
    )
    assert SPLINT_DEFAULT_ENABLED is False
    assert resolve_splint() is False                       # default OFF
    assert resolve_splint(force=True) is True
    assert resolve_splint(no_splint=True) is False
    assert resolve_splint(no_splint=True, force=True) is False  # no > force
    assert resolve_splint(environ='1') is True
    assert resolve_splint(environ='0') is False
    assert resolve_splint(default_enabled=True) == 'auto'
    assert resolve_splint(no_splint=True, environ='1') is False
    print('ok test_resolve_splint_precedence')


def test_registry_and_order():
    from sutura_engine.methods import get_method
    import sutura_engine.pipeline.order as order
    m = get_method(18)
    assert m is not None and m.id == 'splint' and m.num == 18
    assert m.invents_geometry is True and m.self_guarded is True
    assert m.core_required is True
    assert m.kwargs.get('splint') is True
    assert order.method_num('splint') == 18
    assert order.DEEP_TIER_ORDER.index('splint') < \
        order.DEEP_TIER_ORDER.index('ftetwild')
    assert order.DEEP_TIER_ORDER.index('dressing') < \
        order.DEEP_TIER_ORDER.index('splint')
    print('ok test_registry_and_order')


def test_health_fails_not_skips_without_splint_cut():
    # A stale sutura_geom (the mandatory core symbols present but splint_cut
    # missing) must make Splint FAIL, never SKIP: Splint is core_required, so a
    # missing symbol is a broken install, not an optional addon.
    import types
    from sutura_engine import health
    saved = sys.modules.get('sutura_geom')
    fake = types.ModuleType('sutura_geom')
    fake.__spec__ = importlib.machinery.ModuleSpec('sutura_geom', None)
    for _sym in ('flap_fill', 'morph_close', 'tamponade'):
        setattr(fake, _sym, lambda *a, **k: None)
    sys.modules['sutura_geom'] = fake
    try:
        detail = health.check_availability_detail()
        st, reason = detail['splint']
        assert st is health.Status.FAIL, (st, reason)
        assert 'splint_cut' in (reason or ''), reason
    finally:
        if saved is not None:
            sys.modules['sutura_geom'] = saved
        else:
            sys.modules.pop('sutura_geom', None)
    print('ok test_health_fails_not_skips_without_splint_cut')


class _FakeMesh:
    def __init__(self, vertex_matrix=None, face_matrix=None):
        self._v = np.asarray(vertex_matrix, np.float32)
        self._f = np.asarray(face_matrix, np.int32)

    def vertex_matrix(self):
        return self._v

    def face_matrix(self):
        return self._f


class _FakeMS:
    def __init__(self):
        self._m = None

    def add_mesh(self, m):
        self._m = m

    def current_mesh(self):
        return self._m


class _FakeML:
    Mesh = _FakeMesh
    MeshSet = _FakeMS


def _install_fake_core(out_v, out_t, report=None, b2=None):
    import types
    mod = types.ModuleType('sutura_geom')
    rep = {'seconds': 0.01, 'output_faces': int(len(out_t)),
           'output_vertices': int(len(out_v)), 'output_parts': 1,
           'out_manifold': True}
    rep.update(report or {})

    def splint_cut(verts, tris, res, adaptive=None):
        return out_v, out_t, rep

    if b2 is None:
        # Default fake Manifold rebuild: identity (constructs with NoError).
        def stage2_rebuild(verts, tris):
            return (np.asarray(verts, np.float32),
                    np.asarray(tris, np.int32),
                    {'construct_status': 'Error.NoError', 'ok': True})
    else:
        def stage2_rebuild(verts, tris):
            v, t, st = b2
            return (np.asarray(v, np.float32), np.asarray(t, np.int32),
                    {'construct_status': st, 'ok': st == 'Error.NoError'})
    mod.splint_cut = splint_cut
    mod.stage2_rebuild = stage2_rebuild
    sys.modules['sutura_geom'] = mod
    return mod


def _run_tier(cand_v, cand_t, spec=None, stats=None, b2=None):
    from sutura_engine.pipeline.tiers.splint import splint_tier
    _install_fake_core(cand_v, cand_t, b2=b2)
    stats = stats if stats is not None else {'stage1': {}}
    after = {}

    class _Spec:
        splint_max_faces = 300000
        splint_timeout = 180.0
        splint_res = 128
    v, f = _cube()
    out_v, out_t, after = splint_tier(
        v, f, v, f, after, stats, tmpdir='/tmp', splint=True,
        spec=(spec if spec is not None else _Spec()))
    return out_v, after, stats['splint']


def test_tier_adopts_watertight_candidate():
    v, f = _cube()
    ms, after, rec = _run_tier(v, f)
    assert rec['ran'] and rec['adopted'] is True, rec
    assert rec['holes'] == 0 and rec['non_manifold'] == 0
    assert after and ms is not None
    print('ok test_tier_adopts_watertight_candidate')


def test_tier_rejects_holed_candidate():
    v, f = _cube()
    # Drop one triangle -> the candidate has a boundary (hole) that even the
    # (identity) Manifold rebuild cannot close, so it is not adopted.
    holed_f = f[:-1]
    ms, after, rec = _run_tier(v, holed_f)
    assert rec['ran'] and rec['adopted'] is False, rec
    assert rec['construct_status'] == 'Error.NoError', rec
    assert rec['reason'].startswith('candidate not strict-watertight'), rec
    print('ok test_tier_rejects_holed_candidate')


def test_tier_manifold_postprocess_adopts_closed_rebuild():
    v, f = _cube()
    # A fin triangle on an existing edge (0,1) makes it non-manifold (3 faces)
    # with no hole; the Manifold rebuild (faked to a clean cube) closes it.
    nm_f = np.vstack([f, [[0, 1, 7]]])
    clean_v, clean_f = _cube()
    ms, after, rec = _run_tier(v, nm_f,
                               b2=(clean_v, clean_f, 'Error.NoError'))
    assert rec['ran'] and rec['adopted'] is True, rec
    assert rec['manifold_postprocessed'] is True, rec
    assert rec['holes'] == 0 and rec['non_manifold'] == 0
    assert rec['construct_status'] == 'Error.NoError'
    print('ok test_tier_manifold_postprocess_adopts_closed_rebuild')


def test_tier_skips_over_cap():
    v, f = _cube()

    class _Tiny:
        splint_max_faces = 2
        splint_timeout = 180.0
        splint_res = 128
    ms, after, rec = _run_tier(v, f, spec=_Tiny())
    assert rec['skipped_budget'] is True and rec['ran'] is False, rec
    assert rec['reject_reason'] == 'too_large'
    print('ok test_tier_skips_over_cap')


def test_tier_aborts_on_timeout():
    v, f = _cube()

    class _Zero:
        splint_max_faces = 300000
        splint_timeout = 1e-9    # any positive elapsed aborts
        splint_res = 128
    _ms, _after, rec = _run_tier(v, f, spec=_Zero())
    assert rec['aborted'] is True and rec['adopted'] is False, rec
    assert rec['reject_reason'] == 'aborted_time'
    print('ok test_tier_aborts_on_timeout')


if __name__ == '__main__':
    test_resolve_splint_precedence()
    test_registry_and_order()
    test_health_fails_not_skips_without_splint_cut()
    try:
        import pymeshlab  # noqa: F401
    except Exception:
        print('skip tier tests (pymeshlab unavailable)')
        print('ALL OK')
        sys.exit(0)
    test_tier_adopts_watertight_candidate()
    test_tier_rejects_holed_candidate()
    test_tier_manifold_postprocess_adopts_closed_rebuild()
    test_tier_skips_over_cap()
    test_tier_aborts_on_timeout()
    print('ALL OK')
