#!/usr/bin/env python3
"""Torture tests: adversarial-but-printable geometry scenarios.

Runs the installed `sutura` CLI on four generated scenarios and compares
the mesh before and after:

  1. large sphere  - 5M+ triangles, measure repair time
  2. thin wall     - hollow box with <0.1 mm walls (thin features must not
                     be mistaken for noise and removed)
  3. multi-part    - disconnected legitimate parts around the 8-face
                     debris-removal threshold
  4. scan mesh     - rough, high-triangle surface with many micro-cracks

Usage: torture_tests.py  (needs the installed CLI at ~/.local/bin/sutura)
"""
import json
import math
import os
import struct
import subprocess
import sys
import tempfile

import numpy as np

SUTURA = os.path.expanduser('~/.local/bin/sutura')


def write_stl(path, verts, tris):
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    with open(path, 'wb') as f:
        f.write(b'torture test'.ljust(80, b'\0'))
        f.write(struct.pack('<I', len(t)))
        for a, b, c in t:
            f.write(struct.pack('<3f', 0, 0, 0))
            f.write(struct.pack('<3f', *v[a]))
            f.write(struct.pack('<3f', *v[b]))
            f.write(struct.pack('<3f', *v[c]))
            f.write(struct.pack('<H', 0))


def measure(path):
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.load_new_mesh(path)
    m = ms.current_mesh()
    topo = ms.apply_filter('get_topological_measures')
    geom = ms.apply_filter('get_geometric_measures')
    return {
        'faces': m.face_number(),
        'verts': m.vertex_number(),
        'boundary_edges': topo['boundary_edges'],
        'components': topo['connected_components_number'],
        'volume': geom.get('mesh_volume', 0.0),
    }


def run_sutura(path):
    r = subprocess.run([SUTURA, path], capture_output=True, text=True, timeout=1200)
    try:
        rep = json.loads(r.stdout.strip().splitlines()[-1])
    except Exception:
        return {'error': r.stdout[-200:] or r.stderr[-200:]}
    return rep


# --------------------------------------------------------------- scenarios

def large_sphere(tmp):
    """~5M triangle UV sphere; the point is timing."""
    stacks, slices = 2500, 1000
    phi = np.linspace(0, math.pi, stacks + 1)
    theta = np.linspace(0, 2 * math.pi, slices, endpoint=False)
    P, T = np.meshgrid(phi, theta, indexing='ij')
    x = np.sin(P) * np.cos(T)
    y = np.cos(P)
    z = np.sin(P) * np.sin(T)
    verts = np.stack([x.ravel(), y.ravel(), z.ravel()], axis=1)
    idx = np.arange(verts.shape[0]).reshape(P.shape)
    a = idx[:-1, :-1].ravel()
    b = idx[:-1, 1:].ravel()
    c = idx[1:, :-1].ravel()
    d = idx[1:, 1:].ravel()
    tris = np.vstack([a, b, c, c, b, d]).T.reshape(-1, 3)
    out = os.path.join(tmp, 'large.stl')
    write_stl(out, verts, tris)
    return out, len(tris)


def thin_wall(tmp):
    """A 0.05 mm thick slab (10x10x0.05) - must survive intact."""
    t = 0.00005
    w = 5.0
    v = [(-w, -w, 0), (w, -w, 0), (w, w, 0), (-w, w, 0),
         (-w, -w, t), (w, -w, t), (w, w, t), (-w, w, t)]
    tris = [(0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7),
            (0, 1, 5), (0, 5, 4), (1, 2, 6), (1, 6, 5),
            (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7)]
    out = os.path.join(tmp, 'thin.stl')
    write_stl(out, v, tris)
    return out, len(tris)


def multi_part(tmp):
    """Big box + small box (12 faces, above the 8-face cut) + tiny tetra."""
    def box(center, half, nfaces):
        v = []
        for dx in (-1, 1):
            for dy in (-1, 1):
                for dz in (-1, 1):
                    v.append((center[0] + dx * half, center[1] + dy * half,
                              center[2] + dz * half))
        if nfaces <= 6:
            t = [(0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7),
                 (0, 1, 5), (0, 5, 4)]
        else:
            t = [(0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7),
                 (0, 1, 5), (0, 5, 4), (1, 2, 6), (1, 6, 5),
                 (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7)]
        return v, t

    parts = []
    parts.append(box((0, 0, 0), 5.0, 12))                 # big
    parts.append(box((15, 0, 0), 0.5, 12))                # small but >8 faces
    parts.append(box((30, 0, 0), 0.3, 6))                 # tiny, 6 faces <8

    verts, tris = [], []
    for v, t0 in parts:
        base = len(verts)
        verts.extend(v)
        tris.extend((a + base, b + base, c + base) for a, b, c in t0)
    out = os.path.join(tmp, 'multi.stl')
    write_stl(out, verts, tris)
    return out, len(tris)


