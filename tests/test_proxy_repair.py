#!/usr/bin/env python3
"""Regression test for sutura/proxy_repair.py (proxy-template repair).

Checks:
  1. health_mask detects boundary holes, non-manifold geometry, debris, and dilates.
  2. build_proxy produces watertight 2-manifold surface without debris.
  3. build_proxy is bounded on a fan mesh whose long thin triangles used to
     hang PyMeshLab's resampler (now handled by the native resampler).
  3b. build_proxy actually selects the signed resample strategy on a mesh with
     an inconsistently wound face (native resample + non-manifold cleanup).
  3b-ii. build_proxy's open-flap truncation (thingi10k_100827) is a KNOWN
      limitation, kept as an expected-fail; the opt-in raystab_full=True param
      that would fix it is parked because it degrades the common case.
  3c. build_proxy falls back to the native morph_close strategy when the
      resample strategies are unavailable.
  4. proxy_template_repair repairs broken sphere with hole, flipped face, and debris.
  5. proxy_template_repair repairs broken torus while preserving topology.
  6. Defensive error handling returns honest error reports without raising.
  7. Enforces face budget (output <= max(2x input, 20k)) and projected_fraction_healthy >= 90%.

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

# The in-process stage-2 / proximity stack is optional on the Linux CI legs
# (skip cleanly where an optional dependency is unavailable).
try:
    import trimesh  # noqa: E402
    import sutura_geom  # noqa: E402
    import sutura.proxy_repair as pr  # noqa: E402
except Exception as _exc:  # noqa: BLE001 - optional test dependency
    print('skip tests/test_proxy_repair.py: %s' % _exc)
    raise SystemExit(0)


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

    ok, status = sutura_geom.watertight_check(pv, pt)
    assert ok, f"Proxy Manifold error: {status}"
    print(f"  ✓ test_build_proxy passed (faces: {len(pt)}, method: {info['method']})")


def test_build_proxy_prefers_signed_resample():
    """The signed resample strategy must actually be selected.

    Regression (Faz 4 W3c): the dual-contour signed resample emitted a
    watertight but non-manifold surface where the generalized-winding sign field
    was ambiguous (here: one inconsistently wound face).  ``close_holes`` then
    rejected it, and ``build_proxy`` silently fell through to the
    outward-offset *unsigned* proxy -- a different shape than the signed proxy
    the VCG ``generate_resampled_uniform_mesh`` produced.  ``_manifold_resample``
    re-orients the signed input and splits the residual pinched
    vertices/edges, so the signed path must now win."""
    v, t = _make_broken_sphere()
    pv, pt, info = pr.build_proxy(v, t)
    assert info['method'] == 'uniform_resampled_signed', (
        f"expected the signed resample, got {info['method']}")
    assert len(pt) >= 100, f"proxy too small: {len(pt)} faces"
    ok, status = sutura_geom.watertight_check(pv, pt)
    assert ok, f"Signed proxy Manifold error: {status}"
    print(f"  ✓ test_build_proxy_prefers_signed_resample passed (faces: {len(pt)})")


def test_build_proxy_open_flap_expected_fail():
    """EXPECTED FAIL -- known limitation, kept as evidence (Faz 4 W3c).

    On ``thingi10k_100827`` (a broken scan whose top surface is folded back on
    itself) the generalized-winding sign is ~0 over the top region, so the
    winding-only signed field reads it as *outside* and dual contouring truncates
    the proxy at z ~ 30.1 while the input reaches 34.5 (~38 % of the input
    vertices sit above the cut).

    The native ``resample_uniform`` already has an opt-in ``raystab_full=True``
    that recovers the flap (measured: z_top 34.38, Manifold watertight, faces
    11298 -> 12114 and SI 664 -> 81 on 100827), but enabling it for the whole
    field degrades the common case: on a plain broken sphere the proxy hits the
    face budget (11298 -> 20000 faces) and ``projected_fraction_healthy`` drops
    0.981 -> 0.878, because the full-band vote introduces scattered single-cell
    sign flips that spawn extra surface where the winding was already clean.  The
    proxy therefore keeps the winding-only signed path.

    FOLLOW-UP: narrow the vote rule to the recovery direction only (flip
    outside -> inside, never inside -> outside) so the flap is recovered without
    the scattered flips, then re-measure 100827 and the broken sphere and pass
    ``raystab_full=True`` at the signed call site.  Tracked in
    ``/tmp/followups.md``.
    """
    sample = os.path.join(REPO, 'tests', 'real-world-samples',
                          'thingi10k_100827.stl')
    if not os.path.exists(sample):
        print("  ~ test_build_proxy_open_flap_expected_fail skipped (sample missing)")
        return
    mesh = trimesh.load(sample, process=True)
    v = np.asarray(mesh.vertices, np.float64)
    t = np.asarray(mesh.faces, np.int64)

    _pv, _pt, info = pr.build_proxy(v, t)
    pv = np.asarray(_pv)
    z_top = float(pv[:, 2].max())
    z_max_in = float(v[:, 2].max())
    if z_top >= 34.0:
        print(f"  ! test_build_proxy_open_flap_expected_fail UNEXPECTEDLY PASSED "
              f"(z_top {z_top:.2f}); the limitation may be resolved -- promote it")
        return
    print(f"  x test_build_proxy_open_flap_expected_fail (expected): proxy "
          f"truncates the flap z_top={z_top:.2f} < input {z_max_in:.2f} "
          f"[method {info['method']}]")
    print("    follow-up: narrow the vote rule to recovery-only "
          "(outside -> inside) and enable raystab_full=True")


def test_proxy_template_repair_sphere():
    v, t = _make_broken_sphere()
    v_out, t_out, rep = pr.proxy_template_repair(v, t)

    assert rep['watertight'] is True, f"Repaired sphere not watertight: {rep}"
    assert rep['output_faces'] > 0, "Output mesh has 0 faces"
    assert rep['output_faces'] <= max(len(t) * 2, pr.MIN_FACE_BUDGET), (
        f"Face budget violated: {rep['output_faces']} > {max(len(t) * 2, pr.MIN_FACE_BUDGET)}"
    )
    assert rep['projected_fraction_healthy'] >= 0.90, (
        f"Healthy subset projection too low: {rep['projected_fraction_healthy']:.1%} (< 90%)"
    )
    assert any('proxy' in n for n in rep['notes']), f"Missing required note: {rep['notes']}"
    assert rep['hausdorff_mean'] is not None and rep['hausdorff_mean'] < 0.02, (
        f"Mean Hausdorff distance too high: {rep['hausdorff_mean']}"
    )

    ok, status = sutura_geom.watertight_check(v_out, t_out)
    assert ok, f"Manifold error: {status}"
    print(
        f"  ✓ test_proxy_template_repair_sphere passed "
        f"(faces: {len(t_out)}, proj_healthy: {rep['projected_fraction_healthy']:.1%}, H_mean: {rep['hausdorff_mean']:.5f})"
    )


def test_proxy_template_repair_torus():
    v, t = _make_broken_torus()
    v_out, t_out, rep = pr.proxy_template_repair(v, t)

    assert rep['watertight'] is True, f"Repaired torus not watertight: {rep}"
    assert rep['output_faces'] <= max(len(t) * 2, pr.MIN_FACE_BUDGET), (
        f"Face budget violated: {rep['output_faces']} > {max(len(t) * 2, pr.MIN_FACE_BUDGET)}"
    )
    assert rep['projected_fraction_healthy'] >= 0.90, (
        f"Healthy subset projection too low: {rep['projected_fraction_healthy']:.1%} (< 90%)"
    )
    ok, status = sutura_geom.watertight_check(v_out, t_out)
    assert ok, f"Manifold error: {status}"
    print(f"  ✓ test_proxy_template_repair_torus passed (faces: {len(t_out)})")


def _make_fan_box(edge=1.0, W=40.0, H=4.0):
    """Closed box with a fine top grid but a coarse bottom fan triangle fan.

    The long thin fan triangles (longest edge / median edge ~ 56) make
    PyMeshLab's ``generate_resampled_uniform_mesh`` KdTree construction fail to
    terminate; this is the H1 hang trigger (plate_b_bump)."""
    n = int(round(W / edge))
    xs = np.linspace(0.0, W, n + 1)
    X, Y = np.meshgrid(xs, xs, indexing='ij')
    tris = []
    for i in range(n):
        for j in range(n):
            a = [X[i, j], Y[i, j], H]
            b = [X[i + 1, j], Y[i + 1, j], H]
            c = [X[i, j + 1], Y[i, j + 1], H]
            d = [X[i + 1, j + 1], Y[i + 1, j + 1], H]
            tris.append([a, b, d])
            tris.append([a, d, c])
    top, bot = {}, {}
    for i in range(n + 1):
        top[(i, 0)] = [xs[i], 0.0, H]
        top[(i, n)] = [xs[i], W, H]
        top[(0, i)] = [0.0, xs[i], H]
        top[(n, i)] = [W, xs[i], H]
        bot[(i, 0)] = [xs[i], 0.0, 0.0]
        bot[(i, n)] = [xs[i], W, 0.0]
        bot[(0, i)] = [0.0, xs[i], 0.0]
        bot[(n, i)] = [W, xs[i], 0.0]
    for i in range(n):
        tris.append([top[(i, 0)], bot[(i, 0)], bot[(i + 1, 0)]])
        tris.append([top[(i, 0)], bot[(i + 1, 0)], top[(i + 1, 0)]])
        tris.append([top[(i + 1, n)], bot[(i + 1, n)], bot[(i, n)]])
        tris.append([top[(i + 1, n)], bot[(i, n)], top[(i, n)]])
        tris.append([top[(0, i + 1)], bot[(0, i + 1)], bot[(0, i)]])
        tris.append([top[(0, i + 1)], bot[(0, i)], top[(0, i)]])
        tris.append([top[(n, i)], bot[(n, i)], bot[(n, i + 1)]])
        tris.append([top[(n, i)], bot[(n, i + 1)], top[(n, i + 1)]])
    ring = ([bot[(i, 0)] for i in range(n)] + [bot[(n, i)] for i in range(n)]
            + [bot[(i, n)] for i in range(n, 0, -1)]
            + [bot[(0, i)] for i in range(n, 0, -1)])
    centre = [W / 2.0, W / 2.0, 0.0]
    for k in range(len(ring)):
        tris.append([ring[k], ring[(k + 1) % len(ring)], centre])
    tris = np.asarray(tris, np.float64).reshape(-1, 3, 3)
    flat = tris.reshape(-1, 3)
    v, inv = np.unique(flat, axis=0, return_inverse=True)
    return v, inv.reshape(-1, 3).astype(np.int64)


def test_build_proxy_bounded_on_fan_mesh():
    """The long-thin-triangle mesh must resample bounded via the native pass.

    Regression for the H1 plate_b_bump hang: the former VCG
    ``generate_resampled_uniform_mesh`` KdTree could fail to terminate on these
    triangles and had to be run in a bounded subprocess. The native
    ``sutura_geom.resample_uniform`` (Faz 4 W3c) is a plain voxel-grid pass that
    cannot hang, so ``build_proxy`` must complete promptly through the native
    uniform-resample strategies (no alpha-wrap fallback needed)."""
    v, t = _make_fan_box(edge=1.0)
    t0 = time.time()
    pv, pt, info = pr.build_proxy(v, t)
    elapsed = time.time() - t0
    assert info['method'] in ('uniform_resampled_signed', 'uniform_resampled_unsigned'), (
        f"expected the native uniform-resample path, got {info['method']}")
    assert len(pt) >= 100, f"proxy too small: {len(pt)} faces"
    assert elapsed < 25.0, f"build_proxy was not bounded: {elapsed:.1f}s"
    ok, status = sutura_geom.watertight_check(pv, pt)
    assert ok, f"Proxy Manifold error: {status}"
    print(f"  ✓ test_build_proxy_bounded_on_fan_mesh passed "
          f"(method: {info['method']}, faces: {len(pt)}, {elapsed:.1f}s)")


def test_build_proxy_morph_close_fallback():
    """Strategy 3 (native ``morph_close``) must yield a watertight proxy when the
    resample strategies are unavailable."""
    v, t = _make_broken_sphere()

    def _boom(*_a, **_k):
        raise RuntimeError('forced resample failure')

    orig = pr.topology.resample_uniform
    pr.topology.resample_uniform = _boom
    try:
        pv, pt, info = pr.build_proxy(v, t)
    finally:
        pr.topology.resample_uniform = orig
    assert info['method'] == 'morph_close', f"expected morph_close, got {info['method']}"
    assert len(pt) >= 100, f"proxy too small: {len(pt)} faces"
    ok, status = sutura_geom.watertight_check(pv, pt)
    assert ok, f"Proxy Manifold error: {status}"
    print(f"  ✓ test_build_proxy_morph_close_fallback passed (faces: {len(pt)})")


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
    test_build_proxy_prefers_signed_resample()
    test_build_proxy_open_flap_expected_fail()
    test_build_proxy_bounded_on_fan_mesh()
    test_build_proxy_morph_close_fallback()
    test_proxy_template_repair_sphere()
    test_proxy_template_repair_torus()
    test_defensive_error_handling()
    elapsed = time.time() - t0
    print(f"\nAll tests passed in {elapsed:.2f} s (< 30 s target).")
    assert elapsed < 30.0, f"Tests exceeded 30 s limit: {elapsed:.2f} s"


if __name__ == '__main__':
    main()
