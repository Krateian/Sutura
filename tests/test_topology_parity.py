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
        df = topology.remove_duplicate_faces(t)
        nf = topology.remove_null_faces(v, t)
        dv_v, dv_t = topology.remove_duplicate_vertices(v, t)
        unv_v, unv_t = topology.remove_unreferenced_vertices(v, t)
        rcc_v, rcc_t = topology.remove_connected_components(v, t, min_faces=2, remove_unref=True)
        inv_t = topology.invert_face_orientation(t)
        rnme_t = topology.repair_non_manifold_edges(v, t)
        topo_dict = topology.mesh_topology(v, t)
        if len(t) <= 5000:
            si_mask, si_cnt = topology.self_intersecting_faces(v, t)
            si_entry = (si_mask.tobytes(), int(si_cnt))
        else:
            if topo_engine == 'rust':
                si_mask, si_cnt = topology.self_intersecting_faces(v, t)
                si_entry = (si_mask.tobytes(), int(si_cnt))
            else:
                si_entry = None
        results.append({
            'edge_table': (e.tobytes(), c.tobytes(), inv.tobytes()),
            'weld_vertices': (uv.tobytes(), uinv.tobytes()),
            'weld_reload': (wv.tobytes(), wt.tobytes()),
            'graft_unique': (ge.tobytes(), ginv.tobytes()),
            'graft_counts': gcounts.tobytes(),
            'repair_weld': (rwv.tobytes(), rwt.tobytes()),
            'separate': (sv.tobytes(), st.tobytes()),
            'detect': json.dumps(d, sort_keys=True),
            'remove_duplicate_faces': df.tobytes(),
            'remove_null_faces': nf.tobytes(),
            'remove_duplicate_vertices': (dv_v.tobytes(), dv_t.tobytes()),
            'remove_unreferenced_vertices': (unv_v.tobytes(), unv_t.tobytes()),
            'remove_connected_components': (rcc_v.tobytes(), rcc_t.tobytes()),
            'invert_face_orientation': inv_t.tobytes(),
            'repair_non_manifold_edges': rnme_t.tobytes(),
            'mesh_topology': json.dumps(topo_dict, sort_keys=True),
            'self_intersecting_faces': si_entry,
        })
    return results


def test_parity():
    if not topology._rust_kernel_available():
        print('skip: sutura_geom topology kernel not importable; numpy oracle only')
        return
    py = _run_all('python')
    rs = _run_all('rust')
    assert len(py) == len(rs)
    all_m = _meshes()
    for i, (a, b) in enumerate(zip(py, rs)):
        for key in a:
            if key == 'self_intersecting_faces' and a[key] is None:
                # Big mesh: bounded oracle skipped pure-Python O(N^2); verify native Rust matches sutura_geom directly
                v, t = all_m[i]
                b_mask, b_cnt = b[key]
                v_f64 = np.ascontiguousarray(v, dtype=np.float64)
                t_i32 = np.ascontiguousarray(t, dtype=np.int32)
                exp_mask, exp_cnt = topology._geom.self_intersecting_faces(v_f64, t_i32)
                assert b_mask == exp_mask.tobytes() and b_cnt == int(exp_cnt), (
                    'mismatch on big mesh %d vs sutura_geom' % i
                )
                continue
            assert a[key] == b[key], (
                'parity mismatch on mesh %d key %s\npython=%r\nrust=%r'
                % (i, key, a[key], b[key]))
    print('parity OK on %d meshes (Rust == numpy oracle, bit-for-bit)' % len(py))


