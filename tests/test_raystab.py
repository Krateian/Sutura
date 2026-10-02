#!/usr/bin/env python3
"""Smoke tests for the ray-stabbing inside/outside core (sutura_geom.raystab_*)
and the opt-in Graft integration.

The vote is the oriented net crossing count with an orientation-independent
parity fallback; a ray escaping without a hit is a strong outside vote.  The
feature is OFF by default and must be byte-identical to the winding-only path
when off.

Run with the venv that has the maturin-built extension (no pymeshlab needed):
    <venv>/bin/python tests/test_raystab.py

pytest-compatible (plain ``test_*`` functions) and runnable directly.
"""
import os
import sys
from collections import Counter

import numpy as np
import trimesh

import sutura_geom


# --------------------------------------------------------------------------- #
# helpers
# --------------------------------------------------------------------------- #
def as_arrays(mesh):
    return (np.asarray(mesh.vertices, dtype=np.float64),
            np.asarray(mesh.faces, dtype=np.int32))


def cube_arrays():
    return as_arrays(trimesh.creation.box(extents=[1.0, 1.0, 1.0]))


def cube_with_hole_arrays():
    mesh = trimesh.creation.box(extents=[1.0, 1.0, 1.0])
    c = mesh.triangles_center
    mesh.update_faces(c[:, 2] < 0.49)  # cut the top face off -> open hole
    mesh.remove_unreferenced_vertices()
    return as_arrays(mesh)


def bits(arr):
    return (tuple(arr.shape), np.asarray(arr).tobytes())


def manifold_ok(tris):
    d = Counter()
    for a, b, c in tris:
        for u, v in ((a, b), (b, c), (c, a)):
            d[(int(u), int(v))] += 1
    for (u, v), cnt in d.items():
        if cnt != 1 or d.get((v, u), 0) != 1:
            return False
    return True


# --------------------------------------------------------------------------- #
# raystab_points
# --------------------------------------------------------------------------- #
def test_raystab_points_inside_outside():
    v, f = cube_arrays()
    # box extents [1,1,1] is centred at the origin.
    pts = np.array([[0.0, 0.0, 0.0], [5.0, 5.0, 5.0], [-0.25, 0.1, 0.2]],
                   dtype=np.float64)
    inside, iv, ov, ev = sutura_geom.raystab_points(v, f, pts)
    inside = np.asarray(inside)
    assert bool(inside[0]) is True, "cube centre voted outside"
    assert bool(inside[1]) is False, "far point voted inside"
    # Every ray from the interior leaves through exactly one face.
    assert int(np.asarray(iv)[0]) == int(sutura_geom.RAYSTAB_DEFAULT_DIRS)
    # The far point sees no crossings at all -> all escape votes.
    assert int(np.asarray(ev)[1]) == int(sutura_geom.RAYSTAB_DEFAULT_DIRS)


def test_raystab_points_escape_weight_and_parity_args():
    v, f = cube_arrays()
    pts = np.array([[0.0, 0.0, 0.0]], dtype=np.float64)
    for parity in (False, True):
        inside, _, _, _ = sutura_geom.raystab_points(
            v, f, pts, n_dirs=7, seed=3, parity=parity, escape_weight=1.0)
        assert bool(np.asarray(inside)[0]) is True, f"parity={parity}"


def test_raystab_rejects_bad_points():
    v, f = cube_arrays()
    try:
        sutura_geom.raystab_points(v, f, np.zeros((2, 2), dtype=np.float64))
    except ValueError:
        pass
    else:
        raise AssertionError("expected an error for non-Mx3 points")


# --------------------------------------------------------------------------- #
# raystab_grid
# --------------------------------------------------------------------------- #
def test_raystab_grid_centre_and_outside():
    v, f = cube_arrays()
    grid, score, info = sutura_geom.raystab_grid(
        v, f, voxel=0.5, box=np.array([[-0.5, -0.5, -0.5], [1.5, 1.5, 1.5]]),
        n_dirs=7)
    grid = np.asarray(grid)
    info_dims = list(info["dims"])
    assert list(grid.shape) == info_dims
    o = info["origin"]
    c = [int(round((0.5 - o[i]) / info["voxel"])) for i in range(3)]
    assert grid[c[0], c[1], c[2]] == 1, "grid centre not inside"
    assert grid[0, 0, 0] == 0, "grid corner not outside"


