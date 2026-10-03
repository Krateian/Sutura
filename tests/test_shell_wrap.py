#!/usr/bin/env python3
"""Regression tests for Graft (#13, shell wrap) — sutura/shell_wrap.py.

Runs under the venv with the maturin-built sutura_geom extension (no
pymeshlab).  pytest-compatible; runnable directly.

    <venv>/bin/python tests/test_shell_wrap.py
"""
import os
import sys
from collections import Counter

import numpy as np
import trimesh

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__))), "sutura"))

from shell_wrap import (  # noqa: E402
    DISPLAY_NAME, _hybrid_close, _stitch_local, shell_wrap)
from repair import _damaged_region, reload_strict_holes_nm  # noqa: E402

BUDGET = 80_000  # small grid budget keeps the tests fast


# --------------------------------------------------------------------------- #
# helpers
# --------------------------------------------------------------------------- #
def as_arrays(mesh):
    return (np.asarray(mesh.vertices, np.float64),
            np.asarray(mesh.faces, np.int64))


def manifold_ok(tris):
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
        for u, v in ((a, b), (b, c)):
            ra, rb = find(int(u)), find(int(v))
            if ra != rb:
                parent[ra] = rb
    return len({find(i) for i in range(nverts)})


def euler_genus(nverts, tris):
    edges = set()
    for a, b, c in tris:
        for u, v in ((a, b), (b, c), (c, a)):
            edges.add((min(int(u), int(v)), max(int(u), int(v))))
    chi = nverts - len(edges) + len(tris)
    return (2 - chi) // 2, chi


def signed_volume(verts, tris):
    s = 0.0
    for a, b, c in tris:
        s += float(np.dot(verts[a], np.cross(verts[b], verts[c]))) / 6.0
    return s


def sphere_with_hole():
    mesh = trimesh.creation.icosphere(subdivisions=3, radius=1.0)
    c = mesh.triangles_center
    mesh.update_faces(c[:, 2] < 0.55)
    mesh.remove_unreferenced_vertices()
    return as_arrays(mesh)


def overlapping_boxes():
    a = trimesh.creation.box(extents=[1.0, 1.0, 1.0])
    b = trimesh.creation.box(extents=[1.0, 1.0, 1.0])
    b.apply_translation([0.4, 0.0, 0.0])
    return as_arrays(trimesh.util.concatenate([a, b]))


def nested(cavity):
    outer = trimesh.creation.box(extents=[3.0, 3.0, 3.0])
    inner = trimesh.creation.box(extents=[1.0, 1.0, 1.0])
    if cavity:
        inner.invert()
    return as_arrays(trimesh.util.concatenate([outer, inner]))


def torus_with_patch():
    mesh = trimesh.creation.torus(major_radius=1.0, minor_radius=0.35,
                                  major_segments=48, minor_segments=24)
    c = mesh.triangles_center
    mesh.update_faces(~((c[:, 0] > 0.9) & (c[:, 2] > 0.15)))
    mesh.remove_unreferenced_vertices()
    return as_arrays(mesh)


# --------------------------------------------------------------------------- #
# tests
# --------------------------------------------------------------------------- #
def test_sphere_with_hole_watertight_and_report():
    v, f = sphere_with_hole()
    ov, ot, rep = shell_wrap(v, f, grid_budget=BUDGET, voxel=0.08)
    assert rep["ok"] and rep["category"] == "watertight"
    assert rep["holes"] == 0 and rep["non_manifold"] == 0
    assert manifold_ok(ot)
    assert n_components(len(ov), ot) == 1
    assert rep["display_name"] == DISPLAY_NAME == "Graft"
    assert rep["invents_geometry"] is True
    assert rep["method"] == 13
    assert rep["tries"] >= 1
    # Pass 0 (sign field, r = 0) either succeeds or the closing ladder runs;
    # either way the chosen radius is non-negative and inside the ladder.
    assert rep["r_used"] >= 0
    assert len(rep["per_vertex_deviation"]) == len(ov)
    # the hole is gone: volume close to a full unit sphere
    vol = signed_volume(ov, ot)
    assert 3.0 < vol < 6.5, f"volume {vol}"
    # without pymeshlab the pure envelope path is used; its healthy fidelity
    # may exceed the strict target (the verbatim hybrid handles that when a
    # live pymeshlab module is supplied).
    assert isinstance(rep["fidelity_ok"], bool)


