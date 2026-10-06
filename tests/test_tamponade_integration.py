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

    def fake(verts, tris, *, intensity=None, ml=None):
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
            'healthy_faces_kept': 11, 'seconds': 0.25}
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
    assert rep['seconds'] == 0.25, rep
    # h_min defaults to the input median edge and the budget comes from the
    # intensity preset.
    assert geom.calls[0]['h_min'] > 0, geom.calls
    assert geom.calls[0]['time_budget_s'] == 60.0, geom.calls
    print('ok  wrapper maps the Rust info keys and the intensity budget')


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

def _tier(in_f, after=None, out_v=None, out_t=None, healthy=None, si_mode='report'):
    ml = _FakeML()
    ms = _FakeMeshSet(_CUBE_V, in_f)
    stats = {}
    info = {'healthy_faces_kept': healthy if healthy is not None else len(in_f)}
    _install_tier(_CUBE_V if out_v is None else out_v,
                  _CUBE_F if out_t is None else out_t, info)
    try:
        repair.tamponade_tier(ml, ms, after or {'non_two_manifold_edges': 0},
                              stats, '/tmp', tamponade=True,
                              intensity='balanced', si_mode=si_mode)
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


# --- 3. flag / env / constant resolution ------------------------------------

def test_resolve_tamponade():
    assert repair.resolve_tamponade() is False           # default switch off
    assert repair.resolve_tamponade(force=True) is True
    assert repair.resolve_tamponade(no_tamponade=True, force=True) is False
    assert repair.resolve_tamponade(environ={'SUTURA_TAMPONADE': '1'}) is True
    assert repair.resolve_tamponade(environ={'SUTURA_TAMPONADE': '0'}) is False
    # an invalid env value falls back to the default switch
    assert repair.resolve_tamponade(environ={'SUTURA_TAMPONADE': 'maybe'}) is False
    # the default switch can be overridden per run / by env
    assert repair.resolve_tamponade(default_enabled=True) is True
    assert repair.resolve_tamponade(environ={}, default_enabled=True) is True
    # --no-tamponade beats a truthy env; --tamponade beats a falsy env
    assert repair.resolve_tamponade(no_tamponade=True,
                                    environ={'SUTURA_TAMPONADE': '1'}) is False
    assert repair.resolve_tamponade(force=True,
                                    environ={'SUTURA_TAMPONADE': '0'}) is True
    assert repair.TAMPONADE_DEFAULT_ENABLED is False
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


def main():
    tests = [v for k, v in sorted(globals().items())
             if k.startswith('test_') and callable(v)]
    for fn in tests:
        fn()
    print('tamponade integration tests passed (%d)' % len(tests))


if __name__ == '__main__':
    main()
