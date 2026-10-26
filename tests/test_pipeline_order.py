#!/usr/bin/env python3
"""Characterization tests for the deep-repair tier ORDER (P1b refactor gate).

This file exists so the "single order table" refactor (which lifts the tier
order out of ``triage._auto`` / ``deep_repair_ladder`` into
``sutura_engine.pipeline.order``) can be proven byte-identical: it pins the
observable order and tie-break behaviour of the two code paths that choose
between the last-resort tiers, with every tier faked so the assertions are
deterministic, and checks the table itself.

It covers:
  0. ``sutura_engine.pipeline.order`` -- table contents and the
     Graft-before-fTetWild re-order helper.
  1. ``triage._auto`` (driven through ``methods.repair_with_methods`` with a
     fake attempt/evaluate/rank + a fake method registry):
       - the Graft (#13) before fTetWild (#7) re-order guarantee even when the
         ranked recommendations list #7 first;
       - a faithful Graft is adopted immediately and fTetWild is never tried;
       - a fidelity-failing Graft is deferred and the lower healthy deviation
         wins (the in-loop fTetWild attempt and the after-loop deferred block);
       - the None-deviation edge semantics of the deferred comparison;
       - a Graft that failed the input-coverage gate is never adopted as a last
         resort (fTetWild unavailable or not superseding keeps stage-1), and a
         fidelity-ok but coverage-failed Graft is not adopted immediately.
  2. ``deep_repair_ladder`` (with a fake ``graft_tier`` / ``_ftetwild_tier``):
       - Graft faithful -> adopted, fTetWild skipped;
       - Graft fidelity-fail -> fTetWild tried, lower deviation wins;
       - input-coverage failure always lets fTetWild supersede;
       - the None-deviation edge semantics.

Usage: /opt/homebrew/Caskroom/miniforge/base/envs/sutura-env/bin/python tests/test_pipeline_order.py
"""
import os
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402

import methods  # noqa: E402
import repair  # noqa: E402
import sutura_engine.pipeline.pipeline as pipeline  # noqa: E402

OPEN_SAMPLE = os.path.join(REPO, 'tests', 'real-world-samples',
                           'thingi10k_100827.stl')
CUBE_V = [(x, y, z) for x in (0, 1) for y in (0, 1) for z in (0, 1)]
CUBE_T = [(0, 1, 3), (0, 3, 2), (4, 6, 7), (4, 7, 5), (0, 4, 5), (0, 5, 1),
          (2, 3, 7), (2, 7, 6), (0, 2, 6), (0, 6, 4), (1, 5, 7), (1, 7, 3)]


# --------------------------------------------------------------------------- #
# Part 1: triage._auto                                                         #
# --------------------------------------------------------------------------- #

class _FakeMethod:
    """Minimal RepairMethodProtocol stand-in for the auto policy."""

    def __init__(self, num, mid, name, marker, invents=False, available=True):
        self.num = num
        self.id = mid
        self.name = name
        self.kwargs = {'marker': marker}
        self.invents_geometry = invents
        self._available = available

    def available(self):
        return self._available, None


def _fake_registry(ft_available=True):
    reg = {
        3: _FakeMethod(3, 'full_mend', 'Full Mend', 'baseline'),
        2: _FakeMethod(2, 'local_mend', 'Local Mend', 'local'),
        13: _FakeMethod(13, 'graft', 'Graft', 'graft'),
        7: _FakeMethod(7, 'ftetwild', 'fTetWild', 'ftetwild',
                       available=ft_available),
    }
    return lambda num: reg.get(num)


def _rec(num):
    return methods.Recommendation(num, str(num), 'm%d' % num, 0.9, 'x',
                                  'mechanical')