def test_asymmetric_grid_axis_order():
    # BUG-02 regression: the flat grid is i-fastest (Fortran); the Python array
    # must expose [i, j, k] with matching strides, not a transposed X/Z view.
    # Asymmetric dims (nx != nz) are what expose an order mismatch.
    m = trimesh.creation.box(extents=(4.0, 1.0, 1.0))
    m.apply_translation((1.0, 0.0, 0.0))
    v = np.asarray(m.vertices, np.float64)
    f = np.asarray(m.faces, np.int32)
    w, _u, _s, info = sutura_geom.sdf_grid(v, f, voxel=0.5)
    w = np.asarray(w)
    assert info["dims"][0] != info["dims"][1], "fixture must be asymmetric"
    o = info["origin"]
    vx = info["voxel"]

    def w_at(p):
        c = [int(round((p[i] - o[i]) / vx)) for i in range(3)]
        return c, float(w[c[0], c[1], c[2]])

    _c_in, w_in = w_at((2.5, 0.0, 0.0))     # strictly inside the box
    _c_out, w_out = w_at((-1.5, 0.5, 0.0))  # just outside along X
    assert w_in > 0.5, ("inside point read as outside (axis order)", w_in)
    assert w_out < 0.5, ("outside point read as inside", w_out)

    grid, _score, ginfo = sutura_geom.raystab_grid(v, f, voxel=0.5, n_dirs=7)
    grid = np.asarray(grid)
    go = ginfo["origin"]
    gv = ginfo["voxel"]
    gc = [int(round((p - go[i]) / gv)) for i, p in enumerate((2.5, 0.0, 0.0))]
    assert grid[gc[0], gc[1], gc[2]] == 1, "raystab_grid axis order"


# --------------------------------------------------------------------------- #
# sdf_grid / morph_close: off is byte-identical, on is reported
# --------------------------------------------------------------------------- #
def test_sdf_grid_raystab_off_is_identical():
    v, f = cube_with_hole_arrays()
    a = sutura_geom.sdf_grid(v, f, 0.08)
    b = sutura_geom.sdf_grid(v, f, 0.08, raystab=False)
    for arr_a, arr_b in zip(a[:3], b[:3]):
        assert bits(arr_a) == bits(arr_b), "raystab=False changed the field"
    assert "raystab" not in a[3] and "raystab" not in b[3]


def test_sdf_grid_raystab_on_reports_and_keeps_shape():
    v, f = cube_with_hole_arrays()
    w, u, s, info = sutura_geom.sdf_grid(v, f, 0.08, raystab=True)
    assert "raystab" in info
    rc = info["raystab"]
    assert rc["enabled"] is True
    assert int(rc["dirs"]) == int(sutura_geom.RAYSTAB_DEFAULT_DIRS)
    assert list(w.shape) == list(u.shape) == list(s.shape) == list(info["dims"])


def test_morph_close_raystab_off_is_identical():
    v, f = cube_with_hole_arrays()
    a_v, a_t, _ = sutura_geom.morph_close(v, f, 0.15, 0.08, None, False)
    b_v, b_t, _ = sutura_geom.morph_close(v, f, 0.15, 0.08, None, False,
                                          raystab=False)
    assert bits(a_v) == bits(b_v) and bits(a_t) == bits(b_t)


def test_morph_close_raystab_on_is_manifold():
    v, f = cube_with_hole_arrays()
    ov, ot, info = sutura_geom.morph_close(v, f, 0.15, 0.08, None, False,
                                           raystab=True)
    ot = np.asarray(ot)
    assert info["manifold"], "raystab morph_close not manifold"
    assert manifold_ok(ot)
    assert "raystab" in info


# --------------------------------------------------------------------------- #
# cast.py wrappers
# --------------------------------------------------------------------------- #
def test_cast_wrappers_are_available():
    sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(
        os.path.abspath(__file__))), "sutura"))
    from sutura_engine import cast  # noqa: E402
    assert cast.is_available()
    v, f = cube_arrays()
    inside, _, _, _ = cast.raystab_points(
        v, f, np.array([[0.0, 0.0, 0.0]], dtype=np.float64))
    assert bool(np.asarray(inside)[0]) is True
    grid, score, info = cast.raystab_grid(v, f, voxel=0.25)
    assert np.asarray(grid).sum() > 0


# --------------------------------------------------------------------------- #
# Graft opt-in
# --------------------------------------------------------------------------- #
def test_graft_raystab_off_is_identical_and_on_reports():
    sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(
        os.path.abspath(__file__))), "sutura"))
    from shell_wrap import shell_wrap  # noqa: E402
    v, f = cube_with_hole_arrays()
    av, at, arep = shell_wrap(v, f, voxel=0.12, max_tries=1, remesh=False,
                              guard=False, grid_budget=80_000)
    bv, bt, brep = shell_wrap(v, f, voxel=0.12, max_tries=1, remesh=False,
                              guard=False, grid_budget=80_000, raystab=False)
    assert bits(av) == bits(bv) and bits(at) == bits(bt)
    assert arep.get("raystab") is None and brep.get("raystab") is None

    cv, ct, crep = shell_wrap(v, f, voxel=0.12, max_tries=1, remesh=False,
                              guard=False, grid_budget=80_000, raystab=True)
    assert crep.get("raystab") is not None
    assert len(ct) > 0


if __name__ == "__main__":
    fns = [v for k, v in sorted(globals().items())
           if k.startswith("test_") and callable(v)]
    for fn in fns:
        fn()
        print(f"ok  {fn.__name__}")
    print(f"all {len(fns)} raystab smoke tests passed")
