#!/usr/bin/env python3
"""Throwaway measurement: fTetWild parameters vs the 180 s fallback budget.

For each mesh and parameter variant this runs pytetwild.tetrahedralize in a
killable child process (no budget unless --max-seconds is given), then
follows the production path of sutura/repair.py on the result: boundary
extraction (ftetwild_bridge.extract_boundary), the manifold3d post-process
(repair._ftetwild_manifold_postprocess) and the pinched-vertex split
(repair._split_pinched_vertices). Run it from this branch (it sits on
claude/ftetwild-bowtie-stage2, which has both helpers). It records
time, holes, non-manifold edges, two-manifold, strict watertight after the
post-process, the manifold3d verdict and the one-sided output->input
Hausdorff distance (max/mean, also relative to the input bbox diagonal).

Needs an interpreter with pymeshlab, trimesh, pytetwild (+ pyvista) and
manifold3d (the macOS conda env has all of them). No repository file is
written; the JSON goes to --out.

    python scripts/ftetwild_param_sweep.py \\
        tests/real-world-samples/artec_metal-nut.stl \\
        tests/real-world-samples/thingi10k_46012.stl \\
        --out /tmp/ftetwild_sweep.json --max-seconds 900
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile
import time

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

VARIANTS = [
    ('default', {}),
    ('optimize_off', {'optimize': False}),
    ('opt_iter_20', {'num_opt_iter': 20}),
    ('stop_energy_50', {'stop_energy': 50.0}),
    ('edge_0.1', {'edge_length_fac': 0.1}),
    ('optimize_off+edge_0.1', {'optimize': False, 'edge_length_fac': 0.1}),
]


def load_like_bridge(path):
    """Same loading as ftetwild_bridge.run_bridge (trimesh, float64/int32)."""
    import trimesh
    mesh = trimesh.load(path, force='mesh')
    return (np.asarray(mesh.vertices, dtype=np.float64),
            np.asarray(mesh.faces, dtype=np.int32))


def worker(src, params_json, out_npz):
    """Child process: one fTetWild call, tets saved to out_npz."""
    import pytetwild
    verts, tris = load_like_bridge(src)
    t0 = time.perf_counter()
    tv, tc = pytetwild.tetrahedralize(verts, tris, quiet=True, **json.loads(params_json))
    np.savez(out_npz, verts=np.asarray(tv), cells=np.asarray(tc),
             seconds=time.perf_counter() - t0)


def topo_of(ml, v, t):
    import repair
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(v, np.float64),
                        face_matrix=np.asarray(t, np.int32)))
    topo = ms.apply_filter('get_topological_measures')
    holes = repair.boundary_loop_stats(np.asarray(v, np.float64), np.asarray(t))[0]
    return ms, topo, holes, topo.get('non_two_manifold_edges', 0)


def _referenced_only(v, t):
    """Drop vertices no face references: get_hausdorff_distance with
    samplevert=True samples them too, and a raw fTetWild boundary keeps
    every tetrahedron vertex (interior ones included)."""
    t = np.asarray(t, dtype=np.int64)
    used = np.unique(t)
    remap = np.full(len(v), -1, dtype=np.int64)
    remap[used] = np.arange(len(used))
    return np.asarray(v, np.float64)[used], remap[t]


def hausdorff(ml, out_v, out_t, in_v, in_t):
    """One-sided Hausdorff: samples on the output, distance to the input
    (referenced vertices only)."""
    out_v, out_t = _referenced_only(out_v, out_t)
    in_v, in_t = _referenced_only(in_v, in_t)
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(in_v, np.float64),
                        face_matrix=np.asarray(in_t, np.int32)))      # id 0
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(out_v, np.float64),
                        face_matrix=np.asarray(out_t, np.int32)))     # id 1
    r = ms.apply_filter('get_hausdorff_distance', sampledmesh=1, targetmesh=0,
                        samplevert=True, sampleface=True,
                        samplenum=int(min(max(len(out_t), 10000), 200000)),
                        maxdist=ml.PercentageValue(100))
    diag = float(np.linalg.norm(np.max(in_v, 0) - np.min(in_v, 0)))
    return {'max': r.get('max'), 'mean': r.get('mean'),
            'max_rel_diag': (r.get('max') or 0) / diag if diag else None,
            'mean_rel_diag': (r.get('mean') or 0) / diag if diag else None}


def measure(src, name, params, max_seconds, tmp):
    import pymeshlab as ml
    import repair
    import defects
    try:
        import manifold_bridge
    except ImportError:          # manifold3d not importable here -> n/a
        manifold_bridge = None
    from ftetwild_bridge import extract_boundary
    entry = {'mesh': os.path.basename(src), 'variant': name, 'params': params}
    out_npz = os.path.join(tmp, 'tets.npz')
    t0 = time.perf_counter()
    try:
        r = subprocess.run([sys.executable, os.path.abspath(__file__), '--_worker',
                            src, json.dumps(params), out_npz],
                           capture_output=True, text=True, cwd=tmp,
                           timeout=max_seconds)
    except subprocess.TimeoutExpired:
        entry.update(status='timeout', wall_seconds=round(time.perf_counter() - t0, 1))
        return entry
    entry['wall_seconds'] = round(time.perf_counter() - t0, 1)
    if r.returncode != 0 or not os.path.exists(out_npz):
        entry.update(status='error', stderr=r.stderr[-600:])
        return entry
    d = np.load(out_npz)
    tv, tc = d['verts'], d['cells']
    entry['ftetwild_seconds'] = round(float(d['seconds']), 1)
    entry['tet_cells'] = int(len(tc))
    bt = np.asarray(extract_boundary(tv, tc), np.int32)
    entry['boundary_faces'] = int(len(bt))
    ms, topo, holes, nm = topo_of(ml, tv, bt)
    entry['raw'] = {'holes': holes, 'nm_edges': nm,
                    'nm_vertices': topo.get('non_two_manifold_vertices', 0),
                    'two_manifold': bool(topo.get('is_mesh_two_manifold'))}
    # production post-process (manifold3d) and pinched-vertex split
    t1 = time.perf_counter()
    cv, ct, cms, ctopo, ch, cnm, post = repair._ftetwild_manifold_postprocess(
        ml, tmp, tv, bt, ms, topo, holes, nm)
    cms, ctopo, pinch = repair._split_pinched_vertices(ml, cms, ctopo)
    entry['post_seconds'] = round(time.perf_counter() - t1, 1)
    m = cms.current_mesh()
    fv, ft = np.asarray(m.vertex_matrix()), np.asarray(m.face_matrix())
    det = defects.detect(fv, ft)
    if manifold_bridge is not None:
        m3_ok, m3_status = manifold_bridge.watertight_check(fv, ft)
    else:
        m3_ok, m3_status = None, 'manifold3d not importable'
    entry['after'] = {'manifold_postprocessed': bool(post), 'pinched_split': pinch,
                      'holes': len(det['holes']), 'nm_edges': len(det['non_manifold']),
                      'two_manifold': bool(ctopo.get('is_mesh_two_manifold')),
                      'strict_watertight': bool(not det['holes'] and not det['non_manifold']),
                      'manifold3d_watertight': m3_ok, 'manifold3d_status': m3_status,
                      'faces': int(len(ft))}
    in_v, in_t = load_like_bridge(src)
    entry['hausdorff_out_to_in'] = hausdorff(ml, fv, ft, in_v, in_t)
    entry['status'] = 'ok'
    return entry


def main():
    if len(sys.argv) > 1 and sys.argv[1] == '--_worker':
        worker(*sys.argv[2:5])
        return
    p = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    p.add_argument('meshes', nargs='+')
    p.add_argument('--out', required=True, help='JSON output path')
    p.add_argument('--max-seconds', type=float, default=None,
                   help='optional per-variant ceiling (default: no budget)')
    p.add_argument('--variants', default=','.join(n for n, _ in VARIANTS),
                   help='comma-separated subset of: %s' % ', '.join(n for n, _ in VARIANTS))
    args = p.parse_args()
    wanted = args.variants.split(',')
    results = []
    for src in args.meshes:
        for name, params in VARIANTS:
            if name not in wanted:
                continue
            with tempfile.TemporaryDirectory(prefix='ftw-sweep-') as tmp:
                e = measure(os.path.abspath(src), name, params, args.max_seconds, tmp)
            results.append(e)
            a = e.get('after', {})
            h = e.get('hausdorff_out_to_in', {})
            print('%-26s %-22s %-7s ftw=%7ss post=%5ss raw(h=%s nm=%s 2m=%s) '
                  'after(h=%s nm=%s wt=%s m3d=%s) hd_max=%.4f hd_mean=%.5f (rel diag)' % (
                      e['mesh'][:26], name, e['status'], e.get('ftetwild_seconds', e.get('wall_seconds')),
                      e.get('post_seconds', '-'), e.get('raw', {}).get('holes'),
                      e.get('raw', {}).get('nm_edges'), e.get('raw', {}).get('two_manifold'),
                      a.get('holes'), a.get('nm_edges'), a.get('strict_watertight'),
                      a.get('manifold3d_watertight'), h.get('max_rel_diag') or 0,
                      h.get('mean_rel_diag') or 0), flush=True)
            with open(args.out, 'w') as f:
                json.dump(results, f, indent=1)
    print('wrote %s (%d entries)' % (args.out, len(results)))


if __name__ == '__main__':
    main()
