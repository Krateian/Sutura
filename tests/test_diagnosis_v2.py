#!/usr/bin/env python3
"""Diagnosis v2 phase 3: channels, masks, permutation and JSON wrapper."""
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(HERE, "sutura"))
sys.path.insert(0, HERE)

import sutura_geom


def cube():
    v = np.array([[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.],
                  [0., 0., 1.], [1., 0., 1.], [1., 1., 1.], [0., 1., 1.]],
                 dtype=np.float32)
    f = np.array([[0, 1, 2], [0, 2, 3], [4, 6, 5], [4, 7, 6],
                  [0, 4, 5], [0, 5, 1], [3, 2, 6], [3, 6, 7],
                  [0, 3, 7], [0, 7, 4], [1, 5, 6], [1, 6, 2]],
                 dtype=np.int32)
    return v, f


def decode(mask):
    out = np.zeros(int(mask["length"]), dtype=np.uint8)
    for start, length, value in mask["runs"]:
        out[int(start):int(start) + int(length)] = int(value)
    return out


def scaled_cube(lo, hi):
    v, f = cube()
    return v * (hi - lo) + lo, f


def box(sx, sy, sz):
    v, f = cube()
    return v * np.array([sx, sy, sz], dtype=np.float64), f


def many_thin_slabs(n=20):
    # 5x5x0.05 slabs: thin relative to the global diagonal, while the side
    # faces stay above the needle threshold (degenerate would otherwise fill
    # the global 256-region budget before thin_wall).
    base_v, base_f = box(5.0, 5.0, 0.05)
    vs, fs = [], []
    off = 0
    for j in range(n):
        for i in range(n):
            vs.append(base_v + [i * 6.0, j * 6.0, 0.0])
            fs.append(base_f + off)
            off += len(base_v)
    return np.vstack(vs), np.vstack(fs)


def region_keys(r):
    return sorted(
        (g["channel"], g["kind"], tuple(g["bbox"]), tuple(g["centroid"]),
         g["faces"], g["vertices"], tuple(g["components"]),
         g["severity"], g["confidence"])
        for g in r["regions"])


def permuted(v, f, seed):
    rng = np.random.default_rng(seed)
    pv = rng.permutation(len(v))
    inv_v = np.empty_like(pv)
    inv_v[pv] = np.arange(len(pv))
    pf = rng.permutation(len(f))
    return v[pv], inv_v[f][pf], pv, pf


def check_permutation(v, f, r, seed, detail="fast"):
    vp, fp, pv, pf = permuted(v, f, seed)
    p = sutura_geom.diagnose(vp, fp, detail)
    for ch in ("orientation", "nm_vertex", "duplicate", "degenerate",
               "self_intersection", "loops", "interior", "thin_wall", "noise"):
        assert p["channels"][ch]["count"] == r["channels"][ch]["count"], ch
    assert p["interior"] == r["interior"]
    assert p["thin_wall"] == r["thin_wall"]
    assert p["noise"] == r["noise"]
    assert region_keys(p) == region_keys(r)
    back = np.zeros_like(decode(p["face_mask"]))
    back[pf] = decode(p["face_mask"])
    assert np.array_equal(back, decode(r["face_mask"]))
    back_v = np.zeros_like(decode(p["vertex_mask"]))
    back_v[pv] = decode(p["vertex_mask"])
    assert np.array_equal(back_v, decode(r["vertex_mask"]))


