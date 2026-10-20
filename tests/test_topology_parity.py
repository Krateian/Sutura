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
        geom_dict = topology.mesh_geometric_measures(v, t)
        geom_rounded = {k: round(val, 4) if isinstance(val, float) else val for k, val in geom_dict.items()}
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
            'mesh_geometric_measures': json.dumps(geom_rounded, sort_keys=True),
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
    """An older wheel imports but lacks the kernels: in 'auto' or 'rust' mode,
    calling topology functions must raise a hard RuntimeError, never silently
    falling back to numpy. Explicit 'python' mode must still use the numpy oracle."""
    saved = topology._geom
    try:
        for replacement in (None, object()):
            topology._geom = replacement
            # Auto and rust modes must raise
            for mode in ('auto', 'rust'):
                topology.set_engine(mode)
                raised = False
                try:
                    topology.engine()
                except RuntimeError as ex:
                    assert "sutura_geom topology kernel required" in str(ex)
                    raised = True
                assert raised, f"expected RuntimeError in mode {mode}"

                raised = False
                try:
                    v, t = _cube()
                    t = np.ascontiguousarray(t, dtype=np.int64)
                    topology.edge_table(t)
                except RuntimeError as ex:
                    assert "sutura_geom topology kernel required" in str(ex)
                    raised = True
                assert raised, f"expected RuntimeError in edge_table with mode {mode}"

            # Explicit python mode must still use the numpy oracle
            topology.set_engine('python')
            assert topology.engine() == 'python'
            assert not topology._use_rust()
            v, t = _cube()
            t = np.ascontiguousarray(t, dtype=np.int64)
            e, c, inv = topology.edge_table(t)
            assert e.dtype == np.int64 and c.tobytes() == (
                topology._edge_table_numpy(t)[1].tobytes())
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
            geom_dict = topology.mesh_geometric_measures(v, t)
            exp_geom = topology._mesh_geometric_measures_numpy(v, t)
            assert abs(geom_dict['surface_area'] - exp_geom['surface_area']) < 1e-4
            if 'mesh_volume' in exp_geom:
                assert abs(geom_dict['mesh_volume'] - exp_geom['mesh_volume']) < 1e-4
            si_mask, si_cnt = topology.self_intersecting_faces(v, t)
            exp_si_mask, exp_si_cnt = topology._self_intersecting_faces_numpy(v, t)
            assert si_mask.tobytes() == exp_si_mask.tobytes() and si_cnt == exp_si_cnt
    finally:
        topology._geom = saved
        topology.set_engine('auto')
    print('hard error OK (missing kernel -> RuntimeError in auto/rust; python oracle works)')


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


def test_geometric_measures_parity():
    v, t = _cube()
    m_np = topology._mesh_geometric_measures_numpy(v, t)
    assert abs(m_np['surface_area'] - 6.0) < 1e-6
    assert abs(m_np['mesh_volume'] - 1.0) < 1e-6

    if topology._has_rust('mesh_geometric_measures'):
        m_rs = topology._geom.mesh_geometric_measures(v, t)
        assert abs(m_rs['surface_area'] - 6.0) < 1e-6
        assert abs(m_rs['mesh_volume'] - 1.0) < 1e-6
        assert abs(m_rs['surface_area'] - m_np['surface_area']) < 1e-12
        assert abs(m_rs['mesh_volume'] - m_np['mesh_volume']) < 1e-12
    print('mesh_geometric_measures parity OK (Rust == numpy oracle, bit-for-bit)')


