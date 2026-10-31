import inspect
import os
import sys
import tempfile
from pathlib import Path

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
for path in (SUTURA, REPO):
    if path not in sys.path:
        sys.path.insert(0, path)

from sutura_engine import triage as t

TOL = 0.0125


def _base(**over):
    d = {'si': 5002, 'si_stage1': 1065, 'holes': 0, 'nm': 0,
         'parts': 70, 'shape_max_mm': 2.0, 'shape_p95_mm': 0.0,
         'volume_mm3': 1000.0}
    d.update(over)
    return d


def _cand(**over):
    d = {'si': 0, 'si_stage1': 0, 'holes': 0, 'nm': 0,
         'parts': 8, 'shape_max_mm': 1.5, 'shape_p95_mm': 0.0,
         'volume_mm3': 1000.0}
    d.update(over)
    return d


def test_si_verdict_adopts_strictly_better():
    ok, why = t._si_verdict(_base(), _cand(), TOL)
    assert ok is True
    assert 'never-worse' in why


def test_si_verdict_rejects_not_lower():
    ok, why = t._si_verdict(_base(), _cand(si=5002), TOL)
    assert ok is False
    assert 'not strictly lower' in why


def test_si_verdict_rejects_parts_worse():
    ok, why = t._si_verdict(_base(parts=8), _cand(parts=9), TOL)
    assert ok is False
    assert 'parts worse' in why


def test_si_verdict_rejects_holes_and_nm_worse():
    ok, why = t._si_verdict(_base(), _cand(holes=1), TOL)
    assert ok is False
    assert 'holes worse' in why
    ok, why = t._si_verdict(_base(), _cand(nm=2), TOL)
    assert ok is False
    assert 'non-manifold worse' in why


def test_si_verdict_rejects_max_beyond_tolerance():
    ok, why = t._si_verdict(_base(shape_max_mm=1.0),
                            _cand(shape_max_mm=1.0 + TOL + 0.001), TOL)
    assert ok is False
    assert 'two-sided max worse' in why


def test_si_verdict_accepts_max_within_tolerance():
    ok, _why = t._si_verdict(_base(shape_max_mm=1.0),
                             _cand(shape_max_mm=1.0 + TOL), TOL)
    assert ok is True


def test_si_verdict_rejects_41732_p95_tradeoff():
    base = _base(si=34, parts=1, shape_max_mm=1.6351, shape_p95_mm=0.0)
    cand = _cand(si=0, parts=1, shape_max_mm=0.7652, shape_p95_mm=0.3837)
    ok, why = t._si_verdict(base, cand, TOL)
    assert ok is False
    assert 'two-sided p95 worse' in why


def test_si_verdict_rejects_37989_shape():
    base = _base(si=91, parts=1, shape_max_mm=0.0, shape_p95_mm=0.0)
    cand = _cand(si=84, parts=1, shape_max_mm=5.3914, shape_p95_mm=1.0241)
    ok, why = t._si_verdict(base, cand, TOL)
    assert ok is False
    assert 'two-sided max worse' in why


def test_si_verdict_rejects_37785_shape():
    base = _base(si=4262, parts=8, shape_max_mm=2.883, shape_p95_mm=0.0)
    cand = _cand(si=0, parts=10, shape_max_mm=10.63, shape_p95_mm=0.9148)
    ok, why = t._si_verdict(base, cand, TOL)
    assert ok is False
    assert 'two-sided max worse' in why or 'parts worse' in why


def test_si_verdict_rejects_1038441_volume_shrink():
    base = _base(si=158, parts=1, shape_max_mm=3.2152, shape_p95_mm=0.9305,
                 volume_mm3=13225.628486)
    cand = _cand(si=58, parts=1, shape_max_mm=3.2151, shape_p95_mm=0.5189,
                 volume_mm3=12433.479585)
    ok, why = t._si_verdict(base, cand, TOL)
    assert ok is False
    assert 'volume change' in why


def test_si_verdict_rejects_unmeasurable_shape():
    ok, why = t._si_verdict(_base(shape_max_mm=None),
                            _cand(shape_p95_mm=None), TOL)
    assert ok is False
    assert 'unmeasurable' in why
    ok, why = t._si_verdict(_base(), _cand(shape_p95_mm=None), None)
    assert ok is False
    assert 'tolerance' in why


def test_si_verdict_requires_measurable_volume_on_both_sides():
    ok, why = t._si_verdict(_base(volume_mm3=None), _cand(), TOL)
    assert ok is False
    assert 'volume unmeasurable' in why
    ok, why = t._si_verdict(_base(), _cand(volume_mm3=None), TOL)
    assert ok is False
    assert 'volume unmeasurable' in why
    ok, why = t._si_verdict(_base(volume_mm3=None),
                            _cand(volume_mm3=None), TOL)
    assert ok is False
    assert 'volume unmeasurable' in why


