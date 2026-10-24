# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Coaptation tier (method #19) regression.

Coaptation closes the one-float32-ULP seam of a folded / overlapping
multi-body soup and folds the bodies with the exact union.  The Rust
``sutura_geom.close_holes`` / ``union_pair`` core is only needed for the
end-to-end path; this file keeps the CI-critical checks pure-numpy and
stdlib:

  * ``resolve_coaptation`` precedence (--no-coaptation > --coaptation > env >
    the ON-by-default switch) and the default-enabled constant;
  * the registry entry (#19, aliases, flags) and the deep-tier order table;
  * the pure-numpy seam helpers (``_face_labels``, ``_boundary_vertex_ids``,
    ``_seam_weld``);
  * the cheap gates of ``coaptation_tier`` (disabled / single_body / over-cap);
  * a public-fixture smoke run over ``tests/real-world-samples/thingi10k_55772``
    guarded by the Rust extension's presence.

Plain script, no pytest required: run
  ~/.local/share/sutura/venv/bin/python tests/test_coaptation.py
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


def test_resolve_coaptation_precedence():
    from sutura_engine.pipeline.presets import (
        COAPTATION_DEFAULT_ENABLED, resolve_coaptation,
    )
    assert COAPTATION_DEFAULT_ENABLED is True
    assert resolve_coaptation() == 'auto'                  # default ON (auto)
    assert resolve_coaptation(force=True) is True
    assert resolve_coaptation(no_coaptation=True) is False
    assert resolve_coaptation(no_coaptation=True, force=True) is False  # no > force
    assert resolve_coaptation(environ='1') is True
    assert resolve_coaptation(environ='0') is False
    assert resolve_coaptation(default_enabled=False) is False
    assert resolve_coaptation(default_enabled=True) == 'auto'
    assert resolve_coaptation(no_coaptation=True, environ='1') is False
    print('ok test_resolve_coaptation_precedence')


def test_registry_and_order():
    from sutura_engine.methods import get_method
    import sutura_engine.pipeline.order as order
    m = get_method(19)
    assert m is not None and m.id == 'coaptation' and m.num == 19, m
    assert m.display_name == 'Coaptation'
    assert m.invents_geometry is True and m.self_guarded is True
    assert m.core_required is True
    assert m.kwargs.get('coaptation') is True
    assert get_method('seam_weld') is m and get_method('coapt') is m
    assert order.method_num('coaptation') == 19
    assert order.DEEP_TIER_ORDER.index('coaptation') < \
        order.DEEP_TIER_ORDER.index('ftetwild')
    assert order.DEEP_TIER_ORDER.index('splint') < \
        order.DEEP_TIER_ORDER.index('coaptation')
    print('ok test_registry_and_order')


def test_face_labels_and_boundary():
    from sutura_engine.pipeline.tiers.coaptation import (
        _face_labels, _boundary_vertex_ids,
    )
    # Two triangles sharing one edge form a single edge-connected body; two
    # disjoint triangles form two.
    quad_v = np.array([[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0]], float)
    quad_f = np.array([[0, 1, 2], [0, 2, 3]], dtype=np.int64)
    assert len(np.unique(_face_labels(quad_f))) == 1
    two_f = np.array([[0, 1, 2], [3, 4, 5]], dtype=np.int64)
    assert len(np.unique(_face_labels(two_f))) == 2
    # Every edge of a lone triangle (and of two disjoint triangles) is used
    # once, so every vertex is on the boundary; a closed cube has none.
    assert sorted(_boundary_vertex_ids(np.array([[0, 1, 2]], dtype=np.int64))) \
        == [0, 1, 2]
    assert len(_boundary_vertex_ids(two_f)) == 6
    _cv, cf = _cube()
    assert len(_boundary_vertex_ids(cf)) == 0
    print('ok test_face_labels_and_boundary')


def test_seam_weld_merges_only_close_boundary_vertices():
    from sutura_engine.pipeline.tiers.coaptation import _seam_weld
    # Triangle A at the origin; triangle B far away EXCEPT its first vertex
    # sits ``delta`` from A's first vertex.  A boundary-only tolerance weld
    # merges exactly that pair.
    delta = 1e-7
    v = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0],
                  # B: first vertex displaced by delta, the others far away
                  [delta, 0, 0], [10, 10, 0], [10, 11, 0]], dtype=np.float32)
    f = np.array([[0, 1, 2], [3, 4, 5]], dtype=np.int64)
    v32 = np.asarray(v, np.float32)
    wv, wt, merged = _seam_weld(v32, f, tol=2 * delta)
    assert merged == 1, merged
    assert len(wv) == 5, len(wv)
    # tol=0 never merges anything.
    _v0, _t0, m0 = _seam_weld(v32, f, tol=0.0)
    assert m0 == 0
    print('ok test_seam_weld_merges_only_close_boundary_vertices')


