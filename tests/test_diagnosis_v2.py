#!/usr/bin/env python3
"""Diagnosis v2 phase-1: channels, masks, permutation and JSON wrapper."""
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


def main():
    v, f = cube()
    r = sutura_geom.diagnose(v, f)
    assert r["channels"]["orientation"]["count"] == 0
    assert r["channels"]["nm_vertex"]["count"] == 0
    assert r["parts"]["closed"] == 1
    assert r["face_mask"]["length"] == len(f)
    assert not decode(r["face_mask"]).any()

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
    print("diagnosis v2 phase-1 ok")


if __name__ == "__main__":
    main()
