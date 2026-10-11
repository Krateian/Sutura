#!/usr/bin/env python3
"""Regression tests for the Tamponade integration scaffold (#17).

Covers, with a FAKE ``sutura_geom.tamponade`` (no Rust extension and no
pymeshlab needed):
  1. The ``run_tamponade`` wrapper: unavailable extension, the info-key
     mapping, and that a raising extension never crashes (returns the input).
  2. The ``repair.tamponade_tier`` adoption guard: skip when the mesh is clean,
     adopt when reload-honest holes+non-manifold strictly fall, reject when
     holes rise / component count rises / healthy faces < 99%.
  3. Flag / env / constant resolution (``repair.resolve_tamponade``).
  4. Registry alias resolution (``17``/``m17``/``tamponade``/``lava``).

Usage: ~/.local/share/sutura/venv/bin/python tests/test_tamponade_integration.py
(any interpreter with numpy; the tier itself is exercised through fakes).
"""
import os
import sys
import types

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402

import repair  # noqa: E402
import methods as method_registry  # noqa: E402
from sutura_engine import tamponade as tamp  # noqa: E402


_REAL_RUN_TAMPONADE = tamp.run_tamponade


_CUBE_V = np.array([
    [0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
    [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1],
], dtype=np.float64)

# Outward-wound unit cube (12 triangles).
_CUBE_F = np.array([
    [0, 2, 1], [0, 3, 2],          # bottom
    [4, 5, 6], [4, 6, 7],          # top
    [0, 1, 5], [0, 5, 4],          # front
    [1, 2, 6], [1, 6, 5],          # right
    [2, 3, 7], [2, 7, 6],          # back
    [3, 0, 4], [3, 4, 7],          # left
], dtype=np.int64)

# One missing face -> one boundary loop (11 triangles).
_OPEN1_F = np.array([f for f in _CUBE_F if tuple(f) not in {(4, 5, 6), (4, 6, 7)}],
                    dtype=np.int64)
# Two opposite faces removed -> two boundary loops (10 triangles).
_OPEN2_F = np.array([f for f in _CUBE_F
                     if tuple(f) not in {(4, 5, 6), (4, 6, 7), (0, 2, 1), (0, 3, 2)}],
                    dtype=np.int64)


def _two_cubes():
    v = np.vstack([_CUBE_V, _CUBE_V + np.array([3.0, 0.0, 0.0])])
    f = np.vstack([_CUBE_F, _CUBE_F + 8])
    return v, f


class _FakeMesh:
    def __init__(self, v=None, f=None):
        self._v = v
        self._f = f

    def vertex_matrix(self):
        return self._v

    def face_matrix(self):
        return self._f


class _FakeMeshSet:
    def __init__(self, v=None, f=None):
        self._m = _FakeMesh(v, f)

    def current_mesh(self):
        return self._m

    def add_mesh(self, m):
        self._m = m

    def apply_filter(self, name):
        return {'non_two_manifold_edges': 0, 'boundary_edges': 0,
                'number_holes': 0}


class _FakeML:
    def MeshSet(self):
        return _FakeMeshSet()

    def Mesh(self, vertex_matrix=None, face_matrix=None):
        return _FakeMesh(vertex_matrix, face_matrix)


class _FakeGeom:
    """Minimal stand-in for the ``sutura_geom`` extension."""
    TAMPONADE_SUPPORTED = True

    def __init__(self, out_v, out_t, info=None, raises=None):
        self._out_v = out_v
        self._out_t = out_t
        self._info = info or {}
        self._raises = raises
        self.calls = []

    def tamponade(self, verts, tris, h_min=None, melt_si=True, max_nodes=None,
                  time_budget_s=None):
        self.calls.append({'h_min': h_min, 'melt_si': melt_si,
                           'time_budget_s': time_budget_s})
        if self._raises is not None:
            raise self._raises
        return self._out_v, self._out_t, dict(self._info)


def _install_tier(out_v, out_t, info):
    """Patch ``run_tamponade`` (the wrapper) with a fake returning the given
    candidate. Returns the report dict the fake hands to the tier."""
    rec = {'ran': True, 'adopted': False, 'reason': None}
    rec.update(info)

    def fake(verts, tris, *, intensity=None, ml=None, time_budget_s=None):
        return out_v, out_t, rec
    tamp.run_tamponade = fake
    return rec


# --- 1. wrapper -------------------------------------------------------------

def test_wrapper_unavailable():
    real = tamp._tamponade_module
    tamp._tamponade_module = lambda: None
    try:
        v, t, rep = tamp.run_tamponade(_CUBE_V, _CUBE_F, intensity='balanced')
    finally:
        tamp._tamponade_module = real
    assert rep == {'ran': False, 'reason': 'sutura_geom.tamponade unavailable'}, rep
    assert v is _CUBE_V and t is _CUBE_F
    print('ok  wrapper reports the exact unavailable payload')


def test_wrapper_maps_info():
    info = {'plugs': 2, 'plug_faces': 8, 'melt_layers_tried': 1,
            'melt_layers_accepted': 1, 'melt_layers_rolled_back': 0,
            'micro_welds_ok': 4, 'micro_welds_failed': 0,
            'plugs_rejected': 1, 'plugs_inconclusive': 0,
            'plugs_rejected_size': 1, 'plugs_rejected_dev': 0,
            'healthy_faces_kept': 11, 'seconds': 0.25,
            'reference_used': 2, 'reference_fallback': 1,
            'trimmed_vertices': 3, 'patch_vertices': 1}
    geom = _FakeGeom(_CUBE_V.copy(), _CUBE_F.copy(), info)
    real = tamp._tamponade_module
    tamp._tamponade_module = lambda: geom
    try:
        out_v, out_t, rep = tamp.run_tamponade(_CUBE_V, _CUBE_F,
                                               intensity='balanced')
    finally:
        tamp._tamponade_module = real
    assert rep['ran'] is True, rep
    assert rep['plugs'] == 2 and rep['plug_faces'] == 8, rep
    assert rep['healthy_faces_kept'] == 11, rep
    assert rep['reference_used'] == 2 and rep['reference_fallback'] == 1, rep
    assert rep['trimmed_vertices'] == 3 and rep['patch_vertices'] == 1, rep
    assert rep['seconds'] == 0.25, rep
    assert rep['plugs_rejected_size'] == 1 and rep['plugs_rejected_dev'] == 0, rep
    # h_min defaults to the input median edge and the budget passed to the
    # kernel is the intensity preset (the per-loop allowance lives in Rust).
    assert geom.calls[0]['h_min'] > 0, geom.calls
    assert abs(geom.calls[0]['time_budget_s'] - 60.0) < 1e-9, geom.calls
    print('ok  wrapper maps the Rust info keys and the preset budget')


def test_wrapper_never_raises():
    geom = _FakeGeom(None, None, raises=RuntimeError('rust panic'))
    real = tamp._tamponade_module
    tamp._tamponade_module = lambda: geom
    try:
        out_v, out_t, rep = tamp.run_tamponade(_CUBE_V, _CUBE_F,
                                               intensity='thorough')
    finally:
        tamp._tamponade_module = real
    assert rep['ran'] is False, rep
    assert 'rust panic' in (rep['reason'] or ''), rep
    assert np.array_equal(np.asarray(out_v), _CUBE_V)
    print('ok  a raising extension returns the input unchanged')


# --- 2. adoption guard ------------------------------------------------------

def _tier(in_f, after=None, out_v=None, out_t=None, healthy=None, si_mode='report',
          intensity='balanced'):
    ml = _FakeML()
    ms = _FakeMeshSet(_CUBE_V, in_f)
    stats = {}
    info = {'healthy_faces_kept': healthy if healthy is not None else len(in_f)}
    _install_tier(_CUBE_V if out_v is None else out_v,
                  _CUBE_F if out_t is None else out_t, info)
    try:
        repair.tamponade_tier(ml, ms, after or {'non_two_manifold_edges': 0},
                              stats, '/tmp', tamponade=True,
                              intensity=intensity, si_mode=si_mode)
    finally:
        tamp.run_tamponade = _REAL_RUN_TAMPONADE
    return stats['tamponade']


def test_tier_skips_clean_mesh():
    # A closed cube has no holes/non-manifold/SI -> the tier must not run.
    ml = _FakeML()
    ms = _FakeMeshSet(_CUBE_V, _CUBE_F)
    stats = {}
    called = {'n': 0}

    def fake(*a, **k):
        called['n'] += 1
        return _CUBE_V, _CUBE_F, {'ran': True}
    tamp.run_tamponade = fake
    try:
        repair.tamponade_tier(ml, ms, {'non_two_manifold_edges': 0}, stats,
                              '/tmp', tamponade=True, intensity='balanced')
    finally:
        tamp.run_tamponade = _REAL_RUN_TAMPONADE
    rec = stats['tamponade']
    assert rec['ran'] is False and rec['adopted'] is False, rec
    assert called['n'] == 0, called
    print('ok  tier is skipped on a clean stage-1 mesh')


def test_tier_disabled_is_noop():
    stats = {}
    ml, ms = _FakeML(), _FakeMeshSet(_CUBE_V, _OPEN1_F)
    after = {'non_two_manifold_edges': 0}
    out_ms, out_after = repair.tamponade_tier(ml, ms, after, stats, '/tmp',
                                              tamponade=False)
    assert out_ms is ms and out_after is after, 'disabled tier must be a no-op'
    assert 'tamponade' not in stats, stats
    print('ok  tamponade=False is a byte-identical no-op')


def test_tier_adopts_when_better():
    # Open cube (1 loop) -> closed cube (0/0) with all faces kept.
    rec = _tier(_OPEN1_F, healthy=len(_OPEN1_F))
    assert rec['ran'] is True and rec['adopted'] is True, rec
    assert rec['watertight'] is True and rec['reload_watertight'] is True, rec
    assert rec['candidate_holes'] == 0 and rec['candidate_non_manifold'] == 0, rec
    assert rec['candidate_parts'] == 1, rec
    print('ok  tier adopts a candidate that closes the mesh')


def test_tier_rejects_when_holes_rise():
    rec = _tier(_OPEN1_F, out_t=_OPEN2_F, healthy=len(_OPEN2_F))
    assert rec['ran'] is True and rec['adopted'] is False, rec
    assert rec['candidate_holes'] >= rec['input_holes'], rec
    assert 'holes/non-manifold rose' in (rec['reject_reason'] or ''), rec
    print('ok  tier rejects a candidate whose holes rise')


def test_tier_rejects_when_parts_rise():
    tv, tt = _two_cubes()
    rec = _tier(_OPEN1_F, out_v=tv, out_t=tt, healthy=len(tt))
    assert rec['ran'] is True and rec['adopted'] is False, rec
    assert rec['candidate_parts'] > rec['input_parts'], rec
    assert 'component count rose' in (rec['reject_reason'] or ''), rec
    print('ok  tier rejects a candidate that raises the component count')


def test_tier_rejects_when_healthy_low():
    rec = _tier(_OPEN1_F, healthy=1)
    assert rec['ran'] is True and rec['adopted'] is False, rec
    assert rec['healthy_fraction'] < 0.99, rec
    assert 'healthy faces below 99%' in (rec['reject_reason'] or ''), rec
    print('ok  tier rejects a candidate below the 99% healthy-face floor')


def test_tier_adopts_si_only_improvement():
    # Review C2: a closed mesh whose holes/non-manifold are already 0 but whose
    # exact self-intersections fall must still be adopted (melt-only win).
    calls = {'n': 0}
    real = repair._exact_si_face_count

    def fake_si(verts, tris):
        calls['n'] += 1
        return 10 if calls['n'] == 1 else 4

    repair._exact_si_face_count = fake_si
    try:
        rec = _tier(_CUBE_F, healthy=len(_CUBE_F), intensity='thorough')
    finally:
        repair._exact_si_face_count = real
    assert rec['ran'] is True and rec['adopted'] is True, rec
    assert rec['input_holes'] == 0 and rec['input_non_manifold'] == 0, rec
    assert rec['input_self_intersections'] == 10, rec
    assert rec['candidate_self_intersections'] == 4, rec
    assert rec['si_fall'] is True, rec
    assert 'SI 10 -> 4' in (rec['reason'] or ''), rec
    print('ok  tier adopts an SI-only improvement (C2)')


def test_prepass_uses_local_si_guard_not_full_scan():
    # The prepass must not pay for a full exact-SI scan on the pre-tier mesh or
    # on the candidate: it compares only the faces the seal changed (the Rust
    # subset counter) and reports si_method='local'.
    calls = {'n': 0}
    real = repair._exact_si_face_count
    repair._exact_si_face_count = lambda v, t: calls.__setitem__(
        'n', calls['n'] + 1) or 7
    ml = _FakeML()
    ms = _FakeMeshSet(_CUBE_V, _OPEN1_F)   # one boundary loop -> the prepass runs
    stats = {}
    _install_tier(_CUBE_V, _CUBE_F, {'healthy_faces_kept': len(_CUBE_F)})
    try:
        repair.tamponade_tier(ml, ms, {'non_two_manifold_edges': 0}, stats,
                              '/tmp', tamponade=True, intensity='balanced',
                              prepass=True)
        rec = stats['tamponade']
    finally:
        tamp.run_tamponade = _REAL_RUN_TAMPONADE
        repair._exact_si_face_count = real
    assert rec['ran'] is True, rec
    assert rec['si_method'] == 'local', rec
    assert rec['base_local_si'] is not None and rec['cand_local_si'] is not None, rec
    assert calls['n'] == 0, calls
    print('ok  prepass uses the local SI guard (no full exact-SI scan)')


def test_prepass_budget_zero_skips():
    import os
    ml = _FakeML()
    ms = _FakeMeshSet(_CUBE_V, _OPEN1_F)
    stats = {}
    old = os.environ.get('SUTURA_TAMPONADE_PREPASS_BUDGET')
    os.environ['SUTURA_TAMPONADE_PREPASS_BUDGET'] = '0'
    try:
        repair.tamponade_tier(ml, ms, {'non_two_manifold_edges': 0}, stats,
                              '/tmp', tamponade=True, intensity='balanced',
                              prepass=True)
        rec = stats['tamponade']
    finally:
        if old is None:
            os.environ.pop('SUTURA_TAMPONADE_PREPASS_BUDGET', None)
        else:
            os.environ['SUTURA_TAMPONADE_PREPASS_BUDGET'] = old
    assert rec['ran'] is False, rec
    assert 'budget 0' in (rec['reason'] or ''), rec
    print('ok  prepass budget 0 skips the tier')


def test_tier_skips_closed_si_free_mesh():
    # A closed mesh with no measured self-intersections must still skip.
    real = repair._exact_si_face_count
    repair._exact_si_face_count = lambda v, t: 0
    try:
        rec = _tier(_CUBE_F, healthy=len(_CUBE_F))
    finally:
        repair._exact_si_face_count = real
    assert rec['ran'] is False, rec
    print('ok  tier skips a closed SI-free mesh')


def test_resolve_melt_si():
    assert tamp.resolve_melt_si('quick') is False
    assert tamp.resolve_melt_si('balanced') is False
    assert tamp.resolve_melt_si('thorough') is True
    assert tamp.resolve_melt_si('extreme') is True
    assert tamp.resolve_melt_si(None) is False
    assert tamp.resolve_melt_si('balanced',
                                environ={'SUTURA_TAMPONADE_MELT': '1'}) is True
    assert tamp.resolve_melt_si('extreme',
                                environ={'SUTURA_TAMPONADE_MELT': '0'}) is False
    print('ok  melt is gated to thorough/extreme (env override)')


def test_resolve_prepass_budget():
    assert tamp.resolve_tamponade_prepass_budget('quick') == 0.0
    assert tamp.resolve_tamponade_prepass_budget('balanced') == 15.0
    assert tamp.resolve_tamponade_prepass_budget('thorough') == 40.0
    assert tamp.resolve_tamponade_prepass_budget('extreme') == 60.0
    assert tamp.resolve_tamponade_prepass_budget(None) == 15.0
    assert tamp.resolve_tamponade_prepass_budget(
        'balanced', environ={'SUTURA_TAMPONADE_PREPASS_BUDGET': '3.5'}) == 3.5
    assert tamp.resolve_tamponade_prepass_budget(
        'balanced', environ={'SUTURA_TAMPONADE_PREPASS_BUDGET': 'x'}) == 15.0
    print('ok  prepass budget resolution (preset + env override)')


def test_tier_closed_balanced_skips_without_scan():
    calls = {'n': 0}
    real = repair._exact_si_face_count

    def fake(verts, tris):
        calls['n'] += 1
        return 5

    repair._exact_si_face_count = fake
    try:
        rec = _tier(_CUBE_F, healthy=len(_CUBE_F), intensity='balanced')
    finally:
        repair._exact_si_face_count = real
    assert rec['ran'] is False and rec['melt_enabled'] is False, rec
    assert calls['n'] == 0, calls
    print('ok  closed mesh at balanced skips without an SI scan')


# --- 3. flag / env / constant resolution ------------------------------------

def test_resolve_tamponade():
    assert repair.resolve_tamponade() is True            # default switch ON
    assert repair.resolve_tamponade(force=True) is True
    assert repair.resolve_tamponade(no_tamponade=True, force=True) is False
    assert repair.resolve_tamponade(environ={'SUTURA_TAMPONADE': '1'}) is True
    assert repair.resolve_tamponade(environ={'SUTURA_TAMPONADE': '0'}) is False
    # an invalid env value falls back to the default switch (ON)
    assert repair.resolve_tamponade(environ={'SUTURA_TAMPONADE': 'maybe'}) is True
    # the default switch can be overridden per run / by env
    assert repair.resolve_tamponade(default_enabled=True) is True
    assert repair.resolve_tamponade(default_enabled=False) is False
    assert repair.resolve_tamponade(environ={}, default_enabled=True) is True
    # --no-tamponade beats a truthy env; --tamponade beats a falsy env
    assert repair.resolve_tamponade(no_tamponade=True,
                                    environ={'SUTURA_TAMPONADE': '1'}) is False
    assert repair.resolve_tamponade(force=True,
                                    environ={'SUTURA_TAMPONADE': '0'}) is True
    assert repair.TAMPONADE_DEFAULT_ENABLED is True
    print('ok  resolve_tamponade precedence (flag > env > switch)')


# --- 4. registry aliases ----------------------------------------------------

def test_method_alias_resolution():
    for alias in ('17', 'm17', 'tamponade', 'lava'):
        m = method_registry.get_method(alias)
        assert m.num == 17 and m.id == 'tamponade', (alias, m)
    m = method_registry.get_method(17)
    assert m.display_name == 'Tamponade', m.display_name
    assert m.invents_geometry is True and m.self_guarded is True, m
    assert m.needs_user_input is True, m
    assert m.kwargs.get('tamponade') is True, m.kwargs
    assert m.kwargs.get('deep_repair') == 'off', m.kwargs
    assert m.kwargs.get('ftetwild') is False, m.kwargs
    print('ok  registry resolves every Tamponade alias to #17')


# --- 5. final shape safety net (G11 release blocker) ------------------------

def test_final_guard_flags_bbox_collapse():
    # thingi10k_100827: a 37 mm object rebuilt into a 7 mm fragment. The guard
    # must flag the collapse and pass a full-coverage result.
    in_v = _CUBE_V * np.array([13.0, 13.0, 32.5])
    out_v = _CUBE_V * np.array([5.0, 3.5, 3.5])
    reason, m = repair._tamponade_final_needs_off(in_v, _CUBE_F, out_v, _CUBE_F)
    assert reason == 'bbox_collapse', (reason, m)
    assert m['coverage'] < repair.TAMP_COLLAPSE_BBOX_FRAC, m
    assert m['holes'] == 0 and m['parts'] == 1, m
    reason2, m2 = repair._tamponade_final_needs_off(
        in_v, _CUBE_F, _CUBE_V * 13.0, _CUBE_F)
    assert reason2 is None, (reason2, m2)
    assert m2['coverage'] > repair.TAMP_COLLAPSE_BBOX_FRAC, m2
    print('ok  final guard flags a bbox collapse and passes a full result')


def test_tamponade_off_preferred_rules():
    on = {'coverage': 0.19, 'holes': 0, 'non_manifold': 0, 'parts': 1}
    off = {'coverage': 1.0, 'holes': 0, 'non_manifold': 0, 'parts': 1}
    assert repair._tamponade_off_preferred(on, off) is True
    # OFF never replaces ON when it regresses damage or the part count.
    assert repair._tamponade_off_preferred(
        on, {'coverage': 1.0, 'holes': 0, 'non_manifold': 2, 'parts': 1}) is False
    assert repair._tamponade_off_preferred(
        on, {'coverage': 1.0, 'holes': 0, 'non_manifold': 0, 'parts': 2}) is False
    # A coverage tie keeps the Tamponade result.
    assert repair._tamponade_off_preferred(on, dict(on)) is False
    # A lower outside-the-input leak (G12) also lets OFF replace ON.
    on_leak = {'coverage': 1.0, 'holes': 0, 'non_manifold': 0, 'parts': 1,
               'leak_mm': 1.83}
    off_leak = {'coverage': 1.0, 'holes': 0, 'non_manifold': 0, 'parts': 1,
                'leak_mm': 0.0}
    assert repair._tamponade_off_preferred(on_leak, off_leak) is True
    assert repair._tamponade_off_preferred(off_leak, on_leak) is False
    print('ok  OFF replaces ON only with more coverage and no regression')


def main():
    tests = [v for k, v in sorted(globals().items())
             if k.startswith('test_') and callable(v)]
    for fn in tests:
        fn()
    print('tamponade integration tests passed (%d)' % len(tests))


if __name__ == '__main__':
    main()
