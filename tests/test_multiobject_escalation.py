#!/usr/bin/env python3
"""Regression test for per-object auto escalation in multi-object 3MF.

Checks:
  1. A 2-object 3MF where the baseline leaves object 1 non-watertight;
     escalation runs only on object 1 and adopts the ranked method.
  2. The escalation is driven by a monkeypatched baseline/ranking (no
     in-repo fixture reaches a specialist tier through the real multi-object
     baseline any more, because the mesh-aware maxholesize raise and the
     per-object deep-repair ladder close every real sample first).
  3. Per-object method_used is reported for both objects.
  4. Both objects in the output 3MF reload strict-watertight.

A second test keeps the relief-plate scenario with the REAL baseline: the
mesh-aware maxholesize raise (pipeline.py) lets the baseline close the plate's
308-edge back opening in a single pass, so no escalation is triggered and the
plate stays on the baseline (auto_baseline).

Usage: ~/.local/share/sutura/venv/bin/python tests/test_multiobject_escalation.py
"""
import os
import sys
import tempfile
import zipfile

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import methods  # noqa: E402
import repair  # noqa: E402
from sutura_engine.methods.protocol import RepairMethod  # noqa: E402

_3MF_TMPL = (
    '<?xml version="1.0"?>'
    '<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">'
    '<resources><object id="%d" type="model"><mesh><vertices>%s</vertices>'
    '<triangles>%s</triangles></mesh></object></resources></model>'
)

CUBE_T = [(0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7), (0, 1, 5), (0, 5, 4),
          (1, 2, 6), (1, 6, 5), (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7)]
OPEN_T = CUBE_T[:2] + CUBE_T[4:]  # cube with the bottom two faces removed


def _cube():
    s = 1.0
    v = [(-s, -s, -s), (s, -s, -s), (s, s, -s), (-s, s, -s),
         (-s, -s, s), (s, -s, s), (s, s, s), (-s, s, s)]
    return v, list(CUBE_T)


def _open_cube():
    v, _ = _cube()
    return v, list(OPEN_T)


def _relief_plate(nx=78, ny=78):
    # Relief plate with boundary perimeter > 300 (nx=78 -> 308 edges).
    xs = np.linspace(-1.0, 1.0, nx)
    ys = np.linspace(-1.0, 1.0, ny)
    X, Y = np.meshgrid(xs, ys)
    Z = 0.2 * np.cos(np.pi * X / 2.0) * np.cos(np.pi * Y / 2.0)
    v = np.column_stack([X.ravel(), Y.ravel(), Z.ravel()])
    t = []
    for i in range(ny - 1):
        for j in range(nx - 1):
            a = i * nx + j
            b = i * nx + (j + 1)
            c = (i + 1) * nx + j
            d = (i + 1) * nx + (j + 1)
            t.append((a, b, c))
            t.append((b, d, c))
    return v, t


def _write_3mf(path, objects):
    with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED) as z:
        for oid, (v, t) in enumerate(objects, start=1):
            vs = ''.join(
                '<vertex x="%.7g" y="%.7g" z="%.7g"/>' % (p[0], p[1], p[2])
                for p in v)
            ts = ''.join(
                '<triangle v1="%d" v2="%d" v3="%d"/>' % (tr[0], tr[1], tr[2])
                for tr in t)
            z.writestr('3D/Objects/o%d.model' % oid, _3MF_TMPL % (oid, vs, ts))


def _reload_is_strict_watertight(path):
    reloaded = repair.load_meshes(path)
    for name, v, t in reloaded:
        check_rep, _, _ = repair.repair_mesh_from_arrays(v, t, os.path.dirname(path))
        s1 = check_rep.get('stage1', {})
        assert s1.get('two_manifold') is True, (name, s1)
        assert s1.get('holes_remaining', 0) == 0, (name, s1)
    return len(reloaded)


class _FakeEscalationMethod(RepairMethod):
    """A ranked tier that returns a closed cube, marked watertight by the
    fake evaluator. Deterministic stand-in for a geometry-inventing tier."""

    def run(self, verts, tris, tmpdir, ctx=None):
        v, t = _cube()
        return {
            '_verts': np.asarray(v, dtype=np.float64),
            '_tris': np.asarray(t, dtype=np.int64),
            'simulated': True,
            'stage1': {'two_manifold': True, 'holes_remaining': 0,
                       'non_manifold_edges_remaining': 0},
            'stage2': {'ok': True},
        }