class _AutoHarness:
    """Drives ``_auto`` with faked registry, attempt, evaluate and rank."""

    def __init__(self, rank_nums, graft_fidelity_ok, graft_hdev,
                 ft_watertight, ft_hdev, graft_coverage_ok=None,
                 ft_available=True):
        self.rank_nums = list(rank_nums)
        self.graft_fidelity_ok = graft_fidelity_ok
        self.graft_hdev = graft_hdev
        self.ft_watertight = ft_watertight
        self.ft_hdev = ft_hdev
        self.graft_coverage_ok = graft_coverage_ok
        self.ft_available = ft_available
        self.calls = []

    def _attempt(self, src, out, tmpdir, kwargs, multi):
        marker = kwargs.get('marker', 'baseline')
        self.calls.append(marker)
        with open(out, 'w') as f:
            f.write('ok')
        if marker == 'graft':
            g = {'fidelity_ok': self.graft_fidelity_ok,
                 'hausdorff_healthy': self.graft_hdev}
            if self.graft_coverage_ok is not None:
                g['coverage_ok'] = self.graft_coverage_ok
            return {'stage1': {}, 'graft': g}
        return {'stage1': {}}

    def _evaluate(self, result, path, multi, in_objs, method):
        base = {'watertight': False, 'holes': 1, 'non_manifold': 0,
                'hausdorff_rel': None, 'geom_change_pct': None,
                'reason': 'holes=1 remain'}
        num = getattr(method, 'num', None)
        if num == 13:
            return {'watertight': True, 'holes': 0, 'non_manifold': 0,
                    'hausdorff_rel': self.graft_hdev,
                    'geom_change_pct': None, 'reason': ''}
        if num == 7 and self.ft_watertight:
            return {'watertight': True, 'holes': 0, 'non_manifold': 0,
                    'hausdorff_rel': self.ft_hdev,
                    'geom_change_pct': None, 'reason': ''}
        return base

    def _rank(self, analysis, top_n=6):
        return [_rec(n) for n in self.rank_nums]

    def run(self):
        real = (methods._attempt, methods._evaluate, methods.rank_methods,
                methods.get_method)
        methods._attempt = self._attempt
        methods._evaluate = self._evaluate
        methods.rank_methods = self._rank
        methods.get_method = _fake_registry(self.ft_available)
        try:
            with tempfile.TemporaryDirectory(prefix='sutura-order-') as td:
                src = os.path.join(td, 'in.stl')
                with open(src, 'w') as f:
                    f.write('x')
                return methods.repair_with_methods(
                    src, os.path.join(td, 'out.stl'), td, None, mode='auto',
                    deep_repair='full', ftetwild='auto')
        finally:
            (methods._attempt, methods._evaluate, methods.rank_methods,
             methods.get_method) = real


def test_auto_prefers_graft_before_ftetwild():
    """Graft (#13) is attempted before fTetWild (#7) even when the ranked list
    puts #7 first, and a faithful Graft is adopted without trying fTetWild."""
    h = _AutoHarness(rank_nums=[7, 13], graft_fidelity_ok=True,
                     graft_hdev=0.001, ft_watertight=True, ft_hdev=0.0005)
    res = h.run()
    assert h.calls == ['baseline', 'graft'], h.calls
    assert res['method_used']['num'] == 13, res['method_used']
    assert res['method_reached_watertight'] is True


def test_auto_in_loop_ftetwild_adopted_after_graft_deferral():
    """With the re-order in effect the loop visits Graft (deferred, fidelity
    fail) and then fTetWild; an in-loop watertight fTetWild is adopted without
    consulting the deferred deviation."""
    h = _AutoHarness(rank_nums=[7, 13], graft_fidelity_ok=False,
                     graft_hdev=0.05, ft_watertight=True, ft_hdev=0.02)
    res = h.run()
    assert h.calls == ['baseline', 'graft', 'ftetwild'], h.calls
    assert res['method_used']['num'] == 7, res['method_used']
    assert res['method_reached_watertight'] is True


def test_auto_deferred_graft_tiebreak_lower_ft_deviation_wins():
    """When fTetWild is not in the ranked list, the deferred Graft and a
    freshly-run fTetWild are compared: the lower healthy deviation wins."""
    h = _AutoHarness(rank_nums=[13], graft_fidelity_ok=False,
                     graft_hdev=0.05, ft_watertight=True, ft_hdev=0.02)
    res = h.run()
    assert h.calls == ['baseline', 'graft', 'ftetwild'], h.calls
    assert res['method_used']['num'] == 7, res['method_used']
    assert res['method_used']['note'] == 'lower healthy deviation than Graft'


