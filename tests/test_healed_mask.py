#!/usr/bin/env python3
"""Unit tests for before_after_render.healed_face_mask (tri-state colouring).

Checks that a repaired face is "healed" (green) exactly when it lies within a
halo of an ORIGINAL defect's real extent (from verts_idx, not diameter) and
the repaired mesh has no defect there:
  1. near an old defect + no repaired defect  -> healed True
  2. near an old defect + repaired defect     -> healed False (renders orange)
  3. far from every old defect                -> healed False (stays grey)
  4. no old defects at all                    -> all False
  5. the defect cap (256) is applied: defects beyond the cap never heal
  6. the radius comes from verts_idx extent, not the bbox diameter

Run with any Python that has numpy + PySide6 (the sutura venv), with
QT_QPA_PLATFORM=offscreen so no display is needed.
Usage: QT_QPA_PLATFORM=offscreen ~/.local/share/sutura/venv/bin/python tests/test_healed_mask.py
"""
import os
import sys

os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402
from before_after_render import healed_face_mask  # noqa: E402


def _mesh_at(centers):
    """For each (x,y,z) center build a tiny equilateral triangle whose
    centroid is EXACTLY the center and whose verts are at distance 0.1 from
    it. Returns (verts, tris)."""
    verts = []
    tris = []
    for i, (x, y, z) in enumerate(centers):
        base = 3 * i
        verts.append((x, y + 0.1, z))
        verts.append((x - 0.0866, y - 0.05, z))
        verts.append((x + 0.0866, y - 0.05, z))
        tris.append((base, base + 1, base + 2))
    return np.asarray(verts, dtype=np.float32), np.asarray(tris, dtype=np.int64)


def _defect(center, face_idx, diameter=None):
    """A defect dict whose verts_idx are the 3 verts of the given face.
    The real extent from centroid to those verts is 0.1 (halo 1.5 -> 0.15)."""
    d = {'centroid': list(center),
         'verts_idx': [3 * face_idx, 3 * face_idx + 1, 3 * face_idx + 2]}
    if diameter is not None:
        d['diameter'] = diameter
    return d


def test_near_old_defect_and_no_repair_defect_is_healed():
    overts, otris = _mesh_at([(0, 0, 0), (10, 0, 0), (20, 0, 0)])
    rverts, rtris = _mesh_at([(0, 0, 0), (10, 0, 0), (20, 0, 0)])
    old = {'holes': [_defect((0, 0, 0), 0)], 'non_manifold': []}
    rep = {'holes': [], 'non_manifold': []}
    healed = healed_face_mask(rverts, rtris, old, overts, rep_defects=rep)
    assert bool(healed[0]) is True, healed          # at the old defect -> healed
    assert bool(healed[1]) is False, healed         # 10 away -> not healed
    assert bool(healed[2]) is False, healed         # 20 away -> not healed


def test_still_broken_face_is_not_healed():
    overts, otris = _mesh_at([(0, 0, 0)])
    rverts, rtris = _mesh_at([(0, 0, 0)])
    old = {'holes': [_defect((0, 0, 0), 0)], 'non_manifold': []}
    # the repaired mesh still has a defect exactly on face 0
    rep = {'holes': [_defect((0, 0, 0), 0)], 'non_manifold': []}
    healed = healed_face_mask(rverts, rtris, old, overts, rep_defects=rep)
    assert bool(healed[0]) is False, healed  # near old defect but still broken


def test_far_region_not_healed():
    overts, otris = _mesh_at([(0, 0, 0)])
    rverts, rtris = _mesh_at([(0, 0, 0), (50, 0, 0)])
    old = {'holes': [_defect((0, 0, 0), 0)], 'non_manifold': []}
    healed = healed_face_mask(rverts, rtris, old, overts)
    assert bool(healed[0]) is True, healed
    assert bool(healed[1]) is False, healed  # far from the defect -> grey


def test_no_old_defects_all_false():
    rverts, rtris = _mesh_at([(0, 0, 0), (1, 0, 0)])
    overts, _ = _mesh_at([(0, 0, 0), (1, 0, 0)])
    old = {'holes': [], 'non_manifold': []}
    healed = healed_face_mask(rverts, rtris, old, overts)
    assert healed.shape == (2,) and not healed.any(), healed


def test_defect_cap_applied():
    # 300 equal-radius defects: 44 clustered near x=0 (faces 0..43) and 256
    # clustered near x=300 (faces 44..299). The cap keeps the 256 largest (by
    # radius; all equal -> the last 256, the x=300 cluster), so the 44 at
    # x=0 are dropped and can never heal anything.
    xs = list(range(44)) + [300 + i * 0.0001 for i in range(256)]
    overts, _ = _mesh_at([(x, 0, 0) for x in xs])
    rverts, rtris = _mesh_at([(x, 0, 0) for x in xs])
    old = {'holes': [], 'non_manifold': []}
    for i, x in enumerate(xs):
        old['non_manifold'].append(_defect((x, 0, 0), i))

    healed_capped = healed_face_mask(rverts, rtris, old, overts)
    assert bool(healed_capped[0]) is False, healed_capped   # x=0 defect capped away
    assert bool(healed_capped[299]) is True, healed_capped  # x=300 cluster kept

    healed_uncapped = healed_face_mask(rverts, rtris, old, overts, cap=10000)
    assert bool(healed_uncapped[0]) is True, healed_uncapped  # cap lifted -> heals


def test_radius_from_verts_idx_not_diameter():
    overts, _ = _mesh_at([(0, 0, 0)])
    rverts, rtris = _mesh_at([(0, 0, 0), (0.5, 0, 0)])
    # diameter claims 10 but the real verts_idx extent is 0.1 (halo 0.15);
    # a face 0.5 away must NOT be healed (a diameter-based rule would heal it).
    old = {'holes': [_defect((0, 0, 0), 0, diameter=10.0)], 'non_manifold': []}
    healed = healed_face_mask(rverts, rtris, old, overts)
    assert bool(healed[0]) is True, healed
    assert bool(healed[1]) is False, healed  # 0.5 > 0.15 despite diameter=10


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('healed-mask tests passed')


if __name__ == '__main__':
    main()