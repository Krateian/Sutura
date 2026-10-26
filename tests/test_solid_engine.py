"""P6: solid_engine resolver and ladder slot."""
import json
import os

import numpy as np

import repair
from sutura_engine.pipeline import pipeline, presets


def test_resolver_precedence(tmp_path):
    cfg_path = tmp_path / 'config.json'
    cfg_path.write_text(json.dumps({'solid_engine': 'cdt'}))
    r = presets.resolve_solid_engine
    assert r('ftetwild', environ={}, config_path=str(cfg_path)) == 'ftetwild'
    assert r(None, environ={'SUTURA_SOLID_ENGINE': 'ftetwild'},
             config_path=str(cfg_path)) == 'ftetwild'
    assert r(None, environ={}, config_path=str(cfg_path)) == 'cdt'
    cfg_path.write_text(json.dumps({'solid_engine': 'bogus'}))
    assert r(None, environ={}, config_path=str(cfg_path)) == 'ftetwild'


def _cube():
    v = np.asarray([
        [0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
        [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1],
    ], np.float64)
    t = np.asarray([
        [0, 1, 2], [0, 2, 3], [4, 6, 5], [4, 7, 6],
        [0, 4, 5], [0, 5, 1], [1, 5, 6], [1, 6, 2],
        [2, 6, 7], [2, 7, 3], [3, 7, 4], [3, 4, 0],
    ], np.int64)
    return v, t


def _run(monkeypatch, engine, cdt_adopted):
    v, t = _cube()
    calls = []

    def fake_cdt(vv, tt, iv, it, a, st, tmpdir, **kw):
        calls.append('cdt')
        st['cdt'] = {'adopted': cdt_adopted, 'reason': 'fake',
                     'holes': 0 if cdt_adopted else 1,
                     'non_manifold': 0, 'parts': 1}
        if cdt_adopted:
            return v, t, {'non_two_manifold_edges': 0}
        return vv, tt, a

    def fake_ft(vv, tt, iv, it, a, st, tmpdir, ftetwild, spec=None):
        calls.append('ftetwild')
        st['experimental_ftetwild'] = {'ran': True, 'adopted': True}
        return v, t, {'non_two_manifold_edges': 0}

    monkeypatch.setattr(pipeline, 'cdt_tier', fake_cdt)
    monkeypatch.setattr(pipeline, '_ftetwild_tier', fake_ft)
    stats = {}
    pipeline.deep_repair_ladder(
        v, t, v, t, {'non_two_manifold_edges': 1}, stats, '/tmp',
        mode='full', ftetwild='auto',
        spec=repair.triage.PRESETS['balanced'], graft=False, dressing=False,
        splint=False, coaptation=False, solid_engine=engine)
    return calls, stats


def test_cdt_runs_after_coaptation_before_ftetwild(monkeypatch):
    calls, stats = _run(monkeypatch, 'cdt', True)
    assert calls == ['cdt'], calls
    assert stats['solid_engine'] == {
        'selected': 'cdt', 'produced': 'cdt', 'reason': 'fake'}
    assert stats['experimental_ftetwild'] is False


def test_cdt_failure_falls_through_to_ftetwild(monkeypatch):
    calls, stats = _run(monkeypatch, 'cdt', False)
    assert calls == ['cdt', 'ftetwild'], calls
    assert stats['solid_engine']['selected'] == 'cdt'
    assert stats['solid_engine']['produced'] == 'ftetwild'


def test_ftetwild_engine_does_not_call_cdt(monkeypatch):
    calls, stats = _run(monkeypatch, 'ftetwild', True)
    assert calls == ['ftetwild'], calls
    assert stats['solid_engine']['selected'] == 'ftetwild'
    assert stats['solid_engine']['produced'] == 'ftetwild'


def test_resolver_uses_existing_settings_loader(monkeypatch, tmp_path):
    import updater
    monkeypatch.setattr(updater, 'CONFIG_DIR', str(tmp_path))
    monkeypatch.setattr(updater, 'CONFIG_PATH', str(tmp_path / 'config.json'))
    updater.save_config({'solid_engine': 'cdt'})
    assert presets.resolve_solid_engine(None, environ={}) == 'cdt'


def test_cdt_tier_real_100827():
    import pytest
    pytest.importorskip('sutura_geom')
    import sutura_geom
    if not hasattr(sutura_geom, 'cdt3d_solid'):
        pytest.skip('cdt3d_solid not built')
    import tempfile
    from sutura_engine.pipeline.mesh_io import load_mesh
    from sutura_engine.pipeline.tiers.cdt import cdt_tier
    v, t = load_mesh('tests/real-world-samples/thingi10k_100827.stl')
    stats = {}
    ov, ot, _after = cdt_tier(
        v, t, v, t, {'non_two_manifold_edges': 1}, stats, tempfile.gettempdir(),
        spec=repair.triage.PRESETS['balanced'])
    rec = stats['cdt']
    assert rec['ran'] and rec['adopted'], rec
    assert rec['parts'] == 1
    assert rec['holes'] == 0
    assert rec['non_manifold'] == 0
    assert rec['coverage_certified'] is False
    assert rec['wall_seconds'] is not None
    assert len(ot) > 0