def test_auto_deferred_graft_tiebreak_higher_ft_deviation_keeps_graft():
    h = _AutoHarness(rank_nums=[13], graft_fidelity_ok=False,
                     graft_hdev=0.05, ft_watertight=True, ft_hdev=0.08)
    res = h.run()
    assert h.calls == ['baseline', 'graft', 'ftetwild'], h.calls
    assert res['method_used']['num'] == 13, res['method_used']


def test_auto_deferred_graft_none_deviation_edges():
    """Pins the two None edges of the UNIFIED (ladder) tie-break:
    - a None Graft deviation now KEEPS Graft (it was: fTetWild won this edge
      under the old _auto rule; this is the deliberate normalization);
    - a None fTetWild deviation never beats a present Graft deviation."""
    h = _AutoHarness(rank_nums=[13], graft_fidelity_ok=False,
                     graft_hdev=None, ft_watertight=True, ft_hdev=0.02)
    assert h.run()['method_used']['num'] == 13

    h = _AutoHarness(rank_nums=[13], graft_fidelity_ok=False,
                     graft_hdev=0.05, ft_watertight=True, ft_hdev=None)
    assert h.run()['method_used']['num'] == 13


def test_auto_coverage_failed_graft_never_adopted_without_ftetwild():
    """C1(a): a Graft that failed the input-coverage gate is never a last
    resort. With fTetWild unavailable the stage-1 output is kept and the
    rejection is stated on the report instead of silently dropping Graft."""
    h = _AutoHarness(rank_nums=[13], graft_fidelity_ok=False,
                     graft_coverage_ok=False, graft_hdev=0.05,
                     ft_watertight=True, ft_hdev=0.02, ft_available=False)
    res = h.run()
    assert h.calls == ['baseline', 'graft'], h.calls
    assert res['method_used']['num'] == 3, res['method_used']
    assert res['method_reached_watertight'] is False
    assert 'Graft rejected' in res['method_used']['note']
    assert res['graft']['adopted'] is False
    assert res['graft']['reject_reason'] == 'coverage'
    assert 'input coverage' in res['graft']['reason']


def test_auto_coverage_failed_graft_rejected_when_ftetwild_no_help():
    """C1(a): fTetWild was tried but is not watertight, so it cannot supersede;
    the coverage-failed Graft is still rejected and stage-1 kept."""
    h = _AutoHarness(rank_nums=[13], graft_fidelity_ok=False,
                     graft_coverage_ok=False, graft_hdev=0.05,
                     ft_watertight=False, ft_hdev=None)
    res = h.run()
    assert h.calls == ['baseline', 'graft', 'ftetwild'], h.calls
    assert res['method_used']['num'] == 3, res['method_used']
    assert res['method_reached_watertight'] is False
    assert res['graft']['reject_reason'] == 'coverage'


def test_auto_faithful_graft_coverage_fail_not_adopted_immediately():
    """C1(b): a fidelity-ok Graft with an explicit coverage failure is NOT
    adopted at once; an available watertight fTetWild supersedes it even with a
    higher deviation (a coverage failure always lets fTetWild win)."""
    h = _AutoHarness(rank_nums=[13], graft_fidelity_ok=True,
                     graft_coverage_ok=False, graft_hdev=0.01,
                     ft_watertight=True, ft_hdev=0.5)
    res = h.run()
    assert h.calls == ['baseline', 'graft', 'ftetwild'], h.calls
    assert res['method_used']['num'] == 7, res['method_used']
    assert res['method_reached_watertight'] is True


# --------------------------------------------------------------------------- #
# Part 2: deep_repair_ladder                                                   #
# --------------------------------------------------------------------------- #

def _cube_arrays():
    return (np.asarray(CUBE_V, np.float64), np.asarray(CUBE_T, np.int64))


def _cube_after(marker):
    return {'non_two_manifold_edges': 0, '_tier': marker}