def scan_mesh(tmp):
    """Rough surface with many micro-cracks (photogrammetry-like)."""
    rng = np.random.default_rng(3)
    stacks, slices = 800, 800
    phi = np.linspace(0.2, math.pi - 0.2, stacks)
    theta = np.linspace(0, 2 * math.pi, slices, endpoint=False)
    P, T = np.meshgrid(phi, theta, indexing='ij')
    r = 1.0 + 0.02 * np.sin(7 * P) * np.cos(5 * T) + rng.normal(0, 0.01, P.shape)
    x = r * np.sin(P) * np.cos(T)
    y = r * np.cos(P)
    z = r * np.sin(P) * np.sin(T)
    verts = np.stack([x.ravel(), y.ravel(), z.ravel()], axis=1)
    idx = np.arange(verts.shape[0]).reshape(P.shape)
    a = idx[:-1, :-1].ravel(); b = idx[:-1, 1:].ravel()
    c = idx[1:, :-1].ravel(); d = idx[1:, 1:].ravel()
    tris = np.vstack([a, b, c, c, b, d]).T.reshape(-1, 3)
    # remove a few thousand triangles to open micro-cracks
    drop = rng.choice(len(tris), size=4000, replace=False)
    mask = np.ones(len(tris), dtype=bool)
    mask[drop] = False
    tris = tris[mask]
    out = os.path.join(tmp, 'scan.stl')
    write_stl(out, verts, tris)
    return out, len(tris)


# -------------------------------------------------------------------- main

def check(name, out, fixed, before, after, rep, notes):
    print('\n=== %s ===' % name)
    print('  input : %s faces, %d component(s)' % (before['faces'], before['components']))
    if 'error' in rep:
        print('  ERROR : %s' % rep['error'][:100])
        return
    s1 = rep.get('stage1', {})
    print('  output: %s faces, %d component(s), two-manifold=%s' % (
        after['faces'], after['components'], s1.get('two_manifold')))
    print('  holes : before=%d after=%d' % (
        before['boundary_edges'] // 2, after['boundary_edges'] // 2))
    dv = 0.0
    if before['volume'] and after['volume']:
        dv = abs(after['volume'] - before['volume']) / abs(before['volume']) * 100
    print('  volume: before=%.4f after=%.4f (change %.2f%%)' % (
        before['volume'], after['volume'], dv))
    for n in notes:
        print('  note  : %s' % n)


def main():
    tmp = tempfile.mkdtemp(prefix='sutura-torture-')
    results = []

    # 1) large sphere - timing
    path, ntri = large_sphere(tmp)
    before = measure(path)
    import time
    t0 = time.time()
    rep = run_sutura(path)
    dt = time.time() - t0
    fixed = path[:-4] + '_fixed.stl'
    after = measure(fixed) if os.path.exists(fixed) else {'faces': 0, 'boundary_edges': 0,
                                                          'volume': 0}
    check('1 large sphere (%d tris)' % ntri, path, fixed, before, after, rep,
          ['repair time: %.1fs' % dt])
    results.append(('large_sphere', dt))

    # 2) thin wall
    path, ntri = thin_wall(tmp)
    before = measure(path)
    rep = run_sutura(path)
    fixed = path[:-4] + '_fixed.stl'
    after = measure(fixed) if os.path.exists(fixed) else {'faces': 0, 'boundary_edges': 0,
                                                          'volume': 0}
    check('2 thin wall (0.05mm)', path, fixed, before, after, rep,
          ['thin features must survive; deletion rate should be 0'])

    # 3) multi-part
    path, ntri = multi_part(tmp)
    before = measure(path)
    rep = run_sutura(path)
    fixed = path[:-4] + '_fixed.stl'
    after = measure(fixed) if os.path.exists(fixed) else {'faces': 0, 'boundary_edges': 0,
                                                          'volume': 0}
    check('3 multi-part', path, fixed, before, after, rep,
          ['parts: big(12f) + small-box(12f, >8 must survive) + tiny(6f, <8 may be removed)'])

    # 4) scan mesh
    path, ntri = scan_mesh(tmp)
    before = measure(path)
    rep = run_sutura(path)
    fixed = path[:-4] + '_fixed.stl'
    after = measure(fixed) if os.path.exists(fixed) else {'faces': 0, 'boundary_edges': 0,
                                                          'volume': 0}
    check('4 scan mesh (%d tris)' % ntri, path, fixed, before, after, rep,
          ['rough surface + micro-cracks; residual micro-holes are expected'])

    import shutil
    shutil.rmtree(tmp, ignore_errors=True)
    return 0


if __name__ == '__main__':
    sys.exit(main())