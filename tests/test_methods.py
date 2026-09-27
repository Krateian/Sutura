#!/usr/bin/env python3
"""Fast unit tests for the repair method registry / analysis / ranking (P0).

Synthetic meshes only (numpy cube, cube with a removed face, an open
hemisphere); no corpus runs. Covers the fixed registry numbering, the
register_method hook, template confidences, rank_methods filtering, the
one-sided Hausdorff helper, analyze_mesh, and the new CLI flags.

Usage: ~/.local/share/sutura/venv/bin/python tests/test_methods.py
(any interpreter with numpy + pymeshlab + trimesh; on macOS the conda
`sutura-env`).
"""
import json
import os
import struct
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
REPAIR_PY = os.path.join(SUTURA, 'repair.py')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402

EXPECTED = {
    1: 'fast', 2: 'deep_local', 3: 'deep_full', 4: 'join_components',
    5: 'autorefine', 6: 'indirect_autorefine', 7: 'ftetwild',
    8: 'poisson_close', 9: 'flat_back_close', 10: 'proxy_template',
    11: 'repeat_auto', 12: 'repeat_manual',
}
FAMILIES = {'clean', 'topology', 'si', 'envelope', 'closing', 'template',
            'pattern'}


def _env(tmp):
    env = dict(os.environ, SUTURA_DIR=SUTURA)
    env.pop('SUTURA_INTENSITY', None)
    return env


def _write_stl(path, verts, tris):
    with open(path, 'wb') as f:
        f.write(b'methods-test'.ljust(80, b'\0'))
        f.write(struct.pack('<I', len(tris)))
        for a, b, c in tris:
            f.write(struct.pack('<3f', 0, 0, 0))
            f.write(struct.pack('<3f', *verts[a]))
            f.write(struct.pack('<3f', *verts[b]))
            f.write(struct.pack('<3f', *verts[c]))
            f.write(struct.pack('<H', 0))


def _cube():
    v = np.array([
        (0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0),
        (0, 0, 1), (1, 0, 1), (1, 1, 1), (0, 1, 1)], dtype=np.float32)
    t = np.array([
        (0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7),
        (0, 1, 5), (0, 5, 4), (1, 2, 6), (1, 6, 5),
        (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7)], dtype=np.int32)
    return v, t


def _hemisphere():
    import trimesh
    m = trimesh.creation.icosphere(subdivisions=2)
    v = np.asarray(m.vertices, dtype=np.float32)
    t = np.asarray(m.faces, dtype=np.int64)
    keep = np.array([v[f, 2].mean() >= 0 for f in t])
    return v, t[keep].astype(np.int32)


def _run(args, env=None):
    return subprocess.run([sys.executable, REPAIR_PY] + args,
                          capture_output=True, text=True, timeout=600, env=env)


def _json(r):
    return json.loads(r.stdout.strip().splitlines()[-1])


# --- registry ---------------------------------------------------------------

def test_modules_import_without_heavy_deps(tmp):
    code = ("import sys; sys.path.insert(0, %r); import methods, "
            "object_analysis, templates; "
            "assert 'pymeshlab' not in sys.modules, 'pymeshlab leaked'; "
            "assert 'trimesh' not in sys.modules, 'trimesh leaked'"
            % SUTURA)
    r = subprocess.run([sys.executable, '-c', code], capture_output=True,
                       text=True)
    assert r.returncode == 0, r.stderr


def test_registry_fixed_numbers(tmp):
    import methods
    assert [m.num for m in methods.all_methods()] == list(range(1, 13))
    for m in methods.all_methods():
        assert m.id == EXPECTED[m.num], (m.num, m.id)
        assert m.family in FAMILIES, (m.num, m.family)
        ok, reason = m.available()
        assert isinstance(ok, bool)
        assert (reason is None) if ok else isinstance(reason, str)


def test_register_method_hook(tmp):
    import methods
    dummy = methods.RepairMethod(99, 'dummy', 'Dummy', 'test', 'clean')
    methods.register_method(dummy)
    try:
        assert methods.get_method(99) is dummy
        assert methods.get_method(99) in methods.all_methods()
    finally:
        methods.unregister_method(99)
    assert methods.get_method(99) is None


def test_placeholders_unavailable(tmp):
    import methods
    for num in (11, 12):
        ok, reason = methods.get_method(num).available()
        assert ok is False, num
        assert 'not implemented' in reason, (num, reason)
    assert methods.get_method(12).needs_user_input is True


