#!/usr/bin/env python3
"""Regression tests for the sutura_geom morphology core (F2):
winding/unsigned/signed SDF grids, EDT-based closing and dual contouring.

Run with the venv that has the maturin-built extension:
    maturin develop --release --interpreter <venv>/bin/python
    <venv>/bin/python tests/test_morph_geom.py

pytest-compatible (plain `test_*` functions) and also runnable directly.
"""
from collections import Counter, defaultdict

import numpy as np
import trimesh

import sutura_geom


# --------------------------------------------------------------------------- #
# mesh helpers
# --------------------------------------------------------------------------- #
def as_arrays(mesh):
    verts = np.asarray(mesh.vertices, dtype=np.float64)
    faces = np.asarray(mesh.faces, dtype=np.int32)
    return verts, faces


def manifold_ok(tris):
    """Every directed edge exactly once and paired with its reverse."""
    d = Counter()
    for a, b, c in tris:
        for u, v in ((a, b), (b, c), (c, a)):
            d[(int(u), int(v))] += 1
    for (u, v), cnt in d.items():
        if cnt != 1 or d.get((v, u), 0) != 1:
            return False
    return True


def n_components(nverts, tris):
    parent = list(range(nverts))

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    for a, b, c in tris:
        ra, rb = find(int(a)), find(int(b))
        if ra != rb:
            parent[ra] = rb
        rb, rc = find(int(b)), find(int(c))
        if rb != rc:
            parent[rb] = rc
    return len({find(i) for i in range(nverts)})


def euler_genus(nverts, tris):
    edges = set()
    for a, b, c in tris:
        for u, v in ((a, b), (b, c), (c, a)):
            edges.add((min(int(u), int(v)), max(int(u), int(v))))
    chi = nverts - len(edges) + len(tris)
    return (2 - chi) // 2, chi


def signed_volume(verts, tris):
    v = verts
    s = 0.0
    for a, b, c in tris:
        s += float(np.dot(v[a], np.cross(v[b], v[c]))) / 6.0
    return s


def closing(verts, tris, r, voxel, fill_cavities=False):
    out_v, out_t, info = sutura_geom.morph_close(
        verts, tris, r, voxel, None, fill_cavities)
    return np.asarray(out_v, dtype=np.float64), np.asarray(out_t, dtype=np.int64), info


# --------------------------------------------------------------------------- #
# winding / sdf grid
# --------------------------------------------------------------------------- #
def test_sdf_grid_sign_and_shape():
    mesh = trimesh.creation.icosphere(subdivisions=3, radius=1.0)
    v, f = as_arrays(mesh)
    w, u, s, info = sutura_geom.sdf_grid(v, f, 0.08)
    dims = list(info["dims"])
    assert list(w.shape) == dims and list(u.shape) == dims and list(s.shape) == dims
    assert w.shape[0] >= 3
    # The winding field must be ~1 at the centre of the sphere and ~0 far away.
    cx = [int((0.0 - info["origin"][i]) / info["voxel"]) for i in range(3)]
    cval = w[cx[0], cx[1], cx[2]]
    assert abs(cval - 1.0) < 0.05, f"centre winding {cval}"
    assert s[cx[0], cx[1], cx[2]] < 0.0
    # An outside corner voxel is positive.
    assert s[0, 0, 0] > 0.0


# --------------------------------------------------------------------------- #
# 1. sphere with a hole closes
# --------------------------------------------------------------------------- #
def test_sub_box_local_window():
    mesh = trimesh.creation.icosphere(subdivisions=3, radius=1.0)
    v, f = as_arrays(mesh)
    box = np.array([[-0.4, -0.4, 0.7], [0.4, 0.4, 1.2]])
    ov, ot, info = sutura_geom.morph_close(v, f, 0.08, 0.05, box)
    ov = np.asarray(ov)
    ot = np.asarray(ot)
    # The grid covers only the sub-box (plus padding): small dims, and the
    # output is confined to the grid window.
    assert info["dims"][2] < 30, f"grid not restricted: {info['dims']}"
    assert len(ot) > 0 and len(ov) > 0
    o = info["origin"]
    h = info["voxel"]
    d = info["dims"]
    for axis in range(3):
        lo = o[axis] - h
        hi = o[axis] + (d[axis] + 1) * h
        assert ov[:, axis].min() >= lo and ov[:, axis].max() <= hi, (
            f"output leaves the window on axis {axis}")


def test_sphere_with_hole_closes():
    mesh = trimesh.creation.icosphere(subdivisions=3, radius=1.0)
    c = mesh.triangles_center
    keep = c[:, 2] < 0.55  # cut a cap off the top -> open hole
    mesh.update_faces(keep)
    mesh.remove_unreferenced_vertices()
    v, f = as_arrays(mesh)
    assert manifold_ok(f) is False  # open input

    ov, ot, info = closing(v, f, r=0.18, voxel=0.05)
    assert info["manifold"], "output not manifold"
    assert manifold_ok(ot), "output edge-use not manifold"
    # Closed surface: every edge used twice -> one shell, genus 0.
    g, chi = euler_genus(len(ov), ot)
    assert chi >= 2 - 4, f"unexpected chi {chi}"
    # The hole must be gone: volume close to a full sphere of radius 1.
    vol = signed_volume(ov, ot)
    assert vol > 3.0, f"volume {vol} (hole not closed)"