class _Spec:
    coaptation_max_faces = 300000
    coaptation_work = 1_000_000


def test_tier_disabled_is_identity():
    from sutura_engine.pipeline.tiers.coaptation import coaptation_tier
    v, f = _cube()
    stats = {}
    ov, ot, after = coaptation_tier(v, f, v, f, {}, stats, '/tmp',
                                    coaptation=False, spec=_Spec())
    assert 'coaptation' not in stats
    assert np.array_equal(ov, v.astype(np.float64))
    assert np.array_equal(ot, f.astype(np.int64))
    assert after == {}
    print('ok test_tier_disabled_is_identity')


def test_tier_single_body_gate():
    from sutura_engine.pipeline.tiers.coaptation import coaptation_tier
    v, f = _cube()
    stats = {}
    coaptation_tier(v, f, v, f, {}, stats, '/tmp', coaptation=True, spec=_Spec())
    rec = stats['coaptation']
    assert rec['ran'] is False and rec['adopted'] is False, rec
    assert rec['gate'] == 'single_body' and rec['reject_reason'] == 'single_body'
    print('ok test_tier_single_body_gate')


def test_tier_skips_over_cap():
    from sutura_engine.pipeline.tiers.coaptation import coaptation_tier
    v, f = _cube()

    class _Tiny:
        coaptation_max_faces = 2
        coaptation_work = 1_000_000
    stats = {}
    coaptation_tier(v, f, v, f, {}, stats, '/tmp', coaptation=True,
                    spec=_Tiny())
    rec = stats['coaptation']
    assert rec['skipped_budget'] is True and rec['ran'] is False, rec
    assert rec['reject_reason'] == 'too_large'
    print('ok test_tier_skips_over_cap')


def test_health_fails_not_skips_without_union_pair():
    # A stale sutura_geom (the mandatory symbols present but union_pair
    # missing) must make Coaptation FAIL, never SKIP: it is core_required.
    import importlib.util
    import types
    from sutura_engine import health
    if importlib.util.find_spec('sutura_geom') is None:
        print('skip test_health_fails_not_skips_without_union_pair '
              '(sutura_geom not installed)')
        return
    saved = sys.modules.get('sutura_geom')
    fake = types.ModuleType('sutura_geom')
    fake.__spec__ = importlib.machinery.ModuleSpec('sutura_geom', None)
    for _sym in ('flap_fill', 'morph_close', 'tamponade', 'close_holes'):
        setattr(fake, _sym, lambda *a, **k: None)
    sys.modules['sutura_geom'] = fake
    try:
        detail = health.check_availability_detail()
        st, reason = detail['coaptation']
        assert st is health.Status.FAIL, (st, reason)
        assert 'union_pair' in (reason or ''), reason
    finally:
        if saved is not None:
            sys.modules['sutura_geom'] = saved
        else:
            sys.modules.pop('sutura_geom', None)
    print('ok test_health_fails_not_skips_without_union_pair')


def test_public_fixture_smoke():
    # End-to-end run of the tier on the committed public sample.  The result
    # is not asserted to be adopted (that depends on the exact stage-1 output);
    # the test only proves the whole code path runs and reports well-formed on
    # a real public mesh.
    sample = os.path.join(REPO, 'tests', 'real-world-samples',
                          'thingi10k_55772.stl')
    if not os.path.exists(sample):
        print('skip test_public_fixture_smoke (sample missing)')
        return
    try:
        import trimesh  # noqa: F401
        import sutura_geom  # noqa: F401
    except Exception:
        print('skip test_public_fixture_smoke (trimesh/sutura_geom unavailable)')
        return
    import trimesh
    from sutura_engine.pipeline.tiers.coaptation import coaptation_tier
    m = trimesh.load(sample, process=True)
    v = np.asarray(m.vertices, float)
    f = np.asarray(m.faces, np.int64)
    stats = {}
    ov, ot, _after = coaptation_tier(v, f, v, f, {}, stats, '/tmp',
                                     coaptation=True, spec=_Spec())
    rec = stats.get('coaptation')
    assert rec is not None and isinstance(rec['ran'], bool), rec
    if not rec['ran']:
        # a gate or an unavailable core; either way the input must be returned
        assert rec['adopted'] is False
        assert ov.shape[0] == v.shape[0] and ot.shape[0] == f.shape[0]
    print('ok test_public_fixture_smoke')


if __name__ == '__main__':
    test_resolve_coaptation_precedence()
    test_registry_and_order()
    test_face_labels_and_boundary()
    test_seam_weld_merges_only_close_boundary_vertices()
    test_tier_disabled_is_identity()
    test_tier_single_body_gate()
    test_tier_skips_over_cap()
    test_health_fails_not_skips_without_union_pair()
    test_public_fixture_smoke()
    print('ALL OK')