def test_overlapping_boxes_single_shell():
    v, f = overlapping_boxes()
    ov, ot, rep = shell_wrap(v, f, grid_budget=BUDGET, voxel=0.08)
    assert rep["ok"] and rep["holes"] == 0 and rep["non_manifold"] == 0
    assert manifold_ok(ot)
    assert n_components(len(ov), ot) == 1


def test_nested_cavity_retained_and_solid_absorbed():
    # inverted inner shell = real cavity -> two components
    v, f = nested(cavity=True)
    ov, ot, rep = shell_wrap(v, f, grid_budget=BUDGET, voxel=0.08)
    assert rep["ok"] and rep["holes"] == 0 and rep["non_manifold"] == 0
    assert manifold_ok(ot)
    assert n_components(len(ov), ot) == 2, "cavity shell missing"

    # same-orientation inner shell = solid inside solid -> absorbed
    v2, f2 = nested(cavity=False)
    ov2, ot2, rep2 = shell_wrap(v2, f2, grid_budget=BUDGET, voxel=0.08)
    assert rep2["ok"]
    assert n_components(len(ov2), ot2) == 1, "solid nested shell not absorbed"


def test_torus_patch_keeps_genus():
    v, f = torus_with_patch()
    ov, ot, rep = shell_wrap(v, f, grid_budget=BUDGET, voxel=0.08)
    assert rep["ok"] and rep["holes"] == 0 and rep["non_manifold"] == 0
    assert manifold_ok(ot)
    assert n_components(len(ov), ot) == 1
    g, chi = euler_genus(len(ov), ot)
    assert g == 1, f"genus {g} (chi {chi}); handle lost"


def test_detail_loss_fields_and_warning_shape():
    v, f = nested(cavity=True)
    _ov, _ot, rep = shell_wrap(v, f, grid_budget=BUDGET, voxel=0.08)
    assert rep["detail_tolerance_mm"] is not None
    assert rep["detail_max_mm"] is not None
    assert 0.0 <= rep["detail_area_moved"] <= 1.0
    for w in rep["warnings"]:
        assert w["code"] and w["message_en"] and w["message_tr"]


def test_local_mode_flag_runs_and_reports_mode():
    # The damaged-region bbox is tiny, so the local window path is attempted;
    # a non-watertight original makes the manifold union fall back to whole
    # mode, which must still produce a valid watertight result.
    v, f = sphere_with_hole()
    ov, ot, rep = shell_wrap(v, f, grid_budget=BUDGET, voxel=0.08, local=True,
                             sign_field=False)
    assert rep["mode"] in ("local", "whole")
    assert rep["ok"] and rep["holes"] == 0 and rep["non_manifold"] == 0
    assert manifold_ok(ot)


def test_stitch_local_boolean_path_on_solids():
    # The local-window stitch uses trimesh's manifold3d boolean backend; skip
    # with a clear message where manifold3d is not installed (it is an
    # optional/heavy wheel, not present in every test venv).
    try:
        import manifold3d  # noqa: F401
    except ImportError:
        print('   (skipped: manifold3d not installed)')
        return
    a = trimesh.creation.box(extents=[1.0, 1.0, 1.0])
    b = trimesh.creation.box(extents=[1.0, 1.0, 1.0])
    b.apply_translation([0.4, 0.0, 0.0])
    av, af = as_arrays(a)
    bv, bf = as_arrays(b)
    sv, st, ok = _stitch_local(bv, bf, av, af)
    assert ok, "boolean union path unavailable"
    assert len(st) > 0 and manifold_ok(st)
    assert n_components(len(sv), st) == 1


