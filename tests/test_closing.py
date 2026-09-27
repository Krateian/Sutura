#!/usr/bin/env python3
"""Regression test for sutura/closing.py (scan closing algorithms).

Checks:
  1. boundary_loops extracts ordered boundary loops following triangle winding.
  2. single_side_score scores open scans high and closed meshes / small holes low.
  3. relief_score distinguishes planar heightfield reliefs from non-planar surfaces.
  4. poisson_close reconstructs watertight 2-manifold with honest report & notes.
  5. flat_back_close produces watertight 2-manifold with flat back & side walls.
  6. flat_back_close cleanly handles non-convex loops and secondary holes.
  7. one_sided_hausdorff accurately measures scan preservation.
  8. Closers handle degenerate / empty inputs defensively without throwing.

Runtime target: < 15 s (well within 30 s limit).
Usage: python3 tests/test_closing.py
"""

import os
import sys
import time
import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)
sys.path.insert(0, REPO)

# The in-process stage-2 / proximity stack is optional on the Linux CI legs
# (manifold3d has no py3.14 wheel): skip cleanly where it is unavailable.
try:
    import trimesh  # noqa: E402
    import manifold3d  # noqa: E402
    import sutura.closing as cl  # noqa: E402
except Exception as _exc:  # noqa: BLE001 - optional test dependency
    print('skip tests/test_closing.py: %s' % _exc)
    raise SystemExit(0)


def _make_hemisphere(radius=1.0, subdivisions=2):
    """Synthetic open hemisphere shell (z >= 0)."""
    sphere = trimesh.creation.icosphere(subdivisions=subdivisions, radius=radius)
    mask = sphere.vertices[:, 2] >= 0.0
    faces = [f for f in sphere.faces if all(mask[i] for i in f)]
    m = trimesh.Trimesh(vertices=sphere.vertices, faces=faces)
    m.remove_unreferenced_vertices()
    return np.asarray(m.vertices, np.float64), np.asarray(m.faces, np.int64)


def _make_relief(nx=10, ny=10, amplitude=0.2):
    """Synthetic heightfield relief plate open at the back."""
    xs = np.linspace(-1.0, 1.0, nx)
    ys = np.linspace(-1.0, 1.0, ny)
    X, Y = np.meshgrid(xs, ys)
    Z = amplitude * np.cos(np.pi * X / 2.0) * np.cos(np.pi * Y / 2.0)
    verts = np.column_stack([X.ravel(), Y.ravel(), Z.ravel()])

    faces = []
    for i in range(ny - 1):
        for j in range(nx - 1):
            v0 = i * nx + j
            v1 = i * nx + (j + 1)
            v2 = (i + 1) * nx + j
            v3 = (i + 1) * nx + (j + 1)
            # Outward normal pointing +Z
            faces.append([v0, v1, v2])
            faces.append([v1, v3, v2])
    return np.asarray(verts, np.float64), np.asarray(faces, np.int64)


def _make_l_relief(thickness=0.2):
    """Synthetic non-convex L-shaped relief plate."""
    # 2D L-shape polygon vertices
    poly = np.array([
        [0.0, 0.0],
        [2.0, 0.0],
        [2.0, 1.0],
        [1.0, 1.0],
        [1.0, 2.0],
        [0.0, 2.0],
    ], dtype=np.float64)

    # Triangulate into 2 quads -> 4 triangles
    tris = np.array([
        [0, 1, 3], [1, 2, 3],  # bottom right quad
        [0, 3, 4], [0, 4, 5],  # top left quad
    ], dtype=np.int64)
    # Add a slight dome in Z
    z = np.array([0.0, 0.0, 0.0, thickness, 0.0, 0.0])
    verts = np.column_stack([poly[:, 0], poly[:, 1], z])
    return verts, tris


def test_boundary_loops():
    # Single triangle
    v = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0]], dtype=np.float64)
    t = np.array([[0, 1, 2]], dtype=np.int64)
    loops = cl.boundary_loops(v, t)
    assert len(loops) == 1, f"Expected 1 loop, got {len(loops)}"
    assert loops[0] == [0, 1, 2], f"Expected [0, 1, 2], got {loops[0]}"

    # Closed tetrahedron has 0 loops
    v_tet = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1]], dtype=np.float64)
    t_tet = np.array([[0, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3]], dtype=np.int64)
    loops_tet = cl.boundary_loops(v_tet, t_tet)
    assert len(loops_tet) == 0, f"Expected 0 loops for closed mesh, got {len(loops_tet)}"
    print("  ✓ test_boundary_loops passed")


