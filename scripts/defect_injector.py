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

Run under any interpreter with numpy + the Rust sutura_geom extension
(no pymeshlab):
  ~/.local/share/sutura/venv/bin/python scripts/defect_injector.py ...
"""
import argparse
import os
import sys

import numpy as np

_HERE = os.path.dirname(os.path.abspath(__file__))
_REPO = os.path.dirname(_HERE)
for _p in (os.path.join(_REPO, 'sutura'), _REPO):
    if _p not in sys.path:
        sys.path.insert(0, _p)

from sutura_engine.pipeline.mesh_io import (  # noqa: E402
    load_mesh, save_mesh)


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
    """Add a small triangle that pierces a random face through its interior:
    two of its vertices are lifted out of the face plane and the third is
    pushed the other way, so it crosses the face transversally -> a genuine
    (proper) self-intersection, detected by
    ``topology.self_intersecting_faces``. The piercing triangle shares no
    vertex with the face, so it is not mistaken for an adjacent face."""
    v = v.copy()
    t = t.copy()
    for _ in range(count):
        if len(t) == 0:
            break
        fi = int(rng.integers(0, len(t)))
        va, vb, vc = v[t[fi][0]], v[t[fi][1]], v[t[fi][2]]
        nrm = np.cross(vb - va, vc - va)
        nn = float(np.linalg.norm(nrm))
        if nn < 1e-12:
            continue
        nrm = nrm / nn
        centroid = (va + vb + vc) / 3.0
        size = float(np.linalg.norm(vb - va)) or 1e-9
        u = (vb - va) / (np.linalg.norm(vb - va) + 1e-12)
        w = np.cross(nrm, u)
        e = 0.2 * size
        lift = 0.05 * size
        p0 = centroid + e * u + nrm * lift
        p1 = centroid - e * u + nrm * lift
        p2 = centroid + e * w - nrm * lift
        base = len(v)
        v = np.vstack([v, p0, p1, p2])
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