def _run_ladder(graft_body, ft_body):
    """Run deep_repair_ladder on the open sample with faked tiers.

    ``graft_body`` is the ``stats['graft']`` the fake Graft records (or None to
    forbid the Graft tier); ``ft_body`` is the fake ``experimental_ftetwild``
    report.  Returns ``(stats, calls, after)``.
    """
    v, t = _load(OPEN_SAMPLE)
    after = {'non_two_manifold_edges': 0}
    stats = {}
    calls = []

    def fake_graft(vv, tt, iv, it, a, st, tmpdir, **kw):
        calls.append('graft')
        assert graft_body is not None, 'Graft tier must not run'
        st['graft'] = dict(graft_body)
        cv, ct = _cube_arrays()
        return cv, ct, _cube_after('graft')

    def fake_ft(vv, tt, iv, it, a, st, tmpdir, ftetwild, spec=None):
        calls.append('ftetwild')
        st['experimental_ftetwild'] = dict(ft_body)
        # Like the real tier, an unadopted fTetWild leaves the mesh unchanged.
        if not ft_body.get('adopted'):
            return vv, tt, a
        cv, ct = _cube_arrays()
        return cv, ct, _cube_after('ftetwild')

    saved = (pipeline.graft_tier, pipeline._ftetwild_tier)
    pipeline.graft_tier, pipeline._ftetwild_tier = fake_graft, fake_ft
    try:
        _cv, _ct, out_after = pipeline.deep_repair_ladder(
            v, t, v, t, after, stats, tempfile.gettempdir(), mode='full',
            ftetwild='auto', spec=repair.triage.PRESETS['balanced'],
            graft='auto')
    finally:
        pipeline.graft_tier, pipeline._ftetwild_tier = saved
    return stats, calls, out_after


def _load(path):
    from sutura_engine.pipeline.mesh_io import load_mesh
    return load_mesh(path)


def test_ladder_faithful_graft_skips_ftetwild():
    stats, calls, after = _run_ladder(
        {'adopted': True, 'fidelity_ok': True, 'coverage_ok': True,
         'hausdorff_healthy': 0.001},
        {'ran': True, 'adopted': True, 'hausdorff_rel': 0.02})
    assert calls == ['graft'], calls
    assert after['_tier'] == 'graft'
    dr = stats['deep_repair']
    assert dr['tiers_run'] == ['graft'], dr
    assert stats['experimental_ftetwild'] is False
    assert stats['graft']['adopted'] is True


def test_ladder_graft_coverage_failure_lets_ftetwild_supersede():
    """A coverage failure is held aside (never preferred by the deviation
    tie-break) and fTetWild supersedes even with a HIGHER deviation."""
    stats, calls, after = _run_ladder(
        {'adopted': True, 'fidelity_ok': True, 'coverage_ok': False,
         'hausdorff_healthy': 0.05},
        {'ran': True, 'adopted': True, 'hausdorff_rel': 0.08})
    assert calls == ['graft', 'ftetwild'], calls
    assert after['_tier'] == 'ftetwild'
    dr = stats['deep_repair']
    assert dr['tiers_run'] == ['ftetwild', 'graft'], dr
    assert stats['graft']['adopted'] is False
    assert 'coverage below threshold' in stats['graft']['reason']


def test_ladder_lower_ftetwild_deviation_wins():
    stats, calls, after = _run_ladder(
        {'adopted': True, 'fidelity_ok': False, 'coverage_ok': True,
         'hausdorff_healthy': 0.05},
        {'ran': True, 'adopted': True, 'hausdorff_rel': 0.02})
    assert calls == ['graft', 'ftetwild'], calls
    assert after['_tier'] == 'ftetwild'
    assert stats['graft']['adopted'] is False
    assert 'lower healthy deviation' in stats['graft']['reason']


def test_ladder_higher_ftetwild_deviation_keeps_graft():
    stats, calls, after = _run_ladder(
        {'adopted': True, 'fidelity_ok': False, 'coverage_ok': True,
         'hausdorff_healthy': 0.05},
        {'ran': True, 'adopted': True, 'hausdorff_rel': 0.08})
    assert calls == ['graft', 'ftetwild'], calls
    assert after['_tier'] == 'graft'
    dr = stats['deep_repair']
    assert dr['tiers_run'] == ['graft'], dr
    assert stats['graft']['adopted'] is True
    assert stats['graft']['preferred_over_ftetwild'] is True


