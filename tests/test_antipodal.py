#!/usr/bin/env python3
"""Smoke tests for the robust (antipodal) generalized winding number core
(`sutura_geom.antipodal_points` / `PyMeshBvh.winding_robust_points`).

The value is the sum of an exact signed ``-x`` ray crossing count and an exact
boundary term, coupled through one exact determinant so the field cannot jump by
~+-1 near the surface (Martens, Trettner & Bessmeltsev, ACM TOG 45(4), 2026,
art. 43).  It is deterministic and independent of triangle/vertex ordering.

Run with the venv that has the maturin-built extension (no pymeshlab needed):
    <venv>/bin/python tests/test_antipodal.py

pytest-compatible (plain ``test_*`` functions) and runnable directly.
"""
import glob
import os
import sys

import numpy as np
import trimesh

import sutura_geom


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


def pseudo_random_points(lo, hi, n, seed=1234):
    rng = np.random.default_rng(seed)
    return lo + (hi - lo) * rng.random((n, 3))


# --------------------------------------------------------------------------- #
# basic classification
# --------------------------------------------------------------------------- #
def test_closed_cube_inside_and_outside():
    v, f = cube_arrays()
    inside, w = sutura_geom.antipodal_points(
        v, f, np.array([[0.0, 0.0, 0.0], [5.0, 5.0, 5.0], [-0.25, 0.1, 0.2]]))
    inside = np.asarray(inside)
    w = np.asarray(w)
    assert bool(inside[0]) is True, "cube centre read as outside"
    assert abs(w[0] - 1.0) < 1e-9, w[0]
    assert bool(inside[1]) is False, "far point read as inside"
    assert abs(w[1]) < 1e-9, w[1]
    assert bool(inside[2]) is True, "off-axis interior point read as outside"


def test_open_cube_is_finite_on_grazing_points():
    v, f = cube_with_hole_arrays()
    pts = np.array([[0.5, 0.5, 0.5], [0.5, 0.5, 0.0], [0.5, 0.0, 0.0],
                    [0.0, 0.0, 0.5], [-0.25, 1.0, 1.0]], dtype=np.float64)
    _inside, w = sutura_geom.antipodal_points(v, f, pts)
    w = np.asarray(w)
    assert np.all(np.isfinite(w)), w


def test_rejects_bad_points():
    v, f = cube_arrays()
    try:
        sutura_geom.antipodal_points(v, f, np.zeros((2, 2), dtype=np.float64))
    except ValueError:
        pass
    else:
        raise AssertionError("expected an error for non-Mx3 points")


# --------------------------------------------------------------------------- #
# determinism and order independence
# --------------------------------------------------------------------------- #
def test_deterministic_between_calls():
    v, f = cube_arrays()
    pts = pseudo_random_points(np.array([-2.0] * 3), np.array([2.0] * 3), 300)
    a = sutura_geom.antipodal_points(v, f, pts)
    b = sutura_geom.antipodal_points(v, f, pts)
    assert bits(a[0]) == bits(b[0]) and bits(a[1]) == bits(b[1])


def test_order_independent():
    v, f = cube_arrays()
    pts = pseudo_random_points(np.array([-2.0] * 3), np.array([2.0] * 3), 200)
    base = sutura_geom.antipodal_points(v, f, pts)[1]

    rng = np.random.default_rng(7)
    perm = rng.permutation(len(v))               # relabel vertices
    inv = np.empty_like(perm)
    inv[perm] = np.arange(len(perm))
    v2 = v[perm]
    f2 = inv[f]
    f2 = f2[rng.permutation(len(f2))]            # shuffle triangles
    shuffled = sutura_geom.antipodal_points(v2, f2, pts)[1]
    assert bits(base) == bits(shuffled), "winding changed under reordering"


def test_class_method_matches_module_function():
    v, f = cube_with_hole_arrays()
    pts = pseudo_random_points(np.array([-1.0] * 3), np.array([1.0] * 3), 100)
    _inside, w = sutura_geom.antipodal_points(v, f, pts)
    bvh = sutura_geom.PyMeshBvh(v, f)
    w2 = bvh.winding_robust_points(pts)
    assert bits(np.asarray(w)) == bits(np.asarray(w2))


# --------------------------------------------------------------------------- #
# parity with the classical GWN away from the surface
# --------------------------------------------------------------------------- #
def _clean_parity(verts, tris, seed):
    lo = verts.min(0)
    hi = verts.max(0)
    diag = float(np.linalg.norm(hi - lo))
    margin = 0.25 * diag
    pts = pseudo_random_points(lo - margin, hi + margin, 400, seed=seed)
    _xyz, dist, _fi = sutura_geom.closest_points(verts, tris, pts)
    dist = np.asarray(dist)
    tol = max(1e-5, 1e-3 * diag)
    clean = dist > tol
    if clean.sum() == 0:
        return None
    _ins_a, wa = sutura_geom.antipodal_points(verts, tris, pts[clean])
    wref = np.asarray(sutura_geom.PyMeshBvh(verts, tris).winding_points(pts[clean]))
    agree = int(np.sum((wa > 0.5) == (wref > 0.5)))
    return agree, int(clean.sum()), float(np.max(np.abs(wa - wref)))


def test_parity_with_classical_gwn_on_synthetic():
    for arrays, seed in ((cube_arrays(), 1),
                         (as_arrays(trimesh.creation.icosphere(subdivisions=3)), 2)):
        v, f = arrays
        got = _clean_parity(v, f, seed)
        assert got is not None
        agree, n, maxdiff = got
        assert agree == n, f"closed mesh parity {agree}/{n}, max|dw|={maxdiff:.3g}"