def test_smallest_radius_first_ladder():
    v, f = sphere_with_hole()
    _ov, _ot, rep = shell_wrap(v, f, grid_budget=BUDGET, voxel=0.08)
    assert rep["r_ladder"], "empty r ladder"
    assert rep["r_ladder"][0] == rep["r_ladder"][0]  # finite
    # the ladder is non-decreasing and starts smallest
    assert all(b >= a for a, b in zip(rep["r_ladder"], rep["r_ladder"][1:]))
    assert rep["r_used"] >= rep["r_ladder"][0] - 1e-12


def test_hybrid_keeps_healthy_verbatim():
    """The pymeshlab-backed verbatim hybrid closes a hole while keeping every
    healthy original triangle unchanged (zero healthy deviation)."""
    try:
        import pymeshlab
    except ImportError:
        return
    v, f = sphere_with_hole()
    mask, _ = _damaged_region(f)
    hy = _hybrid_close(v, f, mask, pymeshlab)
    assert hy is not None, "hybrid could not close the sphere hole"
    h, nm = reload_strict_holes_nm(*hy)
    assert h == 0 and nm == 0

    # A coarse envelope loses detail, so shell_wrap must fall back to the
    # verbatim hybrid and report zero healthy deviation (sign-field Pass 0 is
    # disabled here so the closing/hybrid path is the one under test).
    _ov, _ot, rep = shell_wrap(v, f, voxel=0.4, ml=pymeshlab,
                               sign_field=False)
    assert rep["ok"] and rep["mode"] == "hybrid"
    assert rep["detail_max_mm"] == 0.0
    assert rep["hausdorff_healthy"] == 0.0
    assert rep["holes"] == 0 and rep["non_manifold"] == 0


def test_sign_field_pass0_opt_in_and_default_off():
    # Pass 0 is OFF by default: the ladder starts at the first closing radius.
    v, f = sphere_with_hole()
    _ov, _ot, rep0 = shell_wrap(v, f, grid_budget=BUDGET, voxel=0.08,
                               guard=False)
    assert rep0["sign_field"] is False
    assert rep0["r_ladder"][0] > 0.0
    # Opted in explicitly, Pass 0 runs, is adopted and the ladder starts at 0.
    _ov, _ot, rep = shell_wrap(v, f, grid_budget=BUDGET, voxel=0.08,
                               guard=False, sign_field=True)
    assert rep["sign_field"] is True
    assert rep["r_ladder"][0] == 0.0
    assert rep["mode"] == "sign_field"
    assert rep["r_used"] == 0.0
    assert rep["holes"] == 0 and rep["non_manifold"] == 0


def test_sign_field_pass0_not_adopted_when_fidelity_fails():
    # BUG-01 regression: a watertight sign-field candidate that FAILS the
    # fidelity gate must never seed `best`, or it would block every closing
    # ladder attempt (both have 0 holes+nm) and be adopted with a bad shape.
    import sutura_engine.graft as graft_mod
    saved = graft_mod.HEALTHY_HAUSDORFF_MAX
    graft_mod.HEALTHY_HAUSDORFF_MAX = 0.0  # force every real deviation to fail
    try:
        v, f = sphere_with_hole()
        _ov, _ot, rep = shell_wrap(v, f, grid_budget=BUDGET, voxel=0.08,
                                   guard=True, sign_field=True)
    finally:
        graft_mod.HEALTHY_HAUSDORFF_MAX = saved
    assert rep["sign_field"] is True
    assert rep["r_used"] > 0.0, rep
    assert rep["mode"] != "sign_field", rep


