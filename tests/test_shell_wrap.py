#!/usr/bin/env python3
"""Regression tests for Graft (#13, shell wrap) — sutura/shell_wrap.py.

Runs under the venv with the maturin-built sutura_geom extension (no
native toolchain).  pytest-compatible; runnable directly.

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
    # The local-window stitch uses the Rust Manifold kernel (sutura_geom),
    # which is mandatory on every install.
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
    """The native verbatim hybrid closes a hole while keeping every healthy
    original triangle unchanged (zero healthy deviation)."""
    v, f = sphere_with_hole()
    mask, _ = _damaged_region(f)
    hy = _hybrid_close(v, f, mask)
    assert hy is not None, "hybrid could not close the sphere hole"
    h, nm = reload_strict_holes_nm(*hy)
    assert h == 0 and nm == 0

    # A coarse envelope loses detail, so shell_wrap must fall back to the
    # verbatim hybrid and report zero healthy deviation (sign-field Pass 0 is
    # disabled here so the closing/hybrid path is the one under test).
    _ov, _ot, rep = shell_wrap(v, f, voxel=0.4, hybrid=True,
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


def test_time_budget_aborts_all_or_nothing():
    """A spent outer time budget aborts Graft entirely and keeps the input
    (stage-1), never a partial-ladder mesh that depends on machine speed."""
    import sutura_engine.graft as graft_mod

    v, f = sphere_with_hole()
    calls = {"n": 0}
    real_close = graft_mod.sutura_geom.morph_close
    real_ladder = graft_mod._r_ladder
    real_perf = graft_mod.time.perf_counter
    clock = {"n": 0}

    def fake_ladder(gap, voxel, diag, max_tries):
        return [1e-4, 2e-4, 4e-4, 8e-4]

    def fake_close(*args, **kwargs):
        calls["n"] += 1
        return real_close(*args, **kwargs)

    def slow_clock():
        clock["n"] += 1
        return 100.0 * clock["n"]

    graft_mod._r_ladder = fake_ladder
    graft_mod.sutura_geom.morph_close = fake_close
    graft_mod.time.perf_counter = slow_clock
    try:
        ov, ot, rep = shell_wrap(v, f, grid_budget=BUDGET, voxel=0.08,
                                 guard=True, time_budget=1.0)
    finally:
        graft_mod._r_ladder = real_ladder
        graft_mod.sutura_geom.morph_close = real_close
        graft_mod.time.perf_counter = real_perf

    assert rep["aborted"] is True
    assert rep["reject_reason"] == "aborted_time"
    assert rep["budget_exceeded"] is False
    assert rep["tries"] == 0
    assert calls["n"] == 0, calls
    assert "stopped for time" in rep["reason"]
    # The input is returned unchanged (stage-1 result kept).
    assert np.array_equal(np.asarray(ov), np.asarray(v, np.float64))
    assert np.array_equal(np.asarray(ot), np.asarray(f, np.int64))


def test_ladder_result_is_independent_of_wall_clock():
    """The produced mesh must not depend on the clock: a 'slow' clock under a
    generous outer budget gives the same result as no time budget at all."""
    import sutura_engine.graft as graft_mod

    v, f = sphere_with_hole()
    real_close = graft_mod.sutura_geom.morph_close
    real_ladder = graft_mod._r_ladder
    real_max = graft_mod.HEALTHY_HAUSDORFF_MAX
    real_perf = graft_mod.time.perf_counter

    def fake_ladder(gap, voxel, diag, max_tries):
        return [1e-4, 2e-4, 4e-4, 8e-4]

    def fake_close(*args, **kwargs):
        return real_close(*args, **kwargs)

    def run(time_budget, clock=None):
        graft_mod._r_ladder = fake_ladder
        graft_mod.sutura_geom.morph_close = fake_close
        graft_mod.HEALTHY_HAUSDORFF_MAX = 0.0
        if clock is not None:
            graft_mod.time.perf_counter = clock
        try:
            return shell_wrap(v, f, grid_budget=BUDGET, voxel=0.08,
                              guard=True, time_budget=time_budget)
        finally:
            graft_mod._r_ladder = real_ladder
            graft_mod.sutura_geom.morph_close = real_close
            graft_mod.HEALTHY_HAUSDORFF_MAX = real_max
            graft_mod.time.perf_counter = real_perf

    tick = {"n": 0}

    def slow_clock():
        tick["n"] += 1
        return 100.0 * tick["n"]

    ov0, ot0, rep0 = run(None)
    ov1, ot1, rep1 = run(3600.0, clock=slow_clock)

    assert rep0["tries"] == rep1["tries"] == 4
    assert rep1["aborted"] is False
    assert np.array_equal(np.asarray(ov0), np.asarray(ov1))
    assert np.array_equal(np.asarray(ot0), np.asarray(ot1))


def test_work_budget_stops_deterministically():
    """The inner r-ladder stop is a deterministic cell-work budget, not the
    wall clock: a tiny budget stops after the first candidate."""
    import sutura_engine.graft as graft_mod

    v, f = sphere_with_hole()
    real_close = graft_mod.sutura_geom.morph_close
    real_ladder = graft_mod._r_ladder
    real_budget = graft_mod.GRAFT_WORK_BUDGET_BY_INTENSITY
    real_hybrid = graft_mod._hybrid_close

    def fake_ladder(gap, voxel, diag, max_tries):
        return [1e-4, 2e-4, 4e-4, 8e-4]

    graft_mod._r_ladder = fake_ladder
    graft_mod.sutura_geom.morph_close = real_close
    # Isolate the budget policy: the hybrid fast path would close on the first
    # rung and end the ladder before the work budget can trip.
    graft_mod._hybrid_close = lambda *a, **k: None
    graft_mod.GRAFT_WORK_BUDGET_BY_INTENSITY = {
        k: 1 for k in real_budget}
    try:
        _ov, _ot, rep = shell_wrap(v, f, grid_budget=BUDGET, voxel=0.08,
                                   guard=True)
    finally:
        graft_mod._r_ladder = real_ladder
        graft_mod.sutura_geom.morph_close = real_close
        graft_mod._hybrid_close = real_hybrid
        graft_mod.GRAFT_WORK_BUDGET_BY_INTENSITY = real_budget

    assert rep["budget_exceeded"] is True
    assert rep["budget_limited_by"] == "work"
    assert rep["aborted"] is False
    assert rep["tries"] == 1, rep["tries"]


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


def test_resolve_grid_budget_is_a_pure_function_of_intensity():
    from sutura_engine import graft as graft_mod
    from sutura_engine.graft import resolve_grid_budget
    assert resolve_grid_budget("quick") == 1_500_000
    assert resolve_grid_budget("balanced") == 1_500_000
    assert resolve_grid_budget("thorough") == 10_000_000
    assert resolve_grid_budget("extreme") == 25_000_000
    # Unknown/None falls back to the balanced target.
    assert resolve_grid_budget(None) == 1_500_000
    assert resolve_grid_budget("custom-profile") == 1_500_000
    # The free-RAM probe must not influence the resolved budget at all.
    real_probe = graft_mod._available_memory_bytes
    try:
        graft_mod._available_memory_bytes = lambda: 4 * 1024 * 1024
        low = resolve_grid_budget("extreme")
        graft_mod._available_memory_bytes = lambda: 1 << 50
        high = resolve_grid_budget("extreme")
    finally:
        graft_mod._available_memory_bytes = real_probe
    assert low == high == 25_000_000


def test_shell_wrap_grid_params_identical_under_different_free_ram():
    """The free-RAM guard is discrete and deterministic.

    Enough RAM -> the preset budget (identical for any ample value); a preset
    that does not fit -> a reported fallback to the Balanced budget; only when
    even Balanced does not fit -> refuse.  It never picks an arbitrary
    resolution.
    """
    import sutura_engine.graft as graft_mod

    v, f = sphere_with_hole()
    real_probe = graft_mod._available_memory_bytes
    real_close = graft_mod.sutura_geom.morph_close

    def stub_close(*args, **kwargs):
        # Keep the topology (a stub envelope); only the early grid params are
        # under test here.
        return v, f, {}

    def run_with_ram(ram, intensity):
        graft_mod._available_memory_bytes = lambda: ram
        graft_mod.sutura_geom.morph_close = stub_close
        try:
            return shell_wrap(v, f, intensity=intensity)
        finally:
            graft_mod.sutura_geom.morph_close = real_close

    try:
        _a, _b, rep_bal_hi = run_with_ram(256 * 1024 ** 3, "balanced")
        _c, _d, rep_bal_lo = run_with_ram(4 * 1024 ** 3, "balanced")
        _e, _f, rep_ext_hi = run_with_ram(256 * 1024 ** 3, "extreme")
        _g, _h, rep_ext_mid = run_with_ram(2 * 1024 ** 3, "extreme")
        _i, _j, rep_tiny = run_with_ram(64 * 1024 ** 2, "extreme")
    finally:
        graft_mod._available_memory_bytes = real_probe

    # Enough RAM: identical grid for a huge vs an ample value, preset used.
    assert rep_bal_hi["grid_budget"] == rep_bal_lo["grid_budget"] == 1_500_000
    assert rep_bal_hi["voxel"] == rep_bal_lo["voxel"]
    assert rep_bal_hi["low_memory"] is False
    assert rep_ext_hi["grid_budget"] == 25_000_000
    assert rep_ext_hi["grid_fallback"] is None

    # Mid RAM: Extreme does not fit -> reported fallback to Balanced (not a
    # refusal, not a silent arbitrary coarsening).
    assert rep_ext_mid["grid_budget"] == 1_500_000
    assert rep_ext_mid["low_memory"] is False
    assert rep_ext_mid["reject_reason"] is None
    assert rep_ext_mid["grid_fallback"] == {
        "from": 25_000_000, "to": 1_500_000, "intensity": "extreme"}
    assert any(w.get("code") == "grid_fallback"
               for w in rep_ext_mid["warnings"])

    # Tiny RAM: even Balanced does not fit -> refuse with the preset budget
    # left untouched.
    assert rep_tiny["grid_budget"] == 25_000_000
    assert rep_tiny["low_memory"] is True
    assert rep_tiny["reject_reason"] == "low_memory"
    assert rep_tiny["reason"]


if __name__ == "__main__":
    fns = [v for k, v in sorted(globals().items())
           if k.startswith("test_") and callable(v)]
    for fn in fns:
        fn()
        print(f"ok  {fn.__name__}")