def test_ladder_unadopted_ftetwild_keeps_stage1():
    """Pins the current semantics when fTetWild is tried but not adopted: the
    mesh stays at the stage-1 input and the held-aside Graft is recorded as
    superseded (it is NOT restored)."""
    stats, calls, after = _run_ladder(
        {'adopted': True, 'fidelity_ok': False, 'coverage_ok': True,
         'hausdorff_healthy': 0.05},
        {'ran': True, 'adopted': False, 'hausdorff_rel': None})
    assert calls == ['graft', 'ftetwild'], calls
    assert '_tier' not in after
    assert stats['graft']['adopted'] is False
    assert 'superseded by fTetWild' in stats['graft']['reason']


def test_ladder_none_deviation_edges():
    """Both deviations None (fTetWild still adopted) keeps Graft: the ladder
    tie-break requires two present deviations to prefer fTetWild."""
    stats, calls, after = _run_ladder(
        {'adopted': True, 'fidelity_ok': False, 'coverage_ok': True,
         'hausdorff_healthy': None},
        {'ran': True, 'adopted': True, 'hausdorff_rel': None})
    assert calls == ['graft', 'ftetwild'], calls
    assert after['_tier'] == 'graft'
    assert stats['graft']['adopted'] is True


# --------------------------------------------------------------------------- #
# Part 0: the single order table (sutura_engine.pipeline.order)                #
# --------------------------------------------------------------------------- #

def test_order_table_contents():
    import sutura_engine.pipeline.order as order
    assert order.DEEP_TIER_ORDER == ('local', 'graft', 'dressing', 'splint',
                                     'coaptation', 'ftetwild')
    assert order.FINAL_TIER_PRECEDENCE == ('ftetwild', 'graft', 'dressing',
                                           'splint', 'coaptation', 'local')
    assert order.method_num('local') == 2
    assert order.method_num('graft') == 13
    assert order.method_num('ftetwild') == 7
    assert order.method_num('dressing') == 16
    assert order.method_num('splint') == 18
    assert order.method_num('coaptation') == 19
    assert order.GRAFT_METHOD_NUM == 13
    assert order.FTETWILD_METHOD_NUM == 7


def test_order_graft_before_ftetwild_helper():
    import sutura_engine.pipeline.order as order

    class _Rec:
        def __init__(self, num):
            self.num = num

    assert order.graft_precedes_ftetwild('graft', 'ftetwild') is True
    assert order.graft_precedes_ftetwild('ftetwild', 'graft') is False

    # fTetWild ranked first -> Graft is moved to fTetWild's position.
    recs = [_Rec(7), _Rec(2), _Rec(13)]
    out = order.order_graft_before_ftetwild(recs)
    assert [r.num for r in out] == [13, 7, 2]
    # The input list is not mutated.
    assert [r.num for r in recs] == [7, 2, 13]
    # Already ordered, or one side absent -> order preserved.
    assert [r.num for r in order.order_graft_before_ftetwild(
        [_Rec(13), _Rec(2), _Rec(7)])] == [13, 2, 7]
    assert [r.num for r in order.order_graft_before_ftetwild(
        [_Rec(7)])] == [7]


def test_ftetwild_supersedes_graft_helper():
    """The one shared tie-break: fTetWild wins only when both deviations are
    known and lower, or on an input-coverage failure; otherwise Graft is kept."""
    import sutura_engine.pipeline.order as order
    f = order.ftetwild_supersedes_graft
    assert f(0.02, 0.05) is True          # both present, ft lower -> fTetWild
    assert f(0.05, 0.02) is False         # both present, ft higher -> Graft
    assert f(0.02, 0.02) is False         # equal -> keep Graft
    assert f(None, 0.05) is False         # ft deviation unknown -> keep Graft
    assert f(0.02, None) is False         # graft deviation unknown -> keep Graft
    assert f(None, None) is False
    assert f(0.09, 0.05, coverage_failed=True) is True   # coverage override
    assert f(None, None, coverage_failed=True) is True


def test_hausdorff_samples_match_at_default_intensity():
    """C5: the unified tie-break compares two deviations, so at the default
    intensity the _evaluate side and the ladder fTetWild tier must sample the
    same number of points (they diverge only at thorough/extreme)."""
    import repair
    from sutura_engine import triage
    spec = triage.PRESETS[triage.DEFAULT_INTENSITY]
    assert repair.HAUSDORFF_SAMPLES == spec.ftetwild_hausdorff_samples


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('pipeline-order tests passed')
