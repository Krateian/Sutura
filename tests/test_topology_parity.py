#!/usr/bin/env python3
"""Parity test for the topology kernel (Rust vs the numpy oracle).

The pure-numpy implementations of ``edge_table`` / ``weld_vertices`` /
``weld_reload_equivalent`` are the oracle.  This test runs both engines over
synthetic meshes (including the block-stacked edge cases that expose
face-index pairing mistakes and dense duplicate/degenerate triangles) plus, if
present, the bundled real-world samples, and asserts the integer topology is
bit-for-bit identical.

Usage: python3 tests/test_topology_parity.py
       (Rust is exercised when the sutura_geom extension is importable.)
"""
import json
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

import numpy as np  # noqa: E402

import topology  # noqa: E402


def _cube():
    v = np.array([
        (0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0),
        (0, 0, 1), (1, 0, 1), (1, 1, 1), (0, 1, 1)], dtype=np.float32)
    t = np.array([
        (0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7),
        (0, 1, 5), (0, 5, 4), (1, 2, 6), (1, 6, 5),
        (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7)], dtype=np.int32)
    return v, t


def _meshes():
    """Deterministic synthetic mesh list exercising the risky orderings."""
    rng = np.random.default_rng(20260930)
    out = [_cube()]
    # block-stacked non-manifold: force an nm edge in the (1,2) and (2,0)
    # blocks so a wrong face pairing (repeat vs tile) would diverge.
    v = np.array([(0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, 1),
                  (1, 1, 1)], dtype=np.float32)
    out.append((v, np.array([[0, 1, 2], [0, 2, 3], [0, 3, 1], [1, 2, 3],
                             [2, 1, 4], [1, 2, 4], [2, 1, 4]], dtype=np.int64)))
    # dense random soup: many duplicate edges and degenerate faces
    for _ in range(40):
        nv = int(rng.integers(4, 14))
        v = rng.integers(0, 6, size=(nv, 3)).astype(np.float32)
        nf = int(rng.integers(1, 30))
        t = rng.integers(0, nv, size=(nf, 3)).astype(np.int64)
        out.append((v, t))
        # duplicate / reversed faces and a repeated vertex
        if len(t):
            t2 = np.vstack([t, t[0:1], t[0:1][:, ::-1]])
            out.append((v, t2))
    # real-world samples when a loader is available (bit-for-bit on big meshes)
    try:
        import glob
        import trimesh
    except Exception:
        return out
    for f in sorted(glob.glob(os.path.join(
            REPO, 'tests', 'real-world-samples', '*.stl')))[:4]:
        try:
            m = trimesh.load(f, process=True)
            v = np.asarray(m.vertices, np.float32)
            t = np.asarray(m.faces, np.int64)
            if len(t):
                out.append((v, t))
        except Exception:
            pass
    return out


def _run_all(topo_engine):
    import defects
    import repair
    import sutura_engine.graft as graft
    topology.set_engine(topo_engine)
    results = []
    for v, t in _meshes():
        t = np.ascontiguousarray(t, dtype=np.int64)
        e, c, inv = topology.edge_table(t)
        uv, uinv = topology.weld_vertices(np.ascontiguousarray(v, dtype=np.float32))
        wv, wt = topology.weld_reload_equivalent(v, t)
        ge, ginv = graft._unique_edges(t)
        _, gcounts = graft._edge_use_counts(t)
        rwv, rwt = repair.weld_reload_equivalent(v, t)
        sv, st = repair._separate_weld_collisions(v, t)
        d = defects.detect(v, t, with_indices=True)
        results.append({
            'edge_table': (e.tobytes(), c.tobytes(), inv.tobytes()),
            'weld_vertices': (uv.tobytes(), uinv.tobytes()),
            'weld_reload': (wv.tobytes(), wt.tobytes()),
            'graft_unique': (ge.tobytes(), ginv.tobytes()),
            'graft_counts': gcounts.tobytes(),
            'repair_weld': (rwv.tobytes(), rwt.tobytes()),
            'separate': (sv.tobytes(), st.tobytes()),
            'detect': json.dumps(d, sort_keys=True),
        })
    return results


def test_parity():
    if not topology._geom:
        print('skip: sutura_geom not importable; numpy oracle only')
        return
    py = _run_all('python')
    rs = _run_all('rust')
    assert len(py) == len(rs)
    for i, (a, b) in enumerate(zip(py, rs)):
        for key in a:
            assert a[key] == b[key], (
                'parity mismatch on mesh %d key %s\npython=%r\nrust=%r'
                % (i, key, a[key], b[key]))
    print('parity OK on %d meshes (Rust == numpy oracle, bit-for-bit)' % len(py))


def main():
    test_parity()
    topology.set_engine('auto')
    print('topology parity tests passed')


if __name__ == '__main__':
    main()
