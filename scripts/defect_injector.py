#!/usr/bin/env python3
"""Synthetic defect injection tool for the repair pipeline / defect detectors.

Injects one or more defect types into a mesh (STL/OBJ/3MF) and writes a
corrupted copy to a NEW file -- the input is NEVER modified. Used to generate
synthetic broken meshes for testing the repair pipeline, the defect detectors
and future synthetic training/test sets.

Defect types (any combination, applied in this order; each is validated by
the corresponding detector in tests/test_defect_injector.py):

  --hole            delete random face(s)                  -> open hole(s)
  --non-manifold    add two fin triangles on a shared edge -> non-manifold edge
  --self-intersect  drop a triangle that pierces a face    -> self-intersection
  --flipped-normal  reverse the winding order of face(s)   -> inverted normals
  --degenerate      collapse a triangle vertex onto another-> zero-area face

Options:
  --count N     number of defect instances per selected type (default 5)
  --seed N      deterministic RNG seed (default 42)

Usage:
  scripts/defect_injector.py input.stl output.stl --hole --count 5
  scripts/defect_injector.py input.stl output.stl --flipped-normal --count 3
  scripts/defect_injector.py input.stl output.stl --hole --non-manifold --seed 1

Run under the venv (needs pymeshlab):
  ~/.local/share/sutura/venv/bin/python scripts/defect_injector.py ...
"""
import argparse
import os
import sys

import numpy as np


def load_mesh(path):
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.load_new_mesh(path)
    v = np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float64)
    t = np.asarray(ms.current_mesh().face_matrix(), dtype=np.int64)
    return v, t


def save_mesh(path, verts, tris):
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float32),
                        face_matrix=np.asarray(tris, np.int32)))
    ms.save_current_mesh(path)


def _diag(v):
    if len(v) == 0:
        return 1.0
    return float(np.linalg.norm(v.max(axis=0) - v.min(axis=0))) or 1.0


def inject_hole(v, t, count, rng):
    """Delete ``count`` random faces -> open hole(s)."""
    t = t.copy()
    n = len(t)
    if n == 0:
        return v, t
    count = min(count, n - 1)
    drop = rng.choice(n, size=count, replace=False)
    keep = np.setdiff1d(np.arange(n), drop)
    return v, t[keep]


def inject_non_manifold(v, t, count, rng):
    """Add two fin triangles on a random existing edge -> non-manifold edge."""
    v = v.copy()
    t = t.copy()
    d = _diag(v)
    for _ in range(count):
        if len(t) == 0:
            break
        a, b, c = t[int(rng.integers(0, len(t)))]
        for _k in range(2):
            # new vertex near the opposite corner, offset so the fin is not
            # coplanar with the original triangle (still shares edge a-b).
            p = v[c] + rng.normal(0, 0.01 * d + 1e-3, 3)
            v = np.vstack([v, p[None, :]])
            t = np.vstack([t, [[a, b, len(v) - 1]]])
    return v, t


def inject_self_intersect(v, t, count, rng):
    """Drop a small triangle across the middle of random faces: two edge
    midpoints are lifted in OPPOSITE directions out of the face plane, so the
    new triangle passes transversally through the original face's interior ->
    a genuine self-intersection (VCG-detectable)."""
    v = v.copy()
    t = t.copy()
    for _ in range(count):
        if len(t) == 0:
            break
        fi = int(rng.integers(0, len(t)))
        va, vb, vc = v[t[fi][0]], v[t[fi][1]], v[t[fi][2]]
        nrm = np.cross(vb - va, vc - va)
        nrm = nrm / (np.linalg.norm(nrm) + 1e-12)
        size = float(np.linalg.norm(vb - va)) or 1e-9
        lift = 0.05 * size
        m1 = (va + vb) / 2.0
        m2 = (vb + vc) / 2.0 + nrm * lift
        m3 = (vc + va) / 2.0 - nrm * lift
        base = len(v)
        v = np.vstack([v, m1, m2, m3])
        t = np.vstack([t, [[base, base + 1, base + 2]]])
    return v, t


def inject_flipped_normal(v, t, count, rng):
    """Reverse the winding order of ``count`` random faces."""
    t = t.copy()
    n = len(t)
    if n == 0:
        return v, t
    count = min(count, n)
    for fi in rng.choice(n, size=count, replace=False):
        t[fi] = t[fi][::-1]
    return v, t


def inject_degenerate(v, t, count, rng):
    """Collapse one vertex of ``count`` random triangles onto another ->
    zero-area (degenerate) faces."""
    t = t.copy()
    n = len(t)
    if n == 0:
        return v, t
    count = min(count, n)
    for fi in rng.choice(n, size=count, replace=False):
        t[fi][2] = t[fi][0]
    return v, t


INJECTORS = {
    'hole': inject_hole,
    'non_manifold': inject_non_manifold,
    'self_intersect': inject_self_intersect,
    'flipped_normal': inject_flipped_normal,
    'degenerate': inject_degenerate,
}


def main():
    parser = argparse.ArgumentParser(
        prog='defect_injector',
        description='Inject synthetic defects into a mesh and write a corrupted '
                    'copy (input is never modified).')
    parser.add_argument('input', help='input mesh (STL/OBJ/3MF)')
    parser.add_argument('output', help='output corrupted mesh file')
    for name, fn in INJECTORS.items():
        parser.add_argument('--' + name, action='store_true',
                            help='inject: %s' % fn.__doc__.strip().split('->')[-1].strip())
    parser.add_argument('--count', type=int, default=5,
                        help='number of defect instances per selected type (default 5)')
    parser.add_argument('--seed', type=int, default=42,
                        help='deterministic RNG seed (default 42)')
    args = parser.parse_args()

    selected = [name for name in INJECTORS if getattr(args, name)]
    if not selected:
        print('no defect type selected (--hole/--non-manifold/--self-intersect/'
              '--flipped-normal/--degenerate)', file=sys.stderr)
        return 2
    if args.count < 1:
        print('--count must be >= 1', file=sys.stderr)
        return 2
    if os.path.abspath(args.input) == os.path.abspath(args.output):
        print('input and output must differ (the input is never modified)',
              file=sys.stderr)
        return 2

    v, t = load_mesh(args.input)
    if len(v) == 0 or len(t) == 0:
        print('input mesh is empty; nothing to corrupt', file=sys.stderr)
        return 1
    rng = np.random.default_rng(args.seed)
    for name in selected:
        v, t = INJECTORS[name](v, t, args.count, rng)
    save_mesh(args.output, v, t)
    print('injected %s (count=%d, seed=%d) -> %s' % (
        '+'.join(selected), args.count, args.seed, args.output))
    return 0


if __name__ == '__main__':
    sys.exit(main())