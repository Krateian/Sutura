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
    for num in (8, 9, 10, 11, 12):
        ok, reason = methods.get_method(num).available()
        assert ok is False, num
        assert 'not implemented' in reason, (num, reason)
    assert methods.get_method(12).needs_user_input is True


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


# --- execution policy (fakes, no real pipeline run) -------------------------

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
    assert data[7]['available'] is False


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
    r = _run(['--methods', '8,9', '--no-history', path], env=_env(tmp))
    assert r.returncode == 1, r.stdout
    assert 'no requested method could run' in _json(r).get('error', ''), r.stdout


def test_cli_invalid_method_rejected(tmp):
    path = os.path.join(tmp, 'cube.stl')
    v, t = _cube()
    _write_stl(path, v, t)
    r = _run(['--methods', '1,99', path], env=_env(tmp))
    assert r.returncode != 0, r.stdout
    assert 'unknown method 99' in r.stderr, r.stderr


def main():
    with tempfile.TemporaryDirectory(prefix='sutura-methods-') as tmp:
        for name, fn in sorted(globals().items()):
            if name.startswith('test_') and callable(fn):
                fn(tmp)
                print('ok  %s' % name)
    print('methods tests passed')


if __name__ == '__main__':
    main()