def test_parity_with_classical_gwn_on_corpus():
    here = os.path.dirname(os.path.abspath(__file__))
    files = sorted(glob.glob(os.path.join(here, "real-world-samples", "*.stl")))
    if not files:
        print("  (no corpus meshes; skipped)")
        return
    total_ok = total = 0
    worst = []
    for i, path in enumerate(files):
        mesh = trimesh.load(path, force="mesh")
        v = np.asarray(mesh.vertices, dtype=np.float64)
        f = np.asarray(mesh.faces, dtype=np.int32)
        got = _clean_parity(v, f, seed=100 + i)
        if got is None:
            continue
        agree, n, maxdiff = got
        total_ok += agree
        total += n
        if agree < n:
            worst.append((os.path.basename(path), n - agree, round(maxdiff, 4)))
    rate = total_ok / total if total else 1.0
    print(f"  corpus clean-point GWN parity: {total_ok}/{total} = {rate:.4f}")
    for name, bad, md in worst:
        print(f"    {name}: {bad} disagree, max|dw|={md}")
    assert rate >= 0.95, f"corpus parity too low: {rate:.4f}"


# --------------------------------------------------------------------------- #
# cast.py wrapper
# --------------------------------------------------------------------------- #
def test_cast_wrapper():
    sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(
        os.path.abspath(__file__))), "sutura"))
    from sutura_engine import cast  # noqa: E402
    assert cast.is_available()
    assert cast.antipodal_supported()
    v, f = cube_arrays()
    inside, w = cast.antipodal_points(
        v, f, np.array([[0.0, 0.0, 0.0], [3.0, 3.0, 3.0]]))
    assert bool(np.asarray(inside)[0]) is True
    assert bool(np.asarray(inside)[1]) is False


# --------------------------------------------------------------------------- #
# rust sign-field integration (sdf_grid / morph_close, opt-in)
# --------------------------------------------------------------------------- #
def manifold_ok(tris):
    from collections import Counter
    d = Counter()
    for a, b, c in tris:
        for u, v in ((a, b), (b, c), (c, a)):
            d[(int(u), int(v))] += 1
    for (u, v), cnt in d.items():
        if cnt != 1 or d.get((v, u), 0) != 1:
            return False
    return True


def test_sdf_grid_antipodal_off_is_identical():
    v, f = cube_with_hole_arrays()
    a = sutura_geom.sdf_grid(v, f, 0.08)
    b = sutura_geom.sdf_grid(v, f, 0.08, antipodal=False)
    for arr_a, arr_b in zip(a[:3], b[:3]):
        assert bits(arr_a) == bits(arr_b), "antipodal=False changed the field"
    assert "antipodal" not in a[3] and "antipodal" not in b[3]


def test_sdf_grid_antipodal_on_reports_and_keeps_shape():
    v, f = cube_with_hole_arrays()
    w, u, s, info = sutura_geom.sdf_grid(v, f, 0.08, antipodal=True)
    assert "antipodal" in info
    ac = info["antipodal"]
    assert ac["enabled"] is True
    assert list(w.shape) == list(u.shape) == list(s.shape) == list(info["dims"])


def test_sdf_grid_antipodal_is_deterministic():
    v, f = cube_with_hole_arrays()
    a = sutura_geom.sdf_grid(v, f, 0.08, antipodal=True)[2]
    b = sutura_geom.sdf_grid(v, f, 0.08, antipodal=True)[2]
    assert bits(a) == bits(b)


def test_morph_close_antipodal_off_is_identical():
    v, f = cube_with_hole_arrays()
    a_v, a_t, _ = sutura_geom.morph_close(v, f, 0.15, 0.08, None, False)
    b_v, b_t, _ = sutura_geom.morph_close(v, f, 0.15, 0.08, None, False,
                                          antipodal=False)
    assert bits(a_v) == bits(b_v) and bits(a_t) == bits(b_t)


def test_morph_close_antipodal_on_is_manifold():
    v, f = cube_with_hole_arrays()
    ov, ot, info = sutura_geom.morph_close(v, f, 0.15, 0.08, None, False,
                                           antipodal=True)
    assert info["manifold"], "antipodal morph_close not manifold"
    assert manifold_ok(np.asarray(ot))
    assert "antipodal" in info


# --------------------------------------------------------------------------- #
# Graft opt-in wiring (shell_wrap, SUTURA_ANTIPODAL)
# --------------------------------------------------------------------------- #
def test_graft_antipodal_off_is_identical_and_on_reports():
    sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(
        os.path.abspath(__file__))), "sutura"))
    from sutura_engine.graft import shell_wrap  # noqa: E402
    v, f = cube_with_hole_arrays()
    kw = dict(voxel=0.12, max_tries=1, remesh=False, guard=False,
              grid_budget=80_000)
    av, at, arep = shell_wrap(v, f, **kw)
    bv, bt, brep = shell_wrap(v, f, antipodal=False, **kw)
    assert bits(av) == bits(bv) and bits(at) == bits(bt)
    assert arep.get("antipodal") is None and brep.get("antipodal") is None

    cv, ct, crep = shell_wrap(v, f, antipodal=True, **kw)
    assert crep.get("antipodal") is not None
    assert len(ct) > 0

    os.environ["SUTURA_ANTIPODAL"] = "1"
    try:
        dv, dt, drep = shell_wrap(v, f, **kw)
        assert drep.get("antipodal") is not None
    finally:
        del os.environ["SUTURA_ANTIPODAL"]


if __name__ == "__main__":
    fns = [v for k, v in sorted(globals().items())
           if k.startswith("test_") and callable(v)]
    for fn in fns:
        fn()
        print(f"ok  {fn.__name__}")
    print(f"all {len(fns)} antipodal smoke tests passed")