def test_closing_methods_available(tmp):
    import methods
    # 8/9 need closing.py, 10 needs proxy_repair + scipy + trimesh; in the test
    # environment all are installed, so each reports available with no reason.
    for num in (8, 9, 10):
        ok, reason = methods.get_method(num).available()
        assert ok is True, (num, reason)
        assert methods.get_method(num).invents_geometry is True, num


def test_kwargs_mapping(tmp):
    import methods
    assert methods.get_method(1).kwargs['deep_repair'] == 'off'
    assert methods.get_method(1).kwargs['ftetwild'] is False
    assert methods.get_method(2).kwargs['deep_repair'] == 'local'
    assert methods.get_method(3).kwargs['deep_repair'] == 'full'
    assert methods.get_method(4).kwargs['join_components'] is True
    assert methods.get_method(5).kwargs['autorefine'] is True
    assert methods.get_method(6).kwargs['indirect_autorefine'] is True
    assert methods.get_method(7).kwargs['ftetwild'] is True
    assert methods.get_method(7).invents_geometry is True
    assert methods.get_method(8).kwargs['closing'] == 'poisson'
    assert methods.get_method(9).kwargs['closing'] == 'flat_back'
    assert methods.get_method(10).kwargs['proxy_template'] is True
    for num in (8, 9, 10):
        assert methods.get_method(num).kwargs['deep_repair'] == 'off'
        assert methods.get_method(num).kwargs['ftetwild'] is False


# --- templates / ranking ----------------------------------------------------

def test_templates_confidence(tmp):
    import templates
    from object_analysis import ObjectAnalysis as A
    mech = A(mesh_type='mechanical', type_confidence=0.9)
    assert templates.by_id('mechanical').confidence(mech) > 0.8
    assert templates.by_id('organic').confidence(mech) == 0.0
    scan = A(open_area_ratio=0.08, boundary_loops=1, largest_loop_ratio=0.5)
    assert templates.by_id('single_side_scan').confidence(scan) > 0.5
    heavy = A(self_intersections=1000)
    assert templates.by_id('dense_scan_heavy_si').confidence(heavy) > 0.0
    # repetition_score is a P5 stub: the template stays silent.
    assert templates.by_id('repeated_pattern').confidence(A()) == 0.0


def test_rank_methods_filtering(tmp):
    import methods
    from object_analysis import ObjectAnalysis as A
    a = A(mesh_type='mechanical', type_confidence=0.9, boundary_loops=1,
          open_area_ratio=0.05, largest_loop_ratio=0.4)
    recs = methods.rank_methods(a)
    nums = [r.num for r in recs]
    assert 12 not in nums, 'needs_user_input must be excluded from ranking'
    for num in nums:
        assert methods.get_method(num).available()[0], num
    # scores are descending and in range
    scores = [r.score for r in recs]
    assert scores == sorted(scores, reverse=True), scores
    assert all(0.0 <= s <= 1.0 for s in scores), scores


def test_rank_methods_prefers_si_methods_for_si_mesh(tmp):
    import methods
    from object_analysis import ObjectAnalysis as A
    a = A(mesh_type='mechanical', type_confidence=0.9, self_intersections=900)
    nums = [r.num for r in methods.rank_methods(a)]
    assert 5 in nums or 6 in nums, nums


def test_rank_methods_closing_signals(tmp):
    """The closing signals route methods 8/9/10 (P-INT)."""
    import methods
    from object_analysis import ObjectAnalysis as A
    scan = A(single_side_score=0.9, boundary_loops=1, open_area_ratio=0.05)
    scores = {r.num: r.score for r in methods.rank_methods(scan)}
    assert scores.get(8, 0.0) > 0, scores
    relief = A(relief_score=0.9, boundary_loops=1, open_area_ratio=0.05)
    scores = {r.num: r.score for r in methods.rank_methods(relief)}
    assert scores.get(9, 0.0) > 0, scores
    broken = A(boundary_loops=3, non_manifold_edges=2, components=2,
               open_area_ratio=0.1)
    scores = {r.num: r.score for r in methods.rank_methods(broken)}
    assert scores.get(10, 0.0) > 0, scores
    # a clean mesh must not attract any of the three
    clean = A(single_side_score=0.0, relief_score=0.0)
    assert methods._score('poisson_close', clean)[0] == 0.0
    assert methods._score('flat_back_close', clean)[0] == 0.0
    assert methods._score('proxy_template', clean)[0] == 0.0


