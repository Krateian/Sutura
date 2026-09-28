#!/usr/bin/env python3
"""Unit tests for mirror-plane completion (registry method 14).

Synthetic meshes only: the upper half of an icosphere (an open cap cut by a
plane) completes to a closed sphere by mirroring; a closed cube has no mirror
plane and falls back to Poisson; the registry entry is wired to the tier.
No corpus runs.

Usage: <venv>/bin/python tests/test_mirror_repair.py
(any interpreter with numpy + trimesh; pymeshlab only for the Poisson
fallback checks, which skip without it).
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402


def _half_sphere():
    import trimesh
    m = trimesh.creation.icosphere(subdivisions=2, radius=1.0)
    v = np.asarray(m.vertices, dtype=np.float64)
    t = np.asarray(m.faces, dtype=np.int64)
    keep = np.array([v[f, 1].mean() >= 0 for f in t])
    return v, t[keep]


def _cube():
    import trimesh
    m = trimesh.creation.box(extents=(1, 1, 1))
    return (np.asarray(m.vertices, dtype=np.float64),
            np.asarray(m.faces, dtype=np.int64))


def _have_pymeshlab():
    try:
        import pymeshlab  # noqa: F401
        return True
    except ImportError:
        return False


def test_detect_half_sphere_plane():
    import mirror_repair as MR
    v, t = _half_sphere()
    n, c, score, info = MR.detect_mirror_plane(v, t)
    assert n is not None, info
    assert score > MR.MIRROR_MIN_SCORE, (score, info)
    # the cut is at y = 0, so the plane normal is aligned with the y axis
    assert abs(abs(float(n[1])) - 1.0) < 0.15, n
    assert info['dominant_ratio'] > 0.9, info


def test_mirror_close_half_sphere():
    import mirror_repair as MR
    from sutura_engine.methods import closing as C
    v, t = _half_sphere()
    ov, ot, rep = MR.mirror_close(v, t)
    assert rep['mode'] == 'mirror', rep
    assert rep['watertight'] is True, rep
    assert len(ot) > len(t), (len(ot), len(t))
    ok, reason = C._check_validity(np.asarray(ov, dtype=np.float64),
                                   np.asarray(ot, dtype=np.int64))
    assert ok, reason


def test_mirror_close_closed_falls_back():
    import mirror_repair as MR
    v, t = _cube()
    n, c, score, info = MR.detect_mirror_plane(v, t)
    assert n is None and score == 0.0, info
    ov, ot, rep = MR.mirror_close(v, t)
    assert rep['mode'] == 'poisson_fallback', rep
    assert 'watertight' in rep and 'faces' in rep, rep
    assert any('low mirror confidence' in n for n in rep['notes']), rep


def test_registry_entry_wired():
    import methods
    m = methods.get_method(14)
    assert m is not None and m.id == 'mirror_complete', m
    assert m.family == 'closing', m.family
    assert m.invents_geometry is True
    assert m.kwargs.get('closing') == 'mirror', m.kwargs
    assert m.available()[0] is True, m.available()
    # aliases resolve
    assert methods.get_method('mirror').num == 14
    assert methods.get_method('m14').num == 14


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('mirror repair tests passed')


if __name__ == '__main__':
    main()