def test_si_verdict_volume_relative_to_baseline():
    ok, _why = t._si_verdict(_base(volume_mm3=1000.0),
                             _cand(volume_mm3=1004.0), TOL)
    assert ok is True
    ok, why = t._si_verdict(_base(volume_mm3=1000.0),
                            _cand(volume_mm3=1006.0), TOL)
    assert ok is False
    assert 'volume change' in why
    ok, why = t._si_verdict(_base(volume_mm3=0.0),
                            _cand(volume_mm3=1.0), TOL)
    assert ok is False
    assert 'baseline volume is zero' in why


def test_si_metrics_uses_report_geometry():
    result = {
        'stage1': {'self_intersections_remaining': 21,
                   'holes_remaining': 0,
                   'non_manifold_edges_remaining': 0,
                   'components': 1},
        'print_report': {'geometry': {'deviation_max_pct': 0.43,
                                      'volume_change_pct': 0.27}},
    }
    m = t._si_metrics(result)
    assert m == {'si': 21, 'holes': 0, 'nm': 0, 'parts': 1,
                 'dev_pct': 0.43, 'vol_pct': 0.27}


def test_si_candidate_nums_default_and_override():
    assert t._si_candidate_nums({}) == [t.SI_ESCALATION_METHOD_NUM]
    assert t._si_candidate_nums({'si_escalation_methods': [10, 17]}) == [10, 17]
    assert t._si_candidate_nums({'si_escalation_methods': 17}) == [17]
    assert t._si_candidate_nums({'si_escalation_methods': ['bad']}) == \
        [t.SI_ESCALATION_METHOD_NUM]


def test_unmeasurable_si_does_not_trigger(monkeypatch):
    monkeypatch.setattr(t, '_exact_si_path', lambda _p: None)
    base = {'stage1': {'self_intersections_remaining': 0,
                       'holes_remaining': 0,
                       'non_manifold_edges_remaining': 0,
                       'components': 1}}
    out = {}
    assert t._si_escalation('in.stl', 'out.stl', '.', {}, False, base, [],
                            lambda *a, **k: (_ for _ in ()).throw(AssertionError()),
                            lambda *a, **k: (_ for _ in ()).throw(AssertionError()),
                            '.stl') is None
    assert out == {}
    assert base['si_escalation']['triggered'] is False
    assert 'unmeasurable' in base['si_escalation']['reason']


def test_zero_si_does_not_trigger(monkeypatch):
    monkeypatch.setattr(t, '_exact_si_path', lambda _p: 0)
    base = {'stage1': {'self_intersections_remaining': 0,
                       'holes_remaining': 0,
                       'non_manifold_edges_remaining': 0,
                       'components': 1}}
    assert t._si_escalation('in.stl', 'out.stl', '.', {}, False, base, [],
                            lambda *a, **k: (_ for _ in ()).throw(AssertionError()),
                            lambda *a, **k: (_ for _ in ()).throw(AssertionError()),
                            '.stl') is None
    assert base['si_escalation']['triggered'] is False
    assert 'zero' in base['si_escalation']['reason']


def test_non_watertight_final_does_not_trigger(monkeypatch):
    monkeypatch.setattr(t, '_exact_si_path', lambda _p: 12)
    monkeypatch.setattr(t, '_si_file_facts', lambda _p, _o: {
        'holes': 2, 'nm': 1, 'parts': 1,
        'shape_max_mm': 0.0, 'shape_p95_mm': 0.0})
    base = {'stage1': {'self_intersections_remaining': 12,
                       'holes_remaining': 2,
                       'non_manifold_edges_remaining': 1,
                       'components': 1}}
    assert t._si_escalation('in.stl', 'out.stl', '.', {}, False, base, [],
                            lambda *a, **k: (_ for _ in ()).throw(AssertionError()),
                            lambda *a, **k: (_ for _ in ()).throw(AssertionError()),
                            '.stl') is None
    assert base['si_escalation']['triggered'] is False
    assert 'not watertight' in base['si_escalation']['reason']