def test_single_side_scan_prefers_poisson(tmp):
    """The single-sided-scan template prefers #8 (Poisson close), not #9
    (flat-back, which belongs to the relief template) -- 3.1."""
    import templates
    scan = templates.by_id('single_side_scan')
    assert scan.preferred[0] == 8, scan.preferred
    assert 9 not in scan.preferred, scan.preferred
    assert templates.by_id('relief').preferred[0] == 9


def test_get_method_none_is_safe(tmp):
    """get_method(None / non-integer) returns None instead of raising -- 2.2."""
    import methods
    assert methods.get_method(None) is None
    assert methods.get_method('nope') is None
    assert methods.get_method(1).id == 'fast'


def test_external_engines_are_separate(tmp):
    import methods
    from object_analysis import ObjectAnalysis as A
    recs = methods.rank_methods(A(mesh_type='mechanical'))
    engines = methods.external_engines()
    assert isinstance(engines, list)
    # engines never appear in the method ranking namespace
    for e in engines:
        assert 'num' not in e, e


# --- analysis / Hausdorff ---------------------------------------------------

def test_analyze_mesh_cube_and_hole(tmp):
    import object_analysis
    v, t = _cube()
    a = object_analysis.analyze_mesh(v, t)
    d = object_analysis.to_dict(a)
    assert d['faces'] == 12 and d['boundary_loops'] == 0, d
    assert d['non_manifold_edges'] == 0, d
    json.dumps(d)  # JSON-safe
    a2 = object_analysis.analyze_mesh(v, t[2:])  # remove one face -> one loop
    assert a2.boundary_loops == 1, a2
    assert a2.open_area_ratio > 0, a2


def test_analyze_mesh_open_hemisphere(tmp):
    import object_analysis
    v, t = _hemisphere()
    a = object_analysis.analyze_mesh(v, t)
    assert a.boundary_loops >= 1, a.boundary_loops
    assert a.largest_loop_ratio > 0, a.largest_loop_ratio


def test_one_sided_hausdorff_identical(tmp):
    import methods
    v, t = _cube()
    mx, mean = methods.one_sided_hausdorff(v, t, v, t)
    assert mx == 0.0 and mean == 0.0, (mx, mean)


def test_weld_reload_equivalent_flags_reload_non_manifold(tmp):
    """A mesh index-watertight in memory can stop being watertight after the
    STL float32 round-trip (coincident positions weld on reload). The
    generative guard must judge the welded form (P-FIX)."""
    import defects
    import methods
    repair = methods._repair_mod()
    v = np.array([
        (0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, 1), (0, -1, 0),
        (1e-46, 0, 0),  # underflows to (0, 0, 0) in float32 -> welds to v0
    ], dtype=np.float64)
    t = np.array([[0, 1, 2], [0, 1, 3], [5, 1, 4]], dtype=np.int64)
    # in-memory index topology: edge (0,1) is used twice, no non-manifold edge
    assert len(defects.detect(v, t)['non_manifold']) == 0
    wv, wt = repair.weld_reload_equivalent(v, t)
    assert len(wv) == 5 and len(wt) == 3, (len(wv), len(wt))
    # after welding the coincident vertex the shared edge has three faces
    assert len(defects.detect(wv, wt)['non_manifold']) == 1
    holes, nm = methods._strict_holes_nm([(None, v, t)], weld=True)
    assert nm == 1, nm
    holes, nm = methods._strict_holes_nm([(None, v, t)], weld=False)
    assert nm == 0, nm


def _two_cubes_sharing_edge():
    """Two unit cubes sharing an edge, with that edge stored as separate
    vertices in each cube (indices 0..7 and 8..15). The in-memory index
    topology is two closed 2-manifolds (0 holes, 0 nm); after the STL
    save/reload weld the shared edge has four incident faces (nm = 1)."""
    va, ta = _cube()
    vb = va + np.array([1, 1, 0], dtype=np.float32)
    tb = ta + 8
    v = np.vstack([va, vb]).astype(np.float32)
    t = np.vstack([ta, tb]).astype(np.int32)
    return v, t