def _fake_method():
    return _FakeEscalationMethod(
        num=99, id='fake_escalation', name='Fake Escalation',
        description='deterministic escalation stand-in', family='test')


def test_per_object_auto_escalation():
    real_attempt = getattr(methods, '_attempt', None)
    real_evaluate = getattr(methods, '_evaluate', None)
    real_rank = getattr(methods, 'rank_methods', None)
    cube, open_cube = _cube(), _open_cube()

    def fake_attempt(src, out, tmpdir, ctx, multi):
        _write_3mf(out, [cube, open_cube])
        return {
            'object_reports': [
                {'stage1': {'two_manifold': True, 'holes_remaining': 0,
                            'non_manifold_edges_remaining': 0},
                 'stage2': {'ok': True}},
                {'stage1': {'two_manifold': True, 'holes_remaining': 2,
                            'non_manifold_edges_remaining': 0},
                 'stage2': {'ok': False}},
            ],
            'baseline': True,
        }

    def fake_evaluate(result, path, multi, in_objs, method):
        ok = bool(isinstance(result, dict) and result.get('simulated'))
        return {'watertight': ok, 'holes': 0 if ok else 2, 'non_manifold': 0,
                'hausdorff_rel': None, 'geom_change_pct': None,
                'reason': 'strict-watertight' if ok else 'holes remain'}

    def fake_rank(analysis, top_n=6):
        return [methods.Recommendation(99, 'fake_escalation', 'Fake Escalation',
                                       0.9, 'holes remain', 'mechanical')]

    methods._attempt = fake_attempt
    methods._evaluate = fake_evaluate
    methods.rank_methods = fake_rank
    methods.register_method(_fake_method())
    try:
        with tempfile.TemporaryDirectory() as td:
            src = os.path.join(td, 'input.3mf')
            out = os.path.join(td, 'output.3mf')
            _write_3mf(src, [cube, open_cube])

            res = methods.repair_with_methods(
                src, out, td, None, mode='auto', deep_repair='full',
                ftetwild='auto')

            assert res.get('objects_watertight') == 2, res
            assert res.get('method_reached_watertight') is True, res

            reps = res.get('object_reports', [])
            assert len(reps) == 2, reps

            # Object 0: clean cube remains untouched on baseline
            mu0 = reps[0].get('method_used', {})
            assert mu0.get('source') == 'auto_baseline', mu0
            assert mu0.get('num') == 3, mu0

            # Object 1: escalated to the ranked method
            mu1 = reps[1].get('method_used', {})
            assert mu1.get('source') == 'auto_escalated', mu1
            assert mu1.get('num') == 99, mu1

            # Reload the saved 3MF: both meshes are reload-watertight
            assert _reload_is_strict_watertight(out) == 2

        print('test_per_object_auto_escalation passed')
    finally:
        methods.unregister_method(99)
        if real_attempt is None:
            del methods._attempt
        else:
            methods._attempt = real_attempt
        if real_evaluate is None:
            del methods._evaluate
        else:
            methods._evaluate = real_evaluate
        if real_rank is not None:
            methods.rank_methods = real_rank


def test_relief_plate_closed_by_baseline():
    v1, t1 = _relief_plate()
    with tempfile.TemporaryDirectory() as td:
        src = os.path.join(td, 'input.3mf')
        out = os.path.join(td, 'output.3mf')
        _write_3mf(src, [_cube(), (v1, t1)])

        res = methods.repair_with_methods(
            src, out, td, None, mode='auto', deep_repair='full', ftetwild='auto'
        )

        assert res.get('objects_watertight') == 2, res

        reps = res.get('object_reports', [])
        assert len(reps) == 2, reps

        # The mesh-aware raise lets the baseline close the plate in one pass,
        # so neither object escalates.
        for rep in reps:
            mu = rep.get('method_used', {})
            assert mu.get('source') == 'auto_baseline', mu

        assert _reload_is_strict_watertight(out) == 2

    print('test_relief_plate_closed_by_baseline passed')


if __name__ == '__main__':
    test_per_object_auto_escalation()
    test_relief_plate_closed_by_baseline()
