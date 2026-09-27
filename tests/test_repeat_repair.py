#!/usr/bin/env python3
"""Regression test for sutura/repeat_repair.py (repeated-element repair).

Checks:
  1. segment_elements segments feature-edge bounded element patches.
  2. align computes rigid transform T with machine precision.
  3. repair_repeat_auto repairs synthetic 12-knurl cylinder with 1 damaged knurl.
  4. repair_repeat_auto repairs synthetic 3x3 tile grid on plate with 1 damaged tile.
  5. repair_repeat_manual transplants healthy copy between explicit source & target points.
  6. Defensive error handling on invalid/empty inputs without raising exceptions.

Runtime target: < 30 s (< 60 s limit).
Usage: python3 tests/test_repeat_repair.py
"""

import os
import sys
import time
import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)
sys.path.insert(0, REPO)

try:
    import manifold3d  # noqa: F401, E402
except ImportError:
    manifold3d = None
import sutura.repeat_repair as rr  # noqa: E402


def _make_knurl_cylinder(damaged_idx=3):
    """Synthetic cylinder with 12 knurl bumps around the rim, 1 damaged."""
    R = 5.0
    H = 2.0
    n_knurls = 12
    cylinder = manifold3d.Manifold.cylinder(
        height=H, radius_low=R, radius_high=R, circular_segments=64
    ).translate([0, 0, -H / 2])

    knurls = []
    for i in range(n_knurls):
        angle = 2.0 * np.pi * i / n_knurls
        if i == damaged_idx:
            bump = manifold3d.Manifold.cube([0.3, 0.4, H * 0.4], center=True).translate([R, 0, 0])
        else:
            bump = manifold3d.Manifold.cube([0.8, 0.8, H * 0.8], center=True).translate([R, 0, 0])
        T = np.eye(4)
        T[:3, :3] = [
            [np.cos(angle), -np.sin(angle), 0],
            [np.sin(angle),  np.cos(angle), 0],
            [0,              0,             1]
        ]
        bump = bump.transform(T[:3, :])
        knurls.append(bump)

    mesh_m = cylinder
    for b in knurls:
        mesh_m = mesh_m + b

    mesh = mesh_m.to_mesh()
    return np.asarray(mesh.vert_properties, np.float64), np.asarray(mesh.tri_verts, np.int64)


def _make_tile_grid(damaged_ij=(1, 2)):
    """Synthetic plate with 3x3 grid of raised square tiles, 1 damaged."""
    plate_w = 12.0
    plate_h = 1.0
    plate = manifold3d.Manifold.cube([plate_w, plate_w, plate_h], center=True)

    tile_size = 2.0
    tile_height = 0.8
    tile_spacing = 3.5

    tiles = []
    for i in range(3):
        for j in range(3):
            x = (i - 1) * tile_spacing
            y = (j - 1) * tile_spacing
            z = plate_h / 2 + tile_height / 2
            if (i, j) == damaged_ij:
                t = manifold3d.Manifold.cube([tile_size * 0.5, tile_size * 0.5, tile_height * 0.3], center=True)
            else:
                t = manifold3d.Manifold.cube([tile_size, tile_size, tile_height], center=True)
            t = t.translate([x, y, z])
            tiles.append(t)

    grid_m = plate
    for t in tiles:
        grid_m = grid_m + t

    mesh = grid_m.to_mesh()
    return np.asarray(mesh.vert_properties, np.float64), np.asarray(mesh.tri_verts, np.int64)


def _make_helical_thread_cylinder(damaged_idx=3):
    """Synthetic cylinder with 6 helical thread segments along Z, 1 damaged."""
    R = 5.0
    H = 8.0
    n_threads = 6
    theta_step = 2.0 * np.pi / n_threads
    h_step = 1.0

    cylinder = manifold3d.Manifold.cylinder(
        height=H, radius_low=R, radius_high=R, circular_segments=64
    ).translate([0, 0, -H / 2])

    threads = []
    for i in range(n_threads):
        angle = i * theta_step
        z = -2.5 + i * h_step
        if i == damaged_idx:
            bump = manifold3d.Manifold.cube([0.3, 0.4, 0.3], center=True).translate([R, 0, 0])
        else:
            bump = manifold3d.Manifold.cube([0.8, 1.2, 0.6], center=True).translate([R, 0, 0])
        T = np.eye(4)
        T[:3, :3] = [
            [np.cos(angle), -np.sin(angle), 0],
            [np.sin(angle),  np.cos(angle), 0],
            [0,              0,             1]
        ]
        bump = bump.transform(T[:3, :]).translate([0, 0, z])
        threads.append(bump)

    mesh_m = cylinder
    for b in threads:
        mesh_m = mesh_m + b

    mesh = mesh_m.to_mesh()
    return np.asarray(mesh.vert_properties, np.float64), np.asarray(mesh.tri_verts, np.int64)