def test_reload_strict_holes_nm_flags_weld_only(tmp):
    """reload_strict_holes_nm judges the save/reload-equivalent mesh: two
    closed cubes that only share an edge after the float32 weld report nm=0
    in memory but nm=1 after the weld (P-HONEST)."""
    import defects
    import methods
    repair = methods._repair_mod()
    v, t = _two_cubes_sharing_edge()
    d = defects.detect(v, t)
    assert len(d['holes']) == 0 and len(d['non_manifold']) == 0, d
    assert repair.reload_strict_holes_nm(v, t) == (0, 1)


def test_enforce_reload_verdict_downgrades_false_watertight(tmp):
    """A report that claims watertight but whose saved mesh is not
    strict-watertight after the reload weld must be downgraded; the fields
    classify() reads are rewritten so the category can no longer be
    watertight, and a genuinely watertight report is left untouched
    (P-HONEST)."""
    import classification
    import methods
    repair = methods._repair_mod()

    def fake_watertight():
        return {'stage1': {'two_manifold': True, 'holes_remaining': 0},
                'stage2': {'ok': True}}

    v, t = _two_cubes_sharing_edge()
    rep = fake_watertight()
    assert repair.enforce_reload_verdict(rep, v, t) is True
    assert rep['stage1']['two_manifold'] is False, rep
    assert rep['reload_watertight'] is False, rep
    assert rep['stage2']['watertight_after_reload'] is False, rep
    category, _issues, _key = classification.classify(rep)
    assert category == 'warning', (category, rep)

    # a clean, single closed cube is genuinely watertight: no report change
    clean = fake_watertight()
    cv, ct = _cube()
    assert repair.enforce_reload_verdict(clean, cv, ct) is False
    assert clean == fake_watertight(), clean


def test_evaluate_requires_stage2_for_watertight(tmp):
    """_evaluate must not claim watertight when stage 2 did not confirm the
    solid (5.1): a stage-1-closed mesh with stage 2 skipped is a warning."""
    import methods
    v, t = _cube()
    path = os.path.join(tmp, 'stage2_cube.stl')
    _write_stl(path, v, t)
    method = methods.get_method(1)
    skipped = {'stage1': {'two_manifold': True, 'holes_remaining': 0},
               'stage2': {'error': 'Stage 2 skipped: bridge missing'}}
    rec = methods._evaluate(skipped, path, False, None, method)
    assert rec['holes'] == 0 and rec['non_manifold'] == 0, rec
    assert rec['watertight'] is False, rec
    confirmed = {'stage1': {'two_manifold': True, 'holes_remaining': 0},
                 'stage2': {'ok': True}}
    rec2 = methods._evaluate(confirmed, path, False, None, method)
    assert rec2['watertight'] is True, rec2


# --- execution policy (fakes, no real pipeline run) -------------------------

def test_auto_skips_generative_below_min_score(tmp):
    """A generative method whose recommendation score is below
    AUTO_INVENT_MIN_SCORE must never be auto-adopted (1.1)."""
    import methods
    real = (methods._attempt, methods._evaluate, methods.rank_methods)
    calls = []

    def fake_attempt(src, out, tmpdir, kwargs, multi):
        calls.append(1)
        with open(out, 'w') as f:
            f.write('ok')
        return {'stage1': {}, 'simulated': True}

    def fake_evaluate(result, path, multi, in_objs, method):
        return {'watertight': False, 'holes': 1, 'non_manifold': 0,
                'hausdorff_rel': None, 'geom_change_pct': None,
                'reason': 'holes=1 remain'}

    def fake_rank(analysis, top_n=6):
        return [methods.Recommendation(8, 'poisson_close', 'Poisson close',
                                       0.5, 'low signal', 'single_side_scan')]

    methods._attempt, methods._evaluate, methods.rank_methods = \
        fake_attempt, fake_evaluate, fake_rank
    try:
        with tempfile.TemporaryDirectory() as td:
            src = os.path.join(td, 'in.stl')
            with open(src, 'w') as f:
                f.write('x')
            res = methods.repair_with_methods(
                src, os.path.join(td, 'out.stl'), td, None, mode='auto',
                deep_repair='full', ftetwild='auto')
            # only the baseline ran; the 0.5-score generative method was gated
            assert len(calls) == 1, calls
            assert res['method_reached_watertight'] is False, res
    finally:
        methods._attempt, methods._evaluate, methods.rank_methods = real


