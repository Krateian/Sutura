#!/usr/bin/env python3
"""Regression test for scripts/defect_injector.py: every injected defect type
must be genuinely detected by the corresponding detector.

  --hole            -> defects.detect()['holes'] increases
  --non-manifold    -> defects.detect()['non_manifold'] increases
  --flipped-normal  -> defects.defect_type_colors() gains ORANGE faces
  --degenerate      -> defects.defect_type_colors() gains YELLOW faces
  --self-intersect  -> pymeshlab self-intersection selection > 0

A clean closed cube must have none of these. Needs the venv (pymeshlab).

Usage: ~/.local/share/sutura/venv/bin/python tests/test_defect_injector.py
"""
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
SCRIPTS = os.path.join(REPO, 'scripts')
for p in (SUTURA, SCRIPTS):
    if p not in sys.path:
        sys.path.insert(0, p)

import numpy as np  # noqa: E402

import defects  # noqa: E402
import defect_injector as di  # noqa: E402

_ORANGE = (255, 140, 60)
_YELLOW = (250, 210, 60)


def _cube():
    v = np.array([[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
                  [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]], dtype=np.float64)
    def sq(a, b, c, d):
        return [[a, b, c], [a, c, d]]
    # consistent outward winding (verified in tests): -z +z -x +x -y +y
    t = np.array(sq(0, 3, 2, 1) + sq(4, 5, 6, 7) + sq(0, 4, 7, 3) +
                 sq(1, 2, 6, 5) + sq(0, 1, 5, 4) + sq(3, 7, 6, 2),
                 dtype=np.int64)
    return v, t


def _self_intersections(v, t):
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(v, np.float32),
                        face_matrix=np.asarray(t, np.int32)))
    ms.apply_filter('compute_selection_by_self_intersections_per_face')
    return int(ms.current_mesh().face_selection_array().sum())


def test_clean_cube_has_no_defects():
    v, t = _cube()
    det = defects.detect(v, t)
    assert len(det['holes']) == 0, det
    assert len(det['non_manifold']) == 0, det
    colors = defects.defect_type_colors(v, t)
    assert not np.any(np.any(colors, axis=1)), colors
    assert _self_intersections(v, t) == 0


def _count_color(colors, rgb):
    return int(np.sum((colors == np.array(rgb, np.uint8)).all(axis=1)))


def test_hole_detected():
    v, t = di.inject_hole(*_cube(), count=3, rng=np.random.default_rng(42))
    holes = defects.detect(v, t)['holes']
    assert len(holes) > 0, holes


def test_non_manifold_detected():
    v, t = di.inject_non_manifold(*_cube(), count=3, rng=np.random.default_rng(42))
    nm = defects.detect(v, t)['non_manifold']
    assert len(nm) > 0, nm


def test_flipped_normal_detected():
    v, t = di.inject_flipped_normal(*_cube(), count=2, rng=np.random.default_rng(42))
    colors = defects.defect_type_colors(v, t)
    assert _count_color(colors, _ORANGE) > 0, colors


def test_degenerate_detected():
    v, t = di.inject_degenerate(*_cube(), count=2, rng=np.random.default_rng(42))
    colors = defects.defect_type_colors(v, t)
    assert _count_color(colors, _YELLOW) > 0, colors


def test_self_intersect_detected():
    v, t = di.inject_self_intersect(*_cube(), count=3, rng=np.random.default_rng(42))
    assert _self_intersections(v, t) > 0


def test_cli_roundtrip_writes_copy():
    with tempfile.TemporaryDirectory(prefix='sutura-definj-') as tmp:
        src = os.path.join(tmp, 'in.stl')
        di.save_mesh(src, *_cube())
        dst = os.path.join(tmp, 'out.stl')
        r = subprocess.run(
            [sys.executable, os.path.join(SCRIPTS, 'defect_injector.py'),
             src, dst, '--hole', '--count', '2', '--seed', '1'],
            capture_output=True, text=True, timeout=120)
        assert r.returncode == 0, r.stderr
        assert os.path.exists(dst), r.stdout
        # the input must be untouched
        v2, t2 = di.load_mesh(src)
        assert len(t2) == 12
        # output has fewer faces (hole injection) and the input is unchanged
        v3, t3 = di.load_mesh(dst)
        assert len(t3) < len(t2), (len(t3), len(t2))


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('defect_injector tests passed')