def test_segment_elements():
    v, t = _make_knurl_cylinder(damaged_idx=3)
    patches = rr.segment_elements(v, t)
    assert len(patches) == 12, f"Expected 12 candidate element patches, got {len(patches)}"
    for p in patches:
        assert p['area'] > 0.0
        assert 'centroid' in p
        assert 'frame' in p
    print(f"  ✓ test_segment_elements passed ({len(patches)} patches segmented)")


def test_align():
    np.random.seed(42)
    pts = np.random.randn(80, 3)
    theta = 0.5
    R_true = np.array([
        [np.cos(theta), -np.sin(theta), 0],
        [np.sin(theta),  np.cos(theta), 0],
        [0,              0,             1]
    ])
    t_true = np.array([3.0, -2.0, 1.5])
    target = (pts @ R_true.T) + t_true

    T, rms = rr.align(pts, target)
    assert rms < 1e-4, f"Alignment residual too high: {rms}"
    assert np.allclose(T[:3, 3], t_true, atol=1e-3)
    print(f"  ✓ test_align passed (RMS: {rms:.2e})")


def test_knurl_cylinder_auto():
    v, t = _make_knurl_cylinder(damaged_idx=3)
    v_out, t_out, rep = rr.repair_repeat_auto(v, t)

    assert rep['watertight'] is True, f"Repaired cylinder not watertight: {rep}"
    assert rep['repaired'] is True, "Auto repair failed to repair damaged knurl"
    assert rep['pattern_type'] == 'rotational', f"Wrong pattern type: {rep['pattern_type']}"
    assert rep['positions_repaired'] == 1, f"Expected 1 repaired position, got {rep['positions_repaired']}"
    assert rep['hausdorff_outside'] < 1e-4, f"Untouched region changed: H={rep['hausdorff_outside']}"
    print(
        f"  ✓ test_knurl_cylinder_auto passed "
        f"(pattern: {rep['pattern_type']}, repaired: {rep['positions_repaired']}, H_outside: {rep['hausdorff_outside']:.6f})"
    )


def test_tile_grid_auto():
    v, t = _make_tile_grid(damaged_ij=(1, 2))
    v_out, t_out, rep = rr.repair_repeat_auto(v, t)

    assert rep['watertight'] is True, f"Repaired tile grid not watertight: {rep}"
    assert rep['repaired'] is True, "Auto repair failed to repair damaged tile"
    assert rep['pattern_type'] == 'translational', f"Wrong pattern type: {rep['pattern_type']}"
    assert rep['positions_repaired'] == 1, f"Expected 1 repaired position, got {rep['positions_repaired']}"
    assert rep['hausdorff_outside'] < 1e-4, f"Untouched region changed: H={rep['hausdorff_outside']}"
    print(
        f"  ✓ test_tile_grid_auto passed "
        f"(pattern: {rep['pattern_type']}, repaired: {rep['positions_repaired']}, H_outside: {rep['hausdorff_outside']:.6f})"
    )


def test_helical_cylinder_auto():
    v, t = _make_helical_thread_cylinder(damaged_idx=3)
    v_out, t_out, rep = rr.repair_repeat_auto(v, t)

    assert rep['watertight'] is True, f"Repaired helical cylinder not watertight: {rep}"
    assert rep['repaired'] is True, "Auto repair failed to repair damaged thread segment"
    assert rep['pattern_type'] == 'helical', f"Wrong pattern type: {rep['pattern_type']}"
    assert rep['positions_repaired'] == 1, f"Expected 1 repaired position, got {rep['positions_repaired']}"
    assert rep['hausdorff_outside'] < 1e-4, f"Untouched region changed: H={rep['hausdorff_outside']}"
    print(
        f"  ✓ test_helical_cylinder_auto passed "
        f"(pattern: {rep['pattern_type']}, repaired: {rep['positions_repaired']}, H_outside: {rep['hausdorff_outside']:.6f})"
    )