def main():
    v, f = cube()
    r = sutura_geom.diagnose(v, f)
    assert r["channels"]["orientation"]["count"] == 0
    assert r["channels"]["nm_vertex"]["count"] == 0
    assert r["parts"]["closed"] == 1
    assert r["face_mask"]["length"] == len(f)
    assert not decode(r["face_mask"]).any()
    assert r["channels"]["self_intersection"]["available"]
    assert r["channels"]["self_intersection"]["count"] == 0
    assert r["channels"]["loops"]["available"]
    assert r["channels"]["loops"]["count"] == 0
    assert r["self_intersections"]["total"] == 0
    # A mesh below FAST_MAX_RAW_PAIRS takes main's fast path: no prefilter keys
    # and main's method string, matching c137f54 exactly.
    assert "filtered_pairs" not in r["self_intersections"]
    assert "prefilter" not in r["self_intersections"]
    assert r["channels"]["self_intersection"]["method"] == "broad_phase+exact"
    rd = sutura_geom.diagnose(v, f, "deep")
    assert "filtered_pairs" not in rd["self_intersections"]
    assert "prefilter" not in rd["self_intersections"]
    assert rd["channels"]["self_intersection"]["method"] == "broad_phase+exact+shared"
    assert r["loops"]["count"] == 0
    assert r["meta"]["phase"] == 3
    for ch in ("interior", "thin_wall", "noise"):
        assert ch in r["meta"]["channels_available"]
    assert r["interior"]["available"]
    assert r["interior"]["faces"] == 0
    assert r["thin_wall"]["available"]
    assert r["thin_wall"]["units"] == "mm_assumed"
    assert r["thin_wall"]["thin_faces"] == 0
    assert r["thin_wall"]["estimated"]
    assert r["noise"]["available"]
    assert r["noise"]["noise_score"] == 0.0

    # Phase 3: an embedded cube is interior on both sides; an inverted one is
    # a cavity (solid on one side, void on the other) and is not interior.
    outer_v, outer_f = scaled_cube(0.0, 10.0)
    inner_v, inner_f = scaled_cube(3.0, 7.0)
    vv = np.vstack([outer_v, inner_v])
    ff = np.vstack([outer_f, inner_f + len(outer_v)])
    ri = sutura_geom.diagnose(vv, ff)
    assert ri["interior"]["faces"] == 12, ri["interior"]
    assert ri["channels"]["interior"]["count"] == 12
    assert any(g["kind"] == "interior_surface" for g in ri["regions"])
    cavity_f = np.vstack([outer_f, inner_f[:, ::-1] + len(outer_v)])
    rc = sutura_geom.diagnose(vv, cavity_f)
    assert rc["interior"]["faces"] == 0, rc["interior"]

    # Phase 3: a thin slab reports both normalized and mm_assumed values.
    tv, tf = box(10.0, 10.0, 0.05)
    rt = sutura_geom.diagnose(tv, tf)
    tw = rt["thin_wall"]
    assert tw["available"] and tw["thin_faces"] > 0
    assert abs(tw["min"] - 0.05) < 1e-5, tw
    diag = float(np.linalg.norm(tv.max(axis=0) - tv.min(axis=0)))
    assert abs(tw["min_norm"] - tw["min"] / diag) < 1e-12
    assert tw["thin_ratio"] > 0.0
    assert any(g["kind"] == "thin_wall_patch" for g in rt["regions"])
    assert rt["thin_wall"]["samples"] == 512
    assert rt["thin_wall"]["sample_stride"] == 8
    assert "stride" in rt["channels"]["thin_wall"]["method"]
    assert "stride" in rt["channels"]["noise"]["method"]
    rd = sutura_geom.diagnose(tv, tf, "deep")
    assert rd["thin_wall"]["samples"] == 4096
    assert rd["thin_wall"]["sample_stride"] == 1
    assert rd["channels"]["thin_wall"]["method"] == "ray_thickness"
    assert rd["noise"]["sample_stride"] == 1
    assert rd["channels"]["noise"]["method"] == "ring_deviation"

    # An interpenetrating peg is not bbox-contained; its buried end face is
    # found by the AABB-overlap face path.
    cv, cf = cube()
    peg_v = np.empty_like(cv)
    peg_v[:, 0] = 8.0 + 4.0 * cv[:, 0]
    peg_v[:, 1] = 4.0 + 2.0 * cv[:, 1]
    peg_v[:, 2] = 4.0 + 2.0 * cv[:, 2]
    pv = np.vstack([outer_v, peg_v])
    pf = np.vstack([outer_f, cf + len(outer_v)])
    rpeg = sutura_geom.diagnose(pv, pf)
    assert rpeg["interior"]["faces"] >= 1, rpeg["interior"]
    assert rpeg["interior"]["method"] == "gwn"

    check_permutation(vv, ff, ri, 3)
    check_permutation(tv, tf, rt, 5)
    check_permutation(pv, pf, rpeg, 9)

    # More than MAX_THIN_REGIONS (256) equal-thickness regions: the cap must
    # select the same geometric regions under a permutation, not the same face
    # index lists.
    mv, mf = many_thin_slabs()
    rm = sutura_geom.diagnose(mv, mf, "deep")
    thin_regions = sum(1 for g in rm["regions"]
                       if g["kind"] == "thin_wall_patch")
    assert thin_regions == 256, thin_regions
    assert rm["thin_wall"]["thin_faces"] > 256
    check_permutation(mv, mf, rm, 13, detail="deep")

    # Main-path parity: 145065 has 782,768 raw candidates, below
    # FAST_MAX_RAW_PAIRS, so fast mode takes main's path unchanged: no prefilter
    # keys and the totals stay measured, exactly as in main c137f54.
    from sutura_engine.pipeline.mesh_io import load_meshes
    _, gv, gf = load_meshes(
        os.path.join(HERE, "tests", "real-world-samples",
                     "thingi10k_145065.stl"))[0]
    rg = sutura_geom.diagnose(gv.astype(np.float64), gf)
    assert "filtered_pairs" not in rg["self_intersections"]
    assert "prefilter" not in rg["self_intersections"]
    assert rg["self_intersections"]["estimated"] is False
    assert rg["self_intersections"]["total"] is not None

    # 100281 has 1,084,675 raw candidates (> FAST_MAX_RAW_PAIRS): the >1M branch
    # prefilters, scans every survivor (well under FAST_MAX_FILTERED_PAIRS) and
    # still reports an exact total.
    _, kv, kf = load_meshes(
        os.path.join(HERE, "tests", "real-world-samples",
                     "thingi10k_100281.stl"))[0]
    rk = sutura_geom.diagnose(kv.astype(np.float64), kf)
    assert rk["self_intersections"]["prefilter"] == "coplanar_aware_plane"
    assert (rk["self_intersections"]["filtered_pairs"]
            == rk["self_intersections"]["scanned_pairs"])
    assert rk["self_intersections"]["estimated"] is False
    assert rk["self_intersections"]["total"] is not None

    vf, ff = cube()
    ff[0] = [0, 2, 1]
    rf = sutura_geom.diagnose(vf, ff)
    assert rf["channels"]["orientation"]["count"] > 0
    assert rf["orientation"]["flipped_faces"] > 0
    assert decode(rf["face_mask"]).any()

    fd = np.vstack([f, f[1:2]])
    rd = sutura_geom.diagnose(v, fd)
    assert rd["channels"]["duplicate"]["count"] > 0
    assert rd["duplicates"]["faces"] > 0

    vd, fd2 = cube()
    vd[1] = vd[0]
    rz = sutura_geom.diagnose(vd, fd2)
    assert rz["degenerate"]["zero_area"] > 0

    v2 = v.copy()
    v2[:, 0] += 3.0
    vv = np.vstack([v, v2])
    ff2 = np.vstack([f, f + len(v)])
    rp = sutura_geom.diagnose(vv, ff2)
    assert rp["parts"]["closed"] == 2
    assert rp["parts"]["overlap_edges"] == []

    # Interpenetrating cubes: exact inter-body SI refines the AABB overlap.
    v3 = v.copy()
    v3[:, 0] += 0.5
    v3[:, 1] += 0.5
    v3[:, 2] += 0.5
    vi = np.vstack([v, v3])
    fi = np.vstack([f, f + len(v)])
    ri = sutura_geom.diagnose(vi, fi)
    assert ri["self_intersections"]["inter_body"] > 0
    assert ri["self_intersections"]["intra_body"] == 0
    assert len(ri["parts"]["overlap_edges"]) == 1
    assert not ri["parts"]["overlap_estimated"]
    assert any(g["kind"] == "inter_body_overlap" for g in ri["regions"])

    # Open cube: one planar, simple boundary loop with a cap difficulty.
    ro = sutura_geom.diagnose(v, f[:-2])
    assert ro["loops"]["count"] == 1
    loop = ro["loops"]["items"][0]
    assert loop["length"] == 4 and loop["planar"] and loop["simple"]
    assert 0.0 <= loop["difficulty"] <= 1.0

    # Permutation: counts are stable, masks map back exactly.
    rng = np.random.default_rng(7)
    pv = rng.permutation(len(v))
    inv_v = np.empty_like(pv)
    inv_v[pv] = np.arange(len(pv))
    pf = rng.permutation(len(f))
    inv_f = np.empty_like(pf)
    inv_f[pf] = np.arange(len(pf))
    vp = v[pv]
    fp = inv_v[ff][pf]
    perm = sutura_geom.diagnose(vp, fp)
    assert perm["channels"]["orientation"]["count"] == rf["channels"]["orientation"]["count"]
    assert perm["channels"]["nm_vertex"]["count"] == rf["channels"]["nm_vertex"]["count"]
    assert perm["channels"]["self_intersection"]["count"] == rf["channels"]["self_intersection"]["count"]
    assert perm["loops"]["count"] == rf["loops"]["count"]
    back = np.zeros_like(decode(perm["face_mask"]))
    back[pf] = decode(perm["face_mask"])
    assert np.array_equal(back, decode(rf["face_mask"]))
    back_v = np.zeros_like(decode(perm["vertex_mask"]))
    back_v[pv] = decode(perm["vertex_mask"])
    assert np.array_equal(back_v, decode(rf["vertex_mask"]))

    from sutura_engine import diagnosis
    a = diagnosis.analyze_mesh(v, f, use_cache=False)
    assert a.diagnosis_v2 is not None
    d = diagnosis.to_dict(a)
    assert "diagnosis_v2" in d
    assert d["faces"] == 12
    print("diagnosis v2 phase-3 ok")


if __name__ == "__main__":
    main()