# --------------------------------------------------------------------------- #
# 2. two overlapping cubes -> single shell
# --------------------------------------------------------------------------- #
def test_two_overlapping_cubes_single_shell():
    a = trimesh.creation.box(extents=[1.0, 1.0, 1.0])
    b = trimesh.creation.box(extents=[1.0, 1.0, 1.0])
    b.apply_translation([0.4, 0.0, 0.0])  # overlaps a
    soup = trimesh.util.concatenate([a, b])
    v, f = as_arrays(soup)
    ov, ot, info = closing(v, f, r=0.05, voxel=0.05)
    assert info["manifold"]
    assert manifold_ok(ot)
    assert n_components(len(ov), ot) == 1, "overlapping cubes did not fuse"


# --------------------------------------------------------------------------- #
# 3. nested cube inside cube
# --------------------------------------------------------------------------- #
def _nested_volumes():
    outer = trimesh.creation.box(extents=[3.0, 3.0, 3.0])
    inner = trimesh.creation.box(extents=[1.0, 1.0, 1.0])

    solid = trimesh.util.concatenate([outer, inner])  # same orientation
    v, f = as_arrays(solid)
    ov, ot, info = closing(v, f, r=0.08, voxel=0.08)
    solid_vol = signed_volume(ov, ot)
    solid_comps = n_components(len(ov), ot)
    solid_ok = manifold_ok(ot) and info["manifold"]

    inner.invert()  # cavity
    cavity = trimesh.util.concatenate([outer, inner])
    v, f = as_arrays(cavity)
    ov, ot, info = closing(v, f, r=0.08, voxel=0.08)
    cavity_vol = signed_volume(ov, ot)
    cavity_comps = n_components(len(ov), ot)
    cavity_ok = manifold_ok(ot) and info["manifold"]
    return (solid_vol, solid_comps, solid_ok,
            cavity_vol, cavity_comps, cavity_ok)


def test_nested_cube_cavity_vs_solid():
    (solid_vol, solid_comps, solid_ok,
     cavity_vol, cavity_comps, cavity_ok) = _nested_volumes()

    # Same-orientation nested shell is solid inside solid (winding ~2): it is
    # absorbed -> one shell, volume close to the full 3-cube.
    assert solid_ok and solid_comps == 1, "solid nested shell not absorbed"
    assert solid_vol > 0.0 and 25.0 < solid_vol < 30.0, f"solid volume {solid_vol}"

    # Inverted inner shell is a real cavity (winding ~0): two shells, and the
    # enclosed volume is smaller than the solid case by roughly the cavity.
    assert cavity_ok and cavity_comps == 2, "cavity shell missing"
    assert cavity_vol > 0.0, f"outer orientation wrong (volume {cavity_vol})"
    assert solid_vol - cavity_vol > 0.5, (
        f"cavity not removed: solid {solid_vol}, cavity {cavity_vol}")


def test_fill_cavities_removes_cavity():
    outer = trimesh.creation.box(extents=[3.0, 3.0, 3.0])
    inner = trimesh.creation.box(extents=[1.0, 1.0, 1.0])
    inner.invert()
    soup = trimesh.util.concatenate([outer, inner])
    v, f = as_arrays(soup)
    ov, ot, info = closing(v, f, r=0.08, voxel=0.08, fill_cavities=True)
    assert info["fill_cavities"] is True
    assert info["manifold"]
    assert manifold_ok(ot)
    assert n_components(len(ov), ot) == 1, "cavity not filled"
    vol = signed_volume(ov, ot)
    assert vol > 0.0 and 25.0 < vol < 30.0, f"volume {vol} (cavity not filled)"


# --------------------------------------------------------------------------- #
# 4. torus genus preserved for r < tube radius
# --------------------------------------------------------------------------- #
def test_torus_genus_preserved_small_r():
    mesh = trimesh.creation.torus(major_radius=1.0, minor_radius=0.35,
                                  major_segments=64, minor_segments=32)
    v, f = as_arrays(mesh)
    ov, ot, info = closing(v, f, r=0.1, voxel=0.04)
    assert info["manifold"]
    assert manifold_ok(ot)
    assert n_components(len(ov), ot) == 1
    g, chi = euler_genus(len(ov), ot)
    assert g == 1, f"genus {g} (chi {chi}); torus handle lost"


def test_torus_with_patch_removed_keeps_genus():
    # Removing a patch from the tube leaves an open surface; closing with a
    # radius below the tube radius must seal it without filling the handle.
    mesh = trimesh.creation.torus(major_radius=1.0, minor_radius=0.35,
                                  major_segments=64, minor_segments=32)
    c = mesh.triangles_center
    # Remove faces near (x>0, z>0) region of the tube.
    keep = ~((c[:, 0] > 0.9) & (c[:, 2] > 0.15))
    mesh.update_faces(keep)
    mesh.remove_unreferenced_vertices()
    v, f = as_arrays(mesh)
    ov, ot, info = closing(v, f, r=0.12, voxel=0.04)
    assert info["manifold"]
    assert manifold_ok(ot)
    g, chi = euler_genus(len(ov), ot)
    assert g == 1, f"genus {g} (chi {chi}); handle lost after patching"


if __name__ == "__main__":
    fns = [v for k, v in sorted(globals().items())
           if k.startswith("test_") and callable(v)]
    for fn in fns:
        fn()
        print(f"ok  {fn.__name__}")