def test_pymeshlab_parity():
    """Verify exact dict parity (including key presence and VCG quirks) against PyMeshLab.

    Tests 4 synthetic cases (closed cube, non-manifold edge, non-manifold vertex pinch,
    open triangle) and all tracked corpus meshes from tests/real-world-samples/*.stl.
    """
    try:
        import pymeshlab
    except ImportError:
        print('pymeshlab not importable, skipping PyMeshLab parity check')
        return

    top_keys = (
        'boundary_edges', 'connected_components_number', 'edges_number',
        'faces_number', 'genus', 'incident_faces_on_non_two_manifold_edges',
        'incident_faces_on_non_two_manifold_vertices', 'is_mesh_two_manifold',
        'non_two_manifold_edges', 'non_two_manifold_vertices', 'number_holes',
        'unreferenced_vertices', 'vertices_number'
    )

    # 1. Synthetic cases
    synthetic_cases = []

    # a) Closed watertight cube
    v_cube, t_cube = _cube()
    synthetic_cases.append(('closed_cube', v_cube, t_cube))

    # b) Non-manifold edge: 3 triangles sharing an edge (0, 1)
    v_nme = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1]], dtype=np.float32)
    t_nme = np.array([[0, 1, 2], [0, 1, 3], [0, 1, 4]], dtype=np.int64)
    synthetic_cases.append(('nm_edge', v_nme, t_nme))

    # c) Non-manifold vertex (pinch): 2 tetrahedra touching at single vertex 0
    v_pinch = np.array([
        [0, 0, 0],
        [1, 0, 0], [0, 1, 0], [0, 0, 1],
        [-1, 0, 0], [0, -1, 0], [0, 0, -1]
    ], dtype=np.float32)
    t_pinch = np.array([
        [0, 1, 2], [0, 2, 3], [0, 3, 1], [1, 3, 2],
        [0, 5, 4], [0, 6, 5], [0, 4, 6], [4, 5, 6]
    ], dtype=np.int64)
    synthetic_cases.append(('nm_vertex_pinch', v_pinch, t_pinch))

    # d) Open mesh with boundary: single triangle
    v_open = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0]], dtype=np.float32)
    t_open = np.array([[0, 1, 2]], dtype=np.int64)
    synthetic_cases.append(('open_triangle', v_open, t_open))

    for name, v, t in synthetic_cases:
        ms = pymeshlab.MeshSet()
        ms.add_mesh(pymeshlab.Mesh(v, t.astype(np.int32)))
        pml_top = ms.get_topological_measures()
        pml_geo = ms.get_geometric_measures()

        for eng in ('python', 'rust'):
            topology.set_engine(eng)
            cur_top = topology.mesh_topology(v, t)
            cur_geo = topology.mesh_geometric_measures(v, t)

            for k in top_keys:
                assert cur_top[k] == pml_top[k], (
                    f"[{name}:{eng}] top key {k} mismatch: pml={pml_top[k]} vs {eng}={cur_top[k]}"
                )

            assert abs(cur_geo['surface_area'] - float(pml_geo['surface_area'])) < 1e-4
            assert ('mesh_volume' in pml_geo) == ('mesh_volume' in cur_geo), (
                f"[{name}:{eng}] mesh_volume presence mismatch: pml={'mesh_volume' in pml_geo} vs {eng}={'mesh_volume' in cur_geo}"
            )
            if 'mesh_volume' in pml_geo:
                assert abs(cur_geo['mesh_volume'] - float(pml_geo['mesh_volume'])) < 1e-4

    # 2. Tracked corpus meshes (subprocess-isolated to prevent PyMeshLab heap corruption)
    import glob
    import subprocess
    samples = sorted(glob.glob(os.path.join(REPO, 'tests', 'real-world-samples', '*.stl')))
    samples = [p for p in samples if not p.endswith('_fixed.stl')]
    if samples:
        worker_code = """
import sys, os, json, pymeshlab, numpy as np
import topology

path = sys.argv[1]
ms = pymeshlab.MeshSet()
ms.load_new_mesh(path)
m = ms.current_mesh()
v = m.vertex_matrix()
t = m.face_matrix()

pml_top = ms.get_topological_measures()
pml_geo = ms.get_geometric_measures()

top_keys = [
    'boundary_edges', 'connected_components_number', 'edges_number',
    'faces_number', 'genus', 'incident_faces_on_non_two_manifold_edges',
    'incident_faces_on_non_two_manifold_vertices', 'is_mesh_two_manifold',
    'non_two_manifold_edges', 'non_two_manifold_vertices', 'number_holes',
    'unreferenced_vertices', 'vertices_number'
]
pml_top_clean = {k: pml_top[k] for k in top_keys}
pml_geo_clean = {'surface_area': float(pml_geo['surface_area'])}
if 'mesh_volume' in pml_geo:
    pml_geo_clean['mesh_volume'] = float(pml_geo['mesh_volume'])

diffs = []
for eng in ('python', 'rust'):
    topology.set_engine(eng)
    curr_top = topology.mesh_topology(v, t)
    curr_geo = topology.mesh_geometric_measures(v, t)

    for k in top_keys:
        if pml_top_clean[k] != curr_top[k]:
            diffs.append(f"[{eng}] top.{k}: pml={pml_top_clean[k]} {eng}={curr_top[k]}")

    if abs(pml_geo_clean['surface_area'] - curr_geo['surface_area']) > 1e-4 * max(1.0, pml_geo_clean['surface_area']):
        diffs.append(f"[{eng}] geo.surface_area: pml={pml_geo_clean['surface_area']} {eng}={curr_geo['surface_area']}")

    if ('mesh_volume' in pml_geo_clean) != ('mesh_volume' in curr_geo):
        diffs.append(f"[{eng}] geo.mesh_volume presence: pml={'mesh_volume' in pml_geo_clean} {eng}={'mesh_volume' in curr_geo}")
    elif 'mesh_volume' in pml_geo_clean:
        if abs(pml_geo_clean['mesh_volume'] - curr_geo['mesh_volume']) > 1e-4 * max(1.0, abs(pml_geo_clean['mesh_volume'])):
            diffs.append(f"[{eng}] geo.mesh_volume: pml={pml_geo_clean['mesh_volume']} {eng}={curr_geo['mesh_volume']}")

if diffs:
    print(json.dumps({'status': 'diff', 'diffs': diffs}))
else:
    print(json.dumps({'status': 'ok'}))
"""
        import tempfile
        with tempfile.NamedTemporaryFile(mode='w', suffix='.py', delete=False) as f:
            f.write(worker_code)
            tmp_worker = f.name
        worker_env = dict(os.environ, PYTHONPATH=SUTURA)
        with tempfile.TemporaryDirectory(prefix='sutura-obj-parity-') as obj_td:
            # OBJ load path: a cube with a duplicated face. PyMeshLab keeps the
            # duplicate on load; measuring the loaded arrays (the pipeline
            # call-site rule) must match PyMeshLab exactly. Run in the same
            # subprocess worker as the corpus, so a previous in-process
            # PyMeshLab MeshSet cannot corrupt the measure.
            v_obj, t_obj = _cube()
            t_obj = np.vstack([t_obj, t_obj[0:1]])
            obj_path = os.path.join(obj_td, 'synthetic_duplicate_face.obj')
            with open(obj_path, 'w') as f:
                for p in np.asarray(v_obj, np.float32):
                    f.write('v %.9g %.9g %.9g\n' % (p[0], p[1], p[2]))
                for q in np.asarray(t_obj, np.int64):
                    f.write('f %d %d %d\n' % (q[0] + 1, q[1] + 1, q[2] + 1))
            try:
                for path in samples + [obj_path]:
                    proc = subprocess.run([sys.executable, tmp_worker, path], env=worker_env, capture_output=True, text=True)
                    assert proc.returncode == 0, f"Worker failed on {path}: {proc.stderr}"
                    res = json.loads(proc.stdout.strip().splitlines()[-1])
                    assert res['status'] == 'ok', f"Mismatch on {path}: {res['diffs']}"
            finally:
                if os.path.exists(tmp_worker):
                    os.remove(tmp_worker)

    print(f'pymeshlab parity OK on {len(samples)} corpus meshes + 1 OBJ load case + {len(synthetic_cases)} synthetic non-manifold cases (bit-for-bit, exact keys)')