def test_time_budget_stops_between_attempts():
    """A spent budget stops the closing ladder after the current attempt and
    returns the best candidate found so far (never hangs, never a crash)."""
    import time as _time
    import sutura_engine.graft as graft_mod

    v, f = sphere_with_hole()
    calls = {"n": 0}
    real_close = graft_mod.sutura_geom.morph_close
    real_ladder = graft_mod._r_ladder
    real_max = graft_mod.HEALTHY_HAUSDORFF_MAX

    def fake_ladder(gap, voxel, diag, max_tries):
        return [1e-4, 2e-4, 4e-4, 8e-4]

    def fake_close(*args, **kwargs):
        calls["n"] += 1
        _time.sleep(0.02)
        return real_close(*args, **kwargs)

    # Force the fidelity gate to fail so no rung can break early; only the
    # budget may stop the ladder.
    graft_mod.HEALTHY_HAUSDORFF_MAX = 0.0
    graft_mod._r_ladder = fake_ladder
    graft_mod.sutura_geom.morph_close = fake_close
    try:
        _ov, _ot, rep = shell_wrap(v, f, grid_budget=BUDGET, voxel=0.08,
                                   guard=True, time_budget=1e-6)
    finally:
        graft_mod._r_ladder = real_ladder
        graft_mod.sutura_geom.morph_close = real_close
        graft_mod.HEALTHY_HAUSDORFF_MAX = real_max

    assert rep["budget_exceeded"] is True
    assert rep["tries"] == 1, rep["tries"]
    assert calls["n"] == 1, calls
    assert rep["time_budget"] == 1e-6


def test_no_time_budget_runs_full_ladder():
    """Without a budget the same multi-rung ladder is not stopped early."""
    import sutura_engine.graft as graft_mod

    v, f = sphere_with_hole()
    calls = {"n": 0}
    real_close = graft_mod.sutura_geom.morph_close
    real_ladder = graft_mod._r_ladder
    real_max = graft_mod.HEALTHY_HAUSDORFF_MAX

    def fake_ladder(gap, voxel, diag, max_tries):
        return [1e-4, 2e-4, 4e-4, 8e-4]

    def fake_close(*args, **kwargs):
        calls["n"] += 1
        return real_close(*args, **kwargs)

    graft_mod.HEALTHY_HAUSDORFF_MAX = 0.0
    graft_mod._r_ladder = fake_ladder
    graft_mod.sutura_geom.morph_close = fake_close
    try:
        _ov, _ot, rep = shell_wrap(v, f, grid_budget=BUDGET, voxel=0.08,
                                   guard=True)
    finally:
        graft_mod._r_ladder = real_ladder
        graft_mod.sutura_geom.morph_close = real_close
        graft_mod.HEALTHY_HAUSDORFF_MAX = real_max

    assert rep["budget_exceeded"] is False
    assert rep["tries"] == 4, rep["tries"]
    assert calls["n"] == 4, calls


def test_resolve_grid_budget_scales_with_intensity_and_ram():
    from sutura_engine.graft import resolve_grid_budget
    big = 1 << 40
    assert resolve_grid_budget("quick", big) == 1_500_000
    assert resolve_grid_budget("balanced", big) == 1_500_000
    assert resolve_grid_budget("thorough", big) == 10_000_000
    assert resolve_grid_budget("extreme", big) == 25_000_000
    # Unknown/None falls back to the balanced target.
    assert resolve_grid_budget(None, big) == 1_500_000
    assert resolve_grid_budget("custom-profile", big) == 1_500_000
    # A tiny machine must clamp BELOW the nominal floor: the RAM-safe limit is
    # an absolute ceiling down to MIN_GRID_DIV**3 (BUG-03).
    from sutura_engine.graft import MIN_GRID_DIV
    floor = MIN_GRID_DIV ** 3
    assert resolve_grid_budget("extreme", 1_000_000) == floor
    assert resolve_grid_budget("balanced", 1_000_000) == floor
    assert resolve_grid_budget("extreme", 10**12) == 25_000_000
    # A mid-size machine still gets the nominal target when it fits.
    assert resolve_grid_budget("balanced", 10**12) == 1_500_000


if __name__ == "__main__":
    fns = [v for k, v in sorted(globals().items())
           if k.startswith("test_") and callable(v)]
    for fn in fns:
        fn()
        print(f"ok  {fn.__name__}")
