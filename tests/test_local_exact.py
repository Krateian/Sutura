#!/usr/bin/env python3
"""Regression tests for the localized exact self-union of residual SI clusters
(sutura/local_exact.py) and the Rust PyMeshBvh winding binding.

The tier is experimental and OFF by default.  These tests use small synthetic
closed meshes whose self-intersections are genuinely LOCAL (so a k-ring patch
encloses the whole fold), plus the byte-identical no-op and rollback guards.

Usage (needs the venv with the maturin-built extension):
    <venv>/bin/python tests/test_local_exact.py
"""
import os
import sys

import numpy as np  # noqa: E402

sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', 'sutura'))

import local_exact as le  # noqa: E402

try:
    import sutura_geom  # noqa: E402
    _HAS_BVH = hasattr(sutura_geom, 'PyMeshBvh')
except Exception:  # noqa: BLE001
    sutura_geom = None
    _HAS_BVH = False


def _box(cx=0.0, cy=0.0, cz=0.0, s=2.0):
    h = s / 2
    v = np.array([[cx - h, cy - h, cz - h], [cx + h, cy - h, cz - h],
                  [cx + h, cy + h, cz - h], [cx - h, cy + h, cz - h],
                  [cx - h, cy - h, cz + h], [cx + h, cy - h, cz + h],
                  [cx + h, cy + h, cz + h], [cx - h, cy + h, cz + h]],
                 dtype=np.float32)
    f = np.array([[0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7], [0, 1, 5],
                  [0, 5, 4], [2, 3, 7], [2, 7, 6], [1, 2, 6], [1, 6, 5],
                  [3, 0, 4], [3, 4, 7]], dtype=np.int32)
    return v, f


def _swap_corners(pair):
    """Closed cube (same edge-incidence topology) whose geometry
    self-intersects after swapping two opposite vertex positions."""
    v, f = _box()
    i, j = pair
    v = v.copy()
    v[[i, j]] = v[[j, i]]
    return v, f


def _topo(v, t):
    from repair import reload_strict_holes_nm
    return reload_strict_holes_nm(v, t)


def test_local_fold_resolves_and_is_reload_honest():
    if not _HAS_BVH:
        print('skip: sutura_geom.PyMeshBvh unavailable')
        return
    v, t = _swap_corners((0, 6))
    base = _topo(v, t)
    out_v, out_t, rep = le.local_exact_self_union(None, v, t, time_budget=30.0)
    assert rep['applied'] is True, rep
    assert rep['si_before'] > 0
    assert rep['si_after'] == 0, rep
    after = _topo(out_v, out_t)
    assert after[0] <= base[0] and after[1] <= base[1], (base, after)
    assert len(out_t) < len(t)


def test_rollback_preserves_input_byte_identical():
    if not _HAS_BVH:
        print('skip: sutura_geom.PyMeshBvh unavailable')
        return
    # Swapping this pair would introduce a non-manifold edge when stitched, so
    # the per-cluster guard must roll the cluster back and return the input.
    v, t = _swap_corners((1, 7))
    out_v, out_t, rep = le.local_exact_self_union(None, v, t, time_budget=30.0)
    assert rep['applied'] is False, rep
    assert out_v.tobytes() == v.tobytes()
    assert out_t.tobytes() == t.tobytes()


def test_clean_mesh_is_byte_identical_noop():
    v, t = _box()
    out_v, out_t, rep = le.local_exact_self_union(None, v, t, time_budget=20.0)
    assert rep['applied'] is False
    assert rep['si_before'] == 0
    assert rep['reason'] == 'no_si'
    assert out_v.tobytes() == v.tobytes()
    assert out_t.tobytes() == t.tobytes()


def test_zero_budget_is_noop_without_si_scan():
    v, t = _swap_corners((0, 6))
    out_v, out_t, rep = le.local_exact_self_union(None, v, t, time_budget=0.0)
    assert rep['ran'] is False
    assert rep['reason'] == 'budget'
    assert out_v.tobytes() == v.tobytes()
    assert out_t.tobytes() == t.tobytes()


def test_detect_and_cluster_disjoint_folds():
    if not _HAS_BVH:
        print('skip: sutura_geom.PyMeshBvh unavailable')
        return
    v, t = _swap_corners((0, 6))  # two separate single-face SI clusters
    mask, count = le.detect_si_face_mask(None, v, t)
    assert count > 0 and mask.any()
    clusters = le.extract_si_clusters(t, mask)
    assert len(clusters) == 2, [len(c) for c in clusters]


def test_arrangement_preserves_open_patch_boundary():
    if not _HAS_BVH:
        print('skip: sutura_geom.PyMeshBvh unavailable')
        return
    v, f = _box()
    open_f = f[:-1]  # drop one face -> open patch with a 3-edge boundary loop
    av, at, _rep = sutura_geom.arrangement_lite(
        np.ascontiguousarray(v.astype(np.float64)),
        np.ascontiguousarray(open_f.astype(np.int32)))
    av = np.asarray(av)
    at = np.asarray(at, dtype=np.int64)
    ltp = le._match_patch_vertices(v.astype(np.float64), av)
    assert (ltp < 0).sum() == 0  # no new vertices on a clean patch
    mapped = set()
    for (a, b) in le._directed_boundary_edges(at):
        mapped.add((int(ltp[a]), int(ltp[b])))
    assert mapped == le._directed_boundary_edges(open_f), (
        mapped, le._directed_boundary_edges(open_f))


def test_scope_brake_skips_oversized_patch():
    if not _HAS_BVH:
        print('skip: sutura_geom.PyMeshBvh unavailable')
        return
    v, t = _swap_corners((0, 6))
    mask, _count = le.detect_si_face_mask(None, v, t)
    clusters = le.extract_si_clusters(t, mask)
    patches, skipped = le.dilate_and_merge_patches(t, clusters,
                                                   max_patch_faces=1)
    assert patches == []
    assert skipped >= 1


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('local-exact tests passed')


if __name__ == '__main__':
    main()