def test_engine_only_tag_disables_auto_escalation(tmp):
    """A file tagged only with an external engine (auto_escalation=False) runs
    the baseline but never escalates to extra methods (4.2)."""
    import methods
    real = (methods._attempt, methods._evaluate, methods.rank_methods)

    def run(auto_escalation):
        calls = []

        def fake_attempt(src, out, tmpdir, kwargs, multi):
            first = not calls
            calls.append(1)
            with open(out, 'w') as f:
                f.write('ok')
            return {'stage1': {}, 'simulated': not first}

        def fake_evaluate(result, path, multi, in_objs, method):
            ok = bool(result.get('simulated'))
            return {'watertight': ok, 'holes': 0 if ok else 2,
                    'non_manifold': 0, 'hausdorff_rel': None,
                    'geom_change_pct': None, 'reason': 'x'}

        def fake_rank(analysis, top_n=6):
            return [methods.Recommendation(2, 'deep_local', 'Local deep repair',
                                           0.9, 'x', 'mechanical')]

        methods._attempt, methods._evaluate, methods.rank_methods = \
            fake_attempt, fake_evaluate, fake_rank
        with tempfile.TemporaryDirectory() as td:
            src = os.path.join(td, 'in.stl')
            with open(src, 'w') as f:
                f.write('x')
            out = os.path.join(td, 'out.stl')
            res = methods.repair_with_methods(
                src, out, td, None, auto_escalation=auto_escalation,
                mode='auto', deep_repair='full', ftetwild='auto')
            return calls, res

    try:
        calls, res = run(False)
        assert len(calls) == 1, calls
        assert res['method_used']['source'] == 'auto_baseline', res
        # control: with escalation allowed the ranked method runs and is adopted
        calls, res = run(True)
        assert len(calls) == 2, calls
        assert res['method_used']['source'] == 'auto_escalated', res
    finally:
        methods._attempt, methods._evaluate, methods.rank_methods = real

def test_auto_escalation_uses_a_ranked_method(tmp):
    import methods
    real_attempt, real_evaluate = methods._attempt, methods._evaluate
    state = {'n': 0}

    def fake_attempt(src, out, tmpdir, kwargs, multi):
        state['n'] += 1
        if state['n'] == 1:                       # baseline is not watertight
            return {'stage1': {}, 'baseline': True}
        with open(out, 'w') as f:                 # a later method succeeds
            f.write('ok')
        return {'stage1': {}, 'simulated': True}

    def fake_evaluate(result, path, multi, in_objs, method):
        ok = bool(isinstance(result, dict) and result.get('simulated'))
        return {'watertight': ok, 'holes': 0, 'non_manifold': 0,
                'hausdorff_rel': None, 'geom_change_pct': None,
                'reason': 'strict-watertight' if ok else 'holes remain'}

    methods._attempt, methods._evaluate = fake_attempt, fake_evaluate
    try:
        with tempfile.TemporaryDirectory() as td:
            src = os.path.join(td, 'in.stl')
            with open(src, 'w') as f:
                f.write('x')
            out = os.path.join(td, 'out.stl')
            res = methods.repair_with_methods(
                src, out, td, None, mode='auto', profile=None,
                engine='experimental', join_components=False, autorefine=False,
                ftetwild='auto', indirect_autorefine=False,
                extra_features=False, deep_repair='full', triage_spec=None,
                engines=None, engine_chain=None)
            assert res['method_used']['source'] == 'auto_escalated', res
            assert res['method_reached_watertight'] is True, res
    finally:
        methods._attempt, methods._evaluate = real_attempt, real_evaluate


def test_explicit_methods_keeps_best_when_none_watertight(tmp):
    import methods
    real_attempt, real_evaluate = methods._attempt, methods._evaluate

    def fake_attempt(src, out, tmpdir, kwargs, multi):
        with open(out, 'w') as f:
            f.write('ok')
        return {'stage1': {}, 'simulated': True}

    def fake_evaluate(result, path, multi, in_objs, method):
        return {'watertight': False, 'holes': 2, 'non_manifold': 1,
                'hausdorff_rel': None, 'geom_change_pct': None,
                'reason': 'holes=2 non-manifold=1 remain'}

    methods._attempt, methods._evaluate = fake_attempt, fake_evaluate
    try:
        with tempfile.TemporaryDirectory() as td:
            src = os.path.join(td, 'in.stl')
            with open(src, 'w') as f:
                f.write('x')
            out = os.path.join(td, 'out.stl')
            res = methods.repair_with_methods(
                src, out, td, [1, 2], mode='auto', profile=None,
                engine='experimental', deep_repair='full', ftetwild='auto')
            assert res['method_reached_watertight'] is False, res
            assert res['method_used']['num'] == 1, res   # first candidate kept
            assert all(t.get('outcome') != 'accepted'
                       for t in res['methods_tried']), res
    finally:
        methods._attempt, methods._evaluate = real_attempt, real_evaluate