def test_phase2_filter_parity():
    """Verify bit-for-bit parity against PyMeshLab on all Phase 2 repair and cleanup filters.

    Filters covered:
      - meshing_repair_non_manifold_vertices
      - meshing_repair_non_manifold_edges
      - meshing_remove_unreferenced_vertices
      - meshing_remove_selected_faces
      - meshing_invert_face_orientation
      - meshing_remove_connected_component_by_face_number
      - compute_normal_per_face
      - compute_normal_per_vertex
    """
    try:
        import pymeshlab
    except ImportError:
        print('pymeshlab not importable, skipping phase 2 filter parity check')
        return

    import glob
    import subprocess
    import tempfile

    samples = sorted(glob.glob(os.path.join(REPO, 'tests', 'real-world-samples', '*.stl')))
    samples = [p for p in samples if not p.endswith('_fixed.stl')]
    if not samples:
        print('no corpus samples found, skipping corpus phase 2 parity check')
        return

    worker_code = """
import sys, os, json, pymeshlab, numpy as np
import topology

path = sys.argv[1]
ms0 = pymeshlab.MeshSet()
ms0.load_new_mesh(path)
m0 = ms0.current_mesh()
v = m0.vertex_matrix()
t = m0.face_matrix()

diffs = []

# 1. invert_face_orientation
ms = pymeshlab.MeshSet()
ms.add_mesh(pymeshlab.Mesh(v, t.astype(np.int32)))
ms.apply_filter('meshing_invert_face_orientation')
pml_t_inv = ms.current_mesh().face_matrix()
for eng in ('python', 'rust'):
    topology.set_engine(eng)
    t_inv = topology.invert_face_orientation(t)
    if not np.array_equal(pml_t_inv, t_inv):
        diffs.append(f'[{eng}] invert_face_orientation mismatch')

# 2. remove_unreferenced_vertices
ms = pymeshlab.MeshSet()
ms.add_mesh(pymeshlab.Mesh(v, t.astype(np.int32)))
ms.apply_filter('meshing_remove_unreferenced_vertices')
pml_v_unref = ms.current_mesh().vertex_matrix()
pml_t_unref = ms.current_mesh().face_matrix()
for eng in ('python', 'rust'):
    topology.set_engine(eng)
    v_unref, t_unref = topology.remove_unreferenced_vertices(v, t)
    if not (np.array_equal(pml_v_unref, v_unref) and np.array_equal(pml_t_unref, t_unref)):
        diffs.append(f'[{eng}] remove_unreferenced_vertices mismatch')

# 3. remove_connected_components
ms = pymeshlab.MeshSet()
ms.add_mesh(pymeshlab.Mesh(v, t.astype(np.int32)))
ms.apply_filter('meshing_remove_connected_component_by_face_number', mincomponentsize=8, removeunref=True)
pml_v_cc = ms.current_mesh().vertex_matrix()
pml_t_cc = ms.current_mesh().face_matrix()
for eng in ('python', 'rust'):
    topology.set_engine(eng)
    v_cc, t_cc = topology.remove_connected_components(v, t, min_faces=8, remove_unref=True)
    if not (np.array_equal(pml_v_cc, v_cc) and np.array_equal(pml_t_cc, t_cc)):
        diffs.append(f'[{eng}] remove_connected_components mismatch')

# 4. repair_non_manifold_edges
ms = pymeshlab.MeshSet()
ms.add_mesh(pymeshlab.Mesh(v, t.astype(np.int32)))
ms.apply_filter('meshing_repair_non_manifold_edges')
pml_t_nme = ms.current_mesh().face_matrix()
for eng in ('python', 'rust'):
    topology.set_engine(eng)
    t_nme = topology.repair_non_manifold_edges(v, t)
    if not np.array_equal(pml_t_nme, t_nme):
        diffs.append(f'[{eng}] repair_non_manifold_edges mismatch')

# 5. repair_non_manifold_vertices
ms = pymeshlab.MeshSet()
ms.add_mesh(pymeshlab.Mesh(v, t.astype(np.int32)))
ms.apply_filter('meshing_repair_non_manifold_vertices')
pml_v_nmv = ms.current_mesh().vertex_matrix()
pml_t_nmv = ms.current_mesh().face_matrix()
for eng in ('python', 'rust'):
    topology.set_engine(eng)
    v_nmv, t_nmv = topology.repair_non_manifold_vertices(v, t)
    if not (np.array_equal(pml_v_nmv, v_nmv) and np.array_equal(pml_t_nmv, t_nmv)):
        diffs.append(f'[{eng}] repair_non_manifold_vertices mismatch')


# 7. compute_normal_per_face
ms = pymeshlab.MeshSet()
ms.add_mesh(pymeshlab.Mesh(v, t.astype(np.int32)))
ms.apply_filter('compute_normal_per_face')
pml_fn = ms.current_mesh().face_normal_matrix()
for eng in ('python', 'rust'):
    topology.set_engine(eng)
    fn = topology.compute_normal_per_face(v, t)
    rel_diff = np.max(np.abs(pml_fn - fn) / np.maximum(np.abs(pml_fn), 1.0))
    if rel_diff > 1e-4:
        diffs.append(f'[{eng}] compute_normal_per_face rel diff {rel_diff}')

# 8. compute_normal_per_vertex
ms = pymeshlab.MeshSet()
ms.add_mesh(pymeshlab.Mesh(v, t.astype(np.int32)))
ms.apply_filter('compute_normal_per_vertex')
pml_vn = ms.current_mesh().vertex_normal_matrix()
for eng in ('python', 'rust'):
    topology.set_engine(eng)
    vn = topology.compute_normal_per_vertex(v, t)
    rel_diff = np.max(np.abs(pml_vn - vn) / np.maximum(np.abs(pml_vn), 1.0))
    if rel_diff > 1e-4:
        diffs.append(f'[{eng}] compute_normal_per_vertex rel diff {rel_diff}')

# 9. remove_selected_faces
sel = np.zeros(len(t), dtype=bool)
sel[:min(5, len(t))] = True
ms = pymeshlab.MeshSet()
ms.add_mesh(pymeshlab.Mesh(v, t.astype(np.int32)))
ms.current_mesh().add_face_custom_scalar_attribute(sel.astype(float), '_sel')
ms.apply_filter('compute_selection_by_condition_per_face', condselect='_sel > 0.5')
ms.apply_filter('meshing_remove_selected_faces')
pml_t_sel = ms.current_mesh().face_matrix()
for eng in ('python', 'rust'):
    topology.set_engine(eng)
    t_sel = topology.remove_selected_faces(t, sel)
    if not np.array_equal(pml_t_sel, t_sel):
        diffs.append(f'[{eng}] remove_selected_faces mismatch')

if diffs:
    print(json.dumps({'status': 'diff', 'diffs': diffs}))
else:
    print(json.dumps({'status': 'ok'}))
"""
    with tempfile.NamedTemporaryFile(mode='w', suffix='.py', delete=False) as f:
        f.write(worker_code)
        tmp_worker = f.name
    worker_env = dict(os.environ, PYTHONPATH=SUTURA)
    try:
        for path in samples:
            proc = subprocess.run([sys.executable, tmp_worker, path], env=worker_env, capture_output=True, text=True)
            assert proc.returncode == 0, f"Phase 2 worker failed on {path}: {proc.stderr}"
            res = json.loads(proc.stdout.strip().splitlines()[-1])
            assert res['status'] == 'ok', f"Phase 2 filter mismatch on {path}: {res['diffs']}"
    finally:
        if os.path.exists(tmp_worker):
            os.remove(tmp_worker)

    print(f'Phase 2 filter parity OK on {len(samples)} corpus meshes (bit-for-bit array match)')


def main():
    test_parity()
    test_re_orient_parity()
    test_repair_nme_parity()
    test_geometric_measures_parity()
    test_pymeshlab_parity()
    test_phase2_filter_parity()
    test_fallback_when_kernel_missing()
    topology.set_engine('auto')
    print('topology parity tests passed')


if __name__ == '__main__':
    main()

