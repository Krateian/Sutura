#!/usr/bin/env python3
"""Test for the native ``sutura_geom.close_holes`` (Phase 4A).

``close_holes`` ports PyMeshLab's ``meshing_close_holes`` filter.  There is no
numpy oracle for it (unlike the topology kernel), so this test pins the
structural contract of the Rust kernel directly:

* the input vertices and triangles are never modified -- new triangles are
  only appended (the pipeline relies on this);
* original vertex coordinates survive bit-for-bit (the kernel is f64);
* the number of boundary edges never rises and, for a size-eligible loop, a
  hole is always closed (VCG's ``selfintersection`` flag is best-effort: it
  prefers a non-crossing ear but never leaves a closable loop open);
* loops longer than ``max_hole_size`` are left alone;
* a non-edge-manifold (edge-use > 2) input is rejected;
* the result is deterministic.

When PyMeshLab is importable the same inputs are run through the reference
filter as an informational quality comparison (the outcome is not required to
be identical -- see docs/).

Usage: python3 tests/test_hole_parity.py
       (~/.local/share/sutura/venv/bin/python tests/test_hole_parity.py)
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402

try:
    import sutura_geom
except ImportError:  # pragma: no cover - extension not built
    print('sutura_geom extension not importable; skipping close_holes test')
    sys.exit(0)

SAMPLES = os.path.join(REPO, 'tests', 'real-world-samples')


def _f64(a):
    return np.ascontiguousarray(np.asarray(a, dtype=np.float64))


def _i64(a):
    return np.ascontiguousarray(np.asarray(a, dtype=np.int64))


def boundary_nm(verts, tris):
    """(boundary_edges, non_manifold_edges) from plain arrays."""
    t = np.asarray(tris)
    if len(t) == 0:
        return 0, 0
    e = np.concatenate([t[:, [0, 1]], t[:, [1, 2]], t[:, [2, 0]]])
    e = np.sort(e, axis=1)
    _, counts = np.unique(e, axis=0, return_counts=True)
    return int((counts == 1).sum()), int((counts > 2).sum())


def _outward_tetra():
    v = _f64([[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1]])
    # Outward winding; the (0,2,1) base is removed -> triangular hole.
    t = _i64([[0, 1, 3], [1, 2, 3], [2, 0, 3]])
    return v, t


def _clean_tetra():
    v, t = _outward_tetra()
    return v, np.vstack([t, _i64([[0, 2, 1]])])


def _cube_minus_top():
    v = _f64([
        [0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
        [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]])
    full = [
        [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
        [0, 1, 5], [0, 5, 4], [1, 2, 6], [1, 6, 5],
        [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7]]
    # Drop the (4, 5, 6) / (4, 6, 7) top pair -> a square (4-edge) hole.
    t = _i64([f for f in full if f not in ([4, 5, 6], [4, 6, 7])])
    return v, t


def _close(v, t, max_hole_size=1000, self_intersection=True):
    rv, rt, closed, newf = sutura_geom.close_holes(
        _f64(v), _i64(t), max_hole_size=max_hole_size,
        self_intersection=self_intersection)
    return np.asarray(rv), np.asarray(rt), int(closed), int(newf)


def _assert_prefix_preserved(v, t, rv, rt, label):
    assert rv.shape == v.shape, f'{label}: vertex count changed'
    assert np.array_equal(rv, v), f'{label}: input vertices were modified'
    assert rt.shape[0] >= t.shape[0], f'{label}: faces were removed'
    assert np.array_equal(rt[:t.shape[0]], t), f'{label}: input faces changed'
    assert rt.dtype == np.int64, f'{label}: face dtype is not int64'
    assert rv.dtype == np.float64, f'{label}: vertex dtype is not float64'


def test_structural():
    # triangular hole -> refilled with exactly the missing face
    v, t = _outward_tetra()
    b0, _ = boundary_nm(v, t)
    assert b0 == 3
    rv, rt, closed, newf = _close(v, t)
    _assert_prefix_preserved(v, t, rv, rt, 'tetra')
    b1, _ = boundary_nm(rv, rt)
    assert closed == 1 and newf == 1 and b1 == 0, (closed, newf, b1)
    assert list(rt[-1]) == [0, 2, 1], list(rt[-1])

    # already closed mesh -> untouched
    v, t = _clean_tetra()
    rv, rt, closed, newf = _close(v, t)
    _assert_prefix_preserved(v, t, rv, rt, 'clean tetra')
    assert closed == 0 and newf == 0, (closed, newf)
    assert rt.shape[0] == t.shape[0]

    # square hole -> closed, both original loops' faces preserved
    v, t = _cube_minus_top()
    b0, _ = boundary_nm(v, t)
    assert b0 == 4
    rv, rt, closed, newf = _close(v, t)
    _assert_prefix_preserved(v, t, rv, rt, 'cube-top')
    b1, _ = boundary_nm(rv, rt)
    assert closed == 1 and newf == 2 and b1 == 0, (closed, newf, b1)
    print('  structural (tetra/cube) OK: faces appended, inputs preserved')


def test_size_filter():
    v, t = _outward_tetra()
    rv, rt, closed, newf = _close(v, t, max_hole_size=2)
    assert closed == 0 and newf == 0, ('size filter ignored', closed, newf)
    assert np.array_equal(rt, t)
    rv, rt, closed, newf = _close(v, t, max_hole_size=3)
    assert closed == 1 and newf == 1
    print('  size filter OK: loop len 3 closed only at max_hole_size >= 3')


def test_self_intersection_is_best_effort():
    # Best-effort contract: with the guard on, a size-eligible hole is still
    # always closed (VCG never leaves it open because of the SI flag alone).
    v, t = _outward_tetra()
    for si in (False, True):
        rv, rt, closed, newf = _close(v, t, self_intersection=si)
        b1, _ = boundary_nm(rv, rt)
        assert closed == 1 and b1 == 0, ('si=%s left a hole open' % si, closed, b1)
    print('  best-effort SI OK: eligible loop closes with self_intersection on')


def test_determinism():
    v, t = _cube_minus_top()
    a = _close(v, t)
    b = _close(v, t)
    assert np.array_equal(a[0], b[0]) and np.array_equal(a[1], b[1])
    assert a[2] == b[2] and a[3] == b[3]
    print('  determinism OK: two runs identical')


def test_exact_coordinates():
    # Non-representable f64 coordinates must survive unchanged.
    x = np.float64(1.0) / np.float64(3.0)
    y = 0.1 + 0.2
    v = _f64([[0, 0, 0], [x, y, 0], [0, 1, 0], [0, 0, 1]])
    _, t = _outward_tetra()
    rv, _, closed, _ = _close(v, t)
    assert closed == 1
    assert np.array_equal(rv[:4], v), 'f64 coordinates were perturbed'
    print('  exact coordinates OK: f64 vertices bit-identical')


def test_non_manifold_rejected():
    v = _f64([[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1], [0, -1, 0]])
    # Three faces share the (0,1) edge -> non-edge-manifold input.
    t = _i64([[0, 1, 2], [0, 1, 3], [0, 1, 4]])
    raised = False
    try:
        _close(v, t)
    except ValueError:
        raised = True
    assert raised, 'non-manifold input was not rejected'
    print('  non-manifold rejection OK')


def test_planar_cap_on_fold_seam():
    # A planar, simple boundary loop (a scan's fold seam) must be capped flat
    # without introducing self-intersections: the weight-based ear-clipper can
    # otherwise pick ears that slice through the folded body.  thingi10k_1038439
    # holds two such 85-gons; each must close with exact zero proper crossings.
    try:
        import trimesh
        import collections
    except ImportError:
        print('  trimesh absent; planar-cap fold-seam test skipped')
        return
    path = os.path.join(SAMPLES, 'thingi10k_1038439.stl')
    if not os.path.exists(path):
        print('  sample absent; planar-cap fold-seam test skipped')
        return
    m = trimesh.load(path, process=True)
    V = np.asarray(m.vertices, dtype=np.float64)
    T = np.asarray(m.faces, dtype=np.int64)
    ef = collections.defaultdict(list)
    for fi, t in enumerate(T):
        for j in range(3):
            a, b = int(t[j]), int(t[(j + 1) % 3])
            ef[(min(a, b), max(a, b))].append(fi)
    par = list(range(len(T)))

    def find(x):
        while par[x] != x:
            par[x] = par[par[x]]
            x = par[x]
        return x

    for fs in ef.values():
        for k in range(1, len(fs)):
            ra, rb = find(fs[0]), find(fs[k])
            if ra != rb:
                par[ra] = rb
    groups = collections.defaultdict(list)
    for fi in range(len(T)):
        groups[find(fi)].append(fi)
    comps = sorted(groups.values(), key=len, reverse=True)
    assert len(comps) >= 2, 'expected multiple components in the sample'
    for comp in comps[:2]:
        used = np.unique(T[comp])
        remap = {int(v): k for k, v in enumerate(used)}
        cv = V[used]
        ct = np.array([[remap[int(x)] for x in T[fi]] for fi in comp], dtype=np.int64)
        rv, rt, closed, _ = _close(cv, ct)
        _, si = sutura_geom.self_intersecting_faces(_f64(rv), _i64(rt))
        assert closed == 1 and si == 0, ('fold seam not SI-free', closed, si)
    print('  planar cap OK: fold-seam 85-gons close with zero self-intersections')


def test_pymeshlab_quality():
    try:
        import pymeshlab
    except ImportError:
        print('  PyMeshLab absent; quality comparison skipped')
        return
    names = ['thingi10k_100173', 'thingi10k_100827', 'thingi10k_1038439']
    checked = 0
    for name in names:
        path = os.path.join(SAMPLES, name + '.stl')
        if not os.path.exists(path):
            continue
        ms = pymeshlab.MeshSet()
        ms.load_new_mesh(path)
        m = ms.current_mesh()
        v = m.vertex_matrix().astype(np.float64)
        t = m.face_matrix().astype(np.int64)
        b0, nm0 = boundary_nm(v, t)

        rv, rt, closed, newf = _close(v, t, max_hole_size=1000)
        _assert_prefix_preserved(v, t, rv, rt, name)
        b1, nm1 = boundary_nm(rv, rt)

        ms2 = pymeshlab.MeshSet()
        ms2.load_new_mesh(path)
        ms2.apply_filter('meshing_close_holes', maxholesize=1000,
                         selfintersection=True, newfaceselected=False,
                         refinehole=False)
        pm = ms2.current_mesh()
        pb, pnm = boundary_nm(pm.vertex_matrix().astype(np.float64),
                              pm.face_matrix().astype(np.int64))

        # Quality gate: our port never makes the boundary worse than the
        # input.  (The reference may cap some loops with fewer / more edges;
        # the absolute numbers are informational.)
        assert b1 <= b0, (name, 'boundary grew', b0, b1)
        print('    %-20s in b/nm=%d/%d -> ours %d/%d (closed=%d) vs pml %d/%d'
              % (name, b0, nm0, b1, nm1, closed, pb, pnm))
        checked += 1
    print('  PyMeshLab quality comparison OK on %d sample(s)' % checked)


def main():
    test_structural()
    test_size_filter()
    test_self_intersection_is_best_effort()
    test_determinism()
    test_exact_coordinates()
    test_non_manifold_rejected()
    test_planar_cap_on_fold_seam()
    test_pymeshlab_quality()
    print('close_holes parity tests passed')


if __name__ == '__main__':
    main()
