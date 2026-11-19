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


def inject_non_manifold_in(v, t, count, faces, rng):
    """`defect_injector.inject_non_manifold`, but chosen from `faces`.

    Same fins on a shared edge as the injector's version (that function hardcodes
    `rng.integers(0, len(t))`, so it cannot be told where to wound); only the
    choice of host face differs.
    """
    v = v.copy()
    t = t.copy()
    d = float(np.linalg.norm(v.max(axis=0) - v.min(axis=0)))
    for _ in range(count):
        if not faces:
            break
        a, b, c = t[int(rng.choice(faces))]
        for _k in range(2):
            p = v[c] + rng.normal(0, 0.01 * d + 1e-3, 3)
            v = np.vstack([v, p[None, :]])
            t = np.vstack([t, [[a, b, len(v) - 1]]])
    return v, t

def region_centre(v, t, frac=(0.5, 0.5, 0.5), half=(1.0, 1.0, 1.0)):
    """Pick a box of the mesh to concentrate damage in, and report its centre.

    Documentation needs the defects in *one* place, because the frames that show
    them are zoomed to that place: a key with four pinholes spread along 57 units
    of shaft reads as identical before and after at any scale that fits the whole
    part. `frac` moves the box centre and `half` sizes it, both as fractions of
    the bounding box, so this stays independent of units.
    """
    lo, hi = v.min(axis=0), v.max(axis=0)
    span = hi - lo
    centre = lo + span * np.asarray(frac, dtype=float)
    half_extent = span * np.asarray(half, dtype=float) * 0.5
    return centre, half_extent


def inject_patches(t, v, patches, radius, rng, centre=None, half_extent=None):
    """Delete `patches` contiguous regions of roughly `radius` faces each.

    A breadth-first growth over shared edges, so each hole is one connected
    opening rather than scattered singles. Growing is bounded by `radius`, not by
    a physical size, which keeps the result independent of mesh scale.

    When `centre`/`half_extent` are given, every region is seeded inside that box,
    so the openings end up together instead of scattered over the whole scan.
    """
    t = t.copy()
    nbrs = face_neighbours(t)
    removed = set()
    n = len(t)
    centroids = v[t].mean(axis=1) if centre is not None else None

    def inside(i):
        if centroids is None:
            return True
        d = np.abs(centroids[i] - centre)
        return bool(np.all(d <= half_extent))

    for _ in range(patches):
        # A seed face that is not already gone and not on the rim of a hole.
        for _attempt in range(2000):
            seed = int(rng.integers(0, n))
            if seed not in removed and inside(seed):
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
    ap.add_argument('--region', choices=['head', 'centre', 'tail', 'none'],
                    default='head',
                    help='where to concentrate the damage (default: head)')
    ap.add_argument('--seed', type=int, default=42)
    args = ap.parse_args()

    if os.path.abspath(args.input) == os.path.abspath(args.output):
        print('input and output must differ', file=sys.stderr)
        return 2

    v, t = load_mesh(args.input)
    rng = np.random.default_rng(args.seed)

    # Concentrate the damage. `--region` names the box as fractions of the
    # bounding box: "head" is the first quarter along X, where a key's wards are
    # and where the zoomed frames point.
    if args.region == 'none':
        centre = half = None
    else:
        # Deliberately small: the frames that show this damage are zoomed to it,
        # and a box a third of the part wide only looks zoomed when the whole part
        # is what you came from. A few units across is what a 3-4x view can fill.
        frac, size = {
            'head': ((0.20, 0.5, 0.5), (0.14, 0.5, 0.5)),
            'centre': ((0.50, 0.5, 0.5), (0.14, 0.5, 0.5)),
            'tail': ((0.80, 0.5, 0.5), (0.14, 0.5, 0.5)),
        }[args.region]
        centre, half = region_centre(v, t, frac, size)
        print(f'region {args.region}: centre {np.round(centre, 2)} '
              f'half {np.round(half, 2)}')

    # Order matters: holes first (fewer faces to search), then the injector's own
    # wounding passes on the surviving surface.
    t = inject_patches(t, v, args.patches, args.radius, rng, centre, half)

    # The injector's own passes pick faces uniformly at random, which is right for
    # a defect-detection fixture and wrong for a documentation frame: on a 90k
    # scan a dozen random faces are invisible, and worse, they land outside the
    # zoomed region. Re-derive the region after the deletion (indices moved) and
    # wound inside it. Same functions, restricted selection.
    if centre is not None:
        cen = v[t].mean(axis=1)
        inside = [i for i in range(len(t)) if np.all(np.abs(cen[i] - centre) <= half)]
        if not inside:
            print('region emptied by the hole pass; wounding everywhere', file=sys.stderr)
            inside = list(range(len(t)))
    else:
        inside = list(range(len(t)))

    if args.flipped:
        pick = rng.choice(len(inside), size=min(args.flipped, len(inside)), replace=False)
        faces = [inside[i] for i in pick]
        t = t.copy()
        for fi in faces:
            t[fi] = t[fi][::-1]

    if args.non_manifold:
        v, t = inject_non_manifold_in(v, t, args.non_manifold, inside, rng)

    save_mesh(args.output, v, t)
    print(f'{len(t)} faces -> {args.output}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
