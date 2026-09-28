#!/usr/bin/env python3
"""F2 timing run: morph_close on a real-world sample.

Not a pass/fail gate -- reports wall-clock seconds, grid size and the
manifold/fallback flags so the morphology core's cost on a real mesh is
visible.

Usage:
    <venv>/bin/python tests/morph_timing_1038441.py [mesh.stl] [--voxel V] [--r R]
"""
import argparse
import sys
import time
from collections import Counter

import numpy as np
import trimesh

import sutura_geom

DEFAULT_MESH = "tests/real-world-samples/thingi10k_1038441.stl"


def manifold_ok(tris):
    d = Counter()
    for a, b, c in tris:
        for u, v in ((a, b), (b, c), (c, a)):
            d[(int(u), int(v))] += 1
    for (u, v), cnt in d.items():
        if cnt != 1 or d.get((v, u), 0) != 1:
            return False
    return True


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("mesh", nargs="?", default=DEFAULT_MESH)
    ap.add_argument("--voxel", type=float, default=None)
    ap.add_argument("--r", type=float, default=None)
    args = ap.parse_args()

    mesh = trimesh.load(args.mesh, force="mesh")
    v = np.asarray(mesh.vertices, dtype=np.float64)
    f = np.asarray(mesh.faces, dtype=np.int32)
    diag = float(np.linalg.norm(mesh.extents))
    r = args.r if args.r is not None else 0.02 * diag

    print(f"mesh: {args.mesh}")
    print(f"  input: {len(v)} verts, {len(f)} faces, bbox diag {diag:.4f}")
    print(f"  r = {r:.5f}, voxel = {args.voxel} (None = auto target 96)")

    t0 = time.perf_counter()
    ov, ot, info = sutura_geom.morph_close(v, f, r, args.voxel)
    dt = time.perf_counter() - t0

    ov = np.asarray(ov)
    ot = np.asarray(ot)
    print(f"  seconds: {dt:.3f}")
    print(f"  grid: dims={list(info['dims'])} voxel={info['voxel']:.5f} "
          f"voxels={info['voxels']} coarsened={info['caps_coarsened']}")
    print(f"  output: {len(ov)} verts, {len(ot)} faces")
    print(f"  manifold={info['manifold']} edge_manifold={manifold_ok(ot)} "
          f"fallback={info['fallback']}")


if __name__ == "__main__":
    sys.exit(main())
