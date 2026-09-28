#!/usr/bin/env python3
"""Regression test for per-object auto escalation in multi-object 3MF.

Checks:
  1. A 2-object 3MF (one clean cube, one relief plate open at the back that
     the baseline pipeline cannot close due to hole boundary > maxholesize).
  2. Baseline leaves object 0 watertight (untouched, auto_baseline).
  3. Escalation runs only on object 1, selecting Method 9 (flat_back_close).
  4. Per-object method_used is reported for both objects.
  5. Both objects in the output 3MF reload strict-watertight.

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

_3MF_TMPL = (
    '<?xml version="1.0"?>'
    '<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">'
    '<resources><object id="%d" type="model"><mesh><vertices>%s</vertices>'
    '<triangles>%s</triangles></mesh></object></resources></model>'
)


def _make_test_3mf(path):
    # Object 0: clean closed cube
    s = 1.0
    v0 = [(-s, -s, -s), (s, -s, -s), (s, s, -s), (-s, s, -s),
          (-s, -s, s), (s, -s, s), (s, s, s), (-s, s, s)]
    t0 = [(0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7), (0, 1, 5), (0, 5, 4),
          (1, 2, 6), (1, 6, 5), (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7)]

    # Object 1: relief plate with boundary perimeter > 300 (nx=78 -> 308 edges)
    # The default baseline maxholesize (300) leaves it open, but Method 9 (flat_back_close)
    # detects the relief profile and closes it with a planar back surface.
    nx, ny = 78, 78
    xs = np.linspace(-1.0, 1.0, nx)
    ys = np.linspace(-1.0, 1.0, ny)
    X, Y = np.meshgrid(xs, ys)
    Z = 0.2 * np.cos(np.pi * X / 2.0) * np.cos(np.pi * Y / 2.0)
    v1 = np.column_stack([X.ravel(), Y.ravel(), Z.ravel()])
    t1 = []
    for i in range(ny - 1):
        for j in range(nx - 1):
            a = i * nx + j
            b = i * nx + (j + 1)
            c = (i + 1) * nx + j
            d = (i + 1) * nx + (j + 1)
            t1.append((a, b, c))
            t1.append((b, d, c))

    with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED) as z:
        for oid, (v, t) in enumerate(((v0, t0), (v1, t1)), start=1):
            vs = ''.join('<vertex x="%.7g" y="%.7g" z="%.7g"/>' % (p[0], p[1], p[2]) for p in v)
            ts = ''.join('<triangle v1="%d" v2="%d" v3="%d"/>' % (tr[0], tr[1], tr[2]) for tr in t)
            z.writestr('3D/Objects/o%d.model' % oid, _3MF_TMPL % (oid, vs, ts))


def test_per_object_auto_escalation():
    with tempfile.TemporaryDirectory() as td:
        src = os.path.join(td, 'input.3mf')
        out = os.path.join(td, 'output.3mf')
        _make_test_3mf(src)

        res = methods.repair_with_methods(
            src, out, td, None, mode='auto', deep_repair='full', ftetwild='auto'
        )

        assert res.get('objects_watertight') == 2, res
        assert res.get('method_reached_watertight') is True, res

        reps = res.get('object_reports', [])
        assert len(reps) == 2, reps

        # Object 0: clean cube remains untouched on baseline
        rep0 = reps[0]
        mu0 = rep0.get('method_used', {})
        assert mu0.get('source') == 'auto_baseline', mu0
        assert mu0.get('num') == 3, mu0

        # Object 1: relief plate escalated to method 9 (flat_back_close)
        rep1 = reps[1]
        mu1 = rep1.get('method_used', {})
        assert mu1.get('source') == 'auto_escalated', mu1
        assert mu1.get('num') == 9, mu1

        # Reload the saved 3MF: verify both meshes are reload-watertight
        reloaded = repair.load_meshes(out)
        assert len(reloaded) == 2, len(reloaded)
        for name, v, t in reloaded:
            check_rep, _, _ = repair.repair_mesh_from_arrays(v, t, td)
            s1 = check_rep.get('stage1', {})
            assert s1.get('two_manifold') is True, (name, s1)
            assert s1.get('holes_remaining', 0) == 0, (name, s1)

    print('test_per_object_auto_escalation passed')


if __name__ == '__main__':
    test_per_object_auto_escalation()