# --- CLI --------------------------------------------------------------------

def test_cli_list_methods(tmp):
    r = _run(['--list-methods'], env=_env(tmp))
    assert r.returncode == 0, r.stderr
    for num in range(1, 13):
        assert (' %d ' % num) in r.stdout or ('%d ' % num) in r.stdout, num
    assert 'poisson_close' in r.stdout, r.stdout
    rj = _run(['--list-methods', '--json'], env=_env(tmp))
    data = _json(rj)
    assert [m['num'] for m in data] == list(range(1, 13))
    # 8/9/10 are implemented (available); 11/12 are still placeholders.
    assert all(data[n - 1]['available'] is True for n in (8, 9, 10)), data
    assert all(data[n - 1]['available'] is False for n in (11, 12)), data


def test_cli_analyze_json(tmp):
    path = os.path.join(tmp, 'cube.stl')
    v, t = _cube()
    _write_stl(path, v, t)
    r = _run(['--analyze', path], env=_env(tmp))
    assert r.returncode == 0, r.stderr
    d = _json(r)
    assert d['analysis']['faces'] == 12, d
    assert isinstance(d['recommendations'], list) and d['recommendations'], d
    assert 'external_engines' in d, d
    rh = _run(['--analyze', '--human', path], env=_env(tmp))
    assert rh.returncode == 0, rh.stderr
    assert 'Recommended methods:' in rh.stdout, rh.stdout


def test_cli_auto_reports_method_used(tmp):
    path = os.path.join(tmp, 'cube.stl')
    v, t = _cube()
    _write_stl(path, v, t)
    d = _json(_run(['--no-fallback-ftetwild', path], env=_env(tmp)))
    assert d.get('method_used', {}).get('source') == 'auto_baseline', d
    assert len(d.get('methods_tried', [])) >= 1, d


def test_cli_explicit_methods(tmp):
    path = os.path.join(tmp, 'broken.stl')
    subprocess.run([sys.executable, os.path.join(REPO, 'tests',
                                                 'make_broken_stl.py'), path],
                   check=True, capture_output=True)
    d = _json(_run(['--methods', '1,3', '--no-fallback-ftetwild',
                    '--no-history', path], env=_env(tmp)))
    assert d.get('method_used', {}).get('source') == 'tagged', d
    assert d['method_used']['num'] == 1, d
    # unavailable placeholders are reported honestly and produce no output
    r = _run(['--methods', '11', '--no-history', path], env=_env(tmp))
    assert r.returncode == 1, r.stdout
    assert 'no requested method could run' in _json(r).get('error', ''), r.stdout


def test_cli_invalid_method_rejected(tmp):
    path = os.path.join(tmp, 'cube.stl')
    v, t = _cube()
    _write_stl(path, v, t)
    r = _run(['--methods', '1,99', path], env=_env(tmp))
    assert r.returncode != 0, r.stdout
    assert 'unknown method 99' in r.stderr, r.stderr


def test_cli_engines_validation(tmp):
    """--engines is validated against the configured engines and rejected in
    the read-only modes (P3 CLI/GUI parity for per-file engine tagging)."""
    path = os.path.join(tmp, 'cube.stl')
    v, t = _cube()
    _write_stl(path, v, t)
    # isolate the engine config dir: no engines configured -> unknown name
    env = dict(_env(tmp),
               XDG_CONFIG_HOME=os.path.join(tmp, 'cfg-empty'))
    r = _run(['--engines', 'nope', '--no-fallback-ftetwild', path], env=env)
    assert r.returncode != 0, r.stdout
    assert 'unknown engine' in r.stderr, r.stderr
    # read-only modes reject --engines like they reject --methods
    r2 = _run(['--engines', 'x', 'validate', path], env=env)
    assert r2.returncode != 0, r2.stdout
    assert 'not valid with validate' in r2.stdout, r2.stdout


def main():
    with tempfile.TemporaryDirectory(prefix='sutura-methods-') as tmp:
        for name, fn in sorted(globals().items()):
            if name.startswith('test_') and callable(fn):
                fn(tmp)
                print('ok  %s' % name)
    print('methods tests passed')


if __name__ == '__main__':
    main()