def test_closing_free_validity():
    # Valid box
    v_box = np.array([
        [0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
        [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]
    ], dtype=np.float64)
    t_box = np.array([
        [0, 1, 2], [0, 2, 3],  # bottom
        [4, 6, 5], [4, 7, 6],  # top
        [0, 4, 5], [0, 5, 1],  # front
        [2, 6, 7], [2, 7, 3],  # back
        [0, 3, 7], [0, 7, 4],  # left
        [1, 5, 6], [1, 6, 2],  # right
    ], dtype=np.int64)

    is_valid, msg = rr.check_mesh_validity(v_box, t_box)
    assert is_valid is True, f"Valid box failed check: {msg}"

    # Open box (missing 1 face)
    is_valid_open, _ = rr.check_mesh_validity(v_box, t_box[:-1])
    assert is_valid_open is False, "Open box should not be valid"
    print("  ✓ test_closing_free_validity passed")


def test_manual_repair():
    v, t = _make_tile_grid(damaged_ij=(1, 2))
    # Source: healthy tile at (0, 0)
    src_pt = [0.0, 0.0, 1.0]
    # Target: damaged tile at (0, 3.5)
    dst_pt = [0.0, 3.5, 1.0]

    v_out, t_out, rep = rr.repair_repeat_manual(v, t, src_pt, dst_pt)

    assert rep['watertight'] is True, f"Manual repaired mesh not watertight: {rep}"
    assert rep['repaired'] is True, "Manual repair failed"
    assert rep['hausdorff_outside'] < 1e-4, f"Untouched region changed: H={rep['hausdorff_outside']}"
    assert rep['volume_change'] > 0.0, "Expected volume change from replacing damaged tile"
    print(
        f"  ✓ test_manual_repair passed "
        f"(vol_change: {rep['volume_change']:.4f}, H_outside: {rep['hausdorff_outside']:.6f})"
    )


def test_defensive_error_handling():
    empty_v = np.zeros((0, 3), dtype=np.float64)
    empty_t = np.zeros((0, 3), dtype=np.int64)

    _, _, rep = rr.repair_repeat_auto(empty_v, empty_t)
    assert rep['watertight'] is False
    assert 'error' in rep

    _, _, rep_man = rr.repair_repeat_manual(empty_v, empty_t, [0, 0, 0], [1, 1, 1])
    assert rep_man['watertight'] is False
    assert 'error' in rep_man
    print("  ✓ test_defensive_error_handling passed")


def test_csg_bridge_forced():
    # Force out-of-process csg_bridge execution via force_bridge=True parameter
    v, t = _make_helical_thread_cylinder(damaged_idx=3)
    v_out, t_out, rep = rr.repair_repeat_auto(v, t, force_bridge=True)

    assert rep['watertight'] is True, f"Forced bridge helical repair not watertight: {rep}"
    assert rep['repaired'] is True, f"Forced bridge helical repair failed: {rep}"
    assert rep['positions_repaired'] == 1
    assert rep['hausdorff_outside'] < 1e-4

    # Also test manual repair with force_bridge=True
    v_grid, t_grid = _make_tile_grid(damaged_ij=(1, 2))
    src_pt = [0.0, 0.0, 1.0]
    dst_pt = [0.0, 3.5, 1.0]
    v_mout, t_mout, rep_m = rr.repair_repeat_manual(v_grid, t_grid, src_pt, dst_pt, force_bridge=True)
    assert rep_m['watertight'] is True, f"Forced bridge manual repair not watertight: {rep_m}"
    assert rep_m['repaired'] is True
    assert rep_m['hausdorff_outside'] < 1e-4
    assert rep_m['volume_change'] > 0.0
    print("  ✓ test_csg_bridge_forced passed (both auto and manual through csg_bridge.py)")


def test_csg_bridge_env_var():
    # Force out-of-process csg_bridge execution via SUTURA_FORCE_CSG_BRIDGE env var
    old_env = os.environ.get('SUTURA_FORCE_CSG_BRIDGE')
    try:
        os.environ['SUTURA_FORCE_CSG_BRIDGE'] = '1'
        v, t = _make_tile_grid(damaged_ij=(1, 2))
        v_out, t_out, rep = rr.repair_repeat_auto(v, t)
        assert rep['watertight'] is True, f"Env-var forced bridge repair not watertight: {rep}"
        assert rep['repaired'] is True, f"Env-var forced bridge repair failed: {rep}"
        assert rep['positions_repaired'] == 1
        assert rep['hausdorff_outside'] < 1e-4
        print("  ✓ test_csg_bridge_env_var passed (SUTURA_FORCE_CSG_BRIDGE=1)")
    finally:
        if old_env is None:
            os.environ.pop('SUTURA_FORCE_CSG_BRIDGE', None)
        else:
            os.environ['SUTURA_FORCE_CSG_BRIDGE'] = old_env


def main():
    t0 = time.time()
    print("Running repeated-element repair tests (tests/test_repeat_repair.py)...")
    test_closing_free_validity()
    test_segment_elements()
    test_align()
    test_knurl_cylinder_auto()
    test_tile_grid_auto()
    test_helical_cylinder_auto()
    test_manual_repair()
    test_csg_bridge_forced()
    test_csg_bridge_env_var()
    test_defensive_error_handling()
    elapsed = time.time() - t0
    print(f"\nAll tests passed in {elapsed:.2f} s (< 60 s target).")
    assert elapsed < 60.0, f"Tests exceeded 60 s limit: {elapsed:.2f} s"


if __name__ == '__main__':
    main()
