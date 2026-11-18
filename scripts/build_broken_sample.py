#!/usr/bin/env python3
# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
"""Build a visibly broken derivative of a real mesh, for documentation.

Why this exists. `scripts/defect_injector.py --hole` deletes *random* faces, which
on a 90k-face scan is a scatter of pinholes: measurable (boundary loops jump from
4 to 1121) but invisible at preview size, so a before/after screenshot showed two
identical keys. Documentation needs defects a reader can see.

So this uses the injector's own `flipped_normal` and `non_manifold` functions
unchanged, and replaces only the hole step with *contiguous patches*: pick a seed
face, grow a region over shared edges, delete the whole region. That produces a
handful of holes you can see across the room instead of a thousand you cannot.

Deterministic: every step draws from one `numpy` RNG seeded with `--seed`.

Usage:
  build_broken_sample.py INPUT OUTPUT [--patches 6] [--radius 14] [--seed 42]

Writes only OUTPUT; the input is never modified. Keep the output outside the repo
-- it is derived third-party data, and the sample corpus stays pristine for the
gates.
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

from sutura_engine.pipeline.mesh_io import load_mesh, save_mesh  # noqa: E402
from defect_injector import inject_flipped_normal, inject_non_manifold  # noqa: E402


def face_neighbours(t):
    """Map each face to the faces sharing an edge with it."""
    edge_to_faces = {}
    for i, (a, b, c) in enumerate(t):
        for e in ((a, b), (b, c), (c, a)):
            key = (min(e), max(e))
            edge_to_faces.setdefault(key, []).append(i)
    out = [[] for _ in range(len(t))]
    for faces in edge_to_faces.values():
        for f in faces:
            for g in faces:
                if f != g:
                    out[f].append(g)
    return out


def inject_patches(t, patches, radius, rng):
    """Delete `patches` contiguous regions of roughly `radius` faces each.

    A breadth-first growth over shared edges, so each hole is one connected
    opening rather than scattered singles. Growing is bounded by `radius`, not by
    a physical size, which keeps the result independent of mesh scale.
    """
    t = t.copy()
    nbrs = face_neighbours(t)
    removed = set()
    n = len(t)
    for _ in range(patches):
        # A seed face that is not already gone and not on the rim of a hole.
        for _attempt in range(200):
            seed = int(rng.integers(0, n))
            if seed not in removed:
                break
        else:
            break
        region = {seed}
        frontier = [seed]
        while frontier and len(region) < radius:
            nxt = []
            for f in frontier:
                for g in nbrs[f]:
                    if g not in region and len(region) < radius:
                        region.add(g)
                        nxt.append(g)
            frontier = nxt
        removed |= region
    keep = np.array([i for i in range(n) if i not in removed], dtype=np.int64)
    return t[keep]


def main():
    ap = argparse.ArgumentParser(prog='build_broken_sample')
    ap.add_argument('input')
    ap.add_argument('output')
    ap.add_argument('--patches', type=int, default=6,
                    help='number of contiguous holes (default 6)')
    ap.add_argument('--radius', type=int, default=14,
                    help='faces per hole (default 14)')
    ap.add_argument('--flipped', type=int, default=3,
                    help='flipped-winding faces (default 3)')
    ap.add_argument('--non-manifold', type=int, default=2,
                    help='non-manifold fin pairs (default 2)')
    ap.add_argument('--seed', type=int, default=42)
    args = ap.parse_args()

    if os.path.abspath(args.input) == os.path.abspath(args.output):
        print('input and output must differ', file=sys.stderr)
        return 2

    v, t = load_mesh(args.input)
    rng = np.random.default_rng(args.seed)

    # Order matters: holes first (fewer faces to search), then the injector's own
    # wounding passes on the surviving surface.
    t = inject_patches(t, args.patches, args.radius, rng)
    v, t = inject_flipped_normal(v, t, args.flipped, rng)
    v, t = inject_non_manifold(v, t, args.non_manifold, rng)

    save_mesh(args.output, v, t)
    print(f'{len(t)} faces -> {args.output}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