def test_si_escalation_tries_candidates_in_order(monkeypatch, tmp_path):
    import sutura_engine.pipeline.work_estimate as we

    class _Method:
        def __init__(self, num):
            self.num = num
            self.id = 'm%d' % num
            self.name = 'M%d' % num
            self.kwargs = {}

        def available(self):
            return True, None

    class _Registry:
        def get_method(self, num):
            return _Method(num)

    out = str(tmp_path / 'out.stl')
    src = str(tmp_path / 'in.stl')

    monkeypatch.setattr(t, '_methods_mod', lambda: _Registry())
    monkeypatch.setattr(t, '_safe_load', lambda _p: [])
    monkeypatch.setattr(t, '_si_tolerance_mm', lambda _o: 0.05)
    monkeypatch.setattr(t, '_fallback_input_work', lambda _o: {
        'faces': 10, 'nm_edges': 0, 'nm_vertices': 0, 'holes': 0})
    monkeypatch.setattr(t, '_fallback_grid_work', lambda _o, _c: 0)
    monkeypatch.setattr(we, 'damage_work', lambda *a, **k: 0)
    monkeypatch.setattr(we, 'candidate_work', lambda *a, **k: 0)
    monkeypatch.setattr(we, 'graft_grid_work_budget', lambda: 1000)
    monkeypatch.setattr(we, 'fits_budget', lambda *a, **k: True)
    monkeypatch.setattr(we, 'work_class', lambda *a, **k: 'grid')
    monkeypatch.setattr(t, '_si_metrics', lambda _r: {
        'si': None, 'holes': 0, 'nm': 0, 'parts': 1,
        'dev_pct': None, 'vol_pct': 0.0})

    def _si(path):
        if path == out:
            return 10
        if 'm10' in path:
            return 5
        return 0

    def _facts(path, _in):
        if path == out:
            return {'holes': 0, 'nm': 0, 'parts': 1,
                    'volume_mm3': 1000.0,
                    'shape_max_mm': 0.0, 'shape_p95_mm': 0.0}
        if 'm10' in path:
            return {'holes': 0, 'nm': 0, 'parts': 1,
                    'volume_mm3': 1000.0,
                    'shape_max_mm': 0.0, 'shape_p95_mm': 1.0}
        return {'holes': 0, 'nm': 0, 'parts': 1,
                'volume_mm3': 1000.0,
                'shape_max_mm': 0.0, 'shape_p95_mm': 0.0}

    monkeypatch.setattr(t, '_exact_si_path', _si)
    monkeypatch.setattr(t, '_si_file_facts', _facts)

    def _attempt(_src, trial_out, _tmp, _ctx, _multi):
        with open(trial_out, 'wb') as f:
            f.write(trial_out.encode('utf-8'))
        return {'stage1': {}}

    def _eval(*_a, **_k):
        return {'watertight': True, 'holes': 0, 'non_manifold': 0,
                'hausdorff_rel': None, 'geom_change_pct': None,
                'guard_rejected': None, 'guard_reason': None,
                'si_flagged': False, 'adopt_guard': None, 'reason': 'ok'}

    base = {}
    tried = []
    trial = t._si_escalation(
        src, out, str(tmp_path), {'si_escalation_methods': [10, 17]},
        False, base, tried, _attempt, _eval, '.stl')
    assert trial is not None
    esc = trial['si_escalation']
    assert esc['num'] == 17 and esc['outcome'] == 'adopted'
    assert len(esc['candidates']) == 2
    assert esc['candidates'][0]['outcome'] == 'rejected'
    assert 'p95 worse' in esc['candidates'][0]['reason']
    assert esc['candidates'][1]['outcome'] == 'adopted'


def test_repair_with_methods_escalates_once_on_final_output(monkeypatch):
    calls = []
    result = {'stage1': {}, 'methods_tried': []}

    monkeypatch.setattr(t, '_auto', lambda *a, **k: result)
    monkeypatch.setattr(t, '_si_escalate_final',
                        lambda *a, **k: calls.append(a) or result)
    got = t.repair_with_methods('in.stl', 'out.stl', '.', methods=None)
    assert got is result
    assert len(calls) == 1
    assert calls[0][0] == 'in.stl' and calls[0][1] == 'out.stl'


def test_repair_with_methods_respects_si_off_and_auto_off(monkeypatch):
    calls = []
    monkeypatch.setattr(t, '_auto', lambda *a, **k: {'methods_tried': []})
    monkeypatch.setattr(t, '_si_escalate_final',
                        lambda *a, **k: calls.append(a) or {})
    off = t.repair_with_methods('in.stl', 'out.stl', '.', methods=None,
                                auto_escalation=False)
    assert off['si_escalation']['reason'] == 'disabled by user settings'
    t.repair_with_methods('in.stl', 'out.stl', '.', methods=None,
                          si_mode='off')
    t.repair_with_methods('in.stl', 'out.stl', '.', methods=None,
                          deep_repair='off')
    assert calls == []


class _MonkeyPatch:
    def __init__(self):
        self._undo = []

    def setattr(self, obj, name, value):
        old = getattr(obj, name)
        self._undo.append((obj, name, old))
        setattr(obj, name, value)

    def undo(self):
        for obj, name, old in reversed(self._undo):
            setattr(obj, name, old)
        self._undo = []


def _main():
    passed = 0
    for name, fn in sorted(globals().items()):
        if not (name.startswith('test_') and callable(fn)):
            continue
        params = inspect.signature(fn).parameters
        kwargs = {}
        mp = _MonkeyPatch()
        tmp = None
        if 'monkeypatch' in params:
            kwargs['monkeypatch'] = mp
        if 'tmp_path' in params:
            tmp = tempfile.TemporaryDirectory(prefix='si-test-')
            kwargs['tmp_path'] = Path(tmp.name)
        try:
            fn(**kwargs)
        finally:
            mp.undo()
            if tmp is not None:
                tmp.cleanup()
        passed += 1
        print('ok  %s' % name)
    print('si escalation tests passed: %d' % passed)


if __name__ == '__main__':
    _main()
