#!/usr/bin/env python3
"""Regression test for sutura/proxy_repair.py (proxy-template repair).

Checks:
  1. health_mask detects boundary holes, non-manifold geometry, debris, and dilates.
  2. build_proxy produces watertight 2-manifold surface without debris.
  3. proxy_template_repair repairs broken sphere with hole, flipped face, and debris.
  4. proxy_template_repair repairs broken torus while preserving topology.
  5. Defensive error handling returns honest error reports without raising.

Runtime target: < 15 s (< 30 s limit).
Usage: python3 tests/test_proxy_repair.py
"""

import os
import sys
import time
import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)
sys.path.insert(0, REPO)

import trimesh  # noqa: E402
import manifold3d  # noqa: E402
import sutura.proxy_repair as pr  # noqa: E402


def _make_broken_sphere():
    """Synthetic sphere with punched hole, flipped face, and floating debris."""
    sphere = trimesh.creation.icosphere(subdivisions=2, radius=1.0)
    faces = list(sphere.faces)
    del faces[10:15]  # punch hole of 5 faces
    faces[20] = faces[20][[0, 2, 1]]  # flip face orientation

    debris_v = np.array([[2.5, 0.0, 0.0], [2.6, 0.0, 0.0], [2.5, 0.1, 0.0]])
    verts = np.vstack([sphere.vertices, debris_v])
    faces.append([len(sphere.vertices), len(sphere.vertices) + 1, len(sphere.vertices) + 2])
    return np.asarray(verts, np.float64), np.asarray(faces, np.int64)


def _make_broken_torus():
    """Synthetic torus with punched hole."""
    torus = trimesh.creation.torus(major_radius=1.0, minor_radius=0.3, major_sections=24, minor_sections=12)
    faces = list(torus.faces)
    del faces[20:30]  # punch hole
    return np.asarray(torus.vertices, np.float64), np.asarray(faces, np.int64)


def test_health_mask():
    v, t = _make_broken_sphere()
    mask = pr.health_mask(v, t, dilation_rings=2)
    assert len(mask) == len(t), "Mask length mismatch"
    assert not mask[-1], "Debris triangle must be marked unhealthy"
    assert np.sum(~mask) >= 15, "Defective and dilated faces must be marked unhealthy"
    assert np.sum(mask) > 100, "Healthy regions of the sphere must be preserved"

    # Clean sphere must be 100% healthy
    clean_sphere = trimesh.creation.icosphere(subdivisions=2, radius=1.0)
    clean_mask = pr.health_mask(clean_sphere.vertices, clean_sphere.faces)
    assert np.all(clean_mask), "Clean mesh faces must all be healthy"
    print("  ✓ test_health_mask passed")


def test_build_proxy():
    v, t = _make_broken_sphere()
    pv, pt, info = pr.build_proxy(v, t)
    assert len(pt) >= 100, f"Proxy should have sufficient faces, got {len(pt)}"

    mf = manifold3d.Manifold(mesh=manifold3d.Mesh(vert_properties=pv, tri_verts=pt))
    assert mf.status() == manifold3d.Error.NoError, f"Proxy manifold error: {mf.status()}"
    print(f"  ✓ test_build_proxy passed (faces: {len(pt)}, method: {info['method']})")


def test_proxy_template_repair_sphere():
    v, t = _make_broken_sphere()
    v_out, t_out, rep = pr.proxy_template_repair(v, t)

    assert rep['watertight'] is True, f"Repaired sphere not watertight: {rep}"
    assert rep['output_faces'] > 0, "Output mesh has 0 faces"
    assert rep['projected_fraction'] > 0.0, "At least some proxy vertices should be projected"
    assert any('proxy' in n for n in rep['notes']), f"Missing required note: {rep['notes']}"
    assert rep['hausdorff_mean'] is not None and rep['hausdorff_mean'] < 0.02, (
        f"Mean Hausdorff distance too high: {rep['hausdorff_mean']}"
    )

    mf = manifold3d.Manifold(mesh=manifold3d.Mesh(vert_properties=v_out, tri_verts=t_out))
    assert mf.status() == manifold3d.Error.NoError, f"Manifold error: {mf.status()}"
    print(
        f"  ✓ test_proxy_template_repair_sphere passed "
        f"(faces: {len(t_out)}, proj: {rep['projected_fraction']:.1%}, H_mean: {rep['hausdorff_mean']:.5f})"
    )


def test_proxy_template_repair_torus():
    v, t = _make_broken_torus()
    v_out, t_out, rep = pr.proxy_template_repair(v, t)

    assert rep['watertight'] is True, f"Repaired torus not watertight: {rep}"
    mf = manifold3d.Manifold(mesh=manifold3d.Mesh(vert_properties=v_out, tri_verts=t_out))
    assert mf.status() == manifold3d.Error.NoError, f"Manifold error: {mf.status()}"
    print(f"  ✓ test_proxy_template_repair_torus passed (faces: {len(t_out)}, genus: {mf.genus()})")


def test_defensive_error_handling():
    empty_v = np.zeros((0, 3), dtype=np.float64)
    empty_t = np.zeros((0, 3), dtype=np.int64)

    _, _, rep = pr.proxy_template_repair(empty_v, empty_t)
    assert rep['watertight'] is False
    assert 'error' in rep

    bad_v = np.array([[0, 0, 0]], dtype=np.float64)
    bad_t = np.array([[0, 0, 0]], dtype=np.int64)
    _, _, rep_bad = pr.proxy_template_repair(bad_v, bad_t)
    assert rep_bad['watertight'] is False
    print("  ✓ test_defensive_error_handling passed")


def main():
    t0 = time.time()
    print("Running proxy-template repair tests (tests/test_proxy_repair.py)...")
    test_health_mask()
    test_build_proxy()
    test_proxy_template_repair_sphere()
    test_proxy_template_repair_torus()
    test_defensive_error_handling()
    elapsed = time.time() - t0
    print(f"\nAll tests passed in {elapsed:.2f} s (< 30 s target).")
    assert elapsed < 30.0, f"Tests exceeded 30 s limit: {elapsed:.2f} s"


if __name__ == '__main__':
    main()