def test_single_side_score():
    # 1. Open hemisphere shell should score high
    v_hemi, t_hemi = _make_hemisphere()
    score_hemi, info_hemi = cl.single_side_score(v_hemi, t_hemi)
    assert score_hemi >= 0.80, f"Hemisphere single_side_score too low: {score_hemi}"
    assert info_hemi['dominant_ratio'] >= 0.95, f"Dominance ratio should be ~1: {info_hemi}"

    # 2. Synthetic relief should score high
    v_rel, t_rel = _make_relief()
    score_rel, info_rel = cl.single_side_score(v_rel, t_rel)
    assert score_rel >= 0.85, f"Relief single_side_score too low: {score_rel}"

    # 3. Closed tetrahedron should score 0.0
    v_tet = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1]], dtype=np.float64)
    t_tet = np.array([[0, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3]], dtype=np.int64)
    score_tet, _ = cl.single_side_score(v_tet, t_tet)
    assert score_tet == 0.0, f"Closed mesh should score 0.0, got {score_tet}"
    print("  ✓ test_single_side_score passed")


def test_relief_score():
    # 1. Flat relief should have very high relief score (~1.0)
    v_rel, t_rel = _make_relief(nx=10, ny=10)
    r_score, r_info = cl.relief_score(v_rel, t_rel)
    assert r_score >= 0.80, f"Flat relief relief_score should be >= 0.80, got {r_score}"
    assert r_info['rms_rel'] < 0.01, f"Relief RMS relative should be small: {r_info['rms_rel']}"

    # 2. Closed mesh should return 0.0
    v_tet = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1]], dtype=np.float64)
    t_tet = np.array([[0, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3]], dtype=np.int64)
    score_closed, _ = cl.relief_score(v_tet, t_tet)
    assert score_closed == 0.0, f"Closed mesh should score 0.0, got {score_closed}"
    print("  ✓ test_relief_score passed")


def test_poisson_close_hemisphere():
    v_hemi, t_hemi = _make_hemisphere()
    v_out, t_out, rep = cl.poisson_close(v_hemi, t_hemi, depth=8)

    assert rep['watertight'] is True, f"Poisson output not watertight: {rep}"
    assert rep['depth'] == 8, f"Depth mismatch: {rep}"
    assert any('Poisson' in n for n in rep['notes']), f"Missing Poisson note: {rep['notes']}"
    assert len(t_out) > len(t_hemi), "Reconstruction should add faces"

    # Verify with manifold3d
    mf = manifold3d.Manifold(mesh=manifold3d.Mesh(vert_properties=v_out, tri_verts=t_out))
    assert mf.status() == manifold3d.Error.NoError, f"manifold3d error: {mf.status()}"

    # Check fidelity with one_sided_hausdorff
    h_max, h_mean = cl.one_sided_hausdorff(v_hemi, t_hemi, v_out, t_out, samples=10000)
    assert h_max is not None and h_max < 0.02, f"Hausdorff max too high: {h_max}"
    print(f"  ✓ test_poisson_close_hemisphere passed (faces: {len(t_out)}, H_max: {h_max})")


def test_flat_back_close_relief():
    v_rel, t_rel = _make_relief(nx=10, ny=10)
    v_out, t_out, rep = cl.flat_back_close(v_rel, t_rel)

    assert rep['watertight'] is True, f"Flat back output not watertight: {rep}"
    assert rep['thickness'] > 0.0, f"Thickness should be positive: {rep['thickness']}"
    assert any('flat back' in n for n in rep['notes']), f"Missing note: {rep['notes']}"

    # Verify with manifold3d
    mf = manifold3d.Manifold(mesh=manifold3d.Mesh(vert_properties=v_out, tri_verts=t_out))
    assert mf.status() == manifold3d.Error.NoError, f"manifold3d error: {mf.status()}"

    # Input faces should be strictly preserved -> Hausdorff == 0.0
    h_max, h_mean = cl.one_sided_hausdorff(v_rel, t_rel, v_out, t_out, samples=10000)
    assert h_max is not None and h_max < 1e-6, f"Input scan not strictly preserved: {h_max}"
    print(f"  ✓ test_flat_back_close_relief passed (faces: {len(t_out)}, H_max: {h_max})")


def test_flat_back_secondary_hole():
    """Ensure small secondary hole in relief is closed by pymeshlab before flat back."""
    v_rel, t_rel = _make_relief(nx=10, ny=10)
    # Delete one face from the interior to create a small hole
    t_with_hole = np.delete(t_rel, 20, axis=0)

    v_out, t_out, rep = cl.flat_back_close(v_rel, t_with_hole)
    assert rep['watertight'] is True, f"Output with secondary hole not watertight: {rep}"

    mf = manifold3d.Manifold(mesh=manifold3d.Mesh(vert_properties=v_out, tri_verts=t_out))
    assert mf.status() == manifold3d.Error.NoError, f"manifold3d error: {mf.status()}"
    print("  ✓ test_flat_back_secondary_hole passed")


def test_non_convex_l_relief():
    """Verify ear clipping on non-convex boundary loop."""
    v_l, t_l = _make_l_relief()
    v_out, t_out, rep = cl.flat_back_close(v_l, t_l)
    assert rep['watertight'] is True, f"L-relief not watertight: {rep}"

    mf = manifold3d.Manifold(mesh=manifold3d.Mesh(vert_properties=v_out, tri_verts=t_out))
    assert mf.status() == manifold3d.Error.NoError, f"manifold3d error: {mf.status()}"
    print("  ✓ test_non_convex_l_relief passed")


