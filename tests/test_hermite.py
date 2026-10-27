#!/usr/bin/env python3
"""Hermite (exact-data) dual contouring in ``sutura_geom.morph_close``.

The closing field is quantised to the voxel grid; with Hermite data from the
original mesh the envelope of an unchanged region must sit closer to the
original surface and keep its sharp edges, without more self-intersections or
topology defects.  ``hermite=False`` / ``SUTURA_HERMITE=0`` is the opt-out.
"""
import numpy as np
import trimesh

import sutura_geom


def _arrays(mesh):
    return (np.asarray(mesh.vertices, dtype=np.float64),
            np.asarray(mesh.faces, dtype=np.int32))


def _dist(v, t, ev, et, n=4000):
    pts, _ = trimesh.sample.sample_surface(trimesh.Trimesh(v, t, process=False), n, seed=0)
    _q, d_oe, _f = sutura_geom.closest_points(ev, et.astype(np.int64), np.asarray(pts))
    _q, d_eo, _f = sutura_geom.closest_points(v, t.astype(np.int64), ev)
    return float(np.mean(d_oe)), float(np.mean(d_eo))


def _edge_gap(v, t, ev, et):
    m = trimesh.Trimesh(v, t, process=False)
    e = m.face_adjacency_edges[np.degrees(m.face_adjacency_angles) > 30.0]
    pts = 0.5 * (v[e[:, 0]] + v[e[:, 1]])
    _q, d, _f = sutura_geom.closest_points(ev, et.astype(np.int64), pts)
    return float(np.mean(d))


def _counts(ev, et):
    u, inv = np.unique(np.round(ev, 6), axis=0, return_inverse=True)
    t = inv.reshape(-1)[et]
    e = np.sort(np.concatenate([t[:, [0, 1]], t[:, [1, 2]], t[:, [2, 0]]]), axis=1)
    _u, c = np.unique(e, axis=0, return_counts=True)
    return int((c == 1).sum()), int((c > 2).sum())


def _close(v, t, voxel, hermite):
    return sutura_geom.morph_close(v, t, 1.5 * voxel, voxel, hermite=hermite)


def test_hermite_improves_box_and_keeps_topology():
    v, t = _arrays(trimesh.creation.box([30.0, 20.0, 10.0]))
    voxel = 0.4
    bv, bt, binfo = _close(v, t, voxel, False)
    hv, ht, hinfo = _close(v, t, voxel, True)
    assert "hermite" not in binfo
    assert hinfo["hermite"]["used"] and hinfo["hermite"]["edges_exact"] > 0
    assert hinfo["manifold"] and _counts(hv, ht) == (0, 0)
    assert len(ht) == len(bt)
    b_oe, b_eo = _dist(v, t, bv, bt)
    h_oe, h_eo = _dist(v, t, hv, ht)
    assert h_oe < 0.5 * b_oe and h_eo < 0.5 * b_eo, (b_oe, h_oe, b_eo, h_eo)
    assert _edge_gap(v, t, hv, ht) < 0.5 * _edge_gap(v, t, bv, bt)
    ssi = lambda a, b: int(sutura_geom.self_intersecting_faces(a, b)[1])
    assert ssi(hv, ht) <= ssi(bv, bt)


def test_hermite_is_deterministic():
    v, t = _arrays(trimesh.creation.box([30.0, 20.0, 10.0]))
    a = _close(v, t, 0.4, True)
    b = _close(v, t, 0.4, True)
    assert np.array_equal(a[0], b[0]) and np.array_equal(a[1], b[1])


def test_hermite_never_adds_self_intersections_on_a_curved_part():
    v, t = _arrays(trimesh.creation.icosphere(subdivisions=3, radius=10.0))
    for voxel in (0.7, 1.1):
        bv, bt, _ = _close(v, t, voxel, False)
        hv, ht, info = _close(v, t, voxel, True)
        assert info["manifold"]
        assert all(h <= b for h, b in zip(_counts(hv, ht), _counts(bv, bt)))
        assert (int(sutura_geom.self_intersecting_faces(hv, ht)[1])
                <= int(sutura_geom.self_intersecting_faces(bv, bt)[1]))
        assert info["hermite"]["si_hermite"] <= info["hermite"]["si_base"]


def test_env_opt_out_matches_kwarg(monkeypatch):
    v, t = _arrays(trimesh.creation.box([30.0, 20.0, 10.0]))
    base = _close(v, t, 0.4, False)
    monkeypatch.setenv("SUTURA_HERMITE", "0")
    env = sutura_geom.morph_close(v, t, 0.6, 0.4)
    assert "hermite" not in env[2]
    assert np.array_equal(base[0], env[0]) and np.array_equal(base[1], env[1])
    monkeypatch.setenv("SUTURA_HERMITE", "1")
    on = sutura_geom.morph_close(v, t, 0.6, 0.4)
    assert on[2]["hermite"]["used"]
