#!/usr/bin/env python3
"""Diagnosis v2 determinism: process-run and permutation stability.

The Rust edge/duplicate maps are `HashMap`s whose iteration is salted per
process.  A non-orientable mesh makes the orientation DFS order-sensitive, so
a Mobius strip is the sharp regression case; coplanar duplicate faces exercise
the duplicate grouping and the region truncation order.  Each case is dumped
from several fresh interpreter processes and the dumps must be identical.
"""
import json
import math
import os
import subprocess
import sys

import numpy as np

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(HERE, "sutura"))
sys.path.insert(0, HERE)


def cube():
    v = np.array([[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.],
                  [0., 0., 1.], [1., 0., 1.], [1., 1., 1.], [0., 1., 1.]],
                 dtype=np.float64)
    f = np.array([[0, 1, 2], [0, 2, 3], [4, 6, 5], [4, 7, 6],
                  [0, 4, 5], [0, 5, 1], [3, 2, 6], [3, 6, 7],
                  [0, 3, 7], [0, 7, 4], [1, 5, 6], [1, 6, 2]],
                 dtype=np.int32)
    return v, f


def mobius(n=24):
    """Half-twisted open strip: one non-orientable boundary, no closed parts."""
    verts = []
    for i in range(n):
        u = 2.0 * math.pi * i / n
        for s in (-1.0, 1.0):
            r = 2.0 + 0.35 * s
            verts.append([r * math.cos(u), r * math.sin(u),
                          0.35 * s * math.sin(u / 2.0)])
    faces = []
    for i in range(n):
        a = 2 * i
        b = a + 1
        j = (i + 1) % n
        seam = j == 0
        c = 2 * j + (1 if seam else 0)
        d = 2 * j + (0 if seam else 1)
        faces.append([a, c, b])
        faces.append([b, c, d])
    return np.array(verts, dtype=np.float64), np.array(faces, dtype=np.int32)


def coplanar_duplicates():
    """Cube plus two exact duplicate faces (coplanar, reversed too)."""
    v, f = cube()
    extra = np.array([f[0], f[1], f[0, ::-1]], dtype=np.int32)
    return v, np.vstack([f, extra])


def cube_at(lo, hi):
    v, f = cube()
    return v * (hi - lo) + lo, f


def nested_cubes():
    ov, of = cube_at(0.0, 10.0)
    iv, inf = cube_at(3.0, 7.0)
    return np.vstack([ov, iv]), np.vstack([of, inf + len(ov)])


def thin_slab():
    v, f = cube()
    return v * np.array([10.0, 10.0, 0.05], dtype=np.float64), f


def build(case):
    if case == "mobius":
        return mobius()
    if case == "coplanar":
        return coplanar_duplicates()
    if case == "nested":
        return nested_cubes()
    if case == "thin":
        return thin_slab()
    raise SystemExit("unknown case")


def permuted(v, f, seed):
    rng = np.random.default_rng(seed)
    pv = rng.permutation(len(v))
    inv = np.empty_like(pv)
    inv[pv] = np.arange(len(pv))
    pf = rng.permutation(len(f))
    return v[pv], inv[f][pf]


def dump(case):
    import sutura_geom
    if case.startswith("perm-"):
        base, seed = case.split(":")
        v, f = permuted(*build(base[len("perm-"):]), int(seed))
    else:
        v, f = build(case)
    r = sutura_geom.diagnose(v, f)
    r.pop("meta", None)
    r.pop("face_mask", None)
    r.pop("vertex_mask", None)
    return r


def main():
    import sutura_geom
    cases = ["mobius", "coplanar", "nested", "thin",
             "perm-mobius:3", "perm-coplanar:11", "perm-nested:7", "perm-thin:5"]
    for case in cases:
        outs = []
        for _ in range(6):
            p = subprocess.run(
                [sys.executable, os.path.abspath(__file__), "--dump", case],
                capture_output=True, text=True, check=True)
            outs.append(p.stdout)
        assert len(set(outs)) == 1, f"{case} differs between processes"
        r = json.loads(outs[0])
        if "mobius" in case:
            assert r["channels"]["orientation"]["count"] > 0
        if "coplanar" in case:
            assert r["duplicates"]["faces"] > 0
        if "nested" in case:
            assert r["interior"]["faces"] == 12
        if "thin" in case:
            assert r["thin_wall"]["thin_faces"] > 0

    for case in ("mobius", "coplanar", "nested", "thin"):
        base = json.loads(subprocess.run(
            [sys.executable, os.path.abspath(__file__), "--dump", case],
            capture_output=True, text=True, check=True).stdout)
        for seed in (1, 7, 23):
            perm = json.loads(subprocess.run(
                [sys.executable, os.path.abspath(__file__), "--dump",
                 f"perm-{case}:{seed}"],
                capture_output=True, text=True, check=True).stdout)
            if case == "mobius":
                # A non-orientable patch may resolve its flip side differently
                # under permutation; the loop metrics themselves must be
                # bit-exact (canonical cycle order).  Compare every float in
                # loops.items exactly; the phase-3 channels are geometry-driven
                # and must match bit-for-bit too.
                assert perm["loops"] == base["loops"], (
                    f"{case} loop metrics changed under permutation {seed}")
                for ch in ("interior", "thin_wall", "noise"):
                    assert perm[ch] == base[ch], (
                        f"{case} {ch} changed under permutation {seed}")
            else:
                assert perm == base, f"{case} dump changed under permutation {seed}"

    # A 20k-component triangle soup must stay inside the fast budget: the
    # component pair and loop-region work is capped, not quadratic.
    import time
    n = 20_000
    soup_v = np.empty((n * 3, 3), dtype=np.float64)
    for i in range(n):
        soup_v[3 * i] = [float(i), 0.0, 0.0]
        soup_v[3 * i + 1] = [float(i) + 1.0, 0.0, 0.0]
        soup_v[3 * i + 2] = [float(i), 1.0, 0.0]
    soup_f = np.arange(n * 3, dtype=np.int32).reshape(n, 3)
    start = time.perf_counter()
    soup = sutura_geom.diagnose(soup_v, soup_f)
    elapsed = time.perf_counter() - start
    assert soup["loops"]["count"] == n
    assert len(soup["loops"]["items"]) <= 512
    assert soup["meta"]["fast_budget_met"], (
        elapsed, soup["meta"]["channel_seconds"])

    # Out-of-range triangle indices are a clear error, not a panic.
    try:
        sutura_geom.diagnose(np.zeros((3, 3)), np.array([[0, 1, 9]]))
    except ValueError as exc:
        assert "out of range" in str(exc)
    else:
        raise AssertionError("out-of-range index was accepted")
    print("diagnosis v2 determinism ok")


if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "--dump":
        print(json.dumps(dump(sys.argv[2]), sort_keys=True, allow_nan=False))
    else:
        main()