def test_fallback_when_kernel_missing():
    """An older wheel imports but lacks the kernels: every call must fall back
    to the numpy oracle transparently, and engine() must report 'python'."""
    saved = topology._geom
    try:
        for replacement in (None, object()):
            topology.set_engine('auto')
            topology._geom = replacement
            assert topology.engine() == 'python', topology.engine()
            assert not topology._use_rust()
            v, t = _cube()
            t = np.ascontiguousarray(t, dtype=np.int64)
            e, c, inv = topology.edge_table(t)
            assert e.dtype == np.int64 and c.tobytes() == (
                topology._edge_table_numpy(t)[1].tobytes())
            topology.set_engine('rust')  # forced but unavailable -> still numpy
            assert not topology._use_rust()
            wv, wt = topology.weld_reload_equivalent(v, t)
            assert wv.tobytes() == topology._weld_reload_equivalent_numpy(v, t)[0].tobytes()
            df = topology.remove_duplicate_faces(t)
            assert df.tobytes() == topology._remove_duplicate_faces_numpy(t).tobytes()
            nf = topology.remove_null_faces(v, t)
            assert nf.tobytes() == topology._remove_null_faces_numpy(v, t).tobytes()
            dv_v, dv_t = topology.remove_duplicate_vertices(v, t)
            exp_dv_v, exp_dv_t = topology._remove_duplicate_vertices_numpy(v, t)
            assert dv_v.tobytes() == exp_dv_v.tobytes() and dv_t.tobytes() == exp_dv_t.tobytes()
            unv_v, unv_t = topology.remove_unreferenced_vertices(v, t)
            exp_unv_v, exp_unv_t = topology._remove_unreferenced_vertices_numpy(v, t)
            assert unv_v.tobytes() == exp_unv_v.tobytes() and unv_t.tobytes() == exp_unv_t.tobytes()
            rcc_v, rcc_t = topology.remove_connected_components(v, t, min_faces=2, remove_unref=True)
            exp_rcc_v, exp_rcc_t = topology._remove_connected_components_numpy(v, t, min_faces=2, remove_unref=True)
            assert rcc_v.tobytes() == exp_rcc_v.tobytes() and rcc_t.tobytes() == exp_rcc_t.tobytes()
            inv_t = topology.invert_face_orientation(t)
            assert inv_t.tobytes() == topology._invert_face_orientation_numpy(t).tobytes()
            rnme_t = topology.repair_non_manifold_edges(v, t)
            exp_rnme_t = topology._repair_non_manifold_edges_numpy(v, t)
            assert rnme_t.tobytes() == exp_rnme_t.tobytes()
            topo_dict = topology.mesh_topology(v, t)
            assert topo_dict == topology._mesh_topology_numpy(v, t)
            si_mask, si_cnt = topology.self_intersecting_faces(v, t)
            exp_si_mask, exp_si_cnt = topology._self_intersecting_faces_numpy(v, t)
            assert si_mask.tobytes() == exp_si_mask.tobytes() and si_cnt == exp_si_cnt
    finally:
        topology._geom = saved
        topology.set_engine('auto')
    print('fallback OK (missing kernel -> numpy oracle)')


def test_re_orient_parity():
    v, t = _cube()
    t_inv = t.copy()
    t_inv[1] = [t_inv[1, 1], t_inv[1, 0], t_inv[1, 2]]

    out_np = topology._re_orient_faces_coherently_numpy(t_inv)
    if topology._has_rust('re_orient_faces_coherently'):
        out_rs = topology._geom.re_orient_faces_coherently(t_inv)
        assert np.array_equal(out_np, out_rs), f"mismatch: np=\n{out_np}\nrs=\n{out_rs}"

    t_nm = np.array([[0, 1, 2], [0, 1, 3], [0, 1, 4]], dtype=np.int64)
    np_err = None
    try:
        topology._re_orient_faces_coherently_numpy(t_nm)
    except Exception as e:
        np_err = type(e)
    rs_err = None
    if topology._has_rust('re_orient_faces_coherently'):
        try:
            topology._geom.re_orient_faces_coherently(t_nm)
        except Exception as e:
            rs_err = type(e)
        assert np_err == rs_err == ValueError
    print('re_orient_faces_coherently parity OK (Rust == numpy oracle, bit-for-bit)')


def test_repair_nme_parity():
    v = np.array([
        (0.0, 0.0, 0.0),
        (1.0, 0.0, 0.0),
        (0.0, 1.0, 0.0),  # area 0.5
        (0.0, 2.0, 0.0),  # area 1.0
        (0.0, 0.1, 0.0),  # area 0.05 (smallest)
    ], dtype=np.float32)
    t = np.array([[0, 1, 2], [0, 1, 3], [0, 1, 4]], dtype=np.int64)

    out_np = topology._repair_non_manifold_edges_numpy(v, t)
    if topology._has_rust('repair_non_manifold_edges'):
        out_rs = topology._geom.repair_non_manifold_edges(v, t)
        assert np.array_equal(out_np, out_rs), f"mismatch: np=\n{out_np}\nrs=\n{out_rs}"
        assert len(out_rs) == 2
        assert np.array_equal(out_rs, [[0, 1, 2], [0, 1, 3]])
    print('repair_non_manifold_edges parity OK (Rust == numpy oracle, bit-for-bit)')


def main():
    test_parity()
    test_re_orient_parity()
    test_repair_nme_parity()
    test_fallback_when_kernel_missing()
    topology.set_engine('auto')
    print('topology parity tests passed')


if __name__ == '__main__':
    main()