def test_defensive_error_handling():
    # Empty inputs
    empty_v = np.zeros((0, 3), dtype=np.float64)
    empty_t = np.zeros((0, 3), dtype=np.int64)

    _, _, rep_p = cl.poisson_close(empty_v, empty_t)
    assert rep_p['watertight'] is False
    assert 'error' in rep_p

    _, _, rep_fb = cl.flat_back_close(empty_v, empty_t)
    assert rep_fb['watertight'] is False
    assert 'error' in rep_fb

    # Non-mesh bad input
    bad_v = np.array([[0, 0, 0]], dtype=np.float64)
    bad_t = np.array([[0, 0, 0]], dtype=np.int64)
    _, _, rep_bad = cl.poisson_close(bad_v, bad_t)
    assert rep_bad['watertight'] is False
    print("  ✓ test_defensive_error_handling passed")


def test_large_loop_flat_back():
    """Verify size-guarded fast closing on a ~5000-vertex boundary loop (< 5 s)."""
    n_boundary = 5000
    theta = np.linspace(0, 2 * np.pi, n_boundary, endpoint=False)
    x = np.cos(theta)
    y = np.sin(theta)
    z = 0.1 * np.cos(3 * theta)

    center = np.array([[0.0, 0.0, 0.2]])
    boundary_verts = np.column_stack([x, y, z])
    verts = np.vstack([center, boundary_verts])

    tris = []
    for i in range(1, n_boundary + 1):
        next_i = 1 if i == n_boundary else (i + 1)
        tris.append([0, i, next_i])
    tris = np.array(tris, dtype=np.int64)

    t0 = time.time()
    v_out, t_out, rep = cl.flat_back_close(verts, tris)
    elapsed = time.time() - t0

    assert rep['watertight'] is True, f"5000-vertex loop not watertight: {rep}"
    assert elapsed < 5.0, f"Large loop closing took too long: {elapsed:.2f} s"

    mf = manifold3d.Manifold(mesh=manifold3d.Mesh(vert_properties=v_out, tri_verts=t_out))
    assert mf.status() == manifold3d.Error.NoError, f"manifold3d error: {mf.status()}"
    print(f"  ✓ test_large_loop_flat_back passed (5000-vertex loop in {elapsed:.3f} s)")


def _signed_volume(v, t):
    v0 = v[t[:, 0]]
    v1 = v[t[:, 1]]
    v2 = v[t[:, 2]]
    return float(np.sum(np.einsum('ij,ij->i', v0, np.cross(v1, v2))) / 6.0)


def test_large_loop_flat_back_clockwise():
    """The centroid-fan cap must follow the loop orientation (2.3): a clockwise
    boundary loop (reversed triangle winding, > 1500 vertices) must still close
    into a positive-volume, outward-oriented shell, not an inverted cap."""
    n_boundary = 5000
    theta = np.linspace(0, 2 * np.pi, n_boundary, endpoint=False)
    x, y = np.cos(theta), np.sin(theta)
    z = 0.1 * np.cos(3 * theta)
    center = np.array([[0.0, 0.0, 0.2]])
    verts = np.vstack([center, np.column_stack([x, y, z])])
    tris = []
    for i in range(1, n_boundary + 1):
        next_i = 1 if i == n_boundary else (i + 1)
        tris.append([0, next_i, i])          # reversed winding -> CW loop
    tris = np.array(tris, dtype=np.int64)

    v_out, t_out, rep = cl.flat_back_close(verts, tris)
    assert rep['watertight'] is True, f"clockwise loop not watertight: {rep}"
    assert _signed_volume(v_out, t_out) > 0, "inverted cap: negative volume"
    mf = manifold3d.Manifold(mesh=manifold3d.Mesh(vert_properties=v_out, tri_verts=t_out))
    assert mf.status() == manifold3d.Error.NoError, f"manifold3d error: {mf.status()}"
    print("  ✓ test_large_loop_flat_back_clockwise passed")


def main():
    t0 = time.time()
    print("Running scan closing tests (tests/test_closing.py)...")
    test_boundary_loops()
    test_single_side_score()
    test_relief_score()
    test_poisson_close_hemisphere()
    test_flat_back_close_relief()
    test_flat_back_secondary_hole()
    test_non_convex_l_relief()
    test_large_loop_flat_back()
    test_large_loop_flat_back_clockwise()
    test_defensive_error_handling()
    elapsed = time.time() - t0
    print(f"\nAll tests passed in {elapsed:.2f} s (< 30 s target).")
    assert elapsed < 30.0, f"Tests exceeded 30 s limit: {elapsed:.2f} s"


if __name__ == '__main__':
    main